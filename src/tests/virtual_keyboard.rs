//! Virtual keyboards (wtype, arbor-rdp, dictation) with their own keymap: the focused client
//! must get that keymap before the keys.

use std::io::{Read as _, Seek as _, Write as _};
use std::os::fd::{AsFd as _, OwnedFd};
use std::sync::{Arc, Mutex};

use smithay::input::keyboard::xkb;
use wayland_client::protocol::wl_keyboard::{self, WlKeyboard};
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::protocol::wl_seat::{self, WlSeat};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1;
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1;

use super::client::State;
use super::*;

/// Marker user data so these impls don't clash with other tests' ones.
#[derive(Clone, Default)]
struct Vk(Arc<Mutex<Seen>>);

#[derive(Default)]
struct Seen {
    seat: Option<WlSeat>,
    vkm: Option<ZwpVirtualKeyboardManagerV1>,
    /// Events on our wl_keyboard: keymap contents and keys, in order.
    events: Vec<Ev>,
}

#[derive(Debug, Clone)]
enum Ev {
    Keymap(String),
    Key(u32, bool),
}

impl Dispatch<WlRegistry, Vk> for State {
    fn event(
        _: &mut Self,
        registry: &WlRegistry,
        event: wl_registry::Event,
        data: &Vk,
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            let mut seen = data.0.lock().unwrap();
            if interface == WlSeat::interface().name {
                seen.seat = Some(registry.bind(name, version.min(7), qh, data.clone()));
            } else if interface == ZwpVirtualKeyboardManagerV1::interface().name {
                seen.vkm = Some(registry.bind(name, 1, qh, data.clone()));
            }
        }
    }
}

impl Dispatch<WlSeat, Vk> for State {
    fn event(
        _: &mut Self,
        _: &WlSeat,
        _: wl_seat::Event,
        _: &Vk,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

fn read_fd(fd: OwnedFd, size: u32) -> String {
    let mut file = std::fs::File::from(fd);
    file.rewind().unwrap();
    let mut buf = vec![0; size as usize];
    file.read_exact(&mut buf).unwrap();
    let s = String::from_utf8_lossy(&buf).into_owned();
    s.trim_end_matches('\0').to_owned()
}

impl Dispatch<WlKeyboard, Vk> for State {
    fn event(
        _: &mut Self,
        _: &WlKeyboard,
        event: wl_keyboard::Event,
        data: &Vk,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let mut seen = data.0.lock().unwrap();
        match event {
            wl_keyboard::Event::Keymap { fd, size, .. } => {
                seen.events.push(Ev::Keymap(read_fd(fd, size)))
            }
            wl_keyboard::Event::Key { key, state, .. } => seen.events.push(Ev::Key(
                key,
                state == WEnum::Value(wl_keyboard::KeyState::Pressed),
            )),
            _ => (),
        }
    }
}

impl Dispatch<ZwpVirtualKeyboardManagerV1, Vk> for State {
    fn event(
        _: &mut Self,
        _: &ZwpVirtualKeyboardManagerV1,
        _: <ZwpVirtualKeyboardManagerV1 as Proxy>::Event,
        _: &Vk,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpVirtualKeyboardV1, Vk> for State {
    fn event(
        _: &mut Self,
        _: &ZwpVirtualKeyboardV1,
        _: <ZwpVirtualKeyboardV1 as Proxy>::Event,
        _: &Vk,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

/// A wtype-style keymap: keycode 9 (evdev 1) types "h".
const WTYPE_KEYMAP: &str = r#"xkb_keymap {
xkb_keycodes "(unnamed)" {
minimum = 8;
maximum = 10;
<K1> = 9;
<K2> = 10;
};
xkb_types "(unnamed)" { include "complete" };
xkb_compatibility "(unnamed)" { include "complete" };
xkb_symbols "(unnamed)" {
key <K1> {[h]};
key <K2> {[i]};
};
};
"#;

fn keysym_for(keymap: &str, evdev: u32) -> xkb::Keysym {
    let ctx = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
    let keymap = xkb::Keymap::new_from_string(
        &ctx,
        keymap.to_owned(),
        xkb::KEYMAP_FORMAT_TEXT_V1,
        xkb::KEYMAP_COMPILE_NO_FLAGS,
    )
    .expect("keymap compiles");
    let state = xkb::State::new(&keymap);
    state.key_get_one_sym((evdev + 8).into())
}

/// The keysym the focused client computes for each key press, using the last keymap it got.
fn typed(events: &[Ev]) -> Vec<xkb::Keysym> {
    let mut keymap = None;
    let mut out = Vec::new();
    for ev in events {
        match ev {
            Ev::Keymap(k) => keymap = Some(k.clone()),
            Ev::Key(key, true) => out.push(keysym_for(keymap.as_ref().unwrap(), *key)),
            Ev::Key(_, false) => (),
        }
    }
    out
}

#[test]
fn virtual_keyboard_keys_use_its_keymap() {
    let mut f = Fixture::new();
    f.add_output(1, (1920, 1080));

    // The app: a focused window with a wl_keyboard.
    let app = f.add_client();
    let app_vk = Vk::default();
    {
        let client = f.client(app);
        client.display.get_registry(&client.qh, app_vk.clone());
    }
    f.double_roundtrip(app);
    {
        let seat = app_vk.0.lock().unwrap().seat.clone().unwrap();
        let client = f.client(app);
        seat.get_keyboard(&client.qh, app_vk.clone());
    }
    let window = f.client(app).create_window();
    let surface = window.surface.clone();
    window.commit();
    f.roundtrip(app);
    let window = f.client(app).window(&surface);
    window.attach_new_buffer();
    window.set_size(100, 100);
    window.ack_last_and_commit();
    f.double_roundtrip(app);

    // wtype: a virtual keyboard with its own keymap.
    let wtype = f.add_client();
    let w = Vk::default();
    {
        let client = f.client(wtype);
        client.display.get_registry(&client.qh, w.clone());
    }
    f.double_roundtrip(wtype);
    let (seat, vkm) = {
        let seen = w.0.lock().unwrap();
        (seen.seat.clone().unwrap(), seen.vkm.clone().unwrap())
    };
    let client = f.client(wtype);
    let vk = vkm.create_virtual_keyboard(&seat, &client.qh, w.clone());
    let fd = smithay::reexports::rustix::fs::memfd_create(
        "keymap",
        smithay::reexports::rustix::fs::MemfdFlags::CLOEXEC,
    )
    .unwrap();
    let mut file = std::fs::File::from(fd);
    file.write_all(WTYPE_KEYMAP.as_bytes()).unwrap();
    file.write_all(b"\0").unwrap();
    vk.keymap(1, file.as_fd(), WTYPE_KEYMAP.len() as u32 + 1);
    // "h", "i"
    vk.key(0, 1, 1);
    vk.key(1, 1, 0);
    vk.key(2, 2, 1);
    vk.key(3, 2, 0);
    f.double_roundtrip(wtype);
    f.double_roundtrip(app);

    let events = app_vk.0.lock().unwrap().events.clone();
    let got = typed(&events);
    assert_eq!(
        got,
        [xkb::Keysym::h, xkb::Keysym::i],
        "events: {:?}",
        events
            .iter()
            .map(|e| match e {
                Ev::Keymap(k) => format!("keymap({} bytes)", k.len()),
                e => format!("{e:?}"),
            })
            .collect::<Vec<_>>()
    );

    // Real keyboard input afterwards uses the seat keymap again.
    let n = events.len();
    let kbd = f.arbor().seat.get_keyboard().unwrap();
    kbd.input::<(), _>(
        f.arbor_state(),
        smithay::backend::input::Keycode::new(30 + 8),
        smithay::backend::input::KeyState::Pressed,
        0.into(),
        0,
        |_, _, _| smithay::input::keyboard::FilterResult::Forward,
    );
    f.double_roundtrip(app);
    let events = app_vk.0.lock().unwrap().events.clone();
    let got = typed(&events[..]);
    assert_eq!(
        got.last(),
        Some(&xkb::Keysym::a),
        "{:?}",
        &events[n..].len()
    );
}

fn open_app(f: &mut Fixture) -> (super::client::ClientId, Vk) {
    let app = f.add_client();
    let vkd = Vk::default();
    {
        let client = f.client(app);
        client.display.get_registry(&client.qh, vkd.clone());
    }
    f.double_roundtrip(app);
    {
        let seat = vkd.0.lock().unwrap().seat.clone().unwrap();
        let client = f.client(app);
        seat.get_keyboard(&client.qh, vkd.clone());
    }
    let window = f.client(app).create_window();
    let surface = window.surface.clone();
    window.commit();
    f.roundtrip(app);
    let window = f.client(app).window(&surface);
    window.attach_new_buffer();
    window.set_size(100, 100);
    window.ack_last_and_commit();
    f.double_roundtrip(app);
    (app, vkd)
}

/// Runs a wtype-like client: a fresh virtual keyboard with `WTYPE_KEYMAP`, typing "h".
fn wtype_h(f: &mut Fixture) {
    let wtype = f.add_client();
    let w = Vk::default();
    {
        let client = f.client(wtype);
        client.display.get_registry(&client.qh, w.clone());
    }
    f.double_roundtrip(wtype);
    let (seat, vkm) = {
        let seen = w.0.lock().unwrap();
        (seen.seat.clone().unwrap(), seen.vkm.clone().unwrap())
    };
    let client = f.client(wtype);
    let vk = vkm.create_virtual_keyboard(&seat, &client.qh, w.clone());
    let fd = smithay::reexports::rustix::fs::memfd_create(
        "keymap",
        smithay::reexports::rustix::fs::MemfdFlags::CLOEXEC,
    )
    .unwrap();
    let mut file = std::fs::File::from(fd);
    file.write_all(WTYPE_KEYMAP.as_bytes()).unwrap();
    file.write_all(b"\0").unwrap();
    vk.keymap(1, file.as_fd(), WTYPE_KEYMAP.len() as u32 + 1);
    vk.key(0, 1, 1);
    vk.key(1, 1, 0);
    vk.destroy();
    f.double_roundtrip(wtype);
}

/// A client whose wl_keyboard appears while a virtual keymap is active (the app started after
/// an earlier wtype run) still gets the virtual keymap for the next run's keys.
#[test]
fn new_keyboard_after_virtual_keymap_gets_it() {
    let mut f = Fixture::new();
    f.add_output(1, (1920, 1080));
    let (first, first_vk) = open_app(&mut f);
    wtype_h(&mut f);
    f.double_roundtrip(first);
    assert_eq!(typed(&first_vk.0.lock().unwrap().events), [xkb::Keysym::h]);

    // A new app opens (and takes focus) with the virtual keymap still active.
    let (second, second_vk) = open_app(&mut f);
    wtype_h(&mut f);
    f.double_roundtrip(second);
    let events = second_vk.0.lock().unwrap().events.clone();
    assert_eq!(typed(&events), [xkb::Keysym::h]);
}
