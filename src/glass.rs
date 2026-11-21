//! arbor: liquid glass shapes registered over IPC (`SetGlass`) and their matching to layer-shell
//! surfaces and pop-ups.
//!
//! The registry keeps one entry per target (namespace, output, popup, key). Entries are resolved
//! to mapped layer surfaces by `Arbor::refresh_glass()` whenever entries or layer surfaces change;
//! each `MappedLayer` then carries the shapes it draws (see `MappedLayer::render_normal()`).

use arbor_ipc::{GlassParams, GlassShape, SetGlass};
use smithay::backend::renderer::element::Id;
use smithay::backend::renderer::utils::CommitCounter;
use smithay::utils::{Logical, Point, Rectangle, Size};

/// Two surface sizes closer than this (logical px) are the same size.
const SIZE_TOLERANCE: f64 = 1.;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlassTarget {
    pub namespace: String,
    pub output: Option<String>,
    pub popup: bool,
    pub key: String,
}

impl GlassTarget {
    pub fn applies_to_output(&self, output: &str) -> bool {
        self.output.as_deref().is_none_or(|o| o == output)
    }
}

/// One shape with the element id and commit counter its render element uses (stable across
/// updates so the damage tracker can follow it).
#[derive(Debug, Clone)]
pub struct RegisteredShape {
    pub shape: GlassShape,
    pub id: Id,
    pub commit: CommitCounter,
}

#[derive(Debug, Clone)]
pub struct GlassEntry {
    pub target: GlassTarget,
    pub surface_size: Option<(f64, f64)>,
    pub shapes: Vec<RegisteredShape>,
}

/// A shape resolved for one surface: everything the render element needs except the surface's
/// position and the config defaults.
#[derive(Debug, Clone)]
pub struct GlassInstance {
    /// Surface-local logical rectangle.
    pub rect: Rectangle<f64, Logical>,
    /// Corner radius clamped to the rectangle.
    pub radius: f64,
    pub params: GlassParams,
    pub id: Id,
    pub commit: CommitCounter,
}

/// Shapes for the pop-ups of a layer surface.
#[derive(Debug, Clone)]
pub struct PopupGlass {
    pub surface_size: Option<(f64, f64)>,
    pub shapes: Vec<GlassInstance>,
}

#[derive(Debug, Default)]
pub struct GlassRegistry {
    entries: Vec<GlassEntry>,
}

impl GlassRegistry {
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn entries(&self) -> &[GlassEntry] {
        &self.entries
    }

    /// Applies a `SetGlass` request. Returns whether anything changed.
    pub fn set(&mut self, req: SetGlass) -> bool {
        let target = GlassTarget {
            namespace: req.namespace,
            output: req.output,
            popup: req.popup,
            key: req.key,
        };
        let pos = self.entries.iter().position(|e| e.target == target);

        if req.shapes.is_empty() {
            return match pos {
                Some(pos) => {
                    self.entries.remove(pos);
                    true
                }
                None => false,
            };
        }

        let entry = match pos {
            Some(pos) => &mut self.entries[pos],
            None => {
                self.entries.push(GlassEntry {
                    target,
                    surface_size: None,
                    shapes: Vec::new(),
                });
                self.entries.last_mut().unwrap()
            }
        };

        let mut changed = entry.surface_size != req.surface_size;
        entry.surface_size = req.surface_size;

        changed |= entry.shapes.len() != req.shapes.len();
        entry.shapes.truncate(req.shapes.len());
        for (i, shape) in req.shapes.into_iter().enumerate() {
            match entry.shapes.get_mut(i) {
                Some(old) => {
                    if old.shape != shape {
                        old.shape = shape;
                        old.commit.increment();
                        changed = true;
                    }
                }
                None => entry.shapes.push(RegisteredShape {
                    shape,
                    id: Id::new(),
                    commit: CommitCounter::default(),
                }),
            }
        }
        changed
    }

    /// Drops the non-pop-up and pop-up entries that no longer match any mapped layer surface,
    /// after a surface with `namespace` unmapped. `still_mapped(namespace, output)` says whether
    /// some mapped layer surface with that namespace remains on that output (any output when
    /// `None`). Returns whether anything was removed.
    pub fn surface_gone(
        &mut self,
        namespace: &str,
        output: &str,
        still_mapped: impl Fn(&str, Option<&str>) -> bool,
    ) -> bool {
        let before = self.entries.len();
        self.entries.retain(|e| {
            let t = &e.target;
            if t.namespace != namespace || !t.applies_to_output(output) {
                return true;
            }
            still_mapped(&t.namespace, t.output.as_deref())
        });
        before != self.entries.len()
    }
}

/// Which of the candidate surfaces (their logical sizes) an entry with `surface_size` applies to.
///
/// One candidate: that one, whatever its size. Several: the one closest to `surface_size` within
/// [`SIZE_TOLERANCE`], or every candidate when the entry gives no size.
pub fn pick_targets(surface_size: Option<(f64, f64)>, candidates: &[(f64, f64)]) -> Vec<usize> {
    match candidates.len() {
        0 => Vec::new(),
        1 => vec![0],
        _ => {
            let Some((w, h)) = surface_size else {
                return (0..candidates.len()).collect();
            };
            candidates
                .iter()
                .enumerate()
                .map(|(i, (cw, ch))| (i, (cw - w).abs().max((ch - h).abs())))
                .filter(|(_, dist)| *dist <= SIZE_TOLERANCE)
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .map(|(i, _)| vec![i])
                .unwrap_or_default()
        }
    }
}

/// Resolves an entry's shapes into instances (surface-local).
pub fn instances(entry: &GlassEntry) -> Vec<GlassInstance> {
    entry
        .shapes
        .iter()
        .filter(|s| s.shape.width > 0. && s.shape.height > 0.)
        .map(|s| {
            let shape = &s.shape;
            let rect = Rectangle::new(
                Point::from((shape.x, shape.y)),
                Size::from((shape.width, shape.height)),
            );
            GlassInstance {
                rect,
                radius: clamp_radius(shape.radius, rect.size),
                params: shape.params,
                id: s.id.clone(),
                commit: s.commit,
            }
        })
        .collect()
}

pub fn clamp_radius(radius: f64, size: Size<f64, Logical>) -> f64 {
    radius.clamp(0., (size.w.min(size.h) / 2.).max(0.))
}

/// Snaps a logical rectangle's edges to the physical pixel grid of `scale` (each edge rounds on
/// its own, so a shape's far edge lands where the client's own rendering puts it at fractional
/// scales instead of drifting with a separately rounded size).
pub fn snap_rect(rect: Rectangle<f64, Logical>, scale: f64) -> Rectangle<f64, Logical> {
    let x0 = (rect.loc.x * scale).round();
    let y0 = (rect.loc.y * scale).round();
    let x1 = ((rect.loc.x + rect.size.w) * scale).round();
    let y1 = ((rect.loc.y + rect.size.h) * scale).round();
    Rectangle::new(
        Point::from((x0 / scale, y0 / scale)),
        Size::from(((x1 - x0).max(0.) / scale, (y1 - y0).max(0.) / scale)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(ns: &str, shapes: Vec<GlassShape>) -> SetGlass {
        SetGlass {
            namespace: ns.to_owned(),
            shapes,
            ..Default::default()
        }
    }

    fn shape(x: f64, w: f64) -> GlassShape {
        GlassShape {
            x,
            y: 0.,
            width: w,
            height: 40.,
            radius: 30.,
            params: GlassParams::default(),
        }
    }

    #[test]
    fn set_replace_and_clear() {
        let mut reg = GlassRegistry::default();
        assert!(reg.set(req("dock", vec![shape(0., 100.)])));
        let id = reg.entries()[0].shapes[0].id.clone();
        let commit = reg.entries()[0].shapes[0].commit;

        // Same shape again: no change, same id and commit.
        assert!(!reg.set(req("dock", vec![shape(0., 100.)])));
        assert_eq!(reg.entries()[0].shapes[0].commit, commit);

        // Moving keeps the id (the damage tracker follows it) and bumps the commit.
        assert!(reg.set(req("dock", vec![shape(5., 100.), shape(200., 50.)])));
        let s = &reg.entries()[0].shapes;
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].id, id);
        assert_ne!(s[0].commit, commit);
        assert_ne!(s[1].id, id);

        // Other targets are independent.
        let mut other = req("dock", vec![shape(0., 10.)]);
        other.key = "k".into();
        assert!(reg.set(other));
        let mut popup = req("dock", vec![shape(0., 10.)]);
        popup.popup = true;
        assert!(reg.set(popup));
        assert_eq!(reg.entries().len(), 3);

        assert!(reg.set(req("dock", vec![])));
        assert_eq!(reg.entries().len(), 2);
        assert!(!reg.set(req("dock", vec![])));
    }

    #[test]
    fn surface_gone_drops_unmatched_entries() {
        let mut reg = GlassRegistry::default();
        reg.set(req("menu", vec![shape(0., 10.)]));
        let mut on_out = req("menu", vec![shape(0., 10.)]);
        on_out.output = Some("DP-2".into());
        reg.set(on_out);
        reg.set(req("dock", vec![shape(0., 10.)]));

        // Another "menu" surface is still mapped on DP-1 (so the any-output entry stays), none on
        // DP-2.
        let removed = reg.surface_gone("menu", "DP-2", |ns, out| {
            ns == "menu" && out.is_none_or(|o| o == "DP-1")
        });
        assert!(removed);
        let left: Vec<_> = reg
            .entries()
            .iter()
            .map(|e| (e.target.namespace.as_str(), e.target.output.clone()))
            .collect();
        assert_eq!(left, [("menu", None), ("dock", None)]);

        // Last one gone.
        assert!(reg.surface_gone("menu", "DP-1", |_, _| false));
        assert_eq!(reg.entries().len(), 1);
        // Entries for other outputs are not touched.
        let mut on_out = req("dock", vec![shape(0., 10.)]);
        on_out.output = Some("DP-2".into());
        reg.set(on_out);
        assert!(!reg.surface_gone("dock", "DP-1", |_, out| out.is_none()));
        assert_eq!(reg.entries().len(), 2);
    }

    #[test]
    fn pick_targets_by_size() {
        assert!(pick_targets(None, &[]).is_empty());
        // A single candidate always matches (sizes may lag behind while a surface resizes).
        assert_eq!(pick_targets(Some((10., 10.)), &[(300., 200.)]), [0]);
        // Several: by size, within a pixel.
        let c = [(300., 200.), (120., 40.), (120.6, 40.)];
        assert_eq!(pick_targets(Some((120.5, 40.)), &c), [2]);
        assert_eq!(pick_targets(Some((300., 199.5)), &c), [0]);
        assert!(pick_targets(Some((50., 50.)), &c).is_empty());
        assert_eq!(pick_targets(None, &c), [0, 1, 2]);
    }

    #[test]
    fn instances_clamp_and_skip_empty() {
        let mut reg = GlassRegistry::default();
        reg.set(req("x", vec![shape(0., 100.), shape(0., 0.)]));
        let inst = instances(&reg.entries()[0]);
        assert_eq!(inst.len(), 1);
        // Radius 30 on a 40 px tall shape is a pill: 20.
        assert_eq!(inst[0].radius, 20.);
        assert_eq!(clamp_radius(-3., Size::from((10., 10.))), 0.);
    }

    #[test]
    fn snap_rect_rounds_edges_at_fractional_scale() {
        let scale = 4. / 3.;
        let r = snap_rect(
            Rectangle::new((10.2, 0.4).into(), (100.3, 33.3).into()),
            scale,
        );
        // Edges land on whole physical pixels.
        for v in [r.loc.x, r.loc.y, r.loc.x + r.size.w, r.loc.y + r.size.h] {
            let phys = v * scale;
            assert!((phys - phys.round()).abs() < 1e-9, "{v} -> {phys}");
        }
        // And within half a physical pixel of where they were.
        assert!((r.loc.x - 10.2).abs() <= 0.5 / scale);
        assert!((r.loc.x + r.size.w - 110.5).abs() <= 0.5 / scale);
        // Integer scale leaves whole numbers alone.
        let r = snap_rect(Rectangle::new((3., 4.).into(), (5., 6.).into()), 1.);
        assert_eq!(r, Rectangle::new((3., 4.).into(), (5., 6.).into()));
    }
}
