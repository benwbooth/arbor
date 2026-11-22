//! Dock-tree operations on `Workspace`: minimize and restore, tab groups, title bars and the
//! camera.

use smithay::utils::{Logical, Point};

use super::{FloatingActive, Workspace};
use crate::layout::dock::next_minimize_seq;
use crate::layout::title_bar::TitleBarHit;
use crate::layout::LayoutElement;

impl<W: LayoutElement> Workspace<W> {
    /// The dock tree shape (session restore).
    pub fn dock_shape(&self) -> Option<crate::layout::dock::DockShape<W::Id>> {
        self.dock.shape()
    }

    /// The dock camera mode and target (in-place restarts).
    pub fn camera_state(
        &self,
    ) -> (
        crate::layout::camera::CameraMode,
        crate::layout::camera::CameraView,
    ) {
        self.dock.camera_state()
    }

    /// Puts the dock camera back (in-place restarts).
    pub fn restore_camera(
        &mut self,
        mode: crate::layout::camera::CameraMode,
        view: crate::layout::camera::CameraView,
    ) {
        self.dock.restore_camera(mode, view);
    }

    /// Whether `window`'s dock column is maximized (floating windows never are).
    pub fn is_pending_maximized(&self, window: &W::Id) -> bool {
        self.dock
            .columns()
            .find(|col| col.contains(window))
            .is_some_and(|col| col.is_pending_maximized())
    }

    /// Rearranges the docked windows into `shape` (session restore).
    pub fn apply_dock_shape(&mut self, shape: &crate::layout::dock::DockShape<W::Id>) {
        self.dock.apply_shape(shape);
    }

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
        if !self.dock.minimize_window(&id) {
            return false;
        }

        // Nothing docked is shown anymore: focus the Desktop.
        if self.dock.is_empty() && !self.floating.is_empty() {
            self.floating_is_active = FloatingActive::Yes;
        }
        true
    }

    /// The window that restoring without a target brings back: the most recently minimized one.
    pub fn last_minimized_window(&self) -> Option<&W::Id> {
        let docked = self.dock.last_minimized_window();
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
        self.dock.focus_tab(forwards)
    }

    /// Shown windows for the overview's exposé, topmost first: the tile, where it is drawn
    /// (view coordinates) and the scale it is drawn at (the camera zoom for docked windows).
    /// Minimized windows and hidden tabs are left out.
    pub fn expose_tiles(&self) -> Vec<(&crate::layout::tile::Tile<W>, Point<f64, Logical>, f64)> {
        let zoom = self.dock.camera_view().zoom;
        self.tiles_with_render_positions()
            .filter(|(_, _, visible)| *visible)
            .map(|(tile, pos, _)| {
                let through_camera =
                    !self.floating.has_window(tile.window().id()) && tile.sizing_mode().is_normal();
                (tile, pos, if through_camera { zoom } else { 1. })
            })
            .collect()
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
        let zoomed = self.dock.is_zoomed();
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
        &self.dock
    }

    /// The dock tree (for camera actions).
    pub fn dock_mut(&mut self) -> &mut crate::layout::dock::DockSpace<W> {
        &mut self.dock
    }

    /// The docked window under `pos` while the camera is zoomed (when docked windows only take
    /// activation clicks).
    pub fn zoomed_dock_window_under(&self, pos: Point<f64, Logical>) -> Option<&W> {
        if !self.dock.is_zoomed() {
            return None;
        }
        let (win, _) = self.window_under(pos)?;
        (!self.floating.has_window(win.id())).then_some(win)
    }

    pub fn is_minimized(&self, id: &W::Id) -> bool {
        self.dock.is_minimized(id) || self.floating.is_minimized(id)
    }
}
