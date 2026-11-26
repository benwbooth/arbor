//! arbor: KDE Breeze-style server-side title bars.
//!
//! Drawn by the compositor at the top of a tile, for windows that negotiated server-side
//! decorations: the window title centered, and Breeze's minimize, maximize and close buttons on
//! the right. The buttons show a round highlight under the pointer (red for close). The bar is
//! rendered on the CPU with cairo/pango and cached as a texture until anything it shows changes.

use std::cell::RefCell;
use std::f64::consts::PI;

use arbor_config::{Color, CornerRadius, TitleBars};
use pangocairo::cairo::{self, ImageSurface};
use pangocairo::pango::{Alignment, EllipsizeMode, FontDescription};
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::gles::GlesTexture;
use smithay::utils::{Logical, Point, Rectangle, Size, Transform};

use crate::render_helpers::memory::MemoryBuffer;
use crate::render_helpers::primary_gpu_texture::PrimaryGpuTextureRenderElement;
use crate::render_helpers::renderer::ArborRenderer;
use crate::render_helpers::texture::{TextureBuffer, TextureRenderElement};
use crate::utils::to_physical_precise_round;

/// Diameter of a button (and of its hover highlight).
const BUTTON_DIAMETER: f64 = 18.;
/// Distance between button centers.
const BUTTON_SPACING: f64 = 24.;
/// Distance from the right edge of the bar to the center of the last (close) button.
const BUTTON_MARGIN: f64 = 15.;
/// Extra hit radius around a button.
const BUTTON_HIT_SLACK: f64 = 3.;

/// macOS's window title: the system font at 13 pt, semibold (Inter, hearth's UI font, standing
/// in for SF Pro). Sizes here are logical pixels.
const FONT: &str = "Inter, sans Semi-Bold 13";

/// Breeze's "negative" color, used for the close button highlight.
const CLOSE_HOVER: u32 = 0xda4453;

/// Part of a title bar under a point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TitleBarHit {
    Minimize,
    Maximize,
    Close,
    /// The rest of the bar: drag to move, double-click to maximize.
    Bar,
}

impl TitleBarHit {
    pub fn is_button(self) -> bool {
        self != TitleBarHit::Bar
    }
}

/// Buttons from left to right, as in Breeze's default layout.
const BUTTONS: [TitleBarHit; 3] = [
    TitleBarHit::Minimize,
    TitleBarHit::Maximize,
    TitleBarHit::Close,
];

fn button_center(idx: usize, bar_size: Size<f64, Logical>) -> Point<f64, Logical> {
    let from_right = (BUTTONS.len() - 1 - idx) as f64;
    Point::from((
        bar_size.w - BUTTON_MARGIN - BUTTON_SPACING * from_right,
        bar_size.h / 2.,
    ))
}

/// Width taken by the button group at the right edge.
fn buttons_width() -> f64 {
    BUTTON_MARGIN + BUTTON_SPACING * (BUTTONS.len() - 1) as f64 + BUTTON_DIAMETER / 2.
}

/// Hit-tests a point relative to the top-left corner of a bar of `size`.
pub fn hit(size: Size<f64, Logical>, point: Point<f64, Logical>) -> Option<TitleBarHit> {
    if !Rectangle::from_size(size).contains(point) {
        return None;
    }

    for (idx, button) in BUTTONS.into_iter().enumerate() {
        let d = point - button_center(idx, size);
        if d.x.hypot(d.y) <= BUTTON_DIAMETER / 2. + BUTTON_HIT_SLACK {
            return Some(button);
        }
    }
    Some(TitleBarHit::Bar)
}

#[derive(Debug, Clone, PartialEq)]
struct Key {
    size: Size<i32, smithay::utils::Physical>,
    scale: f64,
    title: String,
    is_active: bool,
    is_maximized: bool,
    hovered: Option<TitleBarHit>,
    config: TitleBars,
    radius: CornerRadius,
}

/// A title bar's cached rendering.
#[derive(Default)]
pub struct TitleBar {
    key: Option<Key>,
    buffer: Option<MemoryBuffer>,
    texture: RefCell<Option<TextureBuffer<GlesTexture>>>,
    /// The button under the pointer.
    pub hovered: Option<TitleBarHit>,
}

impl std::fmt::Debug for TitleBar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TitleBar")
            .field("key", &self.key)
            .field("hovered", &self.hovered)
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
        is_maximized: bool,
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
            is_maximized,
            hovered: self.hovered,
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

    pub fn render<R: ArborRenderer>(
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

    let text_color = if key.is_active {
        config.active_text_color
    } else {
        config.inactive_text_color
    };
    let bar_color = if key.is_active {
        config.active_color
    } else {
        config.inactive_color
    };

    // Buttons, drawn like Breeze's on an 18-unit grid.
    let bar_size = Size::from((wf / s, hf / s));
    for (idx, button) in BUTTONS.into_iter().enumerate() {
        let c = button_center(idx, bar_size);
        let (cx, cy) = (c.x * s, c.y * s);
        let r = BUTTON_DIAMETER / 2. * s;
        let hovered = key.hovered == Some(button);

        if hovered {
            cr.new_path();
            cr.arc(cx, cy, r, 0., 2. * PI);
            if button == TitleBarHit::Close {
                rgb(&cr, CLOSE_HOVER);
            } else {
                set_color(&cr, text_color);
            }
            cr.fill()?;
        }

        if !hovered {
            set_color(&cr, text_color);
        } else if button == TitleBarHit::Close {
            cr.set_source_rgb(1., 1., 1.);
        } else {
            set_color(&cr, bar_color);
        }

        // Grid unit, with the grid's (9, 9) at the button center.
        let u = BUTTON_DIAMETER * s / 18.;
        let p = |x: f64, y: f64| (cx + (x - 9.) * u, cy + (y - 9.) * u);
        cr.set_line_width((1.1 * s).max(1.));
        cr.set_line_cap(cairo::LineCap::Round);
        cr.set_line_join(cairo::LineJoin::Miter);
        cr.new_path();
        let line = |points: &[(f64, f64)]| {
            let (x, y) = p(points[0].0, points[0].1);
            cr.move_to(x, y);
            for &(px, py) in &points[1..] {
                let (x, y) = p(px, py);
                cr.line_to(x, y);
            }
        };
        match button {
            TitleBarHit::Minimize => line(&[(4., 7.), (9., 12.), (14., 7.)]),
            TitleBarHit::Maximize if key.is_maximized => {
                line(&[(4., 9.), (9., 4.), (14., 9.), (9., 14.)]);
                cr.close_path();
            }
            TitleBarHit::Maximize => line(&[(4., 11.), (9., 6.), (14., 11.)]),
            TitleBarHit::Close => {
                line(&[(5., 5.), (13., 13.)]);
                line(&[(13., 5.), (5., 13.)]);
            }
            TitleBarHit::Bar => (),
        }
        cr.stroke()?;
    }

    // Title, centered over the whole bar, kept clear of the buttons on both sides.
    let reserved = (buttons_width() + 8.) * s;
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
        set_color(&cr, text_color);
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
        let size = Size::from((400., 30.));
        assert_eq!(
            hit(size, Point::from((385., 15.))),
            Some(TitleBarHit::Close)
        );
        assert_eq!(
            hit(size, Point::from((361., 15.))),
            Some(TitleBarHit::Maximize)
        );
        assert_eq!(
            hit(size, Point::from((337., 15.))),
            Some(TitleBarHit::Minimize)
        );
        assert_eq!(hit(size, Point::from((16., 15.))), Some(TitleBarHit::Bar));
        assert_eq!(hit(size, Point::from((200., 15.))), Some(TitleBarHit::Bar));
        assert_eq!(hit(size, Point::from((300., 15.))), Some(TitleBarHit::Bar));
        assert_eq!(hit(size, Point::from((200., 32.))), None);
    }
}
