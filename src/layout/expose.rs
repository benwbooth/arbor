//! arbor: layout math for the Mission Control-style overview ("exposé").
//!
//! The overview shows every window of a workspace scaled down, side by side, in rows that fill
//! the output's working area. Windows keep their rough relative positions: rows are taken top
//! to bottom by the windows' centres, and each row is ordered left to right. All windows are
//! scaled by the same factor (never above 1), chosen as the largest one that fits, trying every
//! row count. A label strip under each window holds its title.
//!
//! Everything here is pure geometry so it can be unit tested; rendering and input live in
//! `monitor_expose.rs`.

use smithay::utils::{Logical, Point, Rectangle, Size};

type Rect = Rectangle<f64, Logical>;

/// Spacing parameters of the exposé layout.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ExposeParams {
    /// Gap between windows (horizontal) and between rows (vertical, below the labels).
    pub gap: f64,
    /// Height of the label strip under each window, including the gap above it.
    pub label_height: f64,
    /// Largest scale a window may get; 1 keeps windows from growing past their real size.
    pub max_scale: f64,
}

impl Default for ExposeParams {
    fn default() -> Self {
        Self {
            gap: 32.,
            label_height: 32.,
            max_scale: 1.,
        }
    }
}

/// Where a window goes in the exposé: its scaled rectangle (without the label) and its scale.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ExposeSlot {
    pub rect: Rect,
    pub scale: f64,
}

impl ExposeSlot {
    /// The label strip under the window.
    pub fn label_rect(&self, params: &ExposeParams) -> Rect {
        Rect::new(
            Point::from((self.rect.loc.x, self.rect.loc.y + self.rect.size.h)),
            Size::from((self.rect.size.w, params.label_height)),
        )
    }
}

fn center(r: &Rect) -> Point<f64, Logical> {
    Point::from((r.loc.x + r.size.w / 2., r.loc.y + r.size.h / 2.))
}

/// Splits windows (already sorted top to bottom) into `rows` consecutive groups of roughly
/// equal total width.
fn split_rows(order: &[usize], windows: &[Rect], rows: usize) -> Vec<Vec<usize>> {
    let total: f64 = order.iter().map(|&i| windows[i].size.w.max(1.)).sum();
    let target = total / rows as f64;

    let mut out: Vec<Vec<usize>> = Vec::with_capacity(rows);
    let mut current = Vec::new();
    let mut width = 0.;
    for (pos, &i) in order.iter().enumerate() {
        let w = windows[i].size.w.max(1.);
        let remaining_windows = order.len() - pos;
        let remaining_rows = rows - out.len();
        // Start a new row when this window would overshoot the target by more than it
        // undershoots without it, or when every remaining row needs one of the remaining
        // windows.
        let overshoots = width + w - target > target - width;
        if !current.is_empty()
            && remaining_rows > 1
            && (overshoots || remaining_windows < remaining_rows)
        {
            out.push(std::mem::take(&mut current));
            width = 0.;
        }
        current.push(i);
        width += w;
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// Computes the exposé slots for `windows` (their real rectangles, in any common coordinate
/// space) inside `area`. Returns one slot per window, in the same order. Slots never overlap
/// (labels included) and always lie inside `area`.
pub fn layout(windows: &[Rect], area: Rect, params: &ExposeParams) -> Vec<ExposeSlot> {
    let n = windows.len();
    if n == 0 || area.size.w <= 0. || area.size.h <= 0. {
        return windows
            .iter()
            .map(|_| ExposeSlot {
                rect: Rect::new(area.loc, Size::from((0., 0.))),
                scale: 0.,
            })
            .collect();
    }

    // Top to bottom by centre, then left to right, then by index: deterministic.
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| {
        let (ca, cb) = (center(&windows[a]), center(&windows[b]));
        ca.y.total_cmp(&cb.y)
            .then(ca.x.total_cmp(&cb.x))
            .then(a.cmp(&b))
    });

    let gap = params.gap.max(0.);
    let label = params.label_height.max(0.);

    let mut best: Option<(f64, Vec<Vec<usize>>)> = None;
    for rows in 1..=n {
        let mut grouped = split_rows(&order, windows, rows);
        for row in &mut grouped {
            row.sort_by(|&a, &b| {
                center(&windows[a])
                    .x
                    .total_cmp(&center(&windows[b]).x)
                    .then(a.cmp(&b))
            });
        }
        let rows = grouped.len();

        // Largest scale that fits horizontally (every row) and vertically (all rows).
        let mut scale = params.max_scale;
        for row in &grouped {
            let w: f64 = row.iter().map(|&i| windows[i].size.w.max(1.)).sum();
            let avail = area.size.w - gap * (row.len() - 1) as f64;
            scale = scale.min(avail / w);
        }
        let heights: f64 = grouped
            .iter()
            .map(|row| {
                row.iter()
                    .map(|&i| windows[i].size.h.max(1.))
                    .fold(0., f64::max)
            })
            .sum();
        let avail = area.size.h - label * rows as f64 - gap * (rows - 1) as f64;
        scale = scale.min(avail / heights);

        if scale <= 0. {
            continue;
        }
        // Strictly better only: ties keep fewer rows.
        if best.as_ref().is_none_or(|(s, _)| scale > *s + 1e-9) {
            best = Some((scale, grouped));
        }
    }

    let Some((scale, grouped)) = best else {
        // Nothing fits (the area is smaller than the gaps): collapse everything to points.
        return windows
            .iter()
            .map(|_| ExposeSlot {
                rect: Rect::new(center(&area), Size::from((0., 0.))),
                scale: 0.,
            })
            .collect();
    };

    let row_heights: Vec<f64> = grouped
        .iter()
        .map(|row| {
            row.iter()
                .map(|&i| windows[i].size.h.max(1.) * scale)
                .fold(0., f64::max)
        })
        .collect();
    let total_h: f64 = row_heights.iter().sum::<f64>()
        + label * grouped.len() as f64
        + gap * (grouped.len() - 1) as f64;

    let mut slots = vec![
        ExposeSlot {
            rect: Rect::default(),
            scale,
        };
        n
    ];
    let mut y = area.loc.y + ((area.size.h - total_h) / 2.).max(0.);
    for (row, row_h) in grouped.iter().zip(&row_heights) {
        let row_w: f64 = row
            .iter()
            .map(|&i| windows[i].size.w.max(1.) * scale)
            .sum::<f64>()
            + gap * (row.len() - 1) as f64;
        let mut x = area.loc.x + ((area.size.w - row_w) / 2.).max(0.);
        for &i in row {
            let size = Size::from((
                windows[i].size.w.max(1.) * scale,
                windows[i].size.h.max(1.) * scale,
            ));
            // Bottom-align windows in a row so the labels line up.
            let loc = Point::from((x, y + row_h - size.h));
            slots[i] = ExposeSlot {
                rect: Rect::new(loc, size),
                scale,
            };
            x += size.w + gap;
        }
        y += row_h + label + gap;
    }

    slots
}

/// Linear interpolation between two values.
pub fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

/// Thumbnails of the workspace bar along the top: `count` thumbnails with the output's aspect
/// ratio, centred in `area` (the bar's strip), as large as fits up to `area`'s height.
pub fn bar_layout(count: usize, area: Rect, aspect: f64, gap: f64) -> Vec<Rect> {
    if count == 0 || area.size.w <= 0. || area.size.h <= 0. {
        return Vec::new();
    }
    let aspect = if aspect.is_finite() && aspect > 0. {
        aspect
    } else {
        16. / 9.
    };
    let gaps = gap * (count - 1) as f64;
    let mut h = area.size.h;
    let mut w = h * aspect;
    let max_w = (area.size.w - gaps) / count as f64;
    if w > max_w {
        w = max_w.max(0.);
        h = w / aspect;
    }
    let total = w * count as f64 + gaps;
    let x0 = area.loc.x + (area.size.w - total) / 2.;
    let y = area.loc.y + (area.size.h - h) / 2.;
    (0..count)
        .map(|i| Rect::new(Point::from((x0 + i as f64 * (w + gap), y)), Size::from((w, h))))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f64, y: f64, w: f64, h: f64) -> Rect {
        Rect::new(Point::from((x, y)), Size::from((w, h)))
    }

    fn with_label(slot: &ExposeSlot, params: &ExposeParams) -> Rect {
        let r = slot.rect;
        Rect::new(r.loc, Size::from((r.size.w, r.size.h + params.label_height)))
    }

    fn overlaps(a: &Rect, b: &Rect) -> bool {
        a.loc.x < b.loc.x + b.size.w - 1e-6
            && b.loc.x < a.loc.x + a.size.w - 1e-6
            && a.loc.y < b.loc.y + b.size.h - 1e-6
            && b.loc.y < a.loc.y + a.size.h - 1e-6
    }

    fn inside(inner: &Rect, outer: &Rect) -> bool {
        inner.loc.x >= outer.loc.x - 1e-6
            && inner.loc.y >= outer.loc.y - 1e-6
            && inner.loc.x + inner.size.w <= outer.loc.x + outer.size.w + 1e-6
            && inner.loc.y + inner.size.h <= outer.loc.y + outer.size.h + 1e-6
    }

    fn check(windows: &[Rect], area: Rect, params: &ExposeParams) -> Vec<ExposeSlot> {
        let slots = layout(windows, area, params);
        assert_eq!(slots.len(), windows.len());
        for (i, a) in slots.iter().enumerate() {
            let ra = with_label(a, params);
            assert!(inside(&ra, &area), "slot {i} {ra:?} outside {area:?}");
            assert!(a.scale > 0. && a.scale <= params.max_scale + 1e-9);
            // Aspect ratio kept.
            let w = &windows[i];
            assert!((a.rect.size.w / a.rect.size.h - w.size.w / w.size.h).abs() < 1e-6);
            for (j, b) in slots.iter().enumerate().skip(i + 1) {
                let rb = with_label(b, params);
                assert!(!overlaps(&ra, &rb), "slots {i} and {j} overlap: {ra:?} {rb:?}");
            }
        }
        slots
    }

    /// A small deterministic pseudo-random generator for the stress test.
    fn lcg(seed: &mut u64) -> f64 {
        *seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (*seed >> 11) as f64 / (1u64 << 53) as f64
    }

    #[test]
    fn empty() {
        assert!(layout(&[], rect(0., 0., 100., 100.), &ExposeParams::default()).is_empty());
    }

    #[test]
    fn single_window_is_centred_and_not_upscaled() {
        let params = ExposeParams::default();
        let area = rect(0., 100., 1920., 980.);
        let slots = check(&[rect(10., 10., 400., 300.)], area, &params);
        assert_eq!(slots[0].scale, 1.);
        let c = center(&slots[0].rect);
        assert!((c.x - 960.).abs() < 1e-6);
    }

    #[test]
    fn big_window_shrinks_to_fit() {
        let params = ExposeParams::default();
        let area = rect(0., 0., 1000., 600.);
        let slots = check(&[rect(0., 0., 1920., 1080.)], area, &params);
        assert!(slots[0].scale < 1.);
    }

    #[test]
    fn keeps_relative_order() {
        let params = ExposeParams::default();
        let area = rect(0., 0., 1920., 1080.);
        // Side by side: stays left to right.
        let windows = [rect(960., 0., 960., 1080.), rect(0., 0., 960., 1080.)];
        let slots = check(&windows, area, &params);
        assert!(slots[1].rect.loc.x < slots[0].rect.loc.x);

        // Four quadrants: two rows, order kept.
        let windows = [
            rect(0., 0., 960., 540.),
            rect(960., 0., 960., 540.),
            rect(0., 540., 960., 540.),
            rect(960., 540., 960., 540.),
        ];
        let slots = check(&windows, area, &params);
        assert!(slots[0].rect.loc.x < slots[1].rect.loc.x);
        assert!(slots[2].rect.loc.x < slots[3].rect.loc.x);
        assert!(slots[0].rect.loc.y < slots[2].rect.loc.y);
        assert!(slots[1].rect.loc.y < slots[3].rect.loc.y);
    }

    #[test]
    fn deterministic() {
        let params = ExposeParams::default();
        let area = rect(0., 120., 2560., 1320.);
        let windows: Vec<Rect> = (0..7)
            .map(|i| rect(i as f64 * 100., 50., 800., 600.))
            .collect();
        let a = layout(&windows, area, &params);
        let b = layout(&windows, area, &params);
        assert_eq!(a, b);
    }

    #[test]
    fn identical_windows_do_not_overlap() {
        let params = ExposeParams::default();
        let area = rect(0., 0., 1920., 1080.);
        let windows = vec![rect(100., 100., 800., 600.); 5];
        check(&windows, area, &params);
    }

    #[test]
    fn many_random_windows_fit_without_overlap() {
        let mut seed = 42;
        for case in 0..200 {
            let n = 1 + (case % 17);
            let area = rect(
                lcg(&mut seed) * 50.,
                lcg(&mut seed) * 150.,
                400. + lcg(&mut seed) * 3000.,
                300. + lcg(&mut seed) * 1500.,
            );
            let params = ExposeParams {
                gap: lcg(&mut seed) * 40.,
                label_height: lcg(&mut seed) * 40.,
                max_scale: 1.,
            };
            let windows: Vec<Rect> = (0..n)
                .map(|_| {
                    rect(
                        lcg(&mut seed) * 2000. - 200.,
                        lcg(&mut seed) * 1200. - 100.,
                        20. + lcg(&mut seed) * 2500.,
                        20. + lcg(&mut seed) * 1500.,
                    )
                })
                .collect();
            check(&windows, area, &params);
        }
    }

    #[test]
    fn bar_thumbnails_fit() {
        let area = rect(0., 10., 1000., 100.);
        let thumbs = bar_layout(3, area, 16. / 9., 12.);
        assert_eq!(thumbs.len(), 3);
        for t in &thumbs {
            assert!(inside(t, &area));
            assert!((t.size.w / t.size.h - 16. / 9.).abs() < 1e-6);
        }
        assert!(!overlaps(&thumbs[0], &thumbs[1]));

        // Too many for the width: they shrink.
        let thumbs = bar_layout(20, area, 16. / 9., 12.);
        for t in &thumbs {
            assert!(inside(t, &area));
        }
        assert!(thumbs[0].size.h < 100.);
    }
}
