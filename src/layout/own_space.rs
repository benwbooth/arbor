//! arbor: fullscreen and maximized windows get a workspace of their own, as on macOS.
//!
//! A window that goes fullscreen or maximizes while sharing its workspace moves to a new
//! workspace right after that one, and the view follows it. When it is restored it goes back to
//! the workspace it came from, into the same place in the dock tree if nothing there changed
//! meanwhile, and the emptied workspace goes away. Closing it shows the workspace it came from.

use super::dock::DockShape;
use super::workspace::WorkspaceId;
use super::{ActivateWindow, Layout, LayoutElement, MonitorSet};

/// A window living on a workspace of its own.
#[derive(Debug)]
pub(super) struct OwnSpace<Id> {
    window: Id,
    /// The workspace it came from.
    home: WorkspaceId,
    /// Its own workspace.
    own: WorkspaceId,
    /// The home workspace's dock tree just before the window left, to put it back exactly; `None`
    /// when the window was floating.
    home_shape: Option<DockShape<Id>>,
}

impl<W: LayoutElement> Layout<W> {
    /// Whether `window` is maximized or fullscreen (pending).
    fn own_space_is_big(&self, window: &W::Id) -> bool {
        self.windows()
            .find(|(_, win)| win.id() == window)
            .is_some_and(|(_, win)| !win.pending_sizing_mode().is_normal())
    }

    /// Whether `window`'s dock column is maximized (also while fullscreen on top of that).
    pub(super) fn own_space_is_maximized(&self, window: &W::Id) -> bool {
        self.workspaces()
            .find(|(_, _, ws)| ws.has_window(window))
            .is_some_and(|(_, _, ws)| ws.is_pending_maximized(window))
    }

    /// Whether `window` is fullscreen (pending).
    pub(super) fn own_space_is_fullscreen(&self, window: &W::Id) -> bool {
        self.windows()
            .find(|(_, win)| win.id() == window)
            .is_some_and(|(_, win)| win.pending_sizing_mode().is_fullscreen())
    }

    /// Called before `window`'s sizing changes, with whether it will be maximized or fullscreen
    /// afterwards: moves it to its own workspace first, so it sizes to that one. Returns whether
    /// it was maximized or fullscreen, for [`Self::own_space_after_sizing`].
    pub(super) fn own_space_before_sizing(&mut self, window: &W::Id, big_after: bool) -> bool {
        let big_now = self.own_space_is_big(window);
        if !big_now && big_after {
            self.own_space_enter(window);
        }
        big_now
    }

    /// Called after `window`'s sizing changed: once restored, it goes back home (restored first,
    /// so a window that was floating floats again).
    pub(super) fn own_space_after_sizing(&mut self, window: &W::Id, was_big: bool) {
        if was_big && !self.own_space_is_big(window) {
            self.own_space_leave(window);
        }
    }

    /// A window just mapped already maximized or fullscreen: give it its own workspace too.
    pub fn own_space_on_map(&mut self, window: &W::Id) {
        if !self.own_space_is_big(window) {
            return;
        }
        // Maximized is a dock column property and doesn't move with the window.
        let maximized = self.own_space_is_maximized(window);
        self.own_space_enter(window);
        if maximized && !self.own_space_is_maximized(window) {
            if let Some(ws) = self.workspaces_mut().find(|ws| ws.has_window(window)) {
                ws.set_maximized(window, true);
            }
        }
    }

    fn own_space_enter(&mut self, window: &W::Id) {
        if !self.options.layout.fullscreen_own_workspace
            || self.own_spaces.iter().any(|s| &s.window == window)
        {
            return;
        }
        let MonitorSet::Normal { monitors, .. } = &mut self.monitor_set else {
            return;
        };
        let Some((mon, ws_idx)) = monitors.iter_mut().find_map(|mon| {
            let idx = mon.workspaces.iter().position(|ws| ws.has_window(window))?;
            Some((mon, idx))
        }) else {
            return;
        };
        let ws = &mon.workspaces[ws_idx];
        // Alone on an unnamed workspace already: that is its own.
        if ws.name().is_none() && ws.windows().count() <= 1 {
            return;
        }
        let home = ws.id();
        let home_shape = if ws.is_floating(window) {
            None
        } else {
            ws.dock_shape()
        };

        mon.add_workspace_at(ws_idx + 1);
        let own = mon.workspaces[ws_idx + 1].id();
        mon.move_to_workspace(Some(window), ws_idx + 1, ActivateWindow::Yes);

        self.own_spaces.push(OwnSpace {
            window: window.clone(),
            home,
            own,
            home_shape,
        });
    }

    fn own_space_leave(&mut self, window: &W::Id) {
        let Some(idx) = self.own_spaces.iter().position(|s| &s.window == window) else {
            return;
        };
        let space = self.own_spaces.remove(idx);
        let MonitorSet::Normal { monitors, .. } = &mut self.monitor_set else {
            return;
        };
        let Some(mon) = monitors
            .iter_mut()
            .find(|mon| mon.workspaces.iter().any(|ws| ws.id() == space.own))
        else {
            return;
        };
        // Still alone on its own workspace? (It may have been moved, or joined by others.)
        let Some(own) = mon.workspaces.iter().find(|ws| ws.id() == space.own) else {
            return;
        };
        if !own.has_window(window) || own.windows().count() > 1 {
            return;
        }
        let Some(home_idx) = mon.workspaces.iter().position(|ws| ws.id() == space.home) else {
            // Home is gone (or on another monitor): stay.
            return;
        };

        // Put it back into its exact place only if the home tree is as it was without it.
        let home_ws = &mon.workspaces[home_idx];
        let shape = space.home_shape.filter(|saved| {
            let without = saved.filter_map(&|id: &W::Id| (id != window).then(|| id.clone()));
            match (home_ws.dock_shape(), without) {
                (None, None) => true,
                (Some(now), Some(without)) => arrangement(&now) == arrangement(&without),
                _ => false,
            }
        });

        mon.move_to_workspace(Some(window), home_idx, ActivateWindow::Yes);

        if let Some(shape) = shape {
            if let Some(ws) = mon.workspaces.iter_mut().find(|ws| ws.id() == space.home) {
                ws.apply_dock_shape(&shape);
            }
        }
    }

    /// `window` is being removed from the layout (closed or hidden). If it lived on its own
    /// workspace and that is shown, show the workspace it came from.
    pub(super) fn own_space_take_on_remove(&mut self, window: &W::Id) -> Option<OwnSpace<W::Id>> {
        let idx = self.own_spaces.iter().position(|s| &s.window == window)?;
        Some(self.own_spaces.remove(idx))
    }

    pub(super) fn own_space_removed(&mut self, space: OwnSpace<W::Id>) {
        let MonitorSet::Normal { monitors, .. } = &mut self.monitor_set else {
            return;
        };
        for mon in monitors {
            let active = mon.workspaces[mon.active_workspace_idx].id();
            if active != space.own {
                continue;
            }
            if let Some(home_idx) = mon.workspaces.iter().position(|ws| ws.id() == space.home) {
                mon.switch_workspace(home_idx);
            }
            return;
        }
    }
}

/// A dock shape's arrangement, for comparing: splits and tab groups without fractions, which
/// tab is shown, one-child splits or splits nested in the same direction.
fn arrangement<Id: Clone + PartialEq>(shape: &DockShape<Id>) -> DockShape<Id> {
    match shape {
        DockShape::Leaf { tabs, .. } => DockShape::Leaf {
            tabs: tabs.clone(),
            active: 0,
        },
        DockShape::Split {
            vertical, children, ..
        } => {
            let mut out = Vec::new();
            for child in children {
                match arrangement(child) {
                    DockShape::Split {
                        vertical: v,
                        children: c,
                        ..
                    } if v == *vertical => out.extend(c),
                    other => out.push(other),
                }
            }
            if out.len() == 1 {
                out.pop().unwrap()
            } else {
                DockShape::Split {
                    vertical: *vertical,
                    fractions: Vec::new(),
                    children: out,
                }
            }
        }
    }
}
