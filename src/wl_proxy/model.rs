//! What the proxy remembers about one app's connection, enough to rebuild it in a new
//! compositor: every live object (interface, version, how it was created) and the requests that
//! set its state.
//!
//! Ids here are the app's (client) ids. Objects are keyed by a [`Key`] that grows with creation
//! order, because ids get reused while a destroyed object can still be needed (a destroyed
//! `wl_shm_pool` that live buffers were made from, say).

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::os::fd::OwnedFd;
use std::sync::Arc;

use wayland_backend::protocol::{AllowNull, ArgumentType, Interface, MessageDesc};

use super::interfaces;
use super::wire::{decode_args, DecodeError, Dir, Header, Msg, WireError, A, SERVER_ID_START};

pub type Key = u64;

/// A recorded argument: objects by key.
#[derive(Debug, Clone)]
pub enum V {
    Int(i32),
    Uint(u32),
    Fixed(i32),
    Str(Option<Vec<u8>>),
    Obj(Option<Key>),
    New(Key),
    Array(Vec<u8>),
    Fd(Arc<OwnedFd>),
}

/// A recorded request (on the object that holds it).
#[derive(Debug, Clone)]
pub struct Rec {
    pub opcode: u16,
    pub args: Vec<V>,
}

impl Rec {
    pub fn refs(&self) -> impl Iterator<Item = Key> + '_ {
        self.args.iter().filter_map(|a| match a {
            V::Obj(Some(k)) => Some(*k),
            _ => None,
        })
    }
}

#[derive(Debug, Clone)]
pub enum Creation {
    Display,
    /// The request (sent on the parent) that created it.
    Request(Rec),
    /// Created by an event of the parent.
    Event,
}

#[derive(Debug)]
pub struct Obj {
    pub id: u32,
    pub iface: &'static Interface,
    pub version: u32,
    pub parent: Option<Key>,
    pub creation: Creation,
    pub alive: bool,
    /// Whether the current compositor knows this object. Requests on objects it doesn't know
    /// (ones that couldn't be carried over) are dropped.
    pub on_server: bool,
    /// Last value of each setting request (by request name).
    pub sets: BTreeMap<&'static str, Rec>,
    /// Requests that add up (region rectangles, dmabuf planes).
    pub accum: Vec<Rec>,
    /// wl_buffer: held by the compositor (committed and not released).
    pub busy: bool,
    /// wl_surface: attached since the last commit.
    pub attach_pending: bool,
    /// xdg_surface / layer surface: configure serials from this compositor not yet acked.
    pub serials: Vec<u32>,
    /// zwp_text_input_v3: commits so far (the compositor's `done` serial counts them, and the
    /// app ignores a `done` whose serial isn't its own count).
    pub commits: u32,
    /// xdg_activation_token_v1: committed, its token not sent yet.
    /// zwlr_screencopy_frame_v1: neither ready nor failed yet.
    pub awaiting: bool,
    /// zxdg_exported_v2: the handle the app got first (later compositors' handles are aliases
    /// of it, see `Hub::aliases`). zwlr_screencopy_frame_v1: set (empty) once the app asked
    /// for the copy.
    pub handle: Option<Vec<u8>>,
    /// zwlr_screencopy_frame_v1: the shm buffer the compositor asked for (format, width,
    /// height, stride), which the app's buffer matches.
    pub frame_buffer: Option<Vec<u32>>,
    /// Rebuilt by the proxy in the current compositor (not created by the app there).
    pub rebuilt: bool,
    /// Foreign-toplevel handles: app_id, title, identifier (for matching the new compositor's
    /// handle of the same window).
    pub meta: Vec<(&'static str, Vec<u8>)>,
}

/// How a request is remembered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Create,
    Destroy,
    /// Last one wins.
    Set(&'static str),
    Accum,
    /// Not replayed (actions like move or resize).
    Skip,
    Special,
}

/// Interfaces (besides globals) whose objects are rebuilt in a new compositor. Everything else is
/// told it ended (see [`ENDINGS`]) or just stops getting events.
const REPLAYABLE: &[&str] = &[
    "wl_display",
    "wl_registry",
    "wl_callback",
    "wl_surface",
    "wl_region",
    "wl_subsurface",
    "wl_shm_pool",
    "wl_buffer",
    "wl_pointer",
    "wl_keyboard",
    "wl_touch",
    "wl_data_device",
    "xdg_positioner",
    "xdg_surface",
    "xdg_toplevel",
    "zxdg_toplevel_decoration_v1",
    "org_kde_kwin_server_decoration",
    "wp_viewport",
    "wp_fractional_scale_v1",
    "zwp_linux_buffer_params_v1",
    "zwp_linux_dmabuf_feedback_v1",
    "zxdg_output_v1",
    "wp_cursor_shape_device_v1",
    // Stage 2.
    "zwlr_layer_surface_v1",
    "zwp_text_input_v3",
    "zwp_primary_selection_device_v1",
    "zwlr_data_control_device_v1",
    "ext_data_control_device_v1",
    "zwp_locked_pointer_v1",
    "zwp_confined_pointer_v1",
    "zwp_relative_pointer_v1",
    "zwp_pointer_gesture_swipe_v1",
    "zwp_pointer_gesture_pinch_v1",
    "zwp_pointer_gesture_hold_v1",
    "zwp_idle_inhibitor_v1",
    "zwp_keyboard_shortcuts_inhibitor_v1",
    "ext_idle_notification_v1",
    "zwp_tablet_seat_v2",
    "gtk_surface1",
    "org_kde_kwin_appmenu",
    "ext_background_effect_surface_v1",
    "zwp_virtual_keyboard_v1",
    "zwlr_virtual_pointer_v1",
    "zxdg_exported_v2",
    "zxdg_imported_v2",
    "zwp_input_method_v2",
    "zwp_input_method_keyboard_grab_v2",
    "zwp_input_popup_surface_v2",
    "zwlr_gamma_control_v1",
];

/// Selection sources: rebuilt only while they hold a selection (a device's last
/// `set_selection` / `set_primary_selection` names them).
const SOURCES: &[&str] = &[
    "wl_data_source",
    "zwp_primary_selection_source_v1",
    "zwlr_data_control_source_v1",
    "ext_data_control_source_v1",
];

/// Screen and window copies: redone in the new compositor while in flight.
pub const FRAMES: &[&str] = &[
    "zwlr_screencopy_frame_v1",
    "hyprland_toplevel_export_frame_v1",
];

/// Foreign-toplevel handles: matched to the new compositor's handle of the same window.
pub const HANDLES: &[&str] = &[
    "zwlr_foreign_toplevel_handle_v1",
    "ext_foreign_toplevel_handle_v1",
];

/// Role objects whose surface gets a first commit without a buffer and a configure to ack.
pub const CONFIGURED_ROLES: &[&str] = &["xdg_surface", "zwlr_layer_surface_v1"];

/// Events (without arguments) that tell an app an object it holds is over, sent for objects
/// that aren't carried over to the new compositor.
const ENDINGS: &[&str] = &[
    "popup_done",
    "cancelled",
    "closed",
    "finished",
    "removed",
    "discarded",
    "failed",
];

pub fn kind(iface: &str, desc: &MessageDesc) -> Kind {
    if desc.is_destructor {
        return Kind::Destroy;
    }
    if desc.signature.contains(&ArgumentType::NewId) {
        return Kind::Create;
    }
    let n = desc.name;
    match (iface, n) {
        ("wl_surface", "commit") => Kind::Special,
        (
            "wl_surface",
            "attach"
            | "offset"
            | "set_opaque_region"
            | "set_input_region"
            | "set_buffer_transform"
            | "set_buffer_scale",
        ) => Kind::Set(n),
        ("wl_region", "add" | "subtract") => Kind::Accum,
        ("wl_shm_pool", "resize") => Kind::Set(n),
        ("zwp_linux_buffer_params_v1", "add") => Kind::Accum,
        ("zwp_linux_buffer_params_v1", "create") => Kind::Set(n),
        ("wl_subsurface", "set_position") => Kind::Set(n),
        ("wl_subsurface", "place_above" | "place_below") => Kind::Accum,
        ("wl_subsurface", "set_sync" | "set_desync") => Kind::Set("sync"),
        ("xdg_positioner", _) => Kind::Set(n),
        ("xdg_surface", "set_window_geometry") => Kind::Set(n),
        ("xdg_surface" | "zwlr_layer_surface_v1", "ack_configure") => Kind::Special,
        (
            "zwlr_layer_surface_v1",
            "set_size"
            | "set_anchor"
            | "set_exclusive_zone"
            | "set_margin"
            | "set_keyboard_interactivity"
            | "set_layer"
            | "set_exclusive_edge",
        ) => Kind::Set(n),
        ("zwp_text_input_v3", "commit") => Kind::Special,
        ("xdg_activation_token_v1", "commit") => Kind::Special,
        ("wp_security_context_v1", "set_sandbox_engine" | "set_app_id" | "set_instance_id") => {
            Kind::Set(n)
        }
        ("xdg_activation_token_v1", "set_serial" | "set_app_id" | "set_surface") => Kind::Set(n),
        (
            "wl_data_device"
            | "zwp_primary_selection_device_v1"
            | "zwlr_data_control_device_v1"
            | "ext_data_control_device_v1",
            "set_selection" | "set_primary_selection",
        ) => Kind::Set(n),
        (
            "wl_data_source"
            | "zwp_primary_selection_source_v1"
            | "zwlr_data_control_source_v1"
            | "ext_data_control_source_v1",
            "offer",
        ) => Kind::Accum,
        ("wl_data_source", "set_actions") => Kind::Set(n),
        ("zwp_locked_pointer_v1", "set_cursor_position_hint" | "set_region") => Kind::Set(n),
        ("zwp_confined_pointer_v1", "set_region") => Kind::Set(n),
        ("gtk_surface1", "set_dbus_properties") => Kind::Set(n),
        ("gtk_surface1", "set_modal" | "unset_modal") => Kind::Set("modal"),
        ("org_kde_kwin_appmenu", "set_address") => Kind::Set(n),
        ("ext_background_effect_surface_v1", "set_blur_region") => Kind::Set(n),
        ("zwp_virtual_keyboard_v1", "keymap") => Kind::Set(n),
        ("zxdg_imported_v2", "set_parent_of") => Kind::Set(n),
        ("zwlr_gamma_control_v1", "set_gamma") => Kind::Set(n),
        ("zwlr_screencopy_frame_v1", "copy" | "copy_with_damage") => Kind::Set("copy"),
        ("hyprland_toplevel_export_frame_v1", "copy") => Kind::Set("copy"),
        (
            "xdg_toplevel",
            "set_parent" | "set_title" | "set_app_id" | "set_max_size" | "set_min_size",
        ) => Kind::Set(n),
        ("zxdg_toplevel_decoration_v1", "set_mode" | "unset_mode") => Kind::Set("mode"),
        ("org_kde_kwin_server_decoration", "request_mode") => Kind::Set(n),
        ("wp_viewport", "set_source" | "set_destination") => Kind::Set(n),
        _ => Kind::Skip,
    }
}

pub fn request_by_name(iface: &Interface, name: &str) -> Option<u16> {
    iface
        .requests
        .iter()
        .position(|d| d.name == name)
        .map(|i| i as u16)
}

pub fn event_by_name(iface: &Interface, name: &str) -> Option<u16> {
    iface
        .events
        .iter()
        .position(|d| d.name == name)
        .map(|i| i as u16)
}

#[derive(Debug)]
pub struct Model {
    pub objs: BTreeMap<Key, Obj>,
    /// Live objects by client id.
    pub by_id: HashMap<u32, Key>,
    /// Interface and version of each id the client may still see messages for (live objects and
    /// destroyed ones whose id isn't reused yet), for decoding.
    ifaces: HashMap<u32, (&'static Interface, u32)>,
    next_key: Key,
    /// Globals as the client knows them: name -> (interface, version).
    pub globals: BTreeMap<u32, (String, u32)>,
    /// Globals the proxy can't decode, kept from the client.
    hidden: HashSet<u32>,
    /// Configures the proxy acked itself while rebuilding (xdg_surface key, serial): the app's
    /// own ack of them is dropped.
    pub proxy_acked: HashSet<(Key, u32)>,
    /// wl_pointer / wl_keyboard -> the surface it's in.
    pub focus: HashMap<Key, Key>,
    /// After the first compositor change: acks of serials this compositor didn't send are
    /// dropped.
    pub swapped: bool,
    /// A protocol error the compositor sent.
    pub error: Option<String>,
    /// One past the highest compositor-made id the client has seen.
    pub server_end: u32,
}

impl Default for Model {
    fn default() -> Self {
        let display = &interfaces::wl_display();
        let mut m = Self {
            objs: BTreeMap::new(),
            by_id: HashMap::new(),
            ifaces: HashMap::new(),
            next_key: 1,
            globals: BTreeMap::new(),
            hidden: HashSet::new(),
            proxy_acked: HashSet::new(),
            focus: HashMap::new(),
            swapped: false,
            error: None,
            server_end: SERVER_ID_START,
        };
        m.insert(1, display, 1, None, Creation::Display, true);
        m
    }
}

fn is_nullable(ty: &ArgumentType) -> bool {
    matches!(ty, ArgumentType::Object(AllowNull::Yes))
}

impl Model {
    fn insert(
        &mut self,
        id: u32,
        iface: &'static Interface,
        version: u32,
        parent: Option<Key>,
        creation: Creation,
        on_server: bool,
    ) -> Key {
        let key = self.next_key;
        self.next_key += 1;
        self.objs.insert(
            key,
            Obj {
                id,
                iface,
                version,
                parent,
                creation,
                alive: true,
                on_server,
                sets: BTreeMap::new(),
                accum: Vec::new(),
                busy: false,
                attach_pending: false,
                serials: Vec::new(),
                commits: 0,
                awaiting: false,
                handle: None,
                frame_buffer: None,
                rebuilt: false,
                meta: Vec::new(),
            },
        );
        if let Some(old) = self.by_id.insert(id, key) {
            if let Some(o) = self.objs.get_mut(&old) {
                o.alive = false;
            }
        }
        self.ifaces.insert(id, (iface, version));
        key
    }

    pub fn live(&self, id: u32) -> Option<&Obj> {
        self.objs.get(self.by_id.get(&id)?)
    }

    pub fn key_of(&self, id: u32) -> Option<Key> {
        self.by_id.get(&id).copied()
    }

    pub fn iface_of(&self, id: u32) -> Option<&'static Interface> {
        self.ifaces.get(&id).map(|(i, _)| *i)
    }

    pub fn live_count(&self) -> usize {
        self.by_id.len()
    }

    /// Decodes a message to client id `object`.
    pub fn decode(
        &self,
        dir: Dir,
        header: Header,
        object: u32,
        body: &[u8],
        fds: &mut VecDeque<OwnedFd>,
    ) -> Result<(Msg, &'static Interface, &'static MessageDesc), WireError> {
        let iface = self
            .iface_of(object)
            .ok_or(WireError::UnknownObject { id: object, dir })?;
        let list = match dir {
            Dir::Request => iface.requests,
            Dir::Event => iface.events,
        };
        let desc = list
            .get(header.opcode as usize)
            .ok_or(WireError::BadOpcode {
                interface: iface.name,
                opcode: header.opcode,
                dir,
            })?;
        let args = decode_args(desc.signature, body, fds).map_err(|e| match e {
            DecodeError::Malformed => WireError::Truncated {
                interface: iface.name,
                message: desc.name,
            },
            DecodeError::MissingFd => WireError::MissingFd {
                interface: iface.name,
                message: desc.name,
            },
        })?;
        Ok((Msg::new(object, header.opcode, args), iface, desc))
    }

    fn record(&self, msg: &Msg, new_keys: &HashMap<u32, Key>) -> Rec {
        let args = msg
            .args
            .iter()
            .map(|a| match a {
                A::Int(v) => V::Int(*v),
                A::Uint(v) => V::Uint(*v),
                A::Fixed(v) => V::Fixed(*v),
                A::Str(s) => V::Str(s.clone()),
                A::Obj(0) => V::Obj(None),
                A::Obj(id) => V::Obj(self.key_of(*id)),
                A::New(id) => V::New(new_keys[id]),
                A::Array(a) => V::Array(a.clone()),
                // A dup the model keeps (out of fds: the connection's thread panics and only
                // this app loses its connection).
                A::Fd(fd) => V::Fd(Arc::new(fd.try_clone().expect("dup"))),
            })
            .collect();
        Rec {
            opcode: msg.opcode,
            args,
        }
    }

    /// Follows a request from the client. Returns whether to send it on, and events to send the
    /// client in its place. May null out arguments the compositor doesn't know.
    pub fn on_request(
        &mut self,
        msg: &mut Msg,
        iface: &'static Interface,
        desc: &'static MessageDesc,
    ) -> (bool, Vec<Msg>) {
        let mut synth = Vec::new();
        let Some(key) = self.key_of(msg.object) else {
            // A request to a destroyed object: the compositor doesn't know it either.
            return (false, synth);
        };
        let (on_server, version) = {
            let o = &self.objs[&key];
            (o.on_server, o.version)
        };

        // Objects the compositor doesn't know: null them if allowed, else the request can't go.
        let mut forward = on_server;
        for (arg, ty) in msg.args.iter_mut().zip(desc.signature) {
            if let A::Obj(id) = arg {
                if *id != 0 {
                    if let Some(o) = self.live(*id) {
                        if !o.on_server {
                            if is_nullable(ty) {
                                *id = 0;
                            } else {
                                forward = false;
                            }
                        }
                    }
                }
            }
        }
        if iface.name == "wl_registry" && desc.name == "bind" {
            if let Some(A::Uint(name)) = msg.args.first() {
                if !self.globals.contains_key(name) {
                    forward = false;
                }
            }
        }

        let kind = kind(iface.name, desc);
        let mut stale_ack = false;
        if kind == Kind::Special && desc.name == "commit" && iface.name == "zwp_text_input_v3" {
            self.objs.get_mut(&key).unwrap().commits += 1;
        }
        if kind == Kind::Special && desc.name == "commit" && iface.name == "xdg_activation_token_v1"
        {
            self.objs.get_mut(&key).unwrap().awaiting = true;
        }
        if FRAMES.contains(&iface.name) && kind == Kind::Set("copy") {
            self.objs.get_mut(&key).unwrap().handle = Some(Vec::new());
        }
        if kind == Kind::Special && desc.name == "ack_configure" {
            if let Some(A::Uint(serial)) = msg.args.first() {
                let serial = *serial;
                let o = self.objs.get_mut(&key).unwrap();
                if self.proxy_acked.remove(&(key, serial)) {
                    stale_ack = true;
                } else if let Some(pos) = o.serials.iter().position(|s| *s == serial) {
                    o.serials.drain(..=pos);
                } else if self.swapped {
                    stale_ack = true;
                }
            }
        }
        if stale_ack {
            forward = false;
        }

        // New objects.
        let mut new_keys = HashMap::new();
        for (i, arg) in msg.args.iter().enumerate() {
            let A::New(id) = *arg else { continue };
            let (child, ver) = match desc.child_interface {
                Some(child) => (child, version),
                None => {
                    // wl_registry.bind: (name, interface, version, id).
                    let (Some(A::Str(Some(name))), Some(A::Uint(v))) = (
                        msg.args.get(i.wrapping_sub(2)),
                        msg.args.get(i.wrapping_sub(1)),
                    ) else {
                        continue;
                    };
                    let Some(child) = interfaces::by_name(&String::from_utf8_lossy(name)) else {
                        continue;
                    };
                    (child, *v)
                }
            };
            let k = self.insert(id, child, ver, Some(key), Creation::Event, forward);
            if FRAMES.contains(&child.name) {
                self.objs.get_mut(&k).unwrap().awaiting = true;
            }
            new_keys.insert(id, k);
        }

        match kind {
            Kind::Create => {
                let rec = self.record(msg, &new_keys);
                for k in new_keys.values() {
                    self.objs.get_mut(k).unwrap().creation = Creation::Request(rec.clone());
                }
            }
            Kind::Set(group) => {
                let rec = self.record(msg, &new_keys);
                let o = self.objs.get_mut(&key).unwrap();
                if group == "attach" {
                    o.attach_pending = true;
                }
                o.sets.insert(group, rec);
            }
            Kind::Accum => {
                let rec = self.record(msg, &new_keys);
                self.objs.get_mut(&key).unwrap().accum.push(rec);
            }
            Kind::Special if desc.name == "commit" && iface.name == "wl_surface" => {
                let o = self.objs.get_mut(&key).unwrap();
                if o.attach_pending {
                    o.attach_pending = false;
                    let buffer = o.sets.get("attach").and_then(|r| match r.args.first() {
                        Some(V::Obj(Some(b))) => Some(*b),
                        _ => None,
                    });
                    if let Some(b) = buffer.and_then(|b| self.objs.get_mut(&b)) {
                        b.busy = true;
                    }
                }
            }
            Kind::Destroy => {
                let o = self.objs.get_mut(&key).unwrap();
                o.alive = false;
                self.by_id.remove(&msg.object);
                self.focus.retain(|k, s| *k != key && *s != key);
                // The client waits for delete_id before reusing a client id; the compositor
                // won't send it for an object it doesn't know.
                if !on_server && msg.object < SERVER_ID_START {
                    synth.push(Msg::new(1, 1, vec![A::Uint(msg.object)]));
                }
            }
            _ => (),
        }
        (forward, synth)
    }

    /// Follows an event to the client (ids already the client's). Returns whether to pass it on.
    pub fn on_event(
        &mut self,
        msg: &Msg,
        iface: &'static Interface,
        desc: &'static MessageDesc,
    ) -> bool {
        let key = self.key_of(msg.object);

        for (i, arg) in msg.args.iter().enumerate() {
            let A::New(id) = *arg else { continue };
            let Some(child) = desc.child_interface else {
                continue;
            };
            let version = key.map_or(1, |k| self.objs[&k].version);
            let _ = i;
            if id >= SERVER_ID_START {
                self.server_end = self.server_end.max(id + 1);
            }
            self.insert(id, child, version, key, Creation::Event, true);
        }

        match (iface.name, desc.name) {
            ("wl_display", "error") => {
                if let [A::Obj(id), A::Uint(code), A::Str(text)] = &msg.args[..] {
                    let name = self.iface_of(*id).map_or("?", |i| i.name);
                    self.error = Some(format!(
                        "{name}#{id} code {code}: {}",
                        String::from_utf8_lossy(text.as_deref().unwrap_or_default())
                    ));
                }
            }
            ("wl_display", "delete_id") => {
                if let Some(A::Uint(id)) = msg.args.first() {
                    if !self.by_id.contains_key(id) {
                        self.ifaces.remove(id);
                    }
                }
            }
            ("wl_registry", "global") => {
                if let [A::Uint(name), A::Str(Some(i)), A::Uint(v)] = &msg.args[..] {
                    let i = String::from_utf8_lossy(i).into_owned();
                    if interfaces::by_name(&i).is_none() {
                        self.hidden.insert(*name);
                        return false;
                    }
                    self.globals.insert(*name, (i, *v));
                }
            }
            ("wl_registry", "global_remove") => {
                if let Some(A::Uint(name)) = msg.args.first() {
                    if self.hidden.remove(name) {
                        return false;
                    }
                    self.globals.remove(name);
                }
            }
            ("zxdg_exported_v2", "handle") => {
                if let (Some(o), Some(A::Str(Some(h)))) =
                    (key.and_then(|k| self.objs.get_mut(&k)), msg.args.first())
                {
                    o.handle.get_or_insert_with(|| h.clone());
                }
            }
            (
                "zwlr_foreign_toplevel_handle_v1" | "ext_foreign_toplevel_handle_v1",
                "app_id" | "title" | "identifier",
            ) => {
                if let (Some(o), Some(A::Str(Some(v)))) =
                    (key.and_then(|k| self.objs.get_mut(&k)), msg.args.first())
                {
                    o.meta.retain(|(n, _)| *n != desc.name);
                    o.meta.push((desc.name, v.clone()));
                }
            }
            ("zwlr_screencopy_frame_v1" | "hyprland_toplevel_export_frame_v1", "buffer") => {
                if let Some(o) = key
                    .and_then(|k| self.objs.get_mut(&k))
                    .filter(|o| o.frame_buffer.is_none())
                {
                    o.frame_buffer = Some(
                        msg.args
                            .iter()
                            .filter_map(|a| match a {
                                A::Uint(v) => Some(*v),
                                A::Int(v) => Some(*v as u32),
                                _ => None,
                            })
                            .collect(),
                    );
                }
            }
            (
                "zwlr_screencopy_frame_v1" | "hyprland_toplevel_export_frame_v1",
                "ready" | "failed",
            ) => {
                if let Some(o) = key.and_then(|k| self.objs.get_mut(&k)) {
                    o.awaiting = false;
                }
            }
            ("xdg_activation_token_v1", "done") => {
                if let Some(o) = key.and_then(|k| self.objs.get_mut(&k)) {
                    o.awaiting = false;
                }
            }
            ("wl_buffer", "release") => {
                if let Some(o) = key.and_then(|k| self.objs.get_mut(&k)) {
                    o.busy = false;
                }
            }
            ("xdg_surface" | "zwlr_layer_surface_v1", "configure") => {
                if let (Some(A::Uint(serial)), Some(o)) =
                    (msg.args.first(), key.and_then(|k| self.objs.get_mut(&k)))
                {
                    o.serials.push(*serial);
                }
            }
            ("wl_pointer" | "wl_keyboard" | "zwp_text_input_v3", "enter") => {
                let i = if iface.name == "zwp_text_input_v3" {
                    0
                } else {
                    1
                };
                if let (Some(k), Some(A::Obj(s))) = (key, msg.args.get(i)) {
                    if let Some(sk) = self.key_of(*s) {
                        self.focus.insert(k, sk);
                    }
                }
            }
            ("wl_pointer" | "wl_keyboard" | "zwp_text_input_v3", "leave") => {
                if let Some(k) = key {
                    self.focus.remove(&k);
                }
            }
            _ => (),
        }

        if desc.is_destructor {
            if let Some(k) = key {
                self.objs.get_mut(&k).unwrap().alive = false;
                self.by_id.remove(&msg.object);
            }
        }
        true
    }

    /// Refers to other apps' windows (imports, window copies, foreign-toplevel handles): its
    /// rebuild waits for the apps that own windows (`Hub::wait_owners`).
    pub fn is_observer(&self) -> bool {
        self.by_id.values().any(|k| {
            let o = &self.objs[k];
            o.alive
                && matches!(
                    o.iface.name,
                    "zxdg_imported_v2"
                        | "hyprland_toplevel_export_frame_v1"
                        | "zwlr_foreign_toplevel_manager_v1"
                        | "ext_foreign_toplevel_list_v1"
                )
        })
    }

    /// The handle's matching keys: (identifier) for ext handles, (app_id, title) for wlr ones.
    pub fn meta_of(&self, key: Key, name: &str) -> Option<&[u8]> {
        self.objs[&key]
            .meta
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, v)| v.as_slice())
    }

    /// Whether an object would be rebuilt in a new compositor (as far as can be told without
    /// the new compositor's globals).
    pub fn persists(&self, key: Key) -> bool {
        let o = &self.objs[&key];
        let parent_ok = |p: Option<Key>| p.is_none_or(|p| self.persists(p));
        match &o.creation {
            Creation::Display => true,
            Creation::Request(_) => {
                let global = o
                    .parent
                    .is_some_and(|p| self.objs[&p].iface.name == "wl_registry");
                (global || self.replayable_iface(key)) && parent_ok(o.parent) && self.role_ok(key)
            }
            // A buffer made by zwp_linux_buffer_params.create.
            Creation::Event => o.iface.name == "wl_buffer" && parent_ok(o.parent),
        }
    }

    /// Whether objects of this interface can be rebuilt at all (globals always can).
    pub fn replayable_iface(&self, key: Key) -> bool {
        let o = &self.objs[&key];
        let global = o.parent.is_some_and(|p| {
            self.objs
                .get(&p)
                .is_some_and(|p| p.iface.name == "wl_registry")
        });
        global
            || REPLAYABLE.contains(&o.iface.name)
            || (SOURCES.contains(&o.iface.name) && self.holds_selection(key))
            // A token or a screen copy the app is still waiting for: asked for again.
            || ((o.iface.name == "xdg_activation_token_v1" || FRAMES.contains(&o.iface.name))
                && o.awaiting)
    }

    /// A selection source some device's last set_selection / set_primary_selection names.
    pub fn holds_selection(&self, key: Key) -> bool {
        self.objs.values().any(|d| {
            d.alive
                && ["set_selection", "set_primary_selection"].iter().any(|n| {
                    d.sets.get(n).is_some_and(
                        |r| matches!(r.args.first(), Some(V::Obj(Some(k))) if *k == key),
                    )
                })
        })
    }

    /// An xdg_surface is only rebuilt with its toplevel (popups aren't).
    pub fn role_ok(&self, key: Key) -> bool {
        let o = &self.objs[&key];
        if o.iface.name != "xdg_surface" {
            return true;
        }
        self.objs
            .values()
            .any(|c| c.alive && c.parent == Some(key) && c.iface.name == "xdg_toplevel")
    }

    /// The compositor went away. Everything is now unknown to the server side; returns events
    /// that bring the app to a state the next compositor can continue from: focus left, popups
    /// and other objects that won't come back ended, the selection cleared, buffers the old
    /// compositor held released.
    pub fn detach(&mut self) -> Vec<Msg> {
        let mut out = Vec::new();
        self.swapped = true;
        self.proxy_acked.clear();

        // Focus.
        let mut focus: Vec<_> = self.focus.drain().collect();
        focus.sort();
        for (dev, surface) in focus {
            let (Some(d), Some(s)) = (self.objs.get(&dev), self.objs.get(&surface)) else {
                continue;
            };
            if !d.alive || !s.alive {
                continue;
            }
            if let Some(op) = event_by_name(d.iface, "leave") {
                let args = if d.iface.name == "zwp_text_input_v3" {
                    vec![A::Obj(s.id)]
                } else {
                    vec![A::Uint(0), A::Obj(s.id)]
                };
                out.push(Msg::new(d.id, op, args));
            }
            if d.iface.name == "wl_pointer" && d.version >= 5 {
                if let Some(op) = event_by_name(d.iface, "frame") {
                    out.push(Msg::new(d.id, op, vec![]));
                }
            }
        }

        // Buffers the surfaces show now stay held (the new compositor takes them over).
        let current: HashSet<Key> = self
            .objs
            .values()
            .filter(|o| o.alive && o.iface.name == "wl_surface")
            .filter_map(|o| match o.sets.get("attach")?.args.first()? {
                V::Obj(Some(b)) => Some(*b),
                _ => None,
            })
            .collect();

        let keys: Vec<Key> = self.by_id.values().copied().collect();
        let mut keys = keys;
        keys.sort();
        for key in keys {
            let persists = self.persists(key);
            let o = self.objs.get_mut(&key).unwrap();
            o.on_server = false;
            o.serials.clear();
            let iface = o.iface;
            let id = o.id;

            for name in ["selection", "primary_selection"] {
                if let Some(op) = event_by_name(iface, name) {
                    if let [ArgumentType::Object(AllowNull::Yes)] =
                        iface.events[op as usize].signature
                    {
                        out.push(Msg::new(id, op, vec![A::Obj(0)]));
                    }
                }
            }
            if iface.name == "wl_buffer" && o.busy && !current.contains(&key) {
                o.busy = false;
                out.push(Msg::new(id, 0, vec![]));
            }
            // Foreign-toplevel handles stay: the new compositor's handle of the same window takes
            // over (replay.rs); those of windows that don't come back are closed then.
            if !persists && !HANDLES.contains(&iface.name) {
                let ending = ENDINGS.iter().find_map(|name| {
                    let op = event_by_name(iface, name)?;
                    iface.events[op as usize].signature.is_empty().then_some(op)
                });
                if let Some(op) = ending {
                    out.push(Msg::new(id, op, vec![]));
                    if iface.events[op as usize].is_destructor {
                        o.alive = false;
                        self.by_id.remove(&id);
                    }
                }
            }
        }
        out
    }

    /// Forgets destroyed objects nothing needs any more.
    pub fn gc(&mut self) {
        if self.objs.len() < 2 * self.by_id.len() + 256 {
            return;
        }
        let mut keep: HashSet<Key> = HashSet::new();
        let mut stack: Vec<Key> = self.by_id.values().copied().collect();
        while let Some(k) = stack.pop() {
            if !keep.insert(k) {
                continue;
            }
            let Some(o) = self.objs.get(&k) else { continue };
            stack.extend(o.parent);
            if let Creation::Request(rec) = &o.creation {
                stack.extend(rec.refs());
            }
            for rec in o.sets.values().chain(&o.accum) {
                stack.extend(rec.refs());
            }
        }
        self.objs.retain(|k, _| keep.contains(k));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wl_proxy::wire::tests::string;
    use crate::wl_proxy::wire::Splitter;

    fn raw(object: u32, opcode: u16, args: &[u32]) -> Vec<u8> {
        let size = 8 + args.len() * 4;
        let mut out = Vec::new();
        out.extend_from_slice(&object.to_ne_bytes());
        out.extend_from_slice(&(((size as u32) << 16) | opcode as u32).to_ne_bytes());
        for a in args {
            out.extend_from_slice(&a.to_ne_bytes());
        }
        out
    }

    pub fn feed(m: &mut Model, dir: Dir, bytes: &[u8]) -> Vec<(bool, Vec<Msg>)> {
        let mut s = Splitter::default();
        s.push(bytes, vec![]);
        let mut out = Vec::new();
        while let Some((h, body)) = s.next_msg().unwrap() {
            let (mut msg, iface, desc) = m.decode(dir, h, h.object, &body, &mut s.fds).unwrap();
            match dir {
                Dir::Request => out.push(m.on_request(&mut msg, iface, desc)),
                Dir::Event => out.push((m.on_event(&msg, iface, desc), vec![])),
            }
        }
        out
    }

    fn bind(name: u32, iface: &str, version: u32, id: u32) -> Vec<u8> {
        let mut args = vec![name];
        args.extend(string(iface));
        args.extend([version, id]);
        raw(2, 0, &args)
    }

    fn global(name: u32, iface: &str, version: u32) -> Vec<u8> {
        let mut args = vec![name];
        args.extend(string(iface));
        args.push(version);
        raw(2, 0, &args)
    }

    /// A client with a toplevel: registry 2, compositor 3, wm_base 4, surface 5, xdg_surface 6,
    /// toplevel 7.
    fn toplevel() -> Model {
        let mut m = Model::default();
        feed(&mut m, Dir::Request, &raw(1, 1, &[2]));
        let mut ev = global(1, "wl_compositor", 6);
        ev.extend(global(2, "xdg_wm_base", 6));
        ev.extend(global(3, "some_unknown_protocol_v1", 1));
        let shown = feed(&mut m, Dir::Event, &ev);
        assert_eq!(
            shown.iter().map(|s| s.0).collect::<Vec<_>>(),
            [true, true, false]
        );
        let mut req = bind(1, "wl_compositor", 6, 3);
        req.extend(bind(2, "xdg_wm_base", 6, 4));
        req.extend(raw(3, 0, &[5]));
        req.extend(raw(4, 2, &[6, 5]));
        req.extend(raw(6, 1, &[7]));
        let mut title = vec![];
        title.extend(string("hi"));
        req.extend(raw(7, 2, &title));
        feed(&mut m, Dir::Request, &req);
        m
    }

    #[test]
    fn records_state() {
        let m = toplevel();
        let top = m.live(7).unwrap();
        assert_eq!(top.iface.name, "xdg_toplevel");
        assert!(top.sets.contains_key("set_title"));
        assert!(m.persists(m.key_of(6).unwrap()));
        assert_eq!(m.globals.len(), 2);
    }

    #[test]
    fn stale_acks_are_dropped_after_a_swap() {
        let mut m = toplevel();
        // xdg_surface.configure(serial 10)
        feed(&mut m, Dir::Event, &raw(6, 0, &[10]));
        m.detach();
        // The old configure's ack can't go to the new compositor.
        let r = feed(&mut m, Dir::Request, &raw(6, 4, &[10]));
        assert!(!r[0].0);
        // A new one can.
        for o in m.objs.values_mut() {
            o.on_server = true;
        }
        feed(&mut m, Dir::Event, &raw(6, 0, &[20]));
        let r = feed(&mut m, Dir::Request, &raw(6, 4, &[20]));
        assert!(r[0].0);
    }

    #[test]
    fn orphans_get_delete_id() {
        let mut m = toplevel();
        // xdg_wm_base.create_positioner(8), then the compositor goes away.
        feed(&mut m, Dir::Request, &raw(4, 1, &[8]));
        m.detach();
        // Positioners persist; a popup wouldn't: get_popup(9, parent 6, positioner 8) on 6.
        // Here: destroy the positioner while nothing is on the server: delete_id comes back.
        let r = feed(&mut m, Dir::Request, &raw(8, 0, &[]));
        assert!(!r[0].0);
        assert_eq!(r[0].1.len(), 1);
        assert!(matches!(r[0].1[0].args[0], A::Uint(8)));
    }

    #[test]
    fn detach_ends_popups_and_releases_buffers() {
        let mut m = toplevel();
        // A popup: surface 9, xdg_surface 10, positioner 11, popup 12.
        let mut req = raw(3, 0, &[9]);
        req.extend(raw(4, 2, &[10, 9]));
        req.extend(raw(4, 1, &[11]));
        req.extend(raw(10, 2, &[12, 6, 11]));
        feed(&mut m, Dir::Request, &req);
        let synth = m.detach();
        let popup_done = synth.iter().find(|s| s.object == 12).unwrap();
        assert_eq!(popup_done.opcode, 1);
        assert!(!m.persists(m.key_of(10).unwrap()));
        assert!(m.persists(m.key_of(6).unwrap()));
    }

    /// A request by name (opcode looked up) with plain u32 args.
    fn req(m: &mut Model, id: u32, name: &str, args: &[u32]) -> (bool, Vec<Msg>) {
        let iface = m.iface_of(id).unwrap();
        let op = request_by_name(iface, name).unwrap();
        feed(m, Dir::Request, &raw(id, op, args)).remove(0)
    }

    #[test]
    fn text_input_commits_are_counted() {
        let mut m = toplevel();
        let mut ev = global(4, "wl_seat", 8);
        ev.extend(global(5, "zwp_text_input_manager_v3", 1));
        feed(&mut m, Dir::Event, &ev);
        feed(&mut m, Dir::Request, &bind(4, "wl_seat", 8, 20));
        feed(
            &mut m,
            Dir::Request,
            &bind(5, "zwp_text_input_manager_v3", 1, 21),
        );
        req(&mut m, 21, "get_text_input", &[22, 20]);
        for _ in 0..3 {
            req(&mut m, 22, "commit", &[]);
        }
        assert_eq!(m.live(22).unwrap().commits, 3);
        assert!(m.persists(m.key_of(22).unwrap()));
    }

    #[test]
    fn only_the_selection_source_persists() {
        let mut m = toplevel();
        let mut ev = global(4, "wl_seat", 8);
        ev.extend(global(5, "wl_data_device_manager", 3));
        feed(&mut m, Dir::Event, &ev);
        feed(&mut m, Dir::Request, &bind(4, "wl_seat", 8, 20));
        feed(
            &mut m,
            Dir::Request,
            &bind(5, "wl_data_device_manager", 3, 21),
        );
        req(&mut m, 21, "get_data_device", &[22, 20]);
        req(&mut m, 21, "create_data_source", &[23]);
        req(&mut m, 21, "create_data_source", &[24]);
        let mut offer = vec![];
        offer.extend(string("text/plain"));
        req(&mut m, 23, "offer", &offer);
        req(&mut m, 22, "set_selection", &[23, 5]);
        assert!(m.persists(m.key_of(23).unwrap()));
        assert!(!m.persists(m.key_of(24).unwrap()));
        // The kept source isn't cancelled; the other one is.
        let synth = m.detach();
        assert!(synth.iter().any(|e| e.object == 24));
        assert!(!synth.iter().any(|e| e.object == 23));
    }

    #[test]
    fn pending_activation_tokens_persist() {
        let mut m = toplevel();
        feed(&mut m, Dir::Event, &global(4, "xdg_activation_v1", 1));
        feed(&mut m, Dir::Request, &bind(4, "xdg_activation_v1", 1, 20));
        req(&mut m, 20, "get_activation_token", &[21]);
        let key = m.key_of(21).unwrap();
        assert!(!m.persists(key));
        req(&mut m, 21, "commit", &[]);
        assert!(m.persists(key));
        // Once the token came, there's nothing to ask for again.
        let iface = m.iface_of(21).unwrap();
        let op = event_by_name(iface, "done").unwrap();
        let mut ev = vec![];
        ev.extend(string("token"));
        feed(&mut m, Dir::Event, &raw(21, op, &ev));
        assert!(!m.persists(key));
    }

    #[test]
    fn screen_copies_in_flight_persist() {
        let mut m = toplevel();
        let mut ev = global(4, "wl_output", 4);
        ev.extend(global(5, "zwlr_screencopy_manager_v1", 3));
        feed(&mut m, Dir::Event, &ev);
        feed(&mut m, Dir::Request, &bind(4, "wl_output", 4, 20));
        feed(
            &mut m,
            Dir::Request,
            &bind(5, "zwlr_screencopy_manager_v1", 3, 21),
        );
        req(&mut m, 21, "capture_output", &[22, 0, 20]);
        let key = m.key_of(22).unwrap();
        assert!(m.persists(key));
        let iface = m.iface_of(22).unwrap();
        let buffer = event_by_name(iface, "buffer").unwrap();
        feed(&mut m, Dir::Event, &raw(22, buffer, &[1, 640, 480, 2560]));
        assert_eq!(
            m.live(22).unwrap().frame_buffer,
            Some(vec![1, 640, 480, 2560])
        );
        let ready = event_by_name(iface, "ready").unwrap();
        feed(&mut m, Dir::Event, &raw(22, ready, &[0, 0, 0]));
        assert!(!m.persists(key));
    }

    #[test]
    fn exports_keep_their_first_handle() {
        let mut m = toplevel();
        feed(&mut m, Dir::Event, &global(4, "zxdg_exporter_v2", 1));
        feed(&mut m, Dir::Request, &bind(4, "zxdg_exporter_v2", 1, 20));
        req(&mut m, 20, "export_toplevel", &[21, 5]);
        assert!(m.persists(m.key_of(21).unwrap()));
        let iface = m.iface_of(21).unwrap();
        let op = event_by_name(iface, "handle").unwrap();
        for h in ["first", "second"] {
            let mut ev = vec![];
            ev.extend(string(h));
            feed(&mut m, Dir::Event, &raw(21, op, &ev));
        }
        assert_eq!(m.live(21).unwrap().handle.as_deref(), Some(&b"first"[..]));
    }

    #[test]
    fn gamma_ramps_and_input_methods_persist() {
        let mut m = toplevel();
        let mut ev = global(4, "wl_output", 4);
        ev.extend(global(5, "zwlr_gamma_control_manager_v1", 1));
        ev.extend(global(6, "wl_seat", 8));
        ev.extend(global(7, "zwp_input_method_manager_v2", 1));
        feed(&mut m, Dir::Event, &ev);
        feed(&mut m, Dir::Request, &bind(4, "wl_output", 4, 20));
        feed(
            &mut m,
            Dir::Request,
            &bind(5, "zwlr_gamma_control_manager_v1", 1, 21),
        );
        feed(&mut m, Dir::Request, &bind(6, "wl_seat", 8, 22));
        feed(
            &mut m,
            Dir::Request,
            &bind(7, "zwp_input_method_manager_v2", 1, 23),
        );
        req(&mut m, 21, "get_gamma_control", &[24, 20]);
        req(&mut m, 23, "get_input_method", &[22, 25]);
        let synth = m.detach();
        // Neither is told it failed / is gone.
        assert!(!synth.iter().any(|e| e.object == 24 || e.object == 25));
        assert!(m.persists(m.key_of(24).unwrap()));
        assert!(m.persists(m.key_of(25).unwrap()));
    }
}
