//! arbor: icon tiles for docked windows seen too small to be useful.
//!
//! A leaf below the size threshold draws as a rounded tile in a colour derived from the app id,
//! with the app icon (from its desktop entry; PNG icons only) or the app's first letter. A whole
//! subtree below the threshold draws as one tile showing the outlines of its leaves.
//!
//! Icons are looked up and decoded on a helper thread; the letter shows until they're ready.

use std::cell::RefCell;
use std::collections::HashMap;
use std::f64::consts::PI;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use pangocairo::cairo::{self, ImageSurface};
use pangocairo::pango::{Alignment, FontDescription};
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::gles::GlesTexture;
use smithay::utils::{Logical, Physical, Point, Rectangle, Size, Transform};

use crate::render_helpers::memory::MemoryBuffer;
use crate::render_helpers::primary_gpu_texture::PrimaryGpuTextureRenderElement;
use crate::render_helpers::renderer::NiriRenderer;
use crate::render_helpers::texture::{TextureBuffer, TextureRenderElement};
use crate::utils::to_physical_precise_round;

type Rect = Rectangle<f64, Logical>;

/// A decoded icon: ARGB32 premultiplied (cairo format), row by row.
#[derive(Debug)]
pub struct IconImage {
    width: i32,
    height: i32,
    data: Vec<u8>,
}

#[derive(Debug, Clone)]
enum IconState {
    Loading,
    Loaded(Option<Arc<IconImage>>),
}

fn icon_cache() -> &'static Mutex<HashMap<String, IconState>> {
    static CACHE: OnceLock<Mutex<HashMap<String, IconState>>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// The icon of an app if it has been loaded; starts loading it otherwise.
pub fn app_icon(app_id: &str) -> Option<Arc<IconImage>> {
    let mut cache = icon_cache().lock().unwrap();
    match cache.get(app_id) {
        Some(IconState::Loaded(icon)) => icon.clone(),
        Some(IconState::Loading) => None,
        None => {
            cache.insert(app_id.to_owned(), IconState::Loading);
            let app_id = app_id.to_owned();
            let res = std::thread::Builder::new()
                .name("arbor-icon".to_owned())
                .spawn(move || {
                    let icon = find_icon(&app_id)
                        .and_then(|path| load_png(&path))
                        .map(Arc::new);
                    icon_cache()
                        .lock()
                        .unwrap()
                        .insert(app_id, IconState::Loaded(icon));
                });
            if let Err(err) = res {
                warn!("error spawning icon loader thread: {err:?}");
            }
            None
        }
    }
}

fn data_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    match std::env::var_os("XDG_DATA_HOME") {
        Some(home) => dirs.push(PathBuf::from(home)),
        None => {
            if let Some(home) = std::env::var_os("HOME") {
                dirs.push(Path::new(&home).join(".local/share"));
            }
        }
    }
    let system =
        std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".to_owned());
    dirs.extend(
        system
            .split(':')
            .filter(|d| !d.is_empty())
            .map(PathBuf::from),
    );
    dirs
}

/// The `Icon=` of the app's desktop entry, or the app id itself.
fn icon_name(app_id: &str, dirs: &[PathBuf]) -> String {
    let candidates = [app_id.to_owned(), app_id.to_lowercase()];
    for dir in dirs {
        for name in &candidates {
            let path = dir.join("applications").join(format!("{name}.desktop"));
            let Ok(contents) = std::fs::read_to_string(&path) else {
                continue;
            };
            let mut in_entry = false;
            for line in contents.lines() {
                let line = line.trim();
                if line.starts_with('[') {
                    in_entry = line == "[Desktop Entry]";
                } else if in_entry {
                    if let Some(icon) = line.strip_prefix("Icon=") {
                        return icon.trim().to_owned();
                    }
                }
            }
        }
    }
    app_id.to_owned()
}

fn find_icon(app_id: &str) -> Option<PathBuf> {
    let dirs = data_dirs();
    let name = icon_name(app_id, &dirs);

    let path = Path::new(&name);
    if path.is_absolute() {
        return (path.extension().is_some_and(|e| e == "png") && path.exists())
            .then(|| path.to_owned());
    }

    const SIZES: [&str; 7] = [
        "256x256", "192x192", "128x128", "96x96", "64x64", "48x48", "32x32",
    ];
    for dir in &dirs {
        for size in SIZES {
            let path = dir
                .join("icons/hicolor")
                .join(size)
                .join("apps")
                .join(format!("{name}.png"));
            if path.exists() {
                return Some(path);
            }
        }
    }
    for dir in &dirs {
        let path = dir.join("pixmaps").join(format!("{name}.png"));
        if path.exists() {
            return Some(path);
        }
    }
    None
}

fn load_png(path: &Path) -> Option<IconImage> {
    let file = std::fs::File::open(path).ok()?;
    let mut decoder = png::Decoder::new(std::io::BufReader::new(file));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    let (w, h) = (info.width as usize, info.height as usize);
    let channels = match info.color_type {
        png::ColorType::Rgba => 4,
        png::ColorType::Rgb => 3,
        png::ColorType::GrayscaleAlpha => 2,
        png::ColorType::Grayscale => 1,
        png::ColorType::Indexed => return None,
    };

    // To cairo ARGB32: premultiplied, native-endian 0xAARRGGBB.
    let mut data = Vec::with_capacity(w * h * 4);
    for px in buf[..w * h * channels].chunks_exact(channels) {
        let (r, g, b, a) = match channels {
            4 => (px[0], px[1], px[2], px[3]),
            3 => (px[0], px[1], px[2], 255),
            2 => (px[0], px[0], px[0], px[1]),
            _ => (px[0], px[0], px[0], 255),
        };
        let pm = |c: u8| ((u16::from(c) * u16::from(a) + 127) / 255) as u8;
        let argb =
            u32::from(a) << 24 | u32::from(pm(r)) << 16 | u32::from(pm(g)) << 8 | u32::from(pm(b));
        data.extend_from_slice(&argb.to_ne_bytes());
    }
    Some(IconImage {
        width: w as i32,
        height: h as i32,
        data,
    })
}

/// Background colour for an app: a stable hue from the app id.
fn app_color(app_id: &str) -> (f64, f64, f64) {
    let mut hash: u32 = 2166136261;
    for b in app_id.bytes() {
        hash = (hash ^ u32::from(b)).wrapping_mul(16777619);
    }
    let hue = f64::from(hash % 360) / 360.;
    hsl_to_rgb(hue, 0.35, 0.32)
}

fn hsl_to_rgb(h: f64, s: f64, l: f64) -> (f64, f64, f64) {
    let q = if l < 0.5 { l * (1. + s) } else { l + s - l * s };
    let p = 2. * l - q;
    let f = |mut t: f64| {
        if t < 0. {
            t += 1.;
        }
        if t > 1. {
            t -= 1.;
        }
        if t < 1. / 6. {
            p + (q - p) * 6. * t
        } else if t < 0.5 {
            q
        } else if t < 2. / 3. {
            p + (q - p) * (2. / 3. - t) * 6.
        } else {
            p
        }
    };
    (f(h + 1. / 3.), f(h), f(h - 1. / 3.))
}

/// The letter shown for an app without an icon: the first letter of the last app id component.
fn app_letter(app_id: &str) -> String {
    app_id
        .rsplit('.')
        .next()
        .and_then(|s| s.chars().find(|c| c.is_alphanumeric()))
        .map(|c| c.to_uppercase().collect())
        .unwrap_or_else(|| "?".to_owned())
}

/// What an icon tile shows.
#[derive(Debug, Clone, PartialEq)]
pub enum TileContent {
    /// One window.
    App { app_id: String, has_icon: bool },
    /// A collapsed subtree: outlines of its leaves, relative to the tile (0..1).
    Group { leaves: Vec<Rect>, app_id: String },
}

#[derive(Debug, Clone, PartialEq)]
struct Key {
    size: Size<i32, Physical>,
    scale: f64,
    content: TileContent,
    is_active: bool,
}

#[derive(Default)]
struct Cached {
    key: Option<Key>,
    buffer: Option<MemoryBuffer>,
    texture: Option<TextureBuffer<GlesTexture>>,
}

/// Icon tile renderings, cached per tile id.
#[derive(Default)]
pub struct IconTiles {
    cache: RefCell<HashMap<u64, Cached>>,
}

impl std::fmt::Debug for IconTiles {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IconTiles").finish_non_exhaustive()
    }
}

impl IconTiles {
    /// Renders an icon tile with id `id` in `rect` (view coordinates).
    #[allow(clippy::too_many_arguments)]
    pub fn render<R: NiriRenderer>(
        &self,
        renderer: &mut R,
        id: u64,
        rect: Rect,
        scale: f64,
        content: TileContent,
        is_active: bool,
    ) -> Option<PrimaryGpuTextureRenderElement> {
        let size: Size<i32, Physical> = Size::from((
            to_physical_precise_round::<i32>(scale, rect.size.w),
            to_physical_precise_round::<i32>(scale, rect.size.h),
        ));
        if size.w <= 0 || size.h <= 0 {
            return None;
        }

        let icon = match &content {
            TileContent::App { app_id, .. } => app_icon(app_id),
            TileContent::Group { .. } => None,
        };
        let content = match content {
            TileContent::App { app_id, .. } => TileContent::App {
                app_id,
                has_icon: icon.is_some(),
            },
            group => group,
        };
        let key = Key {
            size,
            scale,
            content,
            is_active,
        };

        let mut cache = self.cache.borrow_mut();
        let cached = cache.entry(id).or_default();
        if cached.key.as_ref() != Some(&key) {
            cached.buffer = match draw(&key, icon.as_deref()) {
                Ok(buffer) => Some(buffer),
                Err(err) => {
                    warn!("error drawing icon tile: {err:?}");
                    None
                }
            };
            cached.texture = None;
            cached.key = Some(key);
        }

        let buffer = cached.buffer.as_ref()?;
        if cached.texture.is_none() {
            cached.texture =
                TextureBuffer::from_memory_buffer(renderer.as_gles_renderer(), buffer).ok();
        }
        let texture = cached.texture.clone()?;
        let elem = TextureRenderElement::from_texture_buffer(
            texture,
            rect.loc,
            1.,
            None,
            None,
            Kind::Unspecified,
        );
        Some(PrimaryGpuTextureRenderElement(elem))
    }

    /// Drops cached tiles whose ids are no longer used.
    pub fn retain(&self, keep: impl Fn(u64) -> bool) {
        self.cache.borrow_mut().retain(|id, _| keep(*id));
    }
}

fn rounded_rect(cr: &cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    let r = r.min(w / 2.).min(h / 2.).max(0.);
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -PI / 2., 0.);
    cr.arc(x + w - r, y + h - r, r, 0., PI / 2.);
    cr.arc(x + r, y + h - r, r, PI / 2., PI);
    cr.arc(x + r, y + r, r, PI, 1.5 * PI);
    cr.close_path();
}

fn draw(key: &Key, icon: Option<&IconImage>) -> anyhow::Result<MemoryBuffer> {
    let _span = tracy_client::span!("icon_tile::draw");

    let s = key.scale;
    let (w, h) = (key.size.w, key.size.h);
    let (wf, hf) = (f64::from(w), f64::from(h));
    let surface = ImageSurface::create(cairo::Format::ARgb32, w, h)?;
    let cr = cairo::Context::new(&surface)?;

    let app_id = match &key.content {
        TileContent::App { app_id, .. } | TileContent::Group { app_id, .. } => app_id.as_str(),
    };
    let (r, g, b) = app_color(app_id);
    let radius = (10. * s).min(wf.min(hf) / 4.);
    rounded_rect(&cr, 0., 0., wf, hf, radius);
    cr.set_source_rgb(r, g, b);
    cr.fill_preserve()?;
    if key.is_active {
        cr.set_source_rgba(0.5, 0.75, 1., 1.);
        cr.set_line_width((2. * s).max(1.));
    } else {
        cr.set_source_rgba(1., 1., 1., 0.15);
        cr.set_line_width(s.max(1.));
    }
    cr.stroke()?;

    match &key.content {
        TileContent::App { .. } => {
            let side = (wf.min(hf) * 0.5).max(1.);
            if let Some(icon) = icon {
                let img = ImageSurface::create_for_data(
                    icon.data.clone(),
                    cairo::Format::ARgb32,
                    icon.width,
                    icon.height,
                    icon.width * 4,
                )?;
                let k = side / f64::from(icon.width.max(icon.height));
                cr.save()?;
                cr.translate(
                    (wf - f64::from(icon.width) * k) / 2.,
                    (hf - f64::from(icon.height) * k) / 2.,
                );
                cr.scale(k, k);
                cr.set_source_surface(&img, 0., 0.)?;
                cr.paint()?;
                cr.restore()?;
            } else {
                let mut font = FontDescription::from_string("sans bold");
                font.set_absolute_size(side * 0.8 * f64::from(pangocairo::pango::SCALE));
                let layout = pangocairo::functions::create_layout(&cr);
                layout.set_font_description(Some(&font));
                layout.set_alignment(Alignment::Center);
                layout.set_text(&app_letter(app_id));
                let (tw, th) = layout.pixel_size();
                cr.move_to((wf - f64::from(tw)) / 2., (hf - f64::from(th)) / 2.);
                cr.set_source_rgba(1., 1., 1., 0.9);
                pangocairo::functions::show_layout(&cr, &layout);
            }
        }
        TileContent::Group { leaves, .. } => {
            // Outlines of the leaves: the shape of the split.
            let inset = (3. * s).max(1.);
            cr.set_source_rgba(1., 1., 1., 0.55);
            cr.set_line_width(s.max(1.));
            for leaf in leaves {
                let x = leaf.loc.x * wf + inset;
                let y = leaf.loc.y * hf + inset;
                let lw = leaf.size.w * wf - inset * 2.;
                let lh = leaf.size.h * hf - inset * 2.;
                if lw > 0. && lh > 0. {
                    rounded_rect(&cr, x, y, lw, lh, radius / 2.);
                    cr.stroke()?;
                }
            }
        }
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

/// Relative rectangle of `inner` within `outer` (0..1 coordinates).
pub fn relative_rect(outer: Rect, inner: Rect) -> Rect {
    let w = outer.size.w.max(1.);
    let h = outer.size.h.max(1.);
    Rect::new(
        Point::from((
            (inner.loc.x - outer.loc.x) / w,
            (inner.loc.y - outer.loc.y) / h,
        )),
        Size::from((inner.size.w / w, inner.size.h / h)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letters_and_colors() {
        assert_eq!(app_letter("org.kde.konsole"), "K");
        assert_eq!(app_letter("firefox"), "F");
        assert_eq!(app_letter(""), "?");
        assert_eq!(app_color("a"), app_color("a"));
        let (r, g, b) = app_color("org.kde.konsole");
        assert!((0. ..=1.).contains(&r) && (0. ..=1.).contains(&g) && (0. ..=1.).contains(&b));
    }

    #[test]
    fn relative_rects() {
        let outer = Rect::new(Point::from((100., 100.)), Size::from((200., 100.)));
        let inner = Rect::new(Point::from((200., 100.)), Size::from((100., 50.)));
        assert_eq!(
            relative_rect(outer, inner),
            Rect::new(Point::from((0.5, 0.)), Size::from((0.5, 0.5)))
        );
    }
}
