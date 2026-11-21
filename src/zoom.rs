//! Screen zoom (magnifier), like KWin's Zoom effect.
//!
//! A final scale of each output's whole frame, around the pointer, applied after everything else
//! (independent of the dock camera). Scaling around the pointer position `p` maps a point `x` to
//! `p + (x - p) * zoom`: the pointer stays where it is on screen, so the content under it is the
//! content it points at and input needs no remapping. Moving the pointer pans the view.
//! Screencasts and screenshots stay unzoomed.

use smithay::backend::renderer::element::utils::RescaleRenderElement;
use smithay::backend::renderer::element::{Element, Id, Kind, RenderElement, UnderlyingStorage};
use smithay::backend::renderer::gles::{GlesError, GlesFrame, GlesRenderer};
use smithay::backend::renderer::utils::{CommitCounter, DamageSet, OpaqueRegions};
use smithay::utils::user_data::UserDataMap;
use smithay::utils::{Buffer, Physical, Point, Rectangle, Scale, Transform};

use crate::animation::{Animation, Clock};
use crate::arbor::OutputRenderElements;
use crate::backend::tty::{TtyFrame, TtyRenderer, TtyRendererError};
use crate::render_helpers::renderer::ArborRenderer;

/// Below this the zoom counts as off (no wrapping, plain rendering path).
const EPSILON: f64 = 0.001;

#[derive(Debug)]
pub struct ScreenZoom {
    clock: Clock,
    /// Level we're heading to (1 = off).
    target: f64,
    anim: Option<Animation>,
}

impl ScreenZoom {
    pub fn new(clock: Clock) -> Self {
        Self {
            clock,
            target: 1.,
            anim: None,
        }
    }

    /// The current (possibly animating) zoom level.
    pub fn level(&self) -> f64 {
        match &self.anim {
            Some(anim) if !anim.is_done() => anim.value().max(1.),
            _ => self.target,
        }
    }

    pub fn target(&self) -> f64 {
        self.target
    }

    pub fn is_active(&self) -> bool {
        self.level() > 1. + EPSILON
    }

    pub fn is_animating(&self) -> bool {
        self.anim.as_ref().is_some_and(|anim| !anim.is_done())
    }

    pub fn advance_animations(&mut self) {
        if self.anim.as_ref().is_some_and(|anim| anim.is_done()) {
            self.anim = None;
        }
    }

    /// Zooms to `level` (clamped to 1..=max), animated with `config`.
    pub fn set(&mut self, level: f64, max: f64, config: arbor_config::Animation) {
        let level = level.clamp(1., max.max(1.));
        if (level - self.target).abs() < EPSILON {
            return;
        }
        let from = self.level();
        self.target = level;
        self.anim = Some(Animation::new(self.clock.clone(), from, level, 0., config));
    }

    pub fn zoom_in(&mut self, step: f64, max: f64, config: arbor_config::Animation) {
        self.set(self.target * step, max, config);
    }

    pub fn zoom_out(&mut self, step: f64, max: f64, config: arbor_config::Animation) {
        let level = self.target / step;
        // Snap to off instead of creeping towards 1.
        let level = if level < 1. + (step - 1.) / 4. {
            1.
        } else {
            level
        };
        self.set(level, max, config);
    }

    pub fn reset(&mut self, config: arbor_config::Animation) {
        self.set(1., 1., config);
    }
}

/// Where a point `x` (output-local) ends up on screen at `zoom` around `origin`.
pub fn zoomed_point(
    x: Point<f64, Physical>,
    origin: Point<f64, Physical>,
    zoom: f64,
) -> Point<f64, Physical> {
    origin + (x - origin).upscale(zoom)
}

/// An output render element behind a pointer, so that it can be nested in a rescale element
/// inside `OutputRenderElements` itself.
#[derive(Debug)]
pub struct BoxedOutputElement<R: ArborRenderer>(pub Box<OutputRenderElements<R>>);

pub type ZoomedOutputElement<R> = RescaleRenderElement<BoxedOutputElement<R>>;

/// Wraps an output element for the zoom around `origin` (output-local, physical).
pub fn zoom_element<R: ArborRenderer>(
    elem: OutputRenderElements<R>,
    origin: Point<i32, Physical>,
    zoom: f64,
) -> OutputRenderElements<R> {
    OutputRenderElements::Zoomed(RescaleRenderElement::from_element(
        BoxedOutputElement(Box::new(elem)),
        origin,
        zoom,
    ))
}

impl<R: ArborRenderer> Element for BoxedOutputElement<R> {
    fn id(&self) -> &Id {
        self.0.id()
    }

    fn current_commit(&self) -> CommitCounter {
        self.0.current_commit()
    }

    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.0.geometry(scale)
    }

    fn transform(&self) -> Transform {
        self.0.transform()
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        self.0.src()
    }

    fn damage_since(
        &self,
        scale: Scale<f64>,
        commit: Option<CommitCounter>,
    ) -> DamageSet<i32, Physical> {
        self.0.damage_since(scale, commit)
    }

    fn opaque_regions(&self, scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        self.0.opaque_regions(scale)
    }

    fn alpha(&self) -> f32 {
        self.0.alpha()
    }

    fn kind(&self) -> Kind {
        self.0.kind()
    }

    fn is_framebuffer_effect(&self) -> bool {
        self.0.is_framebuffer_effect()
    }
}

impl RenderElement<GlesRenderer> for BoxedOutputElement<GlesRenderer> {
    fn capture_framebuffer(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        cache: &UserDataMap,
    ) -> Result<(), GlesError> {
        RenderElement::<GlesRenderer>::capture_framebuffer(&*self.0, frame, src, dst, cache)
    }

    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        cache: Option<&UserDataMap>,
    ) -> Result<(), GlesError> {
        RenderElement::<GlesRenderer>::draw(
            &*self.0,
            frame,
            src,
            dst,
            damage,
            opaque_regions,
            cache,
        )
    }

    fn underlying_storage(&self, renderer: &mut GlesRenderer) -> Option<UnderlyingStorage<'_>> {
        self.0.underlying_storage(renderer)
    }
}

impl<'render> RenderElement<TtyRenderer<'render>> for BoxedOutputElement<TtyRenderer<'render>> {
    fn capture_framebuffer(
        &self,
        frame: &mut TtyFrame<'render, '_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        cache: &UserDataMap,
    ) -> Result<(), TtyRendererError<'render>> {
        RenderElement::<TtyRenderer<'render>>::capture_framebuffer(&*self.0, frame, src, dst, cache)
    }

    fn draw(
        &self,
        frame: &mut TtyFrame<'render, '_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        cache: Option<&UserDataMap>,
    ) -> Result<(), TtyRendererError<'render>> {
        RenderElement::<TtyRenderer<'render>>::draw(
            &*self.0,
            frame,
            src,
            dst,
            damage,
            opaque_regions,
            cache,
        )
    }

    fn underlying_storage(
        &self,
        renderer: &mut TtyRenderer<'render>,
    ) -> Option<UnderlyingStorage<'_>> {
        self.0.underlying_storage(renderer)
    }
}

impl crate::arbor::Arbor {
    /// The zoom origin for an output: the pointer, clamped into the output (output-local,
    /// physical).
    pub fn screen_zoom_origin(&self, output: &smithay::output::Output) -> Point<i32, Physical> {
        let Some(geo) = self.global_space.output_geometry(output) else {
            return Point::default();
        };
        let pointer = self
            .tablet_cursor_location
            .unwrap_or_else(|| self.seat.get_pointer().unwrap().current_location());
        let local = pointer - geo.loc.to_f64();
        let x = local.x.clamp(0., f64::from(geo.size.w));
        let y = local.y.clamp(0., f64::from(geo.size.h));
        let scale = output.current_scale().fractional_scale();
        Point::<f64, smithay::utils::Logical>::from((x, y))
            .to_physical(scale)
            .to_i32_round()
    }

    pub fn zoom_action(
        &mut self,
        f: impl FnOnce(&mut ScreenZoom, f64, f64, arbor_config::Animation),
    ) {
        let (step, max, anim) = {
            let config = self.config.borrow();
            (
                config.accessibility.zoom_step,
                config.accessibility.zoom_max,
                config.animations.horizontal_view_movement.0,
            )
        };
        f(&mut self.screen_zoom, step, max, anim);
        self.queue_redraw_all();
    }

    pub fn set_reduce_motion(&mut self, on: Option<bool>) {
        self.reduce_motion_override = on;
        let config = self.config.borrow();
        let reduce = on.unwrap_or(config.accessibility.reduce_motion);
        let off = config.animations.off;
        drop(config);
        self.clock.set_complete_instantly(off || reduce);
    }

    pub fn reduce_motion(&self) -> bool {
        self.reduce_motion_override
            .unwrap_or(self.config.borrow().accessibility.reduce_motion)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> arbor_config::Animation {
        arbor_config::animations::HorizontalViewMovementAnim::default().0
    }

    #[test]
    fn zoom_steps_and_clamps() {
        let mut clock = Clock::with_time(std::time::Duration::ZERO);
        clock.set_complete_instantly(true);
        let mut z = ScreenZoom::new(clock);
        assert!(!z.is_active());
        z.zoom_in(2., 5., cfg());
        assert_eq!(z.target(), 2.);
        z.zoom_in(2., 5., cfg());
        z.zoom_in(2., 5., cfg());
        assert_eq!(z.target(), 5.);
        z.zoom_out(2., 5., cfg());
        assert_eq!(z.target(), 2.5);
        z.zoom_out(2., 5., cfg());
        assert_eq!(z.target(), 1.25);
        z.zoom_out(2., 5., cfg());
        assert_eq!(z.target(), 1.);
        z.zoom_in(1.25, 16., cfg());
        z.reset(cfg());
        assert_eq!(z.target(), 1.);
    }

    #[test]
    fn pointer_stays_fixed() {
        let p = Point::from((300., 200.));
        assert_eq!(zoomed_point(p, p, 3.), p);
        let x = Point::from((310., 200.));
        assert_eq!(zoomed_point(x, p, 3.), Point::from((330., 200.)));
    }
}
