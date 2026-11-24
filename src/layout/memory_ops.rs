//! arbor: what window memory (window_memory.rs) needs from the layout.

use super::{Layout, LayoutElement};
use crate::window_memory::Placement;

impl<W: LayoutElement> Layout<W> {
    /// Where `window` is now, for remembering it: its output, docked or floating, and for a
    /// floating window its position and size. `None` while maximized or fullscreen, or when it
    /// isn't in the layout.
    pub fn window_placement(&self, window: &W::Id) -> Option<Placement> {
        for mon in self.monitors() {
            for ws in &mon.workspaces {
                if !ws.has_window(window) {
                    continue;
                }
                let output = mon.output().name();
                // Fullscreen or maximized: nothing to remember (the last ordinary placement
                // stands). A floating window that goes fullscreen is docked meanwhile, so
                // recording it then made the app reopen docked, behind the Desktop's floating
                // windows, where it couldn't be brought to the front.
                if ws
                    .windows()
                    .find(|w| w.id() == window)
                    .is_some_and(|w| !w.pending_sizing_mode().is_normal())
                {
                    return None;
                }
                if !ws.is_floating(window) {
                    return Some(Placement {
                        output,
                        docked: true,
                        pos: None,
                        size: None,
                    });
                }
                let (pos, size) = ws.floating_placement(window)?;
                return Some(Placement {
                    output,
                    docked: false,
                    pos: Some((pos.x.round(), pos.y.round())),
                    size: Some((size.w, size.h)),
                });
            }
        }
        None
    }
}
