//! A surface drawn with one colour (its background) see-through and everything else as
//! opaque as it was: for apps that draw their menus opaque (Chromium, Electron), so the
//! blur behind shows through the menu like it does through a translucent one, while text
//! and highlights stay solid.

use arbor_config::Color;
use smithay::backend::renderer::element::surface::WaylandSurfaceRenderElement;
use smithay::backend::renderer::element::{Element, Id, Kind, RenderElement, UnderlyingStorage};
use smithay::backend::renderer::gles::{GlesError, GlesFrame, GlesRenderer, GlesTexProgram, Uniform};
use smithay::backend::renderer::utils::{CommitCounter, DamageSet, OpaqueRegions};
use smithay::utils::user_data::UserDataMap;
use smithay::utils::{Buffer, Physical, Rectangle, Scale, Transform};

use super::renderer::{ArborRenderer, AsGlesFrame as _};
use super::shaders::Shaders;
use crate::backend::tty::{TtyFrame, TtyRenderer, TtyRendererError};

#[derive(Debug)]
pub struct SeeThroughSurfaceRenderElement<R: ArborRenderer> {
    inner: WaylandSurfaceRenderElement<R>,
    program: GlesTexProgram,
    key: [f32; 3],
    key_alpha: f32,
}

impl<R: ArborRenderer> SeeThroughSurfaceRenderElement<R> {
    pub fn new(
        elem: WaylandSurfaceRenderElement<R>,
        program: GlesTexProgram,
        key: Color,
        key_alpha: f32,
    ) -> Self {
        Self {
            inner: elem,
            program,
            key: [key.r, key.g, key.b],
            key_alpha: key_alpha.clamp(0., 1.),
        }
    }

    pub fn shader(renderer: &mut R) -> Option<&GlesTexProgram> {
        Shaders::get(renderer).see_through.as_ref()
    }

    fn uniforms(&self) -> Vec<Uniform<'static>> {
        vec![
            Uniform::new("key", self.key),
            Uniform::new("key_alpha", self.key_alpha),
        ]
    }
}

impl<R: ArborRenderer> Element for SeeThroughSurfaceRenderElement<R> {
    fn id(&self) -> &Id {
        self.inner.id()
    }

    fn current_commit(&self) -> CommitCounter {
        self.inner.current_commit()
    }

    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.inner.geometry(scale)
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        self.inner.src()
    }

    fn transform(&self) -> Transform {
        self.inner.transform()
    }

    fn damage_since(
        &self,
        scale: Scale<f64>,
        commit: Option<CommitCounter>,
    ) -> DamageSet<i32, Physical> {
        self.inner.damage_since(scale, commit)
    }

    fn opaque_regions(&self, _scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        // The key colour is drawn see-through wherever it is.
        OpaqueRegions::default()
    }

    fn alpha(&self) -> f32 {
        self.inner.alpha()
    }

    fn kind(&self) -> Kind {
        self.inner.kind()
    }
}

impl RenderElement<GlesRenderer> for SeeThroughSurfaceRenderElement<GlesRenderer> {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        cache: Option<&UserDataMap>,
    ) -> Result<(), GlesError> {
        frame.override_default_tex_program(self.program.clone(), self.uniforms());
        RenderElement::<GlesRenderer>::draw(
            &self.inner,
            frame,
            src,
            dst,
            damage,
            opaque_regions,
            cache,
        )?;
        frame.clear_tex_program_override();
        Ok(())
    }

    fn underlying_storage(&self, _renderer: &mut GlesRenderer) -> Option<UnderlyingStorage<'_>> {
        None
    }
}

impl<'render> RenderElement<TtyRenderer<'render>>
    for SeeThroughSurfaceRenderElement<TtyRenderer<'render>>
{
    fn draw(
        &self,
        frame: &mut TtyFrame<'render, '_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        cache: Option<&UserDataMap>,
    ) -> Result<(), TtyRendererError<'render>> {
        frame
            .as_gles_frame()
            .override_default_tex_program(self.program.clone(), self.uniforms());
        RenderElement::draw(&self.inner, frame, src, dst, damage, opaque_regions, cache)?;
        frame.as_gles_frame().clear_tex_program_override();
        Ok(())
    }

    fn underlying_storage(
        &self,
        _renderer: &mut TtyRenderer<'render>,
    ) -> Option<UnderlyingStorage<'_>> {
        None
    }
}
