//! arbor additions to `Layout`, kept out of `mod.rs` for easy rebasing.

use smithay::output::Output;
use smithay::utils::{Logical, Point};

use super::title_bar::TitleBarHit;
use super::{InteractiveMoveState, Layout, LayoutElement, MonitorSet};

impl<W: LayoutElement> Layout<W> {
    /// Minimizes a docked window (the focused one if `window` is `None`).
    pub fn minimize_window(&mut self, window: Option<&W::Id>) {
        if let Some(InteractiveMoveState::Moving(move_)) = &self.interactive_move {
            if window.is_none_or(|win| win == move_.tile.window().id()) {
                return;
            }
        }

        let workspace = match window {
            Some(window) => self.workspaces_mut().find(|ws| ws.has_window(window)),
            None => self.active_workspace_mut(),
        };
        if let Some(ws) = workspace {
            ws.minimize_window(window);
        }
    }

    /// Restores a minimized window into its slot and focuses it. With `window` set to `None`,
    /// restores the most recently minimized window on the active workspace.
    pub fn restore_window(&mut self, window: Option<&W::Id>) {
        let window = match window {
            Some(window) => window.clone(),
            None => {
                let Some(ws) = self.active_workspace() else {
                    return;
                };
                let Some(window) = ws.last_minimized_window() else {
                    return;
                };
                window.clone()
            }
        };

        // Activating a minimized window restores it.
        self.activate_window(&window);
    }

    /// Shows the next (or previous) tab of the focused tab group.
    pub fn focus_tab(&mut self, forwards: bool) {
        if let Some(ws) = self.active_workspace_mut() {
            ws.focus_tab(forwards);
        }
    }

    /// The title bar part under the pointer, and its window.
    pub fn title_bar_under(
        &self,
        output: &Output,
        pos_within_output: Point<f64, Logical>,
    ) -> Option<(&W, TitleBarHit)> {
        if self.interactive_move.is_some() || self.is_overview_open() {
            return None;
        }
        let mon = self.monitor_for_output(output)?;
        let (ws, geo) = mon.workspace_under(pos_within_output)?;
        ws.title_bar_under(pos_within_output - geo.loc)
    }

    /// Updates the title bar button hover state for the pointer. Returns whether anything
    /// changed.
    pub fn update_title_bar_hover(
        &mut self,
        pointer: Option<(&Output, Point<f64, Logical>)>,
    ) -> bool {
        let overview = self.is_overview_open();
        let MonitorSet::Normal { monitors, .. } = &mut self.monitor_set else {
            return false;
        };

        let mut changed = false;
        for mon in monitors {
            let under = pointer
                .filter(|(output, _)| !overview && *output == &mon.output)
                .and_then(|(_, pos)| {
                    let (ws, geo) = mon.workspace_under(pos)?;
                    Some((ws.id(), pos - geo.loc))
                });
            for ws in &mut mon.workspaces {
                let pos = under.filter(|(id, _)| *id == ws.id()).map(|(_, pos)| pos);
                changed |= ws.update_title_bar_hover(pos);
            }
        }
        changed
    }

    /// Scale of the interactively moved window: the overview zoom, times the camera zoom of the
    /// workspace under it for docked windows.
    pub(super) fn interactive_move_zoom(&self) -> f64 {
        let zoom = self.overview_zoom();
        let Some(InteractiveMoveState::Moving(move_)) = &self.interactive_move else {
            return zoom;
        };
        if move_.is_floating {
            return zoom;
        }
        let camera_zoom = self
            .monitor_for_output(&move_.output)
            .and_then(|mon| mon.workspace_under(move_.pointer_pos_within_output))
            .map_or(1., |(ws, _)| ws.dock().camera_view().zoom);
        zoom * camera_zoom
    }

    /// Runs a camera action on the active workspace's dock tree.
    pub fn camera_action(&mut self, f: impl FnOnce(&mut super::dock::DockSpace<W>)) {
        if let Some(ws) = self.active_workspace_mut() {
            f(ws.dock_mut());
        }
    }

    /// Frames `window` with its workspace's camera and focuses it. A window drawn as part of
    /// an icon tile frames the whole tile.
    pub fn camera_frame_window(&mut self, window: &W::Id) {
        self.activate_window(window);
        if let Some(ws) = self.workspaces_mut().find(|ws| ws.has_window(window)) {
            let dock = ws.dock_mut();
            if !dock.camera_frame_icon_tile(Some(window)) {
                dock.camera_frame_window(Some(window));
            }
        }
    }

    /// Frames the icon tile of the focused docked window, if it is drawn as one. Returns
    /// whether it did.
    pub fn frame_focused_icon_tile(&mut self) -> bool {
        let Some(ws) = self.active_workspace_mut() else {
            return false;
        };
        if ws
            .active_window()
            .is_none_or(|win| ws.is_floating(win.id()))
        {
            return false;
        }
        ws.dock_mut().camera_frame_icon_tile(None)
    }

    /// Zooms the camera of the workspace under the pointer by `factor` around the pointer.
    pub fn camera_zoom_at(
        &mut self,
        output: &Output,
        pos_within_output: Point<f64, Logical>,
        factor: f64,
    ) {
        if self.is_overview_open() {
            return;
        }
        let Some(mon) = self.monitor_for_output(output) else {
            return;
        };
        let Some((ws, geo)) = mon.workspace_under(pos_within_output) else {
            return;
        };
        let (id, pos) = (ws.id(), pos_within_output - geo.loc);
        if let Some(ws) = self.workspaces_mut().find(|ws| ws.id() == id) {
            ws.dock_mut().camera_zoom_by(factor, pos);
        }
    }

    /// Whether a plain pinch at the pointer zooms the camera: over a zoomed-out dock tree, or
    /// over empty space.
    pub fn camera_pinch_allowed(
        &self,
        output: &Output,
        pos_within_output: Point<f64, Logical>,
    ) -> bool {
        if self.interactive_move.is_some() || self.is_overview_open() {
            return false;
        }
        let Some(mon) = self.monitor_for_output(output) else {
            return false;
        };
        let Some((ws, geo)) = mon.workspace_under(pos_within_output) else {
            return false;
        };
        let pos = pos_within_output - geo.loc;
        match ws.window_under(pos) {
            None => true,
            // Zoomed-out docked windows take no input themselves; floating ones keep theirs.
            Some((win, _)) => ws.dock().is_zoomed() && !ws.is_floating(win.id()),
        }
    }

    /// Ends a pinch on the workspace under the pointer.
    pub fn camera_pinch_end(&mut self, output: &Output, pos_within_output: Point<f64, Logical>) {
        let Some(mon) = self.monitor_for_output(output) else {
            return;
        };
        let Some((ws, _)) = mon.workspace_under(pos_within_output) else {
            return;
        };
        let id = ws.id();
        if let Some(ws) = self.workspaces_mut().find(|ws| ws.id() == id) {
            ws.dock_mut().camera_pinch_end();
        }
    }

    /// The docked window under the pointer while its camera is zoomed.
    pub fn zoomed_dock_window_under(
        &self,
        output: &Output,
        pos_within_output: Point<f64, Logical>,
    ) -> Option<&W> {
        if self.interactive_move.is_some() || self.is_overview_open() {
            return None;
        }
        let mon = self.monitor_for_output(output)?;
        let (ws, geo) = mon.workspace_under(pos_within_output)?;
        ws.zoomed_dock_window_under(pos_within_output - geo.loc)
    }

    pub fn is_minimized(&self, window: &W::Id) -> bool {
        self.workspaces()
            .any(|(_, _, ws)| ws.has_window(window) && ws.is_minimized(window))
    }
}
