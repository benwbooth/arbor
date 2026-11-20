//! arbor additions to `Workspace`, kept out of `workspace.rs` for easy rebasing.

use smithay::utils::{Logical, Point};

use super::{FloatingActive, Workspace};
use crate::layout::dock::next_minimize_seq;
use crate::layout::title_bar::TitleBarHit;
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

    /// The title bar part under `pos`, on the topmost window under it.
    pub fn title_bar_under(&self, pos: Point<f64, Logical>) -> Option<(&W, TitleBarHit)> {
        let (tile, tile_pos) = self.topmost_tile_under(pos)?;
        let hit = tile.title_bar_hit(pos - tile_pos)?;
        Some((tile.window(), hit))
    }

    fn topmost_tile_under(
        &self,
        pos: Point<f64, Logical>,
    ) -> Option<(&crate::layout::tile::Tile<W>, Point<f64, Logical>)> {
        let zoomed = self.scrolling.is_zoomed();
        self.tiles_with_render_positions()
            .filter(|(_, _, visible)| *visible)
            // Zoomed-out docked windows have no title bar interaction.
            .filter(|(tile, _, _)| !zoomed || self.floating.has_window(tile.window().id()))
            .find(|(tile, tile_pos, _)| tile.hit(pos - *tile_pos).is_some())
            .map(|(tile, tile_pos, _)| (tile, tile_pos))
    }

    /// Updates the title bar button hover state for the pointer at `pos` (or away from this
    /// workspace for `None`). Returns whether anything changed.
    pub fn update_title_bar_hover(&mut self, pos: Option<Point<f64, Logical>>) -> bool {
        let target = pos.and_then(|pos| {
            self.topmost_tile_under(pos)
                .map(|(tile, tile_pos)| (tile.window().id().clone(), pos - tile_pos))
        });

        let mut changed = false;
        for tile in self.tiles_mut() {
            let point = target
                .as_ref()
                .filter(|(id, _)| id == tile.window().id())
                .map(|(_, point)| *point);
            changed |= tile.set_title_bar_hover(point);
        }
        changed
    }

    /// The dock tree.
    pub fn dock(&self) -> &crate::layout::dock::DockSpace<W> {
        &self.scrolling
    }

    /// The dock tree (for camera actions).
    pub fn dock_mut(&mut self) -> &mut crate::layout::dock::DockSpace<W> {
        &mut self.scrolling
    }

    /// The docked window under `pos` while the camera is zoomed (when docked windows only take
    /// activation clicks).
    pub fn zoomed_dock_window_under(&self, pos: Point<f64, Logical>) -> Option<&W> {
        if !self.scrolling.is_zoomed() {
            return None;
        }
        let (win, _) = self.window_under(pos)?;
        (!self.floating.has_window(win.id())).then_some(win)
    }

    pub fn is_minimized(&self, id: &W::Id) -> bool {
        self.scrolling.is_minimized(id) || self.floating.is_minimized(id)
    }
}
