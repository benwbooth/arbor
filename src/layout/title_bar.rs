//! arbor: macOS-style server-side title bars.
//!
//! Drawn by the compositor at the top of a tile, for windows that negotiated server-side
//! decorations: traffic-light buttons on the left (close, minimize, zoom), the window title
//! centered. The bar is rendered on the CPU with cairo/pango and cached as a texture until
//! anything it shows changes.

use std::cell::RefCell;
use std::f64::consts::PI;

use niri_config::{Color, CornerRadius, TitleBars};
use pangocairo::cairo::{self, ImageSurface};
use pangocairo::pango::{Alignment, EllipsizeMode, FontDescription};
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::gles::GlesTexture;
use smithay::utils::{Logical, Point, Rectangle, Size, Transform};

use crate::render_helpers::memory::MemoryBuffer;
use crate::render_helpers::primary_gpu_texture::PrimaryGpuTextureRenderElement;
use crate::render_helpers::renderer::NiriRenderer;
use crate::render_helpers::texture::{TextureBuffer, TextureRenderElement};
use crate::utils::to_physical_precise_round;

/// Diameter of a traffic-light button.
const BUTTON_DIAMETER: f64 = 12.;
/// Distance between button centers.
const BUTTON_SPACING: f64 = 20.;
/// Distance from the left edge of the bar to the center of the first button.
const BUTTON_MARGIN: f64 = 16.;
/// Extra hit radius around a button.
const BUTTON_HIT_SLACK: f64 = 3.;

const FONT: &str = "sans 10";

/// Part of a title bar under a point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TitleBarHit {
    Close,
    Minimize,
    Zoom,
    /// The rest of the bar: drag to move, double-click to zoom.
    Bar,
}

const BUTTONS: [TitleBarHit; 3] = [TitleBarHit::Close, TitleBarHit::Minimize, TitleBarHit::Zoom];

fn button_center(idx: usize, bar_height: f64) -> Point<f64, Logical> {
    Point::from((BUTTON_MARGIN + BUTTON_SPACING * idx as f64, bar_height / 2.))
}

/// Hit-tests a point relative to the top-left corner of a bar of `size`.
pub fn hit(size: Size<f64, Logical>, point: Point<f64, Logical>) -> Option<TitleBarHit> {
    if !Rectangle::from_size(size).contains(point) {
        return None;
    }

    for (idx, button) in BUTTONS.into_iter().enumerate() {
        let d = point - button_center(idx, size.h);
        if d.x.hypot(d.y) <= BUTTON_DIAMETER / 2. + BUTTON_HIT_SLACK {
            return Some(button);
        }
    }
    Some(TitleBarHit::Bar)
}

/// Whether a point is over the button group (which shows the button symbols).
pub fn is_over_buttons(size: Size<f64, Logical>, point: Point<f64, Logical>) -> bool {
    let right = BUTTON_MARGIN + BUTTON_SPACING * 2. + BUTTON_DIAMETER / 2. + BUTTON_HIT_SLACK;
    Rectangle::from_size(Size::from((right, size.h))).contains(point)
}

#[derive(Debug, Clone, PartialEq)]
struct Key {
    size: Size<i32, smithay::utils::Physical>,
    scale: f64,
    title: String,
    is_active: bool,
    buttons_hovered: bool,
    config: TitleBars,
    radius: CornerRadius,
}

/// A title bar's cached rendering.
#[derive(Default)]
pub struct TitleBar {
    key: Option<Key>,
    buffer: Option<MemoryBuffer>,
    texture: RefCell<Option<TextureBuffer<GlesTexture>>>,
    /// The pointer is over the buttons.
    pub buttons_hovered: bool,
}

impl std::fmt::Debug for TitleBar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TitleBar")
            .field("key", &self.key)
            .field("buttons_hovered", &self.buttons_hovered)
            .finish_non_exhaustive()
    }
}

impl TitleBar {
    /// Re-renders the bar if anything it shows changed.
    #[allow(clippy::too_many_arguments)]
    pub fn update(
        &mut self,
        size: Size<f64, Logical>,
        scale: f64,
        title: String,
        is_active: bool,
        config: TitleBars,
        radius: CornerRadius,
    ) {
        let size_px = Size::from((
            to_physical_precise_round::<i32>(scale, size.w),
            to_physical_precise_round::<i32>(scale, size.h),
        ));
        let key = Key {
            size: size_px,
            scale,
            title,
            is_active,
            buttons_hovered: self.buttons_hovered,
            config,
            radius,
        };
        if self.key.as_ref() == Some(&key) {
            return;
        }

        self.buffer = if size_px.w > 0 && size_px.h > 0 {
            match draw(&key) {
                Ok(buffer) => Some(buffer),
                Err(err) => {
                    warn!("error drawing title bar: {err:?}");
                    None
                }
            }
        } else {
            None
        };
        self.texture.borrow_mut().take();
        self.key = Some(key);
    }

    pub fn render<R: NiriRenderer>(
        &self,
        renderer: &mut R,
        location: Point<f64, Logical>,
    ) -> Option<PrimaryGpuTextureRenderElement> {
        let buffer = self.buffer.as_ref()?;
        let mut texture = self.texture.borrow_mut();
        if texture.is_none() {
            match TextureBuffer::from_memory_buffer(renderer.as_gles_renderer(), buffer) {
                Ok(buffer) => *texture = Some(buffer),
                Err(err) => {
                    warn!("error uploading title bar texture: {err:?}");
                    return None;
                }
            }
        }
        let elem = TextureRenderElement::from_texture_buffer(
            texture.clone()?,
            location,
            1.,
            None,
            None,
            Kind::Unspecified,
        );
        Some(PrimaryGpuTextureRenderElement(elem))
    }
}

fn set_color(cr: &cairo::Context, color: Color) {
    let [r, g, b, a] = color.to_array_unpremul();
    cr.set_source_rgba(r.into(), g.into(), b.into(), a.into());
}

fn rgb(cr: &cairo::Context, hex: u32) {
    let r = f64::from((hex >> 16) & 0xff) / 255.;
    let g = f64::from((hex >> 8) & 0xff) / 255.;
    let b = f64::from(hex & 0xff) / 255.;
    cr.set_source_rgb(r, g, b);
}

fn draw(key: &Key) -> anyhow::Result<MemoryBuffer> {
    let _span = tracy_client::span!("title_bar::draw");

    let s = key.scale;
    let (w, h) = (key.size.w, key.size.h);
    let (wf, hf) = (f64::from(w), f64::from(h));

    let surface = ImageSurface::create(cairo::Format::ARgb32, w, h)?;
    let cr = cairo::Context::new(&surface)?;

    // Background with rounded top corners.
    let rtl = (f64::from(key.radius.top_left) * s).min(hf).min(wf / 2.);
    let rtr = (f64::from(key.radius.top_right) * s).min(hf).min(wf / 2.);
    cr.new_path();
    cr.move_to(0., hf);
    cr.line_to(0., rtl);
    if rtl > 0. {
        cr.arc(rtl, rtl, rtl, PI, 1.5 * PI);
    }
    cr.line_to(wf - rtr, 0.);
    if rtr > 0. {
        cr.arc(wf - rtr, rtr, rtr, 1.5 * PI, 2. * PI);
    }
    cr.line_to(wf, hf);
    cr.close_path();
    let config = &key.config;
    set_color(
        &cr,
        if key.is_active {
            config.active_color
        } else {
            config.inactive_color
        },
    );
    cr.fill()?;

    // Traffic lights.
    let lit = key.is_active || key.buttons_hovered;
    let colors = [0xff5f57, 0xfebc2e, 0x28c840];
    let bar_h = hf / s;
    for (idx, color) in colors.into_iter().enumerate() {
        let c = button_center(idx, bar_h);
        let (cx, cy) = (c.x * s, c.y * s);
        let r = BUTTON_DIAMETER / 2. * s;
        cr.new_path();
        cr.arc(cx, cy, r, 0., 2. * PI);
        if lit {
            rgb(&cr, color);
        } else {
            rgb(&cr, 0x5a5f66);
        }
        cr.fill()?;

        if key.buttons_hovered {
            // Symbols: ×, −, +.
            cr.set_source_rgba(0., 0., 0., 0.6);
            cr.set_line_width((1.2 * s).max(1.));
            let a = r * 0.45;
            cr.new_path();
            match idx {
                0 => {
                    cr.move_to(cx - a, cy - a);
                    cr.line_to(cx + a, cy + a);
                    cr.move_to(cx + a, cy - a);
                    cr.line_to(cx - a, cy + a);
                }
                1 => {
                    cr.move_to(cx - a * 1.2, cy);
                    cr.line_to(cx + a * 1.2, cy);
                }
                _ => {
                    cr.move_to(cx - a * 1.2, cy);
                    cr.line_to(cx + a * 1.2, cy);
                    cr.move_to(cx, cy - a * 1.2);
                    cr.line_to(cx, cy + a * 1.2);
                }
            }
            cr.stroke()?;
        }
    }

    // Title, centered over the whole bar, kept clear of the buttons on both sides.
    let reserved = (BUTTON_MARGIN + BUTTON_SPACING * 2. + BUTTON_DIAMETER / 2. + 12.) * s;
    let text_w = wf - reserved * 2.;
    if text_w > 0. && !key.title.is_empty() {
        let mut font = FontDescription::from_string(FONT);
        font.set_absolute_size(to_physical_precise_round(s, font.size()));

        let layout = pangocairo::functions::create_layout(&cr);
        layout.context().set_round_glyph_positions(false);
        layout.set_font_description(Some(&font));
        layout.set_alignment(Alignment::Center);
        layout.set_ellipsize(EllipsizeMode::End);
        layout.set_width((text_w * f64::from(pangocairo::pango::SCALE)) as i32);
        layout.set_single_paragraph_mode(true);
        layout.set_text(&key.title);

        let (_, text_h) = layout.pixel_size();
        cr.move_to(reserved, ((hf - f64::from(text_h)) / 2.).round());
        set_color(
            &cr,
            if key.is_active {
                config.active_text_color
            } else {
                config.inactive_text_color
            },
        );
        pangocairo::functions::show_layout(&cr, &layout);
    }

    drop(cr);
    let data = surface.take_data().unwrap();
    Ok(MemoryBuffer::new(
        data.to_vec(),
        Fourcc::Argb8888,
        (w, h),
        s,
        Transform::Normal,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hit_buttons_and_bar() {
        let size = Size::from((400., 28.));
        assert_eq!(hit(size, Point::from((16., 14.))), Some(TitleBarHit::Close));
        assert_eq!(
            hit(size, Point::from((36., 14.))),
            Some(TitleBarHit::Minimize)
        );
        assert_eq!(hit(size, Point::from((56., 14.))), Some(TitleBarHit::Zoom));
        assert_eq!(hit(size, Point::from((200., 14.))), Some(TitleBarHit::Bar));
        assert_eq!(hit(size, Point::from((26., 2.))), Some(TitleBarHit::Bar));
        assert_eq!(hit(size, Point::from((200., 30.))), None);
        assert!(is_over_buttons(size, Point::from((60., 5.))));
        assert!(!is_over_buttons(size, Point::from((120., 5.))));
    }
}
