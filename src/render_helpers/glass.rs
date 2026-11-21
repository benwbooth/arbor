//! arbor: liquid glass render element.
//!
//! A framebuffer effect: smithay calls `capture_framebuffer()` whenever something behind the
//! element changed (and only then), which copies the backdrop under the shape into a texture and
//! blurs it; `draw()` then draws that texture through the lens shader (`shaders/glass.frag`).
//! Unchanged glass over an unchanged backdrop costs nothing; glass over a playing video recaptures
//! with every video frame.

use arbor_config::GlassLook;
use glam::{Mat3, Vec2};
use smithay::backend::renderer::element::{Element, Id, RenderElement};
use smithay::backend::renderer::gles::{GlesError, GlesFrame, GlesRenderer, Uniform};
use smithay::backend::renderer::utils::CommitCounter;
use smithay::backend::renderer::{Frame as _, Texture as _};
use smithay::utils::user_data::UserDataMap;
use smithay::utils::{Buffer, Logical, Physical, Rectangle, Scale, Transform};

use crate::backend::tty::{TtyFrame, TtyRenderer, TtyRendererError};
use crate::render_helpers::blur::BlurOptions;
use crate::render_helpers::framebuffer_effect::{capture_backdrop, captured_backdrop};
use crate::render_helpers::renderer::AsGlesFrame as _;
use crate::render_helpers::shaders::{mat3_uniform, Shaders};

#[derive(Debug, Clone)]
pub struct GlassElement {
    id: Id,
    commit: CommitCounter,
    /// Logical geometry on the output, snapped to physical pixels.
    geometry: Rectangle<f64, Logical>,
    radius: f64,
    look: GlassLook,
    scale: f64,
}

impl GlassElement {
    pub fn new(
        id: Id,
        commit: CommitCounter,
        geometry: Rectangle<f64, Logical>,
        radius: f64,
        look: GlassLook,
        scale: f64,
    ) -> Self {
        Self {
            id,
            commit,
            geometry,
            radius,
            look,
            scale,
        }
    }

    fn blur_options(&self) -> Option<BlurOptions> {
        blur_options_for_radius(self.look.blur_radius * self.scale)
    }

    fn uniforms(
        &self,
        crop: Rectangle<f64, Logical>,
        transform: Transform,
    ) -> Vec<Uniform<'static>> {
        let input_to_geo = input_to_geo(crop, self.geometry.size, transform);
        let geo_to_tex = input_to_geo.inverse();
        let size = (self.geometry.size.w as f32, self.geometry.size.h as f32);
        let look = &self.look;
        vec![
            Uniform::new("niri_scale", self.scale as f32),
            Uniform::new("geo_size", size),
            Uniform::new("radius", self.radius as f32),
            mat3_uniform("input_to_geo", input_to_geo),
            mat3_uniform("geo_to_tex", geo_to_tex),
            Uniform::new("refraction", look.refraction_px as f32),
            Uniform::new("bezel", look.bezel as f32),
            Uniform::new("dispersion", look.dispersion as f32),
            Uniform::new("saturation", look.saturation as f32),
            Uniform::new("glass_tint", look.tint),
            Uniform::new("rim", look.rim as f32),
        ]
    }
}

/// Dual-kawase passes and offset that blur by roughly `radius` physical px (each pass halves the
/// resolution, so the reach doubles per pass).
pub fn blur_options_for_radius(radius: f64) -> Option<BlurOptions> {
    if radius.is_nan() || radius < 0.5 {
        return None;
    }
    let passes = (radius / 2.).log2().ceil().clamp(1., 6.);
    let offset = (radius / 2f64.powf(passes)).clamp(0.5, 8.);
    Some(BlurOptions {
        passes: passes as u8,
        offset,
    })
}

/// Maps the shader's `v_coords` (texture coordinates of the captured backdrop, cropped to `crop`
/// in geometry-local logical px) to [0, 1] across the shape of `size`.
fn input_to_geo(
    crop: Rectangle<f64, Logical>,
    size: smithay::utils::Size<f64, Logical>,
    transform: Transform,
) -> Mat3 {
    let offset = Vec2::new(crop.loc.x as f32, crop.loc.y as f32);
    let crop_size = Vec2::new(crop.size.w as f32, crop.size.h as f32);
    let size = Vec2::new(size.w as f32, size.h as f32);

    let m = Mat3::from_scale(crop_size / size) * Mat3::from_translation(offset / crop_size);
    // Revert the effect of the texture transform.
    let transform_mat = Mat3::from_translation(Vec2::new(0.5, 0.5))
        * Mat3::from_cols_array(transform.matrix().as_ref())
        * Mat3::from_translation(Vec2::new(-0.5, -0.5));
    m * transform_mat
}

impl Element for GlassElement {
    fn id(&self) -> &Id {
        &self.id
    }

    fn current_commit(&self) -> CommitCounter {
        self.commit
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        // Not used for drawing, but tells us how we were cropped.
        let size = self.geometry.size.to_buffer(1., Transform::Normal);
        Rectangle::from_size(size)
    }

    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.geometry.to_physical_precise_round(scale)
    }

    fn alpha(&self) -> f32 {
        self.look.opacity as f32
    }

    fn is_framebuffer_effect(&self) -> bool {
        true
    }
}

impl RenderElement<GlesRenderer> for GlassElement {
    fn capture_framebuffer(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        cache: &UserDataMap,
    ) -> Result<(), GlesError> {
        let _span = tracy_client::span!("GlassElement::capture_framebuffer");
        capture_backdrop(
            frame,
            src,
            dst,
            cache,
            self.scale as f32,
            self.blur_options(),
        )
    }

    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        _opaque_regions: &[Rectangle<i32, Physical>],
        cache: Option<&UserDataMap>,
    ) -> Result<(), GlesError> {
        let _span = tracy_client::span!("GlassElement::draw");

        let Some(texture) = cache.and_then(captured_backdrop) else {
            return Ok(());
        };
        let Some(program) = Shaders::get_from_frame(frame).glass.clone() else {
            return Ok(());
        };

        // Clamp the same way as capture_backdrop() does.
        let output_rect = Rectangle::from_size(frame.output_size());
        let Some(clamped_dst) = dst.intersection(output_rect) else {
            return Ok(());
        };
        let clamp_offset = clamped_dst.loc - dst.loc;

        let damage: Vec<_> = if clamped_dst == dst {
            damage.to_vec()
        } else {
            let r = Rectangle::new(clamp_offset, clamped_dst.size);
            damage
                .iter()
                .filter_map(|d| {
                    let mut crop = d.intersection(r)?;
                    crop.loc -= clamp_offset;
                    Some(crop)
                })
                .collect()
        };
        if damage.is_empty() {
            return Ok(());
        }

        // Adjust src proportionally to the dst clamping.
        let src_loc = src.loc.to_logical(1., Transform::Normal, &src.size);
        let dst_to_src = src.size / dst.size.to_f64();
        let crop = Rectangle::new(
            src_loc + clamp_offset.to_f64().upscale(dst_to_src).to_logical(1.),
            clamped_dst.size.to_f64().upscale(dst_to_src).to_logical(1.),
        );

        let uniforms = self.uniforms(crop, frame.transformation());
        frame.render_texture_from_to(
            &texture,
            Rectangle::from_size(texture.size().to_f64()),
            clamped_dst,
            &damage,
            &[],
            // The captured texture has the same transform as the frame.
            frame.transformation().invert(),
            self.look.opacity as f32,
            Some(&program),
            &uniforms,
        )
    }
}

impl<'render> RenderElement<TtyRenderer<'render>> for GlassElement {
    fn capture_framebuffer(
        &self,
        frame: &mut TtyFrame<'_, '_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        cache: &UserDataMap,
    ) -> Result<(), TtyRendererError<'render>> {
        let gles_frame = frame.as_gles_frame();
        RenderElement::<GlesRenderer>::capture_framebuffer(self, gles_frame, src, dst, cache)?;
        Ok(())
    }

    fn draw(
        &self,
        frame: &mut TtyFrame<'_, '_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        cache: Option<&UserDataMap>,
    ) -> Result<(), TtyRendererError<'render>> {
        let gles_frame = frame.as_gles_frame();
        RenderElement::<GlesRenderer>::draw(
            self,
            gles_frame,
            src,
            dst,
            damage,
            opaque_regions,
            cache,
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blur_radius_to_passes() {
        assert_eq!(blur_options_for_radius(0.), None);
        assert_eq!(blur_options_for_radius(f64::NAN), None);
        let o = blur_options_for_radius(8.).unwrap();
        assert_eq!(o.passes, 2);
        assert!((o.offset - 2.).abs() < 1e-9);
        let o = blur_options_for_radius(1.).unwrap();
        assert_eq!(o.passes, 1);
        let o = blur_options_for_radius(1000.).unwrap();
        assert_eq!(o.passes, 6);
        assert_eq!(o.offset, 8.);
    }

    #[test]
    fn geo_to_tex_inverts_crop() {
        // A shape 100x50 cropped to its right half: v_coords 0..1 cover x 50..100.
        let crop = Rectangle::new((50., 0.).into(), (50., 50.).into());
        let m = input_to_geo(crop, (100., 50.).into(), Transform::Normal);
        let p = m * glam::Vec3::new(0., 0., 1.);
        assert!((p.x - 0.5).abs() < 1e-6 && p.y.abs() < 1e-6);
        let p = m * glam::Vec3::new(1., 1., 1.);
        assert!((p.x - 1.).abs() < 1e-6 && (p.y - 1.).abs() < 1e-6);
        let back = m.inverse() * glam::Vec3::new(0.75, 0.5, 1.);
        assert!((back.x - 0.5).abs() < 1e-6 && (back.y - 0.5).abs() < 1e-6);
    }
}
