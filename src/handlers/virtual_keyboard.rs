//! Virtual keyboards (`zwp_virtual_keyboard_v1`: wtype, arbor-rdp, dictation tools).
//!
//! smithay switches every wl_keyboard to a virtual keyboard's keymap when that keymap differs
//! from the active one, but a wl_keyboard created while a virtual keymap is active gets the
//! seat keymap and is never told about the virtual one when a virtual keyboard with the same
//! keymap types again (an app started after one wtype run, then wtype again: "h" arrives as
//! Escape). So before forwarding a virtual key, arbor sends the keymap itself to the focused
//! client's keyboards that haven't had it.

use std::collections::{HashMap, HashSet};
use std::io::Write as _;
use std::os::fd::AsFd as _;
use std::os::unix::fs::FileExt as _;
use std::sync::Arc;

use smithay::reexports::rustix;
use smithay::reexports::wayland_protocols_misc::zwp_virtual_keyboard_v1::server::zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1;
use smithay::reexports::wayland_protocols_misc::zwp_virtual_keyboard_v1::server::zwp_virtual_keyboard_v1::{
    self, ZwpVirtualKeyboardV1,
};
use smithay::reexports::wayland_server::backend::{ClientId, ObjectId};
use smithay::reexports::wayland_server::protocol::wl_keyboard::KeymapFormat;
use smithay::reexports::wayland_server::{
    delegate_dispatch, delegate_global_dispatch, Client, DataInit, Dispatch, DisplayHandle,
    Resource,
};
use smithay::wayland::virtual_keyboard::{
    VirtualKeyboardManagerGlobalData, VirtualKeyboardManagerState, VirtualKeyboardUserData,
};

use crate::arbor::State;

/// Keymaps of virtual keyboards, and which wl_keyboards have the active one.
#[derive(Debug, Default)]
pub struct VirtualKeymaps {
    /// Keymap text (as sent, `size` bytes) per virtual keyboard.
    keymaps: HashMap<ObjectId, Arc<Vec<u8>>>,
    /// The virtual keymap that smithay last made active, and the wl_keyboards known to have it.
    active: Option<(Arc<Vec<u8>>, HashSet<ObjectId>)>,
}

delegate_global_dispatch!(State: [ZwpVirtualKeyboardManagerV1: VirtualKeyboardManagerGlobalData] => VirtualKeyboardManagerState);
delegate_dispatch!(State: [ZwpVirtualKeyboardManagerV1: ()] => VirtualKeyboardManagerState);

type Inner = VirtualKeyboardManagerState;

impl Dispatch<ZwpVirtualKeyboardV1, VirtualKeyboardUserData<State>> for State {
    fn request(
        state: &mut Self,
        client: &Client,
        resource: &ZwpVirtualKeyboardV1,
        request: zwp_virtual_keyboard_v1::Request,
        data: &VirtualKeyboardUserData<State>,
        dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match &request {
            zwp_virtual_keyboard_v1::Request::Keymap { fd, size, .. } => {
                let mut buf = vec![0; *size as usize];
                let file = fd.try_clone().map(std::fs::File::from);
                match file.and_then(|f| f.read_exact_at(&mut buf, 0)) {
                    Ok(()) => {
                        state
                            .arbor
                            .virtual_keymaps
                            .keymaps
                            .insert(resource.id(), Arc::new(buf));
                    }
                    Err(err) => debug!("error reading virtual keyboard keymap: {err:?}"),
                }
            }
            zwp_virtual_keyboard_v1::Request::Key { .. }
            | zwp_virtual_keyboard_v1::Request::Modifiers { .. } => {
                state.ensure_virtual_keymap_sent(resource);
            }
            _ => (),
        }

        <Inner as Dispatch<ZwpVirtualKeyboardV1, VirtualKeyboardUserData<State>, State>>::request(
            state, client, resource, request, data, dh, data_init,
        );
    }

    fn destroyed(
        state: &mut Self,
        client: ClientId,
        resource: &ZwpVirtualKeyboardV1,
        data: &VirtualKeyboardUserData<State>,
    ) {
        state.arbor.virtual_keymaps.keymaps.remove(&resource.id());
        <Inner as Dispatch<ZwpVirtualKeyboardV1, VirtualKeyboardUserData<State>, State>>::destroyed(
            state, client, resource, data,
        );
    }
}

impl State {
    fn ensure_virtual_keymap_sent(&mut self, vk: &ZwpVirtualKeyboardV1) {
        let Some(keymap) = self.arbor.virtual_keymaps.keymaps.get(&vk.id()).cloned() else {
            return;
        };
        let Some(keyboard) = self.arbor.seat.get_keyboard() else {
            return;
        };
        let focused_client = keyboard.current_focus().and_then(|focus| focus.client());
        let kbds: Vec<_> = focused_client
            .map(|client| keyboard.client_keyboards(&client).collect())
            .unwrap_or_default();

        let vks = &mut self.arbor.virtual_keymaps;
        match &mut vks.active {
            Some((active, sent)) if **active == *keymap => {
                // smithay considers this keymap active and won't resend it: send it to the
                // keyboards that came after.
                for kbd in &kbds {
                    if sent.contains(&kbd.id()) {
                        continue;
                    }
                    if let Err(err) = send_keymap(kbd, &keymap) {
                        warn!("error sending virtual keymap: {err:?}");
                        continue;
                    }
                    sent.insert(kbd.id());
                }
            }
            _ => {
                // A different keymap: smithay sends it to every keyboard.
                vks.active = Some((keymap, kbds.iter().map(|k| k.id()).collect()));
            }
        }
    }
}

fn send_keymap(
    kbd: &smithay::reexports::wayland_server::protocol::wl_keyboard::WlKeyboard,
    keymap: &[u8],
) -> std::io::Result<()> {
    let fd = rustix::fs::memfd_create("arbor-keymap", rustix::fs::MemfdFlags::CLOEXEC)?;
    let mut file = std::fs::File::from(fd);
    file.write_all(keymap)?;
    kbd.keymap(KeymapFormat::XkbV1, file.as_fd(), keymap.len() as u32);
    Ok(())
}
