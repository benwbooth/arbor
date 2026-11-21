//! arbor: clicks, drags and hover in the Mission Control-style overview (exposé).
//!
//! - Left click on a window: focus it and close the overview.
//! - Left click on its close button (top right, on hover) or middle click: close the window.
//! - Drag a window onto a workspace thumbnail: move it to that workspace (the "+" thumbnail at the
//!   end makes a new one).
//! - Left click on a workspace thumbnail: show that workspace.
//! - Left click on empty space: close the overview.

use smithay::backend::input::MouseButton;
use smithay::desktop::Window;
use smithay::input::pointer::{
    AxisFrame, ButtonEvent, CursorIcon, CursorImageStatus, Focus, GestureHoldBeginEvent,
    GestureHoldEndEvent, GesturePinchBeginEvent, GesturePinchEndEvent, GesturePinchUpdateEvent,
    GestureSwipeBeginEvent, GestureSwipeEndEvent, GestureSwipeUpdateEvent,
    GrabStartData as PointerGrabStartData, MotionEvent, PointerGrab, PointerInnerHandle,
    RelativeMotionEvent,
};
use smithay::input::SeatHandler;
use smithay::output::Output;
use smithay::utils::{Logical, Point, Serial};

use crate::arbor::State;
use crate::layout::expose::ExposeHit;

/// Pointer movement that turns a press on a window into a drag.
const DRAG_THRESHOLD: f64 = 8.;

impl State {
    /// Handles a button press in the overview. Returns whether it was over the exposé.
    pub(super) fn try_expose_press(
        &mut self,
        button: Option<MouseButton>,
        button_code: u32,
        serial: Serial,
    ) -> bool {
        let pointer = self.arbor.seat.get_pointer().unwrap();
        if !self.arbor.layout.is_overview_open() || pointer.is_grabbed() {
            return false;
        }
        if self.arbor.is_locked()
            || self.arbor.screenshot_ui.is_open()
            // Panels (top layer) stay clickable above the overview.
            || self.arbor.pointer_contents.layer.is_some()
        {
            return false;
        }

        let location = pointer.current_location();
        let Some((output, pos)) = self.arbor.output_under(location) else {
            return false;
        };
        let output = output.clone();
        let Some(hit) = self.arbor.layout.expose_hit(&output, pos) else {
            return false;
        };

        // A portal asked the user to pick a window: a click picks it.
        if self.arbor.portal_window_pick.is_some() {
            match (button, hit) {
                (Some(MouseButton::Left), ExposeHit::Window { id, .. }) => {
                    let picked = self
                        .arbor
                        .layout
                        .windows()
                        .find(|(_, m)| m.window == id)
                        .map(|(_, m)| m.id());
                    if let Some(tx) = self.arbor.portal_window_pick.take() {
                        let _ = tx.try_send(picked);
                    }
                    self.arbor.layout.close_overview();
                }
                (Some(MouseButton::Left), ExposeHit::Workspace(idx)) => {
                    self.arbor.layout.focus_output(&output);
                    self.arbor.layout.switch_workspace(idx);
                }
                (Some(MouseButton::Left), ExposeHit::Empty) => {
                    // Closing the overview cancels the pick (see State::refresh).
                    self.arbor.layout.close_overview();
                }
                _ => (),
            }
            self.arbor.queue_redraw_all();
            return true;
        }

        match (button, hit) {
            (Some(MouseButton::Left), ExposeHit::Window { id, close: true })
            | (Some(MouseButton::Middle), ExposeHit::Window { id, .. }) => {
                if let Some((_, mapped)) = self.arbor.layout.windows().find(|(_, m)| m.window == id)
                {
                    mapped.toplevel().send_close();
                }
            }
            (Some(MouseButton::Left), ExposeHit::Window { id, close: false }) => {
                let start_data = PointerGrabStartData {
                    focus: None,
                    button: button_code,
                    location,
                };
                let grab = ExposeGrab::new(start_data, output, id);
                pointer.set_grab(self, grab, serial, Focus::Clear);
            }
            (Some(MouseButton::Left), ExposeHit::Workspace(idx)) => {
                self.arbor.layout.focus_output(&output);
                self.arbor.layout.switch_workspace(idx);
            }
            (Some(MouseButton::Left), ExposeHit::Empty) => {
                self.arbor.layout.close_overview();
            }
            // Other buttons do nothing over the exposé.
            _ => (),
        }

        self.arbor.queue_redraw_all();
        true
    }

    /// Hover highlight and close button in the overview.
    pub(super) fn update_expose_pointer(&mut self) {
        let pointer = self.arbor.seat.get_pointer().unwrap();
        if pointer.is_grabbed() {
            // A drag updates the exposé itself.
            return;
        }
        let target = if self.arbor.pointer_contents.layer.is_some() {
            None
        } else {
            self.arbor
                .output_under(pointer.current_location())
                .map(|(output, pos)| (output.clone(), pos))
        };
        let changed = self
            .arbor
            .layout
            .expose_set_pointer(target.as_ref().map(|(output, pos)| (output, *pos)));
        if changed {
            self.arbor.queue_redraw_all();
        }
    }
}

/// A press on a window in the exposé: a click focuses it, a drag can move it to another
/// workspace.
pub struct ExposeGrab {
    start_data: PointerGrabStartData<State>,
    output: Output,
    window: Window,
    dragging: bool,
}

impl ExposeGrab {
    fn new(start_data: PointerGrabStartData<State>, output: Output, window: Window) -> Self {
        Self {
            start_data,
            output,
            window,
            dragging: false,
        }
    }

    fn on_motion(&mut self, state: &mut State, location: Point<f64, Logical>) {
        let Some(geo) = state.arbor.global_space.output_geometry(&self.output) else {
            return;
        };
        let pos = location - geo.loc.to_f64();

        if !self.dragging {
            let d = location - self.start_data.location;
            if d.x * d.x + d.y * d.y < DRAG_THRESHOLD * DRAG_THRESHOLD {
                return;
            }
            self.dragging = true;
            state
                .arbor
                .layout
                .expose_drag_begin(&self.output, &self.window, pos);
            state
                .arbor
                .cursor_manager
                .set_cursor_image(CursorImageStatus::Named(CursorIcon::Grabbing));
        }
        state.arbor.layout.expose_drag_update(&self.output, pos);
        state.arbor.queue_redraw(&self.output);
    }

    fn on_ungrab(&mut self, state: &mut State) {
        let layout = &mut state.arbor.layout;
        if self.dragging {
            layout.expose_drag_end();
            state
                .arbor
                .cursor_manager
                .set_cursor_image(CursorImageStatus::default_named());
        } else {
            // A click: focus the window and close the overview, switching to its workspace
            // with the overview animation if needed.
            let res = layout.workspaces().find_map(|(mon, ws_idx, ws)| {
                ws.windows()
                    .any(|w| w.window == self.window)
                    .then(|| (mon.map(|mon| mon.output().clone()), ws_idx))
            });
            if let Some((Some(output), ws_idx)) = res {
                layout.focus_output(&output);
                if layout.is_overview_open() {
                    layout.toggle_overview_to_workspace(ws_idx);
                }
            }
            layout.activate_window(&self.window);
        }
        state.arbor.queue_redraw_all();
    }
}

impl PointerGrab<State> for ExposeGrab {
    fn motion(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        _focus: Option<(<State as SeatHandler>::PointerFocus, Point<f64, Logical>)>,
        event: &MotionEvent,
    ) {
        handle.motion(data, None, event);
        self.on_motion(data, event.location);
    }

    fn relative_motion(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        _focus: Option<(<State as SeatHandler>::PointerFocus, Point<f64, Logical>)>,
        event: &RelativeMotionEvent,
    ) {
        handle.relative_motion(data, None, event);
    }

    fn button(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &ButtonEvent,
    ) {
        handle.button(data, event);
        if handle.current_pressed().is_empty() {
            handle.unset_grab(self, data, event.serial, event.time, true);
        }
    }

    fn axis(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        details: AxisFrame,
    ) {
        handle.axis(data, details);
    }

    fn frame(&mut self, data: &mut State, handle: &mut PointerInnerHandle<'_, State>) {
        handle.frame(data);
    }

    fn gesture_swipe_begin(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GestureSwipeBeginEvent,
    ) {
        handle.gesture_swipe_begin(data, event);
    }

    fn gesture_swipe_update(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GestureSwipeUpdateEvent,
    ) {
        handle.gesture_swipe_update(data, event);
    }

    fn gesture_swipe_end(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GestureSwipeEndEvent,
    ) {
        handle.gesture_swipe_end(data, event);
    }

    fn gesture_pinch_begin(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GesturePinchBeginEvent,
    ) {
        handle.gesture_pinch_begin(data, event);
    }

    fn gesture_pinch_update(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GesturePinchUpdateEvent,
    ) {
        handle.gesture_pinch_update(data, event);
    }

    fn gesture_pinch_end(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GesturePinchEndEvent,
    ) {
        handle.gesture_pinch_end(data, event);
    }

    fn gesture_hold_begin(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GestureHoldBeginEvent,
    ) {
        handle.gesture_hold_begin(data, event);
    }

    fn gesture_hold_end(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GestureHoldEndEvent,
    ) {
        handle.gesture_hold_end(data, event);
    }

    fn start_data(&self) -> &PointerGrabStartData<State> {
        &self.start_data
    }

    fn unset(&mut self, data: &mut State) {
        self.on_ungrab(data);
    }
}
