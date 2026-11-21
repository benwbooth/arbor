//! arbor: defaults for liquid glass (`glass {}`), the refracting material drawn under shapes that
//! clients register over IPC (`SetGlass`).

use arbor_ipc::{GlassParams, GlassVariant};

use crate::utils::MergeWith;
use crate::{Color, FloatOrInt};

/// The `glass {}` section: how glass shapes look unless a shape overrides it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Glass {
    /// Draw no glass at all (shapes are kept, surfaces fall back to their layer rules).
    pub off: bool,
    /// Overall strength 0..1, scales refraction, dispersion, rim and blur.
    pub strength: f64,
    /// Bending at the rim as a fraction of the bezel width.
    pub refraction: f64,
    /// Width of the bending band in logical px; 0 = the shape's corner radius.
    pub bezel: f64,
    /// Chromatic dispersion at the rim.
    pub dispersion: f64,
    /// Blur radius (logical px) of the regular variant.
    pub blur_radius: f64,
    /// Blur radius (logical px) of the clear variant.
    pub clear_blur_radius: f64,
    /// Saturation of the regular variant.
    pub saturation: f64,
    /// Saturation of the clear variant.
    pub clear_saturation: f64,
    /// Specular rim strength.
    pub rim: f64,
    /// Tint over the regular variant.
    pub tint: Color,
    /// Tint over the clear variant (usually a slight dimming so content on top stays legible).
    pub clear_tint: Color,
}

impl Default for Glass {
    fn default() -> Self {
        Self {
            off: false,
            strength: 1.,
            refraction: 0.8,
            bezel: 0.,
            dispersion: 0.12,
            blur_radius: 8.,
            clear_blur_radius: 1.,
            saturation: 1.6,
            clear_saturation: 1.3,
            rim: 0.7,
            tint: Color::from_rgba8_unpremul(0xff, 0xff, 0xff, 0x0c),
            clear_tint: Color::from_rgba8_unpremul(0x00, 0x00, 0x00, 0x14),
        }
    }
}

#[derive(knuffel::Decode, Debug, Default, Clone, Copy, PartialEq)]
pub struct GlassPart {
    #[knuffel(child)]
    pub off: bool,
    #[knuffel(child)]
    pub on: bool,
    #[knuffel(child, unwrap(argument))]
    pub strength: Option<FloatOrInt<0, 1>>,
    #[knuffel(child, unwrap(argument))]
    pub refraction: Option<FloatOrInt<0, 1>>,
    #[knuffel(child, unwrap(argument))]
    pub bezel: Option<FloatOrInt<0, 1000>>,
    #[knuffel(child, unwrap(argument))]
    pub dispersion: Option<FloatOrInt<0, 1>>,
    #[knuffel(child, unwrap(argument))]
    pub blur_radius: Option<FloatOrInt<0, 200>>,
    #[knuffel(child, unwrap(argument))]
    pub clear_blur_radius: Option<FloatOrInt<0, 200>>,
    #[knuffel(child, unwrap(argument))]
    pub saturation: Option<FloatOrInt<0, 10>>,
    #[knuffel(child, unwrap(argument))]
    pub clear_saturation: Option<FloatOrInt<0, 10>>,
    #[knuffel(child, unwrap(argument))]
    pub rim: Option<FloatOrInt<0, 1>>,
    #[knuffel(child)]
    pub tint: Option<Color>,
    #[knuffel(child)]
    pub clear_tint: Option<Color>,
}

impl MergeWith<GlassPart> for Glass {
    fn merge_with(&mut self, part: &GlassPart) {
        self.off |= part.off;
        if part.on {
            self.off = false;
        }
        merge!(
            (self, part),
            strength,
            refraction,
            bezel,
            dispersion,
            blur_radius,
            clear_blur_radius,
            saturation,
            clear_saturation,
            rim,
        );
        if let Some(x) = part.tint {
            self.tint = x;
        }
        if let Some(x) = part.clear_tint {
            self.clear_tint = x;
        }
    }
}

/// Fully resolved look of one glass shape, ready for the shader.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GlassLook {
    /// Displacement at the rim, logical px (at most half the bezel, so the lens never folds).
    pub refraction_px: f64,
    /// Width of the bending band, logical px.
    pub bezel: f64,
    pub dispersion: f64,
    /// Blur radius, logical px (0 = sharp).
    pub blur_radius: f64,
    pub saturation: f64,
    pub rim: f64,
    /// Premultiplied RGBA.
    pub tint: [f32; 4],
    /// Opacity of the whole glass.
    pub opacity: f64,
}

impl Glass {
    /// Resolves a shape's parameters against these defaults for a shape of `size` (logical) with
    /// corner `radius` (already clamped to the size).
    pub fn resolve(&self, params: &GlassParams, size: (f64, f64), radius: f64) -> GlassLook {
        let variant = params.variant.unwrap_or(GlassVariant::Regular);
        let strength = params.strength.unwrap_or(self.strength).clamp(0., 1.);

        let half = (size.0.min(size.1) / 2.).max(0.);
        // The bending band follows the corner radius (like a pill's rounded end), but stays
        // visible on square shapes and never goes past the middle.
        let bezel = params
            .bezel
            .or((self.bezel > 0.).then_some(self.bezel))
            .unwrap_or(radius.max(12.))
            .clamp(0., half);

        let refraction = params.refraction.unwrap_or(self.refraction).clamp(0., 1.);
        let dispersion = params.dispersion.unwrap_or(self.dispersion).clamp(0., 1.);
        let (blur, saturation, tint) = match variant {
            GlassVariant::Regular => (self.blur_radius, self.saturation, self.tint),
            GlassVariant::Clear => (
                self.clear_blur_radius,
                self.clear_saturation,
                self.clear_tint,
            ),
        };
        let blur = params.blur_radius.unwrap_or(blur).max(0.);
        let saturation = params.saturation.unwrap_or(saturation).max(0.);
        let rim = params.rim.unwrap_or(self.rim).clamp(0., 1.);
        let tint = params
            .tint
            .map(|[r, g, b, a]| Color::new_unpremul(r as f32, g as f32, b as f32, a as f32))
            .unwrap_or(tint);
        let [r, g, b, a] = tint.to_array_unpremul().map(|x| x.clamp(0., 1.));

        // The shader's quadratic profile displaces by refraction_px * s^2 across the band, so
        // its slope at the rim is 2 * refraction_px / bezel; keep that (times the dispersion
        // spread) under 1 so the backdrop never folds into a mirror image.
        let max_k = 0.95 / (1. + dispersion * strength);
        let k = (refraction * strength).min(max_k);

        GlassLook {
            refraction_px: k * bezel / 2.,
            bezel,
            dispersion: dispersion * strength,
            blur_radius: blur * strength,
            // Strength fades the vibrancy boost towards neutral too.
            saturation: 1. + (saturation - 1.) * strength,
            rim: rim * strength,
            tint: [r * a, g * a, b * a, a],
            opacity: params.opacity.unwrap_or(1.).clamp(0., 1.),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_defaults_and_overrides() {
        let glass = Glass::default();
        let look = glass.resolve(&GlassParams::default(), (300., 64.), 18.);
        assert_eq!(look.bezel, 18.);
        assert!((look.refraction_px - 0.8 * 18. / 2.).abs() < 1e-9);
        // Never folds: rim slope 2 * refraction / bezel times the dispersion spread stays < 1.
        let full = GlassParams {
            refraction: Some(1.),
            dispersion: Some(1.),
            ..Default::default()
        };
        let look = glass.resolve(&full, (300., 64.), 18.);
        assert!(2. * look.refraction_px / look.bezel * (1. + look.dispersion) < 1.);
        assert_eq!(look.blur_radius, 8.);
        assert_eq!(look.saturation, 1.6);

        // The bezel never passes the middle of a thin shape, and has a minimum on square ones.
        let look = glass.resolve(&GlassParams::default(), (300., 10.), 3.);
        assert_eq!(look.bezel, 5.);
        let look = glass.resolve(&GlassParams::default(), (300., 300.), 0.);
        assert_eq!(look.bezel, 12.);

        let params = GlassParams {
            variant: Some(GlassVariant::Clear),
            strength: Some(0.5),
            tint: Some([1., 0., 0., 0.5]),
            ..Default::default()
        };
        let look = glass.resolve(&params, (300., 64.), 18.);
        assert_eq!(look.blur_radius, 0.5);
        assert!((look.saturation - 1.15).abs() < 1e-9);
        assert_eq!(look.tint, [0.5, 0., 0., 0.5]);
        assert!((look.rim - 0.35).abs() < 1e-9);

        let params = GlassParams {
            strength: Some(0.),
            ..Default::default()
        };
        let look = glass.resolve(&params, (300., 64.), 18.);
        assert_eq!(look.refraction_px, 0.);
        assert_eq!(look.saturation, 1.);
    }
}
