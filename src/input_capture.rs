//! Input capture for arbor's InputCapture portal (Deskflow, Input Leap and other
//! keyboard-and-mouse sharing servers).
//!
//! An app sets pointer barriers on the outer edges of the screens and enables capture. When the
//! pointer pushes through a barrier, the capture activates: arbor stops delivering keyboard and
//! pointer input to windows and sends it to the app over EIS instead (a *receiver* context:
//! relative pointer motion, buttons, scroll, keys with arbor's keymap), until the app releases
//! it (optionally putting the pointer somewhere) or the user presses Super+Shift+Escape.

use std::collections::HashMap;
use std::io;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::net::UnixStream;

use reis::calloop::{EisRequestSource, EisRequestSourceEvent};
use reis::eis;
use reis::request::{Connection, Device, DeviceCapability, EisRequest, Seat};
use smithay::backend::input::{ButtonState, KeyState, Keycode};
use smithay::utils::{Logical, Point, Rectangle};

use crate::arbor::State;

pub const CAP_KEYBOARD: u32 = 1;
pub const CAP_POINTER: u32 = 2;

/// A pointer barrier: an axis-aligned segment on an outer screen edge.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Barrier {
    pub id: u32,
    pub x1: i32,
    pub y1: i32,
    pub x2: i32,
    pub y2: i32,
}

/// What the portal side is told.
#[derive(Debug, Clone, PartialEq)]
pub enum CaptureEvent {
    Activated {
        handle: String,
        activation_id: u32,
        cursor: (f64, f64),
        barrier_id: u32,
    },
    Deactivated {
        handle: String,
        activation_id: u32,
        cursor: (f64, f64),
    },
    Disabled {
        handle: String,
    },
}

/// Checks a barrier against the zones (screen rectangles): it must be horizontal or vertical
/// and lie along an edge of a zone that no other zone continues past.
pub fn barrier_is_valid(b: &Barrier, zones: &[Rectangle<i32, Logical>]) -> bool {
    let vertical = b.x1 == b.x2;
    let horizontal = b.y1 == b.y2;
    if vertical == horizontal {
        return false;
    }
    let (lo, hi) = if vertical {
        (b.y1.min(b.y2), b.y1.max(b.y2))
    } else {
        (b.x1.min(b.x2), b.x1.max(b.x2))
    };
    zones.iter().any(|z| {
        let (left, top) = (z.loc.x, z.loc.y);
        let (right, bottom) = (z.loc.x + z.size.w, z.loc.y + z.size.h);
        if vertical {
            let on_edge = b.x1 == left || b.x1 == right;
            // Allow the inclusive end (y2 = bottom - 1 or bottom).
            let in_range = lo >= top && hi <= bottom;
            on_edge
                && in_range
                && !outer_side_covered(b.x1, (lo + hi) / 2, b.x1 == right, true, zones)
        } else {
            let on_edge = b.y1 == top || b.y1 == bottom;
            let in_range = lo >= left && hi <= right;
            on_edge
                && in_range
                && !outer_side_covered((lo + hi) / 2, b.y1, b.y1 == bottom, false, zones)
        }
    })
}

/// Whether another zone continues past the edge at (x, y) on its outer side.
fn outer_side_covered(
    x: i32,
    y: i32,
    positive: bool,
    vertical: bool,
    zones: &[Rectangle<i32, Logical>],
) -> bool {
    let probe = match (vertical, positive) {
        (true, true) => Point::from((x, y)),
        (true, false) => Point::from((x - 1, y)),
        (false, true) => Point::from((x, y)),
        (false, false) => Point::from((x, y - 1)),
    };
    zones.iter().any(|z| z.contains(probe))
}

/// The barrier the pointer pushes through moving from `from` to `to` (unclamped), if any. The
/// target must be outside every zone (a barrier only matters where the pointer would leave the
/// screens).
pub fn crossed_barrier(
    barriers: &[Barrier],
    zones: &[Rectangle<i32, Logical>],
    from: Point<f64, Logical>,
    to: Point<f64, Logical>,
) -> Option<u32> {
    if zones.iter().any(|z| z.to_f64().contains(to)) {
        return None;
    }
    barriers.iter().find_map(|b| {
        if b.x1 == b.x2 {
            let bx = f64::from(b.x1);
            let (lo, hi) = (f64::from(b.y1.min(b.y2)), f64::from(b.y1.max(b.y2)));
            let crosses = (from.x <= bx && to.x >= bx && to.x > from.x)
                || (from.x >= bx && to.x <= bx && to.x < from.x)
                // Right/bottom edges: the pointer stops one pixel short of the edge line.
                || (from.x < bx && from.x >= bx - 1. && to.x >= bx);
            let y = from.y;
            (crosses && y >= lo && y <= hi + 1.).then_some(b.id)
        } else {
            let by = f64::from(b.y1);
            let (lo, hi) = (f64::from(b.x1.min(b.x2)), f64::from(b.x1.max(b.x2)));
            let crosses = (from.y <= by && to.y >= by && to.y > from.y)
                || (from.y >= by && to.y <= by && to.y < from.y)
                || (from.y < by && from.y >= by - 1. && to.y >= by);
            let x = from.x;
            (crosses && x >= lo && x <= hi + 1.).then_some(b.id)
        }
    })
}

struct Receiver {
    conn: Option<Connection>,
    seat: Option<Seat>,
    keyboard: Option<Device>,
    pointer: Option<Device>,
    token: calloop::RegistrationToken,
    emulating: bool,
}

pub struct CaptureSession {
    pub handle: String,
    pub caps: u32,
    pub enabled: bool,
    pub barriers: Vec<Barrier>,
    receiver: Option<Receiver>,
}

#[derive(Default)]
pub struct InputCapture {
    pub sessions: HashMap<u64, CaptureSession>,
    /// (session, activation id) while capturing.
    pub active: Option<(u64, u32)>,
    next_activation: u32,
    pub events: Option<async_channel::Sender<CaptureEvent>>,
}

impl InputCapture {
    pub fn is_capturing(&self) -> bool {
        self.active.is_some()
    }

    fn send(&self, event: CaptureEvent) {
        if let Some(tx) = &self.events {
            let _ = tx.try_send(event);
        }
    }

    fn with_active_receiver(&self, f: impl FnOnce(&Receiver)) {
        if let Some((id, _)) = self.active {
            if let Some(r) = self.sessions.get(&id).and_then(|s| s.receiver.as_ref()) {
                if r.emulating {
                    f(r);
                }
            }
        }
    }
}

fn now_us() -> u64 {
    crate::utils::get_monotonic_time().as_micros() as u64
}

impl State {
    pub fn input_capture_zones(&self) -> Vec<Rectangle<i32, Logical>> {
        self.arbor
            .global_space
            .outputs()
            .filter_map(|o| self.arbor.global_space.output_geometry(o))
            .collect()
    }

    /// Called for relative pointer motion. Returns true if the motion was consumed (sent to the
    /// capturing app, or it activated the capture).
    pub fn input_capture_motion(&mut self, delta: Point<f64, Logical>) -> bool {
        let ic = &self.arbor.input_capture;
        if ic.is_capturing() {
            let time = now_us();
            ic.with_active_receiver(|r| {
                if let Some(p) = &r.pointer {
                    if let Some(ptr) = p.interface::<eis::Pointer>() {
                        ptr.motion_relative(delta.x as f32, delta.y as f32);
                        p.frame(time);
                        r.flush();
                    }
                }
            });
            return true;
        }

        let enabled: Vec<(u64, Vec<Barrier>)> = ic
            .sessions
            .iter()
            .filter(|(_, s)| s.enabled && !s.barriers.is_empty())
            .map(|(id, s)| (*id, s.barriers.clone()))
            .collect();
        if enabled.is_empty() {
            return false;
        }
        let from = self.arbor.seat.get_pointer().unwrap().current_location();
        let to = from + delta;
        let zones = self.input_capture_zones();
        for (id, barriers) in enabled {
            if let Some(barrier_id) = crossed_barrier(&barriers, &zones, from, to) {
                self.input_capture_activate(id, barrier_id, from);
                return true;
            }
        }
        false
    }

    fn input_capture_activate(&mut self, id: u64, barrier_id: u32, cursor: Point<f64, Logical>) {
        let ic = &mut self.arbor.input_capture;
        ic.next_activation = ic.next_activation.wrapping_add(1).max(1);
        let activation_id = ic.next_activation;
        ic.active = Some((id, activation_id));
        let Some(session) = ic.sessions.get_mut(&id) else {
            return;
        };
        let handle = session.handle.clone();
        if let Some(r) = &mut session.receiver {
            for d in r.keyboard.iter().chain(r.pointer.iter()) {
                d.start_emulating(activation_id);
            }
            r.emulating = true;
            r.flush();
        }
        ic.send(CaptureEvent::Activated {
            handle,
            activation_id,
            cursor: (cursor.x, cursor.y),
            barrier_id,
        });
        debug!(activation_id, barrier_id, "input capture activated");
    }

    /// Ends an active capture (on Release, Disable, session close or the emergency key).
    pub fn input_capture_deactivate(&mut self, cursor: Option<Point<f64, Logical>>) {
        let ic = &mut self.arbor.input_capture;
        let Some((id, activation_id)) = ic.active.take() else {
            return;
        };
        for d in ic.receiver_devices_for(id) {
            d.stop_emulating();
        }
        if let Some(r) = ic.sessions.get_mut(&id).and_then(|s| s.receiver.as_mut()) {
            r.emulating = false;
            r.flush();
        }
        let handle = ic.sessions.get(&id).map(|s| s.handle.clone());
        if let Some(pos) = cursor {
            self.move_cursor(pos);
        }
        let pos = self.arbor.seat.get_pointer().unwrap().current_location();
        if let Some(handle) = handle {
            self.arbor.input_capture.send(CaptureEvent::Deactivated {
                handle,
                activation_id,
                cursor: (pos.x, pos.y),
            });
        }
        self.arbor.queue_redraw_all();
    }

    /// Keys while capturing go to the app. Returns true if consumed.
    pub fn input_capture_key(&mut self, keycode: Keycode, state: KeyState) -> bool {
        if !self.arbor.input_capture.is_capturing() {
            return false;
        }
        // Emergency exit: Super+Shift+Escape.
        if state == KeyState::Pressed && keycode.raw() == 1 + 8 {
            let mods = self.arbor.seat.get_keyboard().unwrap().modifier_state();
            if mods.logo && mods.shift {
                self.input_capture_deactivate(None);
                let ic = &mut self.arbor.input_capture;
                let ids: Vec<u64> = ic.sessions.keys().copied().collect();
                for id in ids {
                    if let Some(s) = ic.sessions.get_mut(&id) {
                        if s.enabled {
                            s.enabled = false;
                            let handle = s.handle.clone();
                            ic.send(CaptureEvent::Disabled { handle });
                        }
                    }
                }
                return true;
            }
        }
        let time = now_us();
        self.arbor.input_capture.with_active_receiver(|r| {
            if let Some(k) = &r.keyboard {
                if let Some(kb) = k.interface::<eis::Keyboard>() {
                    kb.key(
                        keycode.raw().saturating_sub(8),
                        if state == KeyState::Pressed {
                            eis::keyboard::KeyState::Press
                        } else {
                            eis::keyboard::KeyState::Released
                        },
                    );
                    k.frame(time);
                    r.flush();
                }
            }
        });
        true
    }

    /// Buttons while capturing go to the app. Returns true if consumed.
    pub fn input_capture_button(&mut self, button: u32, state: ButtonState) -> bool {
        if !self.arbor.input_capture.is_capturing() {
            return false;
        }
        let time = now_us();
        self.arbor.input_capture.with_active_receiver(|r| {
            if let Some(p) = &r.pointer {
                if let Some(b) = p.interface::<eis::Button>() {
                    b.button(
                        button,
                        if state == ButtonState::Pressed {
                            eis::button::ButtonState::Press
                        } else {
                            eis::button::ButtonState::Released
                        },
                    );
                    p.frame(time);
                    r.flush();
                }
            }
        });
        true
    }

    /// Scrolling while capturing goes to the app. Returns true if consumed.
    pub fn input_capture_scroll(
        &mut self,
        amount: (f64, f64),
        v120: (Option<f64>, Option<f64>),
    ) -> bool {
        if !self.arbor.input_capture.is_capturing() {
            return false;
        }
        let time = now_us();
        self.arbor.input_capture.with_active_receiver(|r| {
            if let Some(p) = &r.pointer {
                if let Some(s) = p.interface::<eis::Scroll>() {
                    if v120.0.is_some() || v120.1.is_some() {
                        s.scroll_discrete(v120.0.unwrap_or(0.) as i32, v120.1.unwrap_or(0.) as i32);
                    } else {
                        s.scroll(amount.0 as f32, amount.1 as f32);
                    }
                    p.frame(time);
                    r.flush();
                }
            }
        });
        true
    }

    /// Serves an EIS receiver connection for capture session `id`.
    pub fn input_capture_connect_eis(&mut self, id: u64) -> io::Result<OwnedFd> {
        let caps = self
            .arbor
            .input_capture
            .sessions
            .get(&id)
            .map(|s| s.caps)
            .ok_or_else(|| io::Error::other("unknown session"))?;
        let (ours, theirs) = UnixStream::pair()?;
        ours.set_nonblocking(true)?;
        let context = eis::Context::new(ours)?;
        let source = EisRequestSource::new(context, 1);
        let token = self
            .arbor
            .event_loop
            .insert_source(source, move |event, conn, state| {
                let Ok(event) = event else {
                    state.input_capture_receiver_gone(id);
                    return Ok(calloop::PostAction::Remove);
                };
                match event {
                    EisRequestSourceEvent::Connected => {
                        if conn.context_type() != eis::handshake::ContextType::Receiver {
                            conn.disconnected(
                                eis::connection::DisconnectReason::Protocol,
                                Some("InputCapture EIS needs a receiver context"),
                            );
                            let _ = conn.flush();
                            return Ok(calloop::PostAction::Remove);
                        }
                        let mut seat_caps = reis::enumflags2::BitFlags::empty();
                        if caps & CAP_KEYBOARD != 0 {
                            seat_caps |= DeviceCapability::Keyboard;
                        }
                        if caps & CAP_POINTER != 0 {
                            seat_caps |= DeviceCapability::Pointer
                                | DeviceCapability::Button
                                | DeviceCapability::Scroll;
                        }
                        let seat = conn.add_seat(Some("default"), seat_caps);
                        if let Some(r) = state.input_capture_receiver(id) {
                            r.seat = Some(seat);
                            r.conn = Some(conn.clone());
                        }
                    }
                    EisRequestSourceEvent::Request(EisRequest::Disconnect) => {
                        state.input_capture_receiver_gone(id);
                        return Ok(calloop::PostAction::Remove);
                    }
                    EisRequestSourceEvent::Request(EisRequest::Bind(bind)) => {
                        state.input_capture_add_devices(id, conn, bind.capabilities);
                    }
                    EisRequestSourceEvent::Request(_) => (),
                }
                let _ = conn.flush();
                Ok(calloop::PostAction::Continue)
            })
            .map_err(|err| io::Error::other(err.to_string()))?;

        if let Some(s) = self.arbor.input_capture.sessions.get_mut(&id) {
            if let Some(old) = s.receiver.take() {
                self.arbor.event_loop.remove(old.token);
            }
            s.receiver = Some(Receiver {
                conn: None,
                seat: None,
                keyboard: None,
                pointer: None,
                token,
                emulating: false,
            });
        }
        Ok(OwnedFd::from(theirs))
    }

    fn input_capture_receiver(&mut self, id: u64) -> Option<&mut Receiver> {
        self.arbor
            .input_capture
            .sessions
            .get_mut(&id)
            .and_then(|s| s.receiver.as_mut())
    }

    fn input_capture_receiver_gone(&mut self, id: u64) {
        if self
            .arbor
            .input_capture
            .active
            .is_some_and(|(a, _)| a == id)
        {
            self.input_capture_deactivate(None);
        }
        if let Some(s) = self.arbor.input_capture.sessions.get_mut(&id) {
            s.receiver = None;
        }
    }

    fn input_capture_add_devices(
        &mut self,
        id: u64,
        conn: &Connection,
        wanted: reis::enumflags2::BitFlags<DeviceCapability>,
    ) {
        let keymap = if wanted.contains(DeviceCapability::Keyboard) {
            crate::eis_server::keymap_fd(self)
        } else {
            None
        };
        let active = self.arbor.input_capture.active;
        let Some(r) = self.input_capture_receiver(id) else {
            return;
        };
        let Some(seat) = r.seat.clone() else {
            return;
        };
        if wanted.contains(DeviceCapability::Keyboard) && r.keyboard.is_none() {
            let device = seat.add_device(
                Some("arbor captured keyboard"),
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
            r.keyboard = Some(device);
        }
        if wanted.contains(DeviceCapability::Pointer) && r.pointer.is_none() {
            let device = seat.add_device(
                Some("arbor captured pointer"),
                eis::device::DeviceType::Virtual,
                DeviceCapability::Pointer | DeviceCapability::Button | DeviceCapability::Scroll,
                |_| {},
            );
            device.resumed();
            r.pointer = Some(device);
        }
        // Joined mid-capture: start emulating right away.
        if let Some((active_id, activation_id)) = active {
            if active_id == id {
                for d in r.keyboard.iter().chain(r.pointer.iter()) {
                    d.start_emulating(activation_id);
                }
                r.emulating = true;
            }
        }
        let _ = conn.flush();
    }

    /// Forgets a capture session (portal Close).
    pub fn input_capture_close(&mut self, id: u64) {
        if self
            .arbor
            .input_capture
            .active
            .is_some_and(|(a, _)| a == id)
        {
            self.input_capture_deactivate(None);
        }
        if let Some(s) = self.arbor.input_capture.sessions.remove(&id) {
            if let Some(r) = s.receiver {
                self.arbor.event_loop.remove(r.token);
            }
        }
    }
}

impl Receiver {
    fn flush(&self) {
        if let Some(conn) = &self.conn {
            let _ = conn.flush();
        }
    }
}

impl InputCapture {
    fn receiver_devices_for(&self, id: u64) -> Vec<Device> {
        self.sessions
            .get(&id)
            .and_then(|s| s.receiver.as_ref())
            .map(|r| r.keyboard.iter().chain(r.pointer.iter()).cloned().collect())
            .unwrap_or_default()
    }

    pub fn add_session(&mut self, id: u64, handle: String, caps: u32) {
        self.sessions.insert(
            id,
            CaptureSession {
                handle,
                caps,
                enabled: false,
                barriers: Vec::new(),
                receiver: None,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zones() -> Vec<Rectangle<i32, Logical>> {
        // Two screens side by side: 1920x1080 and 2560x1440 to its right.
        vec![
            Rectangle::new(Point::from((0, 0)), (1920, 1080).into()),
            Rectangle::new(Point::from((1920, 0)), (2560, 1440).into()),
        ]
    }

    #[test]
    fn barriers_must_be_on_outer_edges() {
        let z = zones();
        let right = Barrier {
            id: 1,
            x1: 4480,
            y1: 0,
            x2: 4480,
            y2: 1439,
        };
        let left = Barrier {
            id: 2,
            x1: 0,
            y1: 0,
            x2: 0,
            y2: 1079,
        };
        let between = Barrier {
            id: 3,
            x1: 1920,
            y1: 0,
            x2: 1920,
            y2: 1079,
        };
        let diagonal = Barrier {
            id: 4,
            x1: 0,
            y1: 0,
            x2: 10,
            y2: 10,
        };
        let top = Barrier {
            id: 5,
            x1: 0,
            y1: 0,
            x2: 1919,
            y2: 0,
        };
        let below_small = Barrier {
            id: 6,
            x1: 0,
            y1: 1080,
            x2: 1919,
            y2: 1080,
        };
        assert!(barrier_is_valid(&right, &z));
        assert!(barrier_is_valid(&left, &z));
        assert!(!barrier_is_valid(&between, &z));
        assert!(!barrier_is_valid(&diagonal, &z));
        assert!(barrier_is_valid(&top, &z));
        assert!(barrier_is_valid(&below_small, &z));
    }

    #[test]
    fn crossing_detection() {
        let z = zones();
        let barriers = [
            Barrier {
                id: 1,
                x1: 4480,
                y1: 0,
                x2: 4480,
                y2: 1439,
            },
            Barrier {
                id: 2,
                x1: 0,
                y1: 0,
                x2: 0,
                y2: 1079,
            },
        ];
        // Pushing right off the right screen.
        assert_eq!(
            crossed_barrier(
                &barriers,
                &z,
                Point::from((4479.5, 100.)),
                Point::from((4485., 100.))
            ),
            Some(1)
        );
        // Pushing left at the left edge (the pointer sits at x = 0).
        assert_eq!(
            crossed_barrier(
                &barriers,
                &z,
                Point::from((0., 500.)),
                Point::from((-3., 500.))
            ),
            Some(2)
        );
        // Moving between the screens is not a crossing.
        assert_eq!(
            crossed_barrier(
                &barriers,
                &z,
                Point::from((1919., 500.)),
                Point::from((1925., 500.))
            ),
            None
        );
        // Outside the barrier's span (left screen is only 1080 tall; no barrier below it).
        assert_eq!(
            crossed_barrier(
                &barriers,
                &z,
                Point::from((100., 1079.)),
                Point::from((100., 1085.))
            ),
            None
        );
    }
}
