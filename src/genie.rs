//! arbor: the minimize effect. A minimized window pours into its dock slot (macOS's Genie) or
//! shrinks into it, and comes back out the same way when it is restored.
//!
//! The shell tells arbor where each window's dock slot is (`SetDockTargets` over IPC). arbor
//! notices windows becoming minimized or restored (whatever asked for it: a key, the title bar,
//! the dock, a foreign-toplevel client) by comparing each window's minimized flag with the last
//! one it saw, once per event-loop pass. A window that was just minimized is captured from its
//! still-attached buffers and drawn at the place it was last seen; a window that was just
//! restored is hidden in the layout while the effect draws it coming out, toward the place the
//! layout gives it (followed every frame, so siblings making room are matched).
//!
//! The deformation itself is a fragment shader (`shaders/genie.frag`) over the area covering
//! the window and its slot.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use arbor_config::animations::{MinimizeEffect, WindowMinimizeAnim};
use arbor_ipc::SetDockTargets;
use glam::{Mat3, Vec2, Vec3};
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture, Uniform};
use smithay::backend::renderer::Texture as _;
use smithay::desktop::layer_map_for_output;
use smithay::output::Output;
use smithay::utils::{Logical, Point, Rectangle, Scale, Size, Transform};

use crate::animation::{Animation, Clock};
use crate::render_helpers::shader_element::ShaderRenderElement;
use crate::render_helpers::shaders::{mat3_uniform, ProgramType, Shaders};
use crate::render_helpers::xray::XrayPos;
use crate::render_helpers::{render_to_texture, RenderCtx, RenderTarget};

type Rect = Rectangle<f64, Logical>;

/// Size of the box a window shrinks into when the shell gave no dock slot for it.
const FALLBACK_SIZE: f64 = 64.;

/// The dock slots on one output.
#[derive(Debug, Default)]
struct OutputTargets {
    /// Layer-shell namespace the rectangles are local to (`None`: output-local).
    namespace: Option<String>,
    edge: Option<Edge>,
    rects: HashMap<u64, Rect>,
}

#[derive(Debug, Default)]
pub struct Genie {
    /// Dock slots per output name.
    targets: HashMap<String, OutputTargets>,
    /// The last state seen per window id.
    known: HashMap<u64, Known>,
    anims: Vec<GenieAnim>,
}

#[derive(Debug)]
struct Known {
    minimized: bool,
    /// Where the window was last seen on screen (output, output-local rectangle).
    seen: Option<(Output, Rect)>,
}

/// A window as seen this pass, for [`Genie::update`].
pub struct SeenWindow {
    pub id: u64,
    pub minimized: bool,
    /// Where it is drawn now (output, output-local rectangle), if on screen.
    pub seen: Option<(Output, Rect)>,
}

/// What [`Genie::update`] wants done: capture these windows and start their effect.
#[derive(Debug)]
pub struct Start {
    pub id: u64,
    pub minimize: bool,
    pub output: Output,
    pub rect: Rect,
}

#[derive(Debug)]
struct GenieAnim {
    id: u64,
    output: Output,
    texture: GlesTexture,
    /// The window's on-screen rectangle (output-local): where it was (minimize) or where it is
    /// going (restore, followed every frame).
    window: Rect,
    /// The dock slot (output-local), once known. A minimizing window follows its slot as the
    /// dock makes room; a restoring one keeps the slot it came out of.
    target: Cell<Option<Target>>,
    minimize: bool,
    effect: MinimizeEffect,
    anim: Animation,
    element: RefCell<ShaderRenderElement>,
}

impl Genie {
    pub fn set_targets(&mut self, req: SetDockTargets) {
        let mut rects = HashMap::new();
        for t in req.targets {
            let ok = [t.x, t.y, t.width, t.height].iter().all(|v| v.is_finite())
                && t.width > 0.
                && t.height > 0.;
            if ok {
                let rect = Rect::new(Point::from((t.x, t.y)), Size::from((t.width, t.height)));
                rects.insert(t.window_id, rect);
            }
        }
        if rects.is_empty() {
            self.targets.remove(&req.output);
        } else {
            let namespace = req.namespace.filter(|ns| !ns.is_empty());
            let edge = match req.edge.as_deref() {
                Some("bottom") => Some(Edge::Bottom),
                Some("top") => Some(Edge::Top),
                Some("left") => Some(Edge::Left),
                Some("right") => Some(Edge::Right),
                _ => None,
            };
            self.targets.insert(
                req.output,
                OutputTargets {
                    namespace,
                    edge,
                    rects,
                },
            );
        }
    }

    /// The dock slot of window `id` on `output`, output-local.
    fn lookup(&self, id: u64, output: &Output) -> Option<Target> {
        let targets = self.targets.get(&output.name())?;
        let rect = *targets.rects.get(&id)?;
        let rect = match &targets.namespace {
            None => rect,
            Some(ns) => {
                let map = layer_map_for_output(output);
                let layer = map.layers().find(|l| l.namespace() == ns)?;
                let geo = map.layer_geometry(layer)?;
                Rect::new(rect.loc + geo.loc.to_f64(), rect.size)
            }
        };
        Some(Target {
            rect,
            edge: targets.edge,
        })
    }

    pub fn is_animating(&self) -> bool {
        !self.anims.is_empty()
    }

    pub fn is_animating_on(&self, output: &Output) -> bool {
        self.anims.iter().any(|a| &a.output == output)
    }

    /// Outputs with an effect running.
    pub fn outputs(&self) -> impl Iterator<Item = &Output> {
        self.anims.iter().map(|a| &a.output)
    }

    /// Compares the windows with the last pass. Returns the effects to start, and the windows
    /// whose tile should be shown again (their restore effect ended or was dropped).
    pub fn update(&mut self, windows: Vec<SeenWindow>, enabled: bool) -> (Vec<Start>, Vec<u64>) {
        let mut starts = Vec::new();
        let mut unhide = Vec::new();

        let mut known = HashMap::with_capacity(windows.len());
        for w in windows {
            let prev = self.known.remove(&w.id);
            if let Some(prev) = &prev {
                if prev.minimized != w.minimized && enabled {
                    if w.minimized {
                        // Where it was before it went away.
                        if let Some((output, rect)) = prev.seen.clone() {
                            starts.push(Start {
                                id: w.id,
                                minimize: true,
                                output,
                                rect,
                            });
                        }
                    } else if let Some((output, rect)) = w.seen.clone() {
                        starts.push(Start {
                            id: w.id,
                            minimize: false,
                            output,
                            rect,
                        });
                    }
                }
            }

            // Restoring windows go where the layout puts them.
            if !w.minimized {
                if let Some((output, rect)) = &w.seen {
                    for anim in &mut self.anims {
                        if anim.id == w.id && !anim.minimize && &anim.output == output {
                            anim.window = *rect;
                        }
                    }
                }
            }

            // Keep the last place it was seen while it is on screen; a minimized window keeps
            // the place it had.
            let seen = if w.minimized {
                prev.and_then(|p| p.seen)
            } else {
                w.seen
            };
            known.insert(
                w.id,
                Known {
                    minimized: w.minimized,
                    seen,
                },
            );
        }
        self.known = known;

        // Effects of closed windows, and effects replaced by a new one, end now; finished ones
        // too.
        self.anims.retain(|a| {
            let replaced = starts.iter().any(|s| s.id == a.id);
            let keep = self.known.contains_key(&a.id) && !replaced && !a.anim.is_done();
            if !keep && !a.minimize {
                unhide.push(a.id);
            }
            keep
        });

        if !starts.is_empty() || !unhide.is_empty() {
            let ids: Vec<_> = starts.iter().map(|s| (s.id, s.minimize, s.rect)).collect();
            debug!("genie: start {ids:?}, show again {unhide:?}");
        }
        (starts, unhide)
    }

    /// Captures the window (`tile`, drawn at `rect`) and starts its effect.
    #[allow(clippy::too_many_arguments)]
    pub fn start<W: crate::layout::LayoutElement>(
        &mut self,
        renderer: &mut GlesRenderer,
        tile: &crate::layout::tile::Tile<W>,
        start: Start,
        clock: Clock,
        config: WindowMinimizeAnim,
    ) -> anyhow::Result<()> {
        let _span = tracy_client::span!("Genie::start");

        if Shaders::get(renderer).program(ProgramType::Genie).is_none() {
            anyhow::bail!("no genie shader");
        }

        let scale = Scale::from(start.output.current_scale().fractional_scale());
        let size = Rect::from_size(tile.tile_size())
            .to_physical_precise_round(scale)
            .size;
        anyhow::ensure!(size.w > 0 && size.h > 0, "empty window");

        let mut elements = Vec::new();
        tile.render_unhidden(
            RenderCtx {
                renderer,
                target: RenderTarget::Output,
                xray: None,
            },
            Point::default(),
            XrayPos::default(),
            false,
            &mut |elem| elements.push(elem),
        );
        let (texture, _sync) = render_to_texture(
            renderer,
            size,
            scale,
            Transform::Normal,
            Fourcc::Abgr8888,
            elements.iter(),
        )?;

        let (from, to) = if start.minimize { (0., 1.) } else { (1., 0.) };
        let anim = Animation::new(clock, from, to, 0., config.anim);
        let target = Cell::new(self.lookup(start.id, &start.output));

        debug!(
            "genie: {} window {} from {:?} to {target:?} ({:?})",
            if start.minimize {
                "minimize"
            } else {
                "restore"
            },
            start.id,
            start.rect,
            config.effect
        );
        self.anims.push(GenieAnim {
            id: start.id,
            output: start.output,
            texture,
            window: start.rect,
            target,
            minimize: start.minimize,
            effect: config.effect,
            anim,
            element: RefCell::new(ShaderRenderElement::empty(
                ProgramType::Genie,
                Kind::Unspecified,
            )),
        });
        Ok(())
    }

    /// Follows the dock slots: a minimizing window's slot moves as the dock makes room for it,
    /// and a restoring window whose slot wasn't known yet picks it up. Called once per pass,
    /// never while rendering (the layer map is locked then).
    pub fn refresh_targets(&self) {
        for anim in &self.anims {
            if anim.minimize || anim.target.get().is_none() {
                if let Some(rect) = self.lookup(anim.id, &anim.output) {
                    anim.target.set(Some(rect));
                }
            }
        }
    }

    /// The effects on `output`, topmost first.
    pub fn render(&self, output: &Output, mut push: impl FnMut(ShaderRenderElement)) {
        let Some(mode) = output.current_mode() else {
            return;
        };
        let scale = output.current_scale().fractional_scale();
        let out_size = output
            .current_transform()
            .transform_size(mode.size)
            .to_f64()
            .to_logical(scale);

        for anim in self.anims.iter().rev() {
            if &anim.output != output {
                continue;
            }
            if let Some(elem) = anim.render(out_size, scale) {
                push(elem);
            }
        }
    }
}

/// A dock slot, output-local, and the edge its dock is on (if the shell said).
#[derive(Debug, Clone, Copy, PartialEq)]
struct Target {
    rect: Rect,
    edge: Option<Edge>,
}

/// Which way the dock is from the window.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Edge {
    Bottom,
    Top,
    Left,
    Right,
}

impl Edge {
    /// The edge of the output nearest to `target`.
    fn of(target: Rect, out: Size<f64, Logical>) -> Self {
        let c = target.loc + target.size.downscale(2.).to_point();
        let d = [
            (out.h - c.y, Edge::Bottom),
            (c.y, Edge::Top),
            (c.x, Edge::Left),
            (out.w - c.x, Edge::Right),
        ];
        d.into_iter()
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, e)| e)
            .unwrap()
    }

    /// Output coordinates to the shader's frame, where the dock is below (+y).
    fn to_canon(self) -> Mat3 {
        match self {
            Edge::Bottom => Mat3::IDENTITY,
            Edge::Top => Mat3::from_scale(Vec2::new(1., -1.)),
            // (x, y) -> (y, -x)
            Edge::Left => Mat3::from_cols(Vec3::new(0., -1., 0.), Vec3::X, Vec3::Z),
            // (x, y) -> (y, x)
            Edge::Right => Mat3::from_cols(Vec3::Y, Vec3::X, Vec3::Z),
        }
    }
}

fn rect_to_canon(m: Mat3, r: Rect) -> [f32; 4] {
    let a = m.transform_point2(Vec2::new(r.loc.x as f32, r.loc.y as f32));
    let b = m.transform_point2(Vec2::new(
        (r.loc.x + r.size.w) as f32,
        (r.loc.y + r.size.h) as f32,
    ));
    let lo = a.min(b);
    let hi = a.max(b);
    [lo.x, lo.y, hi.x, hi.y]
}

impl GenieAnim {
    fn render(&self, out_size: Size<f64, Logical>, scale: f64) -> Option<ShaderRenderElement> {
        let progress = self.anim.value().clamp(0., 1.);
        trace!("genie: window {} progress {progress:.4}", self.id);
        let win = self.window;
        if win.size.w < 1. || win.size.h < 1. {
            return None;
        }

        // Without a slot (or with the scale effect toward the bottom), shrink into a small box
        // at the bottom of the output under the window.
        let (target, edge, genie) = match self.target.get() {
            Some(t) => (t.rect, t.edge, self.effect == MinimizeEffect::Genie),
            None => {
                let cx = (win.loc.x + win.size.w / 2.).clamp(0., out_size.w);
                let t = Rect::new(
                    Point::from((cx - FALLBACK_SIZE / 2., out_size.h - FALLBACK_SIZE)),
                    Size::from((FALLBACK_SIZE, FALLBACK_SIZE)),
                );
                (t, Some(Edge::Bottom), false)
            }
        };

        let edge = edge.unwrap_or_else(|| Edge::of(target, out_size));
        let to_canon = edge.to_canon();

        // The element covers the window and its slot.
        let area = win.merge(target);
        let area = Rect::new(
            area.loc - Point::from((2., 2.)),
            area.size + Size::from((4., 4.)),
        );

        let input_to_canon = to_canon
            * Mat3::from_translation(Vec2::new(area.loc.x as f32, area.loc.y as f32))
            * Mat3::from_scale(Vec2::new(area.size.w as f32, area.size.h as f32));
        let canon_to_tex =
            Mat3::from_scale(Vec2::new(1. / win.size.w as f32, 1. / win.size.h as f32))
                * Mat3::from_translation(Vec2::new(-win.loc.x as f32, -win.loc.y as f32))
                * to_canon.inverse();

        let tex_size = self.texture.size();
        let uniforms: Rc<[Uniform<'static>]> = Rc::new([
            mat3_uniform("input_to_canon", input_to_canon),
            mat3_uniform("canon_to_tex", canon_to_tex),
            Uniform::new("win", rect_to_canon(to_canon, win)),
            Uniform::new("tgt", rect_to_canon(to_canon, target)),
            Uniform::new("progress", progress as f32),
            Uniform::new("mode", if genie { 0f32 } else { 1f32 }),
            Uniform::new("tex_size", [tex_size.w as f32, tex_size.h as f32]),
        ]);

        let mut elem = self.element.borrow_mut();
        elem.update(
            area.size,
            None,
            scale as f32,
            1.,
            uniforms,
            HashMap::from([(String::from("niri_tex"), self.texture.clone())]),
        );
        Some(elem.clone().with_location(area.loc))
    }
}

#[cfg(test)]
mod tests {
    use smithay::output::{PhysicalProperties, Subpixel};

    use super::*;

    fn output() -> Output {
        Output::new(
            String::from("test"),
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: String::new(),
                model: String::new(),
                serial_number: String::new(),
            },
        )
    }

    fn seen(id: u64, minimized: bool, output: &Output) -> SeenWindow {
        let rect = Rect::new(Point::from((10., 20.)), Size::from((300., 200.)));
        SeenWindow {
            id,
            minimized,
            seen: (!minimized).then(|| (output.clone(), rect)),
        }
    }

    #[test]
    fn minimize_and_restore_start_effects() {
        let out = output();
        let mut genie = Genie::default();

        // A window seen for the first time never animates, minimized or not.
        let (starts, _) = genie.update(vec![seen(1, false, &out), seen(2, true, &out)], true);
        assert!(starts.is_empty());

        // Minimized: starts from where it was last seen.
        let (starts, _) = genie.update(vec![seen(1, true, &out), seen(2, true, &out)], true);
        assert_eq!(starts.len(), 1);
        assert!(starts[0].minimize);
        assert_eq!(starts[0].id, 1);
        assert_eq!(starts[0].rect.size, Size::from((300., 200.)));

        // Restored: goes where the layout puts it.
        let (starts, _) = genie.update(vec![seen(1, true, &out), seen(2, false, &out)], true);
        assert_eq!(starts.len(), 1);
        assert!(!starts[0].minimize);
        assert_eq!(starts[0].id, 2);

        // Turned off: nothing.
        let (starts, _) = genie.update(vec![seen(1, false, &out), seen(2, true, &out)], false);
        assert!(starts.is_empty());
    }

    #[test]
    fn targets_replace_per_output() {
        let out = output();
        let mut genie = Genie::default();
        let target = |id| arbor_ipc::DockTarget {
            window_id: id,
            x: 100.,
            y: 700.,
            width: 52.,
            height: 52.,
        };
        genie.set_targets(SetDockTargets {
            output: String::from("test"),
            namespace: None,
            edge: Some(String::from("left")),
            targets: vec![target(1), target(2)],
        });
        let t = genie.lookup(1, &out).unwrap();
        assert_eq!(t.edge, Some(Edge::Left));
        assert_eq!(t.rect.loc, Point::from((100., 700.)));

        genie.set_targets(SetDockTargets {
            output: String::from("test"),
            targets: vec![target(2)],
            ..Default::default()
        });
        assert!(genie.lookup(1, &out).is_none());
        assert_eq!(genie.lookup(2, &out).unwrap().edge, None);

        // Empty clears.
        genie.set_targets(SetDockTargets {
            output: String::from("test"),
            ..Default::default()
        });
        assert!(genie.lookup(2, &out).is_none());
    }
}
