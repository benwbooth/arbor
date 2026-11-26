//! arbor: resizing floating windows by dragging just outside their edges and corners (windows
//! with arbor's title bar have no resize handles of their own).

use smithay::backend::input::MouseButton;
use smithay::desktop::Window;
use smithay::input::pointer::{CursorImageStatus, Focus, GrabStartData as PointerGrabStartData};
use smithay::utils::{Logical, Point, Serial};

use super::resize_grab::ResizeGrab;
use crate::arbor::State;
use crate::utils::ResizeEdge;

impl State {
    /// The floating window resize band under the pointer, if it can be grabbed right now.
    fn floating_edge_under(&self, location: Point<f64, Logical>) -> Option<(Window, ResizeEdge)> {
        let arbor = &self.arbor;
        if arbor.is_locked() || arbor.screenshot_ui.is_open() || arbor.layout.is_overview_open() {
            return None;
        }
        // Panels, menus and launchers above the windows come first.
        if arbor.pointer_contents.layer.is_some() {
            return None;
        }
        let (output, pos_within_output) = arbor.output_under(location)?;
        arbor.layout.floating_edge_under(output, pos_within_output)
    }

    /// Starts resizing a floating window if the left button was pressed in its resize band.
    pub(super) fn try_start_floating_edge_resize(
        &mut self,
        button: Option<MouseButton>,
        button_code: u32,
        serial: Serial,
    ) -> bool {
        let pointer = self.arbor.seat.get_pointer().unwrap();
        if button != Some(MouseButton::Left) || pointer.is_grabbed() {
            return false;
        }
        let location = pointer.current_location();
        let Some((window, edges)) = self.floating_edge_under(location) else {
            return false;
        };
        self.arbor.layout.activate_window(&window);
        if !self
            .arbor
            .layout
            .interactive_resize_begin(window.clone(), edges)
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
        self.arbor
            .cursor_manager
            .set_cursor_image(CursorImageStatus::Named(edges.cursor_icon()));
        self.arbor.floating_edge_hover = false;
        true
    }

    /// Shows the resize cursor while the pointer is in a floating window's resize band.
    pub(super) fn update_floating_edge_cursor(&mut self) {
        let pointer = self.arbor.seat.get_pointer().unwrap();
        if pointer.is_grabbed() {
            return;
        }
        match self.floating_edge_under(pointer.current_location()) {
            Some((_, edges)) => {
                self.arbor
                    .cursor_manager
                    .set_cursor_image(CursorImageStatus::Named(edges.cursor_icon()));
                self.arbor.floating_edge_hover = true;
            }
            None => {
                if self.arbor.floating_edge_hover {
                    self.arbor.floating_edge_hover = false;
                    self.arbor
                        .cursor_manager
                        .set_cursor_image(CursorImageStatus::default_named());
                }
            }
        }
    }
}
