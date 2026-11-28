//! arbor: window zones (hearth's FancyZones) on `Layout`: the zones set over IPC, Shift while
//! dragging, keyboard snapping and what the shell is told about zone drags.

use std::collections::HashMap;

use smithay::utils::{Logical, Point, Rectangle, Size};

use super::tile::Tile;
use super::{InteractiveMoveState, Layout, LayoutElement};
use crate::utils::ensure_min_max_size;

/// The window size that makes `tile` fill a zone of `zone_size` (its tile size).
pub(super) fn zone_window_size<W: LayoutElement>(
    tile: &Tile<W>,
    zone_size: Size<f64, Logical>,
) -> Size<i32, Logical> {
    let w = tile
        .window_width_for_tile_width(zone_size.w)
        .round()
        .max(1.) as i32;
    let h = tile
        .window_height_for_tile_height(zone_size.h)
        .round()
        .max(1.) as i32;
    let win = tile.window();
    let (min, max) = (win.min_size(), win.max_size());
    Size::from((
        ensure_min_max_size(w, min.w, max.w),
        ensure_min_max_size(h, min.h, max.h),
    ))
}

/// Where a tile filling `zone` goes when the window can't shrink to it (its minimum size is
/// bigger): moved just enough to stay inside `area` (the working area), so a zone at the screen's
/// right or bottom edge lines the window up with that edge instead of pushing it off screen
/// (Spotify in a narrow right-hand zone was cut off).
pub(super) fn zone_tile_loc<W: LayoutElement>(
    tile: &Tile<W>,
    zone: Rectangle<f64, Logical>,
    area: Rectangle<f64, Logical>,
) -> Point<f64, Logical> {
    let size = zone_window_size(tile, zone.size);
    let fit_w = tile.window_width_for_tile_width(zone.size.w).round().max(1.);
    let fit_h = tile.window_height_for_tile_height(zone.size.h).round().max(1.);
    // How much bigger than the zone the tile ends up.
    let extra_w = (f64::from(size.w) - fit_w).max(0.);
    let extra_h = (f64::from(size.h) - fit_h).max(0.);
    let (tile_w, tile_h) = (zone.size.w + extra_w, zone.size.h + extra_h);
    let x = zone
        .loc
        .x
        .min(area.loc.x + area.size.w - tile_w)
        .max(area.loc.x);
    let y = zone
        .loc
        .y
        .min(area.loc.y + area.size.h - tile_h)
        .max(area.loc.y);
    Point::from((x, y))
}

fn area_of(r: Rectangle<f64, Logical>) -> [f64; 4] {
    [r.loc.x, r.loc.y, r.size.w, r.size.h]
}

fn rect_of(r: [f64; 4]) -> Rectangle<f64, Logical> {
    Rectangle::new(Point::from((r[0], r[1])), Size::from((r[2], r[3])))
}

impl<W: LayoutElement> Layout<W> {
    pub fn set_zones(&mut self, zones: arbor_ipc::SetZones) {
        self.zones = zones;
        self.update_insert_hint(None);
    }

    /// The zones set over IPC (kept across in-place restarts).
    pub fn zones(&self) -> &arbor_ipc::SetZones {
        &self.zones
    }

    /// Updates whether Shift is held; during an interactive move this switches zone snapping.
    /// Returns whether anything changed.
    pub fn set_zone_modifier_held(&mut self, held: bool) -> bool {
        if self.zone_modifier_held == held {
            return false;
        }
        self.zone_modifier_held = held;
        if matches!(self.interactive_move, Some(InteractiveMoveState::Moving(_))) {
            self.update_insert_hint(None);
            return true;
        }
        false
    }

    /// The ongoing interactive move that snaps into zones, for the shell's zone overlay.
    pub fn zone_drag(&self) -> Option<arbor_ipc::ZoneDrag> {
        let Some(InteractiveMoveState::Moving(move_)) = &self.interactive_move else {
            return None;
        };
        if !super::zone_drag_active(&self.zones, self.zone_modifier_held, move_.is_floating) {
            return None;
        }
        let mon = self.monitor_for_output(&move_.output)?;
        let output_name = move_.output.name();
        let ws = &mon.workspaces[mon.active_workspace_idx];
        let area = ws.working_area();
        let zone = mon
            .insert_hint
            .as_ref()
            .and_then(|hint| hint.area)
            .and_then(|hint_area| {
                self.zones
                    .rects(&output_name, area_of(area))
                    .into_iter()
                    .position(|r| rect_of(r) == hint_area)
            })
            .map(|i| i as u32);
        Some(arbor_ipc::ZoneDrag {
            output: output_name,
            area: area_of(area),
            zone,
        })
    }

    /// Working area of every output's active workspace (output-local logical px).
    pub fn working_areas(&self) -> HashMap<String, [f64; 4]> {
        self.monitors()
            .map(|mon| {
                let ws = &mon.workspaces[mon.active_workspace_idx];
                (mon.output().name(), area_of(ws.working_area()))
            })
            .collect()
    }

    /// Moves `window` (the focused one if `None`) into zone `zone` (0-based) of its output: it
    /// floats and fills the zone. Returns false when there is no such zone or window.
    pub fn move_window_to_zone(&mut self, window: Option<&W::Id>, zone: usize) -> bool {
        if let Some(InteractiveMoveState::Moving(move_)) = &self.interactive_move {
            if window.is_none_or(|win| win == move_.tile.window().id()) {
                return false;
            }
        }

        let zones = self.zones.clone();
        for mon in self.monitors_mut() {
            let output_name = mon.output().name();
            let active_idx = mon.active_workspace_idx;
            for (idx, ws) in mon.workspaces.iter_mut().enumerate() {
                let id = match window {
                    Some(window) if ws.has_window(window) => window.clone(),
                    None if idx == active_idx => match ws.active_window() {
                        Some(win) => win.id().clone(),
                        None => return false,
                    },
                    _ => continue,
                };
                let rects = zones.rects(&output_name, area_of(ws.working_area()));
                let Some(rect) = rects.get(zone).copied().map(rect_of) else {
                    return false;
                };
                ws.set_window_floating(Some(&id), true);
                ws.place_floating_window(&id, rect);
                return true;
            }
        }
        false
    }
}
