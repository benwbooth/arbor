//! arbor: camera input — pinch zoom, clicks on a zoomed-out dock tree.

use smithay::backend::input::MouseButton;
use smithay::input::pointer::{CursorIcon, Focus, GrabStartData as PointerGrabStartData};
use smithay::utils::Serial;

use super::move_grab::MoveGrab;
use super::{modifiers_from_state, PointerOrTouchStartData, DOUBLE_CLICK_TIME};
use crate::arbor::State;
use crate::utils::get_monotonic_time;

impl State {
    /// Starts a camera pinch when Mod is held, or when the pointer is over empty dock space or
    /// a zoomed-out dock tree (elsewhere pinches go to the window under the pointer). Returns
    /// whether the gesture is ours.
    pub(super) fn camera_pinch_begin(&mut self) -> bool {
        let mods = self.arbor.seat.get_keyboard().unwrap().modifier_state();
        let mod_key = self.backend.mod_key(&self.arbor.config.borrow());
        let mod_down = modifiers_from_state(mods).contains(mod_key.to_modifiers());

        let ours = mod_down || {
            let pointer = self.arbor.seat.get_pointer().unwrap();
            self.arbor.pointer_contents.layer.is_none()
                && self
                    .arbor
                    .output_under(pointer.current_location())
                    .is_some_and(|(output, pos)| {
                        self.arbor.layout.camera_pinch_allowed(output, pos)
                    })
        };
        if !ours {
            return false;
        }
        self.arbor.camera_pinch = Some(1.);
        true
    }

    /// Zooms the camera with a pinch in progress. Returns whether the gesture is ours.
    pub(super) fn camera_pinch_update(&mut self, scale: f64) -> bool {
        let Some(last) = self.arbor.camera_pinch else {
            return false;
        };
        if scale <= 0. || last <= 0. {
            return true;
        }
        self.arbor.camera_pinch = Some(scale);

        let pointer = self.arbor.seat.get_pointer().unwrap();
        if let Some((output, pos)) = self.arbor.output_under(pointer.current_location()) {
            let output = output.clone();
            self.arbor.layout.camera_zoom_at(&output, pos, scale / last);
            self.arbor.queue_redraw_all();
        }
        true
    }

    pub(super) fn camera_pinch_end(&mut self) -> bool {
        if self.arbor.camera_pinch.take().is_none() {
            return false;
        }
        // Settle: close to actual size snaps back to it, animated.
        let pointer = self.arbor.seat.get_pointer().unwrap();
        if let Some((output, pos)) = self.arbor.output_under(pointer.current_location()) {
            let output = output.clone();
            self.arbor.layout.camera_pinch_end(&output, pos);
            self.arbor.queue_redraw_all();
        }
        true
    }

    /// Left clicks on docked windows while the camera is zoomed: a double-click frames the
    /// window, a press starts a move (so that dragging still docks).
    pub(super) fn try_zoomed_dock_press(
        &mut self,
        button: Option<MouseButton>,
        button_code: u32,
        serial: Serial,
    ) -> bool {
        let pointer = self.arbor.seat.get_pointer().unwrap();
        if button != Some(MouseButton::Left) || pointer.is_grabbed() {
            return false;
        }
        if self.arbor.is_locked()
            || self.arbor.screenshot_ui.is_open()
            || self.arbor.pointer_contents.layer.is_some()
        {
            return false;
        }

        let location = pointer.current_location();
        let Some((output, pos)) = self.arbor.output_under(location) else {
            return false;
        };
        let output = output.clone();
        let Some(window) = self
            .arbor
            .layout
            .zoomed_dock_window_under(&output, pos)
            .map(|mapped| mapped.window.clone())
        else {
            return false;
        };

        let now = get_monotonic_time();
        let is_double = self
            .arbor
            .title_bar_last_click
            .take()
            .is_some_and(|(time, win)| {
                win == window && now.saturating_sub(time) <= DOUBLE_CLICK_TIME
            });
        if is_double {
            self.arbor.layout.camera_frame_window(&window);
        } else {
            self.arbor.title_bar_last_click = Some((now, window.clone()));
            self.arbor.layout.activate_window(&window);

            let start_data = PointerGrabStartData {
                focus: None,
                button: button_code,
                location,
            };
            let start_data = PointerOrTouchStartData::Pointer(start_data);
            if let Some(grab) = MoveGrab::new(self, start_data, window, Some(CursorIcon::Default)) {
                pointer.set_grab(self, grab, serial, Focus::Clear);
            }
        }

        self.arbor.queue_redraw_all();
        true
    }
}
