//! arbor: dragging the dividers between docked windows.

use smithay::backend::input::MouseButton;
use smithay::desktop::Window;
use smithay::input::pointer::{
    CursorIcon, CursorImageStatus, Focus, GrabStartData as PointerGrabStartData,
};
use smithay::utils::{Logical, Point, Serial};

use super::resize_grab::ResizeGrab;
use crate::niri::State;
use crate::utils::ResizeEdge;

fn divider_cursor(edge: ResizeEdge) -> CursorIcon {
    if edge.intersects(ResizeEdge::LEFT_RIGHT) {
        CursorIcon::ColResize
    } else {
        CursorIcon::RowResize
    }
}

impl State {
    /// The dock divider under the pointer, if dividers can be grabbed right now.
    fn dock_divider_under(&self, location: Point<f64, Logical>) -> Option<(Window, ResizeEdge)> {
        let niri = &self.niri;
        if niri.is_locked() || niri.screenshot_ui.is_open() || niri.layout.is_overview_open() {
            return None;
        }

        // Layer surfaces (panels, launchers) are above the dividers.
        if niri.pointer_contents.layer.is_some() {
            return None;
        }

        let (output, pos_within_output) = niri.output_under(location)?;
        niri.layout.dock_divider_under(output, pos_within_output)
    }

    /// Starts dragging a dock divider if the left button was pressed over one.
    ///
    /// Returns whether a divider drag started.
    pub(super) fn try_start_dock_divider_resize(
        &mut self,
        button: Option<MouseButton>,
        button_code: u32,
        serial: Serial,
    ) -> bool {
        let pointer = self.niri.seat.get_pointer().unwrap();
        if button != Some(MouseButton::Left) || pointer.is_grabbed() {
            return false;
        }

        let location = pointer.current_location();
        let Some((window, edge)) = self.dock_divider_under(location) else {
            return false;
        };

        if !self
            .niri
            .layout
            .interactive_resize_begin(window.clone(), edge)
        {
            return false;
        }

        let start_data = PointerGrabStartData {
            focus: None,
            button: button_code,
            location,
        };
        let grab = ResizeGrab::new(start_data, window);
        pointer.set_grab(self, grab, serial, Focus::Clear);
        self.niri
            .cursor_manager
            .set_cursor_image(CursorImageStatus::Named(divider_cursor(edge)));
        self.niri.dock_divider_hover = false;
        true
    }

    /// Shows a resize cursor while the pointer hovers a dock divider.
    pub(super) fn update_dock_divider_cursor(&mut self) {
        let pointer = self.niri.seat.get_pointer().unwrap();
        if pointer.is_grabbed() {
            return;
        }

        let location = pointer.current_location();
        match self.dock_divider_under(location) {
            Some((_, edge)) => {
                self.niri
                    .cursor_manager
                    .set_cursor_image(CursorImageStatus::Named(divider_cursor(edge)));
                self.niri.dock_divider_hover = true;
            }
            None => {
                if self.niri.dock_divider_hover {
                    self.niri.dock_divider_hover = false;
                    self.niri
                        .cursor_manager
                        .set_cursor_image(CursorImageStatus::default_named());
                }
            }
        }
    }
}
