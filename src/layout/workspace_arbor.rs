//! arbor additions to `Workspace`, kept out of `workspace.rs` for easy rebasing.

use super::{FloatingActive, Workspace};
use crate::layout::dock::next_minimize_seq;
use crate::layout::LayoutElement;

impl<W: LayoutElement> Workspace<W> {
    /// Minimizes a window (the active one if `id` is `None`). A docked window is hidden and its
    /// siblings take its space, but it keeps its slot; a floating window is hidden and keeps its
    /// position and size.
    pub fn minimize_window(&mut self, id: Option<&W::Id>) -> bool {
        let id = match id {
            Some(id) => id.clone(),
            None => match self.active_window() {
                Some(win) => win.id().clone(),
                None => return false,
            },
        };
        if self.floating.has_window(&id) {
            if !self.floating.minimize_window(&id, next_minimize_seq()) {
                return false;
            }
            // Nothing floating is shown anymore: focus the dock tree.
            if self.floating.is_empty() {
                self.floating_is_active = FloatingActive::No;
            }
            return true;
        }
        if !self.scrolling.minimize_window(&id) {
            return false;
        }

        // Nothing docked is shown anymore: focus the Desktop.
        if self.scrolling.is_empty() && !self.floating.is_empty() {
            self.floating_is_active = FloatingActive::Yes;
        }
        true
    }

    /// The window that restoring without a target brings back: the most recently minimized one.
    pub fn last_minimized_window(&self) -> Option<&W::Id> {
        let docked = self.scrolling.last_minimized_window();
        let floating = self.floating.last_minimized_window();
        docked
            .into_iter()
            .chain(floating)
            .max_by_key(|(seq, _)| *seq)
            .map(|(_, id)| id)
    }

    /// Shows the next (or previous) tab of the focused tab group.
    pub fn focus_tab(&mut self, forwards: bool) -> bool {
        if self.floating_is_active.get() {
            return false;
        }
        self.scrolling.focus_tab(forwards)
    }

    pub fn is_minimized(&self, id: &W::Id) -> bool {
        self.scrolling.is_minimized(id) || self.floating.is_minimized(id)
    }
}
