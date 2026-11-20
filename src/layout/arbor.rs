//! arbor additions to `Layout`, kept out of `mod.rs` for easy rebasing.

use super::{InteractiveMoveState, Layout, LayoutElement};

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

    pub fn is_minimized(&self, window: &W::Id) -> bool {
        self.workspaces()
            .any(|(_, _, ws)| ws.has_window(window) && ws.is_minimized(window))
    }
}
