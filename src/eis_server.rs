//! EIS (libei server) connections for remote-desktop clients.
//!
//! `RemoteDesktop.ConnectToEIS` hands the client one end of a socket pair; arbor serves libei's
//! protocol on the other (with the `reis` crate) in a *sender* context: the client emulates
//! devices and arbor injects what they send through `crate::remote_input`, as the session's
//! input source. Devices offered match what the user allowed (keyboard, pointer, touchscreen);
//! the keyboard carries arbor's current keymap; absolute pointer and touch devices get one
//! region per output in global logical coordinates.

use std::collections::HashMap;
use std::io;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::net::UnixStream;

use reis::calloop::{EisRequestSource, EisRequestSourceEvent};
use reis::eis;
use reis::enumflags2::BitFlags;
use reis::request::{Connection, Device, DeviceCapability, EisRequest, Seat};
use smithay::input::keyboard::xkb;
use smithay::reexports::rustix;

use crate::arbor::State;
use crate::remote_input::{AbsTarget, RemoteInput, RemoteSourceId, ScrollSource};

/// RemoteDesktop device type bits.
pub const DEVICE_KEYBOARD: u32 = 1;
pub const DEVICE_POINTER: u32 = 2;
pub const DEVICE_TOUCHSCREEN: u32 = 4;

/// X keysym for a character (Latin-1 keysyms equal the code point; the rest use the Unicode
/// keysym range).
pub fn keysym_for_char(c: char) -> u32 {
    match c {
        '\n' | '\r' => 0xff0d,
        '\t' => 0xff09,
        '\u{8}' => 0xff08,
        c if (0x20..=0x7e).contains(&(c as u32)) || (0xa0..=0xff).contains(&(c as u32)) => c as u32,
        c => 0x0100_0000 | c as u32,
    }
}

/// Seat capabilities for the allowed RemoteDesktop device types.
pub fn capabilities_for(devices: u32) -> BitFlags<DeviceCapability> {
    let mut caps = BitFlags::empty();
    if devices & DEVICE_KEYBOARD != 0 {
        caps |= DeviceCapability::Keyboard | DeviceCapability::Text;
    }
    if devices & DEVICE_POINTER != 0 {
        caps |= DeviceCapability::Pointer
            | DeviceCapability::PointerAbsolute
            | DeviceCapability::Button
            | DeviceCapability::Scroll;
    }
    if devices & DEVICE_TOUCHSCREEN != 0 {
        caps |= DeviceCapability::Touch;
    }
    caps
}

/// Splits accumulated 120ths of a wheel click into whole clicks.
#[derive(Debug, Default, Clone, Copy)]
pub struct DiscreteAccumulator(i32);

impl DiscreteAccumulator {
    pub fn add(&mut self, v120: i32) -> i32 {
        self.0 += v120;
        let steps = self.0 / 120;
        self.0 -= steps * 120;
        steps
    }
}

struct ConnState {
    source: RemoteSourceId,
    devices: u32,
    seat: Option<Seat>,
    added: Vec<Device>,
    bound: BitFlags<DeviceCapability>,
    scroll_x: DiscreteAccumulator,
    scroll_y: DiscreteAccumulator,
}

fn keymap_fd(state: &mut State) -> Option<(OwnedFd, u32)> {
    let keyboard = state.arbor.seat.get_keyboard()?;
    let text = keyboard.with_xkb_state(state, |ctx| {
        let xkb = ctx.xkb().lock().unwrap();
        // SAFETY: the keymap reference does not outlive the lock.
        unsafe { xkb.keymap() }.get_as_string(xkb::KEYMAP_FORMAT_TEXT_V1)
    });
    let fd = rustix::fs::memfd_create("arbor-eis-keymap", rustix::fs::MemfdFlags::CLOEXEC).ok()?;
    let mut bytes = text.into_bytes();
    bytes.push(0);
    let mut written = 0;
    while written < bytes.len() {
        match rustix::io::write(&fd, &bytes[written..]) {
            Ok(n) => written += n,
            Err(_) => return None,
        }
    }
    Some((fd, bytes.len() as u32))
}

fn output_regions(state: &State) -> Vec<(u32, u32, u32, u32, f32)> {
    state
        .arbor
        .global_space
        .outputs()
        .filter_map(|o| {
            let geo = state.arbor.global_space.output_geometry(o)?;
            Some((
                geo.loc.x.max(0) as u32,
                geo.loc.y.max(0) as u32,
                geo.size.w.max(0) as u32,
                geo.size.h.max(0) as u32,
                o.current_scale().fractional_scale() as f32,
            ))
        })
        .collect()
}

impl ConnState {
    fn add_devices(
        &mut self,
        state: &mut State,
        conn: &Connection,
        wanted: BitFlags<DeviceCapability>,
    ) {
        let Some(seat) = self.seat.clone() else {
            return;
        };
        let allowed = capabilities_for(self.devices);
        let wanted = wanted & allowed & !self.bound;

        if wanted.contains(DeviceCapability::Keyboard) {
            let keymap = keymap_fd(state);
            let device = seat.add_device(
                Some("arbor remote keyboard"),
                eis::device::DeviceType::Virtual,
                DeviceCapability::Keyboard.into(),
                |device| {
                    if let (Some((fd, size)), Some(kb)) =
                        (&keymap, device.interface::<eis::Keyboard>())
                    {
                        kb.keymap(eis::keyboard::KeymapType::Xkb, *size, fd.as_fd());
                    }
                },
            );
            device.resumed();
            self.added.push(device);
            self.bound |= DeviceCapability::Keyboard;
        }
        if wanted.contains(DeviceCapability::Text) {
            let device = seat.add_device(
                Some("arbor remote text"),
                eis::device::DeviceType::Virtual,
                DeviceCapability::Text.into(),
                |_| {},
            );
            device.resumed();
            self.added.push(device);
            self.bound |= DeviceCapability::Text;
        }
        let pointer_caps =
            DeviceCapability::Pointer | DeviceCapability::Button | DeviceCapability::Scroll;
        if wanted.contains(DeviceCapability::Pointer) {
            let device = seat.add_device(
                Some("arbor remote pointer"),
                eis::device::DeviceType::Virtual,
                pointer_caps,
                |_| {},
            );
            device.resumed();
            self.added.push(device);
            self.bound |= pointer_caps;
        }
        let regions = output_regions(state);
        let abs_caps =
            DeviceCapability::PointerAbsolute | DeviceCapability::Button | DeviceCapability::Scroll;
        if wanted.contains(DeviceCapability::PointerAbsolute) {
            let device = seat.add_device(
                Some("arbor remote absolute pointer"),
                eis::device::DeviceType::Virtual,
                abs_caps,
                |device| {
                    for &(x, y, w, h, scale) in &regions {
                        device.device().region(x, y, w, h, scale);
                    }
                },
            );
            device.resumed();
            self.added.push(device);
            self.bound |= DeviceCapability::PointerAbsolute;
        }
        if wanted.contains(DeviceCapability::Touch) {
            let device = seat.add_device(
                Some("arbor remote touchscreen"),
                eis::device::DeviceType::Virtual,
                DeviceCapability::Touch.into(),
                |device| {
                    for &(x, y, w, h, scale) in &regions {
                        device.device().region(x, y, w, h, scale);
                    }
                },
            );
            device.resumed();
            self.added.push(device);
            self.bound |= DeviceCapability::Touch;
        }
        let _ = conn.flush();
    }

    /// Translates a sender-context request into remote input.
    fn input(&mut self, request: &EisRequest) -> Vec<RemoteInput> {
        use eis::button::ButtonState;
        use eis::keyboard::KeyState;
        let devices = self.devices;
        let keyboard = devices & DEVICE_KEYBOARD != 0;
        let pointer = devices & DEVICE_POINTER != 0;
        let touch = devices & DEVICE_TOUCHSCREEN != 0;
        match request {
            EisRequest::KeyboardKey(e) if keyboard => vec![RemoteInput::Key {
                keycode: e.key,
                pressed: e.state == KeyState::Press,
            }],
            EisRequest::TextKeysym(e) if keyboard => vec![RemoteInput::Keysym {
                keysym: e.keysym,
                pressed: e.state == KeyState::Press,
            }],
            EisRequest::TextUtf8(e) if keyboard => e
                .text
                .chars()
                .flat_map(|c| {
                    let keysym = keysym_for_char(c);
                    [
                        RemoteInput::Keysym {
                            keysym,
                            pressed: true,
                        },
                        RemoteInput::Keysym {
                            keysym,
                            pressed: false,
                        },
                    ]
                })
                .collect(),
            EisRequest::PointerMotion(e) if pointer => vec![RemoteInput::PointerMotion {
                dx: f64::from(e.dx),
                dy: f64::from(e.dy),
            }],
            EisRequest::PointerMotionAbsolute(e) if pointer => {
                vec![RemoteInput::PointerMotionAbsolute {
                    target: AbsTarget::Global,
                    x: f64::from(e.dx_absolute),
                    y: f64::from(e.dy_absolute),
                }]
            }
            EisRequest::Button(e) if pointer => vec![RemoteInput::PointerButton {
                button: e.button,
                pressed: e.state == ButtonState::Press,
            }],
            EisRequest::ScrollDelta(e) if pointer => vec![RemoteInput::PointerAxis {
                dx: f64::from(e.dx),
                dy: f64::from(e.dy),
                source: ScrollSource::Continuous,
                finish: false,
            }],
            EisRequest::ScrollDiscrete(e) if pointer => {
                let mut out = Vec::new();
                let sx = self.scroll_x.add(e.discrete_dx);
                if sx != 0 {
                    out.push(RemoteInput::PointerAxisDiscrete {
                        horizontal: true,
                        steps: sx,
                    });
                }
                let sy = self.scroll_y.add(e.discrete_dy);
                if sy != 0 {
                    out.push(RemoteInput::PointerAxisDiscrete {
                        horizontal: false,
                        steps: sy,
                    });
                }
                out
            }
            EisRequest::TouchDown(e) if touch => vec![RemoteInput::TouchDown {
                target: AbsTarget::Global,
                slot: e.touch_id,
                x: f64::from(e.x),
                y: f64::from(e.y),
            }],
            EisRequest::TouchMotion(e) if touch => vec![RemoteInput::TouchMotion {
                target: AbsTarget::Global,
                slot: e.touch_id,
                x: f64::from(e.x),
                y: f64::from(e.y),
            }],
            EisRequest::TouchUp(e) if touch => vec![RemoteInput::TouchUp { slot: e.touch_id }],
            EisRequest::TouchCancel(e) if touch => vec![RemoteInput::TouchUp { slot: e.touch_id }],
            _ => Vec::new(),
        }
    }
}

/// EIS connections by input source, to close them with their session.
#[derive(Default)]
pub struct EisConnections {
    tokens: HashMap<RemoteSourceId, Vec<calloop::RegistrationToken>>,
}

impl State {
    /// Serves a new EIS sender connection for `source`, allowing `devices` (RemoteDesktop device
    /// type bits). Returns the client's end of the socket.
    pub fn start_eis_sender(
        &mut self,
        source: RemoteSourceId,
        devices: u32,
    ) -> io::Result<OwnedFd> {
        let (ours, theirs) = UnixStream::pair()?;
        ours.set_nonblocking(true)?;
        let context = eis::Context::new(ours)?;
        let request_source = EisRequestSource::new(context, 1);
        let mut conn_state = ConnState {
            source,
            devices,
            seat: None,
            added: Vec::new(),
            bound: BitFlags::empty(),
            scroll_x: DiscreteAccumulator::default(),
            scroll_y: DiscreteAccumulator::default(),
        };
        let token = self
            .arbor
            .event_loop
            .insert_source(request_source, move |event, conn, state| {
                let event = match event {
                    Ok(event) => event,
                    Err(err) => {
                        debug!("EIS client error: {err}");
                        state.remote_input_source_gone(conn_state.source);
                        return Ok(calloop::PostAction::Remove);
                    }
                };
                match event {
                    EisRequestSourceEvent::Connected => {
                        if conn.context_type() != eis::handshake::ContextType::Sender {
                            conn.disconnected(
                                eis::connection::DisconnectReason::Protocol,
                                Some("arbor's RemoteDesktop EIS only accepts sender contexts"),
                            );
                            let _ = conn.flush();
                            return Ok(calloop::PostAction::Remove);
                        }
                        let seat =
                            conn.add_seat(Some("default"), capabilities_for(conn_state.devices));
                        conn_state.seat = Some(seat);
                    }
                    EisRequestSourceEvent::Request(EisRequest::Disconnect) => {
                        state.remote_input_source_gone(conn_state.source);
                        return Ok(calloop::PostAction::Remove);
                    }
                    EisRequestSourceEvent::Request(EisRequest::Bind(bind)) => {
                        conn_state.add_devices(state, conn, bind.capabilities);
                    }
                    EisRequestSourceEvent::Request(request) => {
                        for input in conn_state.input(&request) {
                            state.remote_input(conn_state.source, input);
                        }
                    }
                }
                let _ = conn.flush();
                Ok(calloop::PostAction::Continue)
            })
            .map_err(|err| io::Error::other(err.to_string()))?;
        self.arbor
            .eis_connections
            .tokens
            .entry(source)
            .or_default()
            .push(token);
        Ok(OwnedFd::from(theirs))
    }

    /// Closes the EIS connections of `source`.
    pub fn close_eis(&mut self, source: RemoteSourceId) {
        if let Some(tokens) = self.arbor.eis_connections.tokens.remove(&source) {
            for token in tokens {
                self.arbor.event_loop.remove(token);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keysyms_for_text() {
        assert_eq!(keysym_for_char('a'), 0x61);
        assert_eq!(keysym_for_char('é'), 0xe9);
        assert_eq!(keysym_for_char('—'), 0x0100_2014);
        assert_eq!(keysym_for_char('\n'), 0xff0d);
    }

    #[test]
    fn capabilities_follow_grants() {
        let caps = capabilities_for(DEVICE_KEYBOARD);
        assert!(caps.contains(DeviceCapability::Keyboard));
        assert!(!caps.contains(DeviceCapability::Pointer));
        let caps = capabilities_for(DEVICE_POINTER | DEVICE_TOUCHSCREEN);
        assert!(caps.contains(DeviceCapability::PointerAbsolute | DeviceCapability::Touch));
        assert!(!caps.contains(DeviceCapability::Keyboard));
    }

    #[test]
    fn discrete_scroll_accumulates() {
        let mut acc = DiscreteAccumulator::default();
        assert_eq!(acc.add(60), 0);
        assert_eq!(acc.add(60), 1);
        assert_eq!(acc.add(-240), -2);
        assert_eq!(acc.add(-119), 0);
    }
}
