//! arbor: what window memory (window_memory.rs) needs from the layout.

use super::{Layout, LayoutElement};
use crate::window_memory::Placement;

impl<W: LayoutElement> Layout<W> {
    /// Where `window` is now, for remembering it: its output, docked or floating, and for a
    /// floating window its position and size. `None` while maximized/fullscreen (floating) or
    /// when it isn't in the layout.
    pub fn window_placement(&self, window: &W::Id) -> Option<Placement> {
        for mon in self.monitors() {
            for ws in &mon.workspaces {
                if !ws.has_window(window) {
                    continue;
                }
                let output = mon.output().name();
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
