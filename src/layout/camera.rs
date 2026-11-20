//! arbor: the camera over the dock tree.
//!
//! The dock tree is laid out on a canvas (canvas coordinates are workspace coordinates when the
//! camera is at rest at the origin). The camera maps canvas to workspace view coordinates:
//! `view = (canvas - offset) * zoom`.

use smithay::utils::{Logical, Point, Rectangle};

use crate::animation::Animation;

type Rect = Rectangle<f64, Logical>;

/// A camera position.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CameraView {
    /// Canvas point shown at the view origin.
    pub offset: Point<f64, Logical>,
    /// Scale from canvas to view; 1 is actual size.
    pub zoom: f64,
}

impl Default for CameraView {
    fn default() -> Self {
        Self {
            offset: Point::from((0., 0.)),
            zoom: 1.,
        }
    }
}

impl CameraView {
    pub fn is_identity(&self) -> bool {
        self.zoom == 1. && self.offset == Point::from((0., 0.))
    }

    pub fn to_view(&self, p: Point<f64, Logical>) -> Point<f64, Logical> {
        (p - self.offset).upscale(self.zoom)
    }

    pub fn to_canvas(&self, p: Point<f64, Logical>) -> Point<f64, Logical> {
        p.downscale(self.zoom) + self.offset
    }

    pub fn rect_to_view(&self, r: Rect) -> Rect {
        Rect::new(self.to_view(r.loc), r.size.upscale(self.zoom))
    }

    /// The canvas rectangle shown in a view area.
    pub fn shown(&self, area: Rect) -> Rect {
        Rect::new(self.to_canvas(area.loc), area.size.downscale(self.zoom))
    }

    /// The camera that shows `rect` centered in `area`, as large as fits but no larger than
    /// actual size.
    pub fn framing(rect: Rect, area: Rect) -> Self {
        let zoom = if rect.size.w > 0. && rect.size.h > 0. {
            f64::min(
                1.,
                f64::min(area.size.w / rect.size.w, area.size.h / rect.size.h),
            )
        } else {
            1.
        };
        let zoom = zoom.max(0.01);
        let rect_center = rect.loc + rect.size.downscale(2.).to_point();
        let area_center = area.loc + area.size.downscale(2.).to_point();
        Self {
            offset: rect_center - area_center.downscale(zoom),
            zoom,
        }
    }

    /// Interpolates between two cameras by interpolating the shown rectangles, which keeps
    /// zooming visually linear.
    pub fn lerp(a: Self, b: Self, t: f64, area: Rect) -> Self {
        let ra = a.shown(area);
        let rb = b.shown(area);
        let loc = ra.loc + (rb.loc - ra.loc).upscale(t);
        let w = ra.size.w + (rb.size.w - ra.size.w) * t;
        let zoom = if w > 0. { area.size.w / w } else { b.zoom };
        Self {
            offset: loc - area.loc.downscale(zoom),
            zoom,
        }
    }
}

/// Pans `view` (at its zoom) as little as possible so that `rect` (plus `margin`) is fully
/// shown in `area`, then clamps so that the view stays within `bounds` where possible.
pub fn pan_to_show(
    view: CameraView,
    rect: Rect,
    margin: f64,
    area: Rect,
    bounds: Rect,
) -> CameraView {
    let shown = view.shown(area);
    let mut offset = view.offset;

    let axis = |lo: f64, len: f64, shown_lo: f64, shown_len: f64| -> f64 {
        let (lo, hi) = (lo - margin, lo + len + margin);
        if hi - lo > shown_len || lo < shown_lo {
            lo - shown_lo
        } else if hi > shown_lo + shown_len {
            hi - (shown_lo + shown_len)
        } else {
            0.
        }
    };
    offset.x += axis(rect.loc.x, rect.size.w, shown.loc.x, shown.size.w);
    offset.y += axis(rect.loc.y, rect.size.h, shown.loc.y, shown.size.h);

    let view = CameraView {
        offset,
        zoom: view.zoom,
    };
    clamp_to_bounds(view, area, bounds)
}

/// Keeps the shown area within `bounds` along each axis where the bounds are larger than the
/// shown area; centers along axes where they are smaller.
pub fn clamp_to_bounds(view: CameraView, area: Rect, bounds: Rect) -> CameraView {
    let shown = view.shown(area);
    let mut offset = view.offset;

    let axis = |shown_lo: f64, shown_len: f64, lo: f64, len: f64| -> f64 {
        if len <= shown_len {
            (lo + len / 2.) - (shown_lo + shown_len / 2.)
        } else if shown_lo < lo {
            lo - shown_lo
        } else if shown_lo + shown_len > lo + len {
            (lo + len) - (shown_lo + shown_len)
        } else {
            0.
        }
    };
    offset.x += axis(shown.loc.x, shown.size.w, bounds.loc.x, bounds.size.w);
    offset.y += axis(shown.loc.y, shown.size.h, bounds.loc.y, bounds.size.h);
    CameraView {
        offset,
        zoom: view.zoom,
    }
}

/// What the camera does when the layout changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CameraMode {
    /// Actual size, panning to keep the focused window visible.
    Follow,
    /// Framing the node at this depth on the path to the focused leaf.
    Framed { depth: usize },
    /// Framing the whole canvas.
    ShowAll,
    /// Left where the user put it (pinch zoom, history).
    Free,
}

/// Camera state of a dock space.
#[derive(Debug)]
pub struct Camera {
    pub mode: CameraMode,
    /// Where the camera is going (or is).
    pub target: CameraView,
    /// Ongoing move: where it started and the progress animation.
    anim: Option<(CameraView, Animation)>,
    /// Past positions for back/forward, with the focused leaf at the time.
    history: Vec<HistoryEntry>,
    /// Position in `history` of the current position when navigating it.
    history_idx: usize,
}

const HISTORY_LEN: usize = 32;

/// A remembered camera position and the focused leaf (by raw id) at the time.
pub type HistoryEntry = (CameraView, CameraMode, Option<u64>);

impl Default for Camera {
    fn default() -> Self {
        Self {
            mode: CameraMode::Follow,
            target: CameraView::default(),
            anim: None,
            history: Vec::new(),
            history_idx: 0,
        }
    }
}

impl Camera {
    /// The camera position right now, considering the animation.
    pub fn current(&self, area: Rect) -> CameraView {
        match &self.anim {
            Some((from, anim)) => CameraView::lerp(*from, self.target, anim.value(), area),
            None => self.target,
        }
    }

    pub fn is_animating(&self) -> bool {
        self.anim.is_some()
    }

    pub fn advance_animations(&mut self) {
        if self.anim.as_ref().is_some_and(|(_, anim)| anim.is_done()) {
            self.anim = None;
        }
    }

    /// Moves to `view`, animating from the current position if `anim` is given.
    pub fn move_to(&mut self, view: CameraView, area: Rect, anim: Option<Animation>) {
        if view == self.target {
            return;
        }
        let from = self.current(area);
        self.target = view;
        self.anim = anim.map(|anim| (from, anim));
    }

    /// Remembers the current position before an explicit camera action.
    pub fn push_history(&mut self, focus: Option<u64>) {
        self.history.truncate(self.history_idx);
        self.history.push((self.target, self.mode, focus));
        if self.history.len() > HISTORY_LEN {
            self.history.remove(0);
        }
        self.history_idx = self.history.len();
    }

    /// Steps back in history; returns the position to go to.
    pub fn back(&mut self, focus: Option<u64>) -> Option<HistoryEntry> {
        if self.history_idx == 0 {
            return None;
        }
        // Remember where we are so that forward can come back.
        if self.history_idx == self.history.len() {
            self.history.push((self.target, self.mode, focus));
        }
        self.history_idx -= 1;
        Some(self.history[self.history_idx])
    }

    /// Steps forward in history; returns the position to go to.
    pub fn forward(&mut self) -> Option<HistoryEntry> {
        if self.history_idx + 1 >= self.history.len() {
            return None;
        }
        self.history_idx += 1;
        Some(self.history[self.history_idx])
    }
}

#[cfg(test)]
mod tests {
    use smithay::utils::Size;

    use super::*;

    fn rect(x: f64, y: f64, w: f64, h: f64) -> Rect {
        Rect::new(Point::from((x, y)), Size::from((w, h)))
    }

    #[test]
    fn mapping_round_trips() {
        let view = CameraView {
            offset: Point::from((100., 50.)),
            zoom: 0.5,
        };
        let p = Point::from((300., 250.));
        assert_eq!(view.to_view(p), Point::from((100., 100.)));
        assert_eq!(view.to_canvas(view.to_view(p)), p);
        assert_eq!(
            view.shown(rect(0., 0., 1000., 500.)),
            rect(100., 50., 2000., 1000.)
        );
    }

    #[test]
    fn framing_fits_and_centers() {
        let area = rect(0., 0., 1000., 500.);
        // Fits at actual size: zoom 1, centered.
        let v = CameraView::framing(rect(100., 100., 400., 200.), area);
        assert_eq!(v.zoom, 1.);
        assert_eq!(
            v.to_view(Point::from((300., 200.))),
            Point::from((500., 250.))
        );
        // Too big: zoomed out to fit the limiting axis.
        let v = CameraView::framing(rect(0., 0., 4000., 1000.), area);
        assert_eq!(v.zoom, 0.25);
        let shown = v.shown(area);
        assert_eq!(shown.size, Size::from((4000., 2000.)));
        assert_eq!(shown.loc, Point::from((0., -500.)));
        // Identity for the whole area.
        assert!(CameraView::framing(area, area).is_identity());
    }

    #[test]
    fn lerp_interpolates_shown_rect() {
        let area = rect(0., 0., 1000., 500.);
        let a = CameraView::default();
        let b = CameraView::framing(rect(0., 0., 2000., 1000.), area);
        assert_eq!(CameraView::lerp(a, b, 0., area), a);
        let end = CameraView::lerp(a, b, 1., area);
        assert!((end.zoom - b.zoom).abs() < 1e-9);
        let mid = CameraView::lerp(a, b, 0.5, area);
        assert!((mid.shown(area).size.w - 1500.).abs() < 1e-9);
    }

    #[test]
    fn pan_to_show_moves_minimally_and_clamps() {
        let area = rect(0., 0., 1000., 500.);
        let bounds = rect(0., 0., 3000., 500.);
        let v = CameraView::default();
        // Already visible: no move.
        assert_eq!(
            pan_to_show(v, rect(100., 0., 300., 500.), 10., area, bounds),
            v
        );
        // Off to the right: pan just enough.
        let v2 = pan_to_show(v, rect(1500., 0., 400., 500.), 10., area, bounds);
        assert_eq!(v2.offset, Point::from((910., 0.)));
        // Back to the left.
        let v3 = pan_to_show(v2, rect(0., 0., 400., 500.), 10., area, bounds);
        assert_eq!(v3.offset, Point::from((0., 0.)));
        // Never past the canvas.
        let v4 = pan_to_show(v, rect(2900., 0., 100., 500.), 50., area, bounds);
        assert_eq!(v4.offset, Point::from((2000., 0.)));
    }

    #[test]
    fn history_back_and_forward() {
        let mut c = Camera::default();
        let area = rect(0., 0., 100., 100.);
        let v1 = CameraView {
            offset: Point::from((10., 0.)),
            zoom: 1.,
        };
        c.push_history(Some(1));
        c.move_to(v1, area, None);
        c.mode = CameraMode::ShowAll;

        let (back, mode, focus) = c.back(Some(2)).unwrap();
        assert_eq!(back, CameraView::default());
        assert_eq!(mode, CameraMode::Follow);
        assert_eq!(focus, Some(1));
        c.move_to(back, area, None);
        assert!(c.back(Some(1)).is_none());

        let (fwd, mode, focus) = c.forward().unwrap();
        assert_eq!(fwd, v1);
        assert_eq!(mode, CameraMode::ShowAll);
        assert_eq!(focus, Some(2));
        assert!(c.forward().is_none());
    }
}
