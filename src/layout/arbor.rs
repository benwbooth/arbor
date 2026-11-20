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

    /// Runs a camera action on the active workspace's dock tree.
    pub fn camera_action(&mut self, f: impl FnOnce(&mut super::dock::DockSpace<W>)) {
        if let Some(ws) = self.active_workspace_mut() {
            f(ws.dock_mut());
        }
    }

    /// Frames `window` with its workspace's camera and focuses it.
    pub fn camera_frame_window(&mut self, window: &W::Id) {
        self.activate_window(window);
        if let Some(ws) = self.workspaces_mut().find(|ws| ws.has_window(window)) {
            ws.dock_mut().camera_frame_window(Some(window));
        }
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
