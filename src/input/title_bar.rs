//! arbor: server-side title bar clicks and hover.

use std::time::Duration;

use smithay::backend::input::MouseButton;
use smithay::desktop::Window;
use smithay::input::pointer::{CursorIcon, Focus, GrabStartData as PointerGrabStartData};
use smithay::utils::Serial;

use super::move_grab::MoveGrab;
use super::{PointerOrTouchStartData, DOUBLE_CLICK_TIME};
use crate::arbor::State;
use crate::layout::title_bar::TitleBarHit;
use crate::utils::get_monotonic_time;

impl State {
    /// Handles a left button press on a server-side title bar. Returns whether it was one.
    pub(super) fn try_title_bar_press(
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
        let Some((output, pos_within_output)) = self.arbor.output_under(location) else {
            return false;
        };
        let output = output.clone();
        let Some((window, hit)) = self
            .arbor
            .layout
            .title_bar_under(&output, pos_within_output)
            .map(|(mapped, hit)| (mapped.window.clone(), hit))
        else {
            return false;
        };

        match hit {
            TitleBarHit::Close => {
                if let Some((_, mapped)) = self
                    .arbor
                    .layout
                    .windows()
                    .find(|(_, m)| m.window == window)
                {
                    mapped.toplevel().send_close();
                }
            }
            TitleBarHit::Minimize => self.arbor.layout.minimize_window(Some(&window)),
            TitleBarHit::Maximize => {
                self.arbor.layout.activate_window(&window);
                self.arbor.layout.toggle_maximized(&window);
            }
            TitleBarHit::Bar => {
                self.arbor.layout.activate_window(&window);

                let now = get_monotonic_time();
                let is_double = self.arbor.title_bar_last_click.take().is_some_and(
                    |(time, win): (Duration, Window)| {
                        win == window && now.saturating_sub(time) <= DOUBLE_CLICK_TIME
                    },
                );
                if is_double {
                    self.arbor.layout.toggle_maximized(&window);
                } else {
                    self.arbor.title_bar_last_click = Some((now, window.clone()));

                    // Drag to move (and to dock).
                    let start_data = PointerGrabStartData {
                        focus: None,
                        button: button_code,
                        location,
                    };
                    let start_data = PointerOrTouchStartData::Pointer(start_data);
                    if let Some(grab) =
                        MoveGrab::new(self, start_data, window, Some(CursorIcon::Default))
                    {
                        pointer.set_grab(self, grab, serial, Focus::Clear);
                    }
                }
            }
        }

        self.arbor.queue_redraw_all();
        true
    }

    /// Highlights the title bar button under the pointer.
    pub(super) fn update_title_bar_hover(&mut self) {
        let pointer = self.arbor.seat.get_pointer().unwrap();
        let target = if pointer.is_grabbed() {
            None
        } else {
            self.arbor.output_under(pointer.current_location())
        };
        let target = target.map(|(output, pos)| (output.clone(), pos));
        let changed = self
            .arbor
            .layout
            .update_title_bar_hover(target.as_ref().map(|(output, pos)| (output, *pos)));
        if changed {
            self.arbor.queue_redraw_all();
        }
    }
}
