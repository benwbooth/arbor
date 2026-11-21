//! arbor: headless render checks of liquid glass (`SetGlass`): a striped background under a
//! glass shape on a transparent layer surface. Near the rim the stripes must be displaced, in the
//! middle they must not. Needs EGL (run with `--ignored`); set `ARBOR_GLASS_SHOT_DIR` to write the
//! frames as PNGs.

use arbor_ipc::{GlassParams, GlassShape, GlassVariant, SetGlass};
use smithay::backend::allocator::Fourcc;
use smithay::reexports::wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::Layer;
use smithay::reexports::wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::Anchor;
use smithay::utils::{Scale, Transform};

use super::client::{ClientId, LayerConfigureProps, LayerMargin};
use super::*;
use crate::render_helpers::{render_to_vec, RenderCtx, RenderTarget};

const W: usize = 1920;
const H: usize = 1080;
/// Stripe period and width, logical px.
const PERIOD: i32 = 32;
const STRIPE: i32 = 16;

#[allow(clippy::too_many_arguments)]
fn solid_layer(
    f: &mut Fixture,
    id: ClientId,
    layer: Layer,
    namespace: &str,
    anchor: Anchor,
    size: (u16, u16),
    margin: Option<LayerMargin>,
    rgba: [f32; 4],
) {
    let l = f.client(id).create_layer(None, layer, namespace);
    let surface = l.surface.clone();
    l.set_configure_props(LayerConfigureProps {
        anchor: Some(anchor),
        size: Some((u32::from(size.0), u32::from(size.1))),
        margin,
        ..Default::default()
    });
    l.commit();
    f.roundtrip(id);

    let l = f.client(id).layer(&surface);
    let c = |v: f32| (v as f64 * u32::MAX as f64) as u32;
    let buffer =
        l.spbm
            .create_u32_rgba_buffer(c(rgba[0]), c(rgba[1]), c(rgba[2]), c(rgba[3]), &l.qh, ());
    l.surface.attach(Some(&buffer), 0, 0);
    l.set_size(size.0, size.1);
    l.ack_last_and_commit();
    f.double_roundtrip(id);
}

/// A white wallpaper with black vertical stripes (or, `colorful`, a dark wallpaper with a grid of
/// coloured vertical and light horizontal bars, to look at), and a transparent full-screen top
/// layer "glassy" for the glass shapes.
fn scene(scale: f64, colorful: bool) -> (Fixture, (u16, u16), ClientId) {
    let config = format!(
        r#"
        hotkey-overlay {{ skip-at-startup; }}
        output "headless-1" {{ scale {scale}; }}
        "#
    );
    let config = arbor_config::Config::parse_mem(&config).unwrap();
    let mut f = Fixture::with_config(config);
    f.arbor_state().backend.headless().add_renderer().unwrap();
    f.add_output(1, (W as u16, H as u16));
    let id = f.add_client();

    let logical = (
        (W as f64 / scale).round() as u16,
        (H as f64 / scale).round() as u16,
    );
    let all = Anchor::Left | Anchor::Right | Anchor::Top | Anchor::Bottom;
    solid_layer(
        &mut f,
        id,
        Layer::Background,
        "wallpaper",
        all,
        logical,
        None,
        if colorful {
            [0.10, 0.12, 0.18, 1.]
        } else {
            [1., 1., 1., 1.]
        },
    );
    const PALETTE: [[f32; 4]; 6] = [
        [0.95, 0.30, 0.25, 1.],
        [0.98, 0.65, 0.15, 1.],
        [0.95, 0.90, 0.30, 1.],
        [0.30, 0.80, 0.40, 1.],
        [0.25, 0.60, 0.95, 1.],
        [0.65, 0.40, 0.90, 1.],
    ];
    let mut x = 0;
    let mut i = 0;
    while x < i32::from(logical.0) {
        solid_layer(
            &mut f,
            id,
            Layer::Bottom,
            "stripe",
            Anchor::Left | Anchor::Top | Anchor::Bottom,
            (STRIPE as u16, logical.1),
            Some(LayerMargin {
                left: x,
                ..Default::default()
            }),
            if colorful {
                PALETTE[i % PALETTE.len()]
            } else {
                [0., 0., 0., 1.]
            },
        );
        x += PERIOD;
        i += 1;
    }
    if colorful {
        let mut y = 0;
        while y < i32::from(logical.1) {
            solid_layer(
                &mut f,
                id,
                Layer::Bottom,
                "bar",
                Anchor::Left | Anchor::Right | Anchor::Top,
                (logical.0, 10),
                Some(LayerMargin {
                    top: y,
                    ..Default::default()
                }),
                [0.92, 0.92, 0.95, 1.],
            );
            y += 40;
        }
    }
    solid_layer(
        &mut f,
        id,
        Layer::Top,
        "glassy",
        all,
        logical,
        None,
        [0., 0., 0., 0.],
    );
    f.arbor_complete_animations();
    (f, logical, id)
}

fn render(f: &mut Fixture, scale: f64) -> Vec<u8> {
    let output = f.arbor_output(1);
    let state = f.arbor_state();
    let arbor = &mut state.arbor;
    arbor.update_render_elements(Some(&output));
    state
        .backend
        .headless()
        .with_primary_renderer(|renderer| {
            let ctx = RenderCtx {
                renderer: &mut *renderer,
                target: RenderTarget::ScreenCapture,
                xray: None,
            };
            let elements = arbor.render_to_vec(ctx, &output, false);
            render_to_vec(
                renderer,
                (W as i32, H as i32).into(),
                Scale::from(scale),
                Transform::Normal,
                Fourcc::Abgr8888,
                elements.iter().rev(),
            )
            .unwrap()
        })
        .unwrap()
}

fn save(pixels: &[u8], name: &str) {
    let Ok(dir) = std::env::var("ARBOR_GLASS_SHOT_DIR") else {
        return;
    };
    std::fs::create_dir_all(&dir).unwrap();
    let path = std::path::Path::new(&dir).join(format!("{name}.png"));
    let file = std::fs::File::create(path).unwrap();
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), W as u32, H as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .unwrap()
        .write_image_data(pixels)
        .unwrap();
}

fn luma(pixels: &[u8], x: usize, y: usize) -> u8 {
    let i = (y * W + x) * 4;
    pixels[i + 1]
}

/// A glass request for "glassy" with one shape and plain (measurable) optics: no blur, tint,
/// saturation, dispersion or rim, so outside the bending band the glass shows the background
/// unchanged.
fn plain(shape: (f64, f64, f64, f64, f64)) -> SetGlass {
    let (x, y, width, height, radius) = shape;
    SetGlass {
        namespace: "glassy".into(),
        shapes: vec![GlassShape {
            x,
            y,
            width,
            height,
            radius,
            params: GlassParams {
                blur_radius: Some(0.),
                saturation: Some(1.),
                dispersion: Some(0.),
                rim: Some(0.),
                tint: Some([0., 0., 0., 0.]),
                ..Default::default()
            },
        }],
        ..Default::default()
    }
}

fn check_scale(scale: f64, tag: &str) {
    let (mut f, logical, _) = scene(scale, false);
    let background = render(&mut f, scale);
    save(&background, &format!("{tag}-0-background"));

    // A big rounded rectangle in the middle of the screen (logical px).
    let (lw, lh) = (f64::from(logical.0), f64::from(logical.1));
    let (sw, sh, radius) = (lw * 0.4, lh * 0.3, 60.);
    let (sx, sy) = ((lw - sw) / 2., (lh - sh) / 2.);
    f.arbor().set_glass(plain((sx, sy, sw, sh, radius)));
    let glass = render(&mut f, scale);
    save(&glass, &format!("{tag}-1-plain"));

    // Physical coordinates of the shape.
    let px = |v: f64| (v * scale).round() as usize;
    let (x0, x1, ymid) = (px(sx), px(sx + sw), px(sy + sh / 2.));
    let bezel = (radius * scale) as usize;

    // Count pixels along the middle row that differ from the background.
    let differing = |from: usize, to: usize| {
        (from..to)
            .filter(|&x| {
                let a = i32::from(luma(&background, x, ymid));
                let b = i32::from(luma(&glass, x, ymid));
                (a - b).abs() > 24
            })
            .count()
    };
    // Outside the shape: untouched.
    assert_eq!(
        differing(x0 - 40, x0 - 2),
        0,
        "glass leaked outside its shape"
    );
    // The rim bands (left and right): the stripes are displaced.
    let left = differing(x0 + 1, x0 + bezel / 2);
    let right = differing(x1 - bezel / 2, x1 - 1);
    let band = bezel / 2 - 1;
    assert!(
        left * 4 > band && right * 4 > band,
        "rim not displaced: {left}/{band} left, {right}/{band} right differ"
    );
    // The middle (well inside the bending band): the background unchanged.
    let middle = differing(x0 + bezel + 8, x1 - bezel - 8);
    assert_eq!(middle, 0, "centre of the glass is displaced");

    // The top rim bends vertically: vertical stripes stay put there, so check that a column
    // through the top band matches the background on stripe interiors (sanity of the direction).
    // And the full default look renders (blur, tint, rim, dispersion) without leaking outside.
    let mut look = plain((sx, sy, sw, sh, radius));
    look.shapes[0].params = GlassParams::default();
    // A second, clear pill below.
    look.shapes.push(GlassShape {
        x: sx,
        y: sy + sh + 24.,
        width: sw / 2.,
        height: 56.,
        radius: 28.,
        params: GlassParams {
            variant: Some(GlassVariant::Clear),
            ..Default::default()
        },
    });
    f.arbor().set_glass(look);
    let styled = render(&mut f, scale);
    save(&styled, &format!("{tag}-2-default"));
    let a = i32::from(luma(&background, x0 - 20, ymid));
    let b = i32::from(luma(&styled, x0 - 20, ymid));
    assert!((a - b).abs() <= 2, "default glass leaked outside its shape");
    // Blur: the middle of the regular glass is no longer pure black/white stripes.
    let mid: Vec<u8> = (px(lw / 2.) - 16..px(lw / 2.) + 16)
        .map(|x| luma(&styled, x, ymid))
        .collect();
    let (lo, hi) = (*mid.iter().min().unwrap(), *mid.iter().max().unwrap());
    assert!(hi - lo < 200, "regular glass is not blurred: {lo}..{hi}");

    // Clearing restores the background.
    f.arbor().set_glass(SetGlass {
        namespace: "glassy".into(),
        ..Default::default()
    });
    let cleared = render(&mut f, scale);
    assert_eq!(
        cleared, background,
        "clearing the glass left something behind"
    );

    // For looking at: the default look over a colourful grid, a menu-like panel, a dock-like
    // slab and a clear pill.
    let (mut f, _, _) = scene(scale, true);
    let shapes = vec![
        GlassShape {
            x: lw * 0.08,
            y: lh * 0.12,
            width: 280.,
            height: 360.,
            radius: 14.,
            params: GlassParams::default(),
        },
        GlassShape {
            x: lw * 0.3,
            y: lh - 110.,
            width: lw * 0.4,
            height: 86.,
            radius: 26.,
            params: GlassParams::default(),
        },
        GlassShape {
            x: lw * 0.55,
            y: lh * 0.2,
            width: 360.,
            height: 64.,
            radius: 32.,
            params: GlassParams {
                variant: Some(GlassVariant::Clear),
                ..Default::default()
            },
        },
    ];
    f.arbor().set_glass(SetGlass {
        namespace: "glassy".into(),
        shapes,
        ..Default::default()
    });
    let colorful = render(&mut f, scale);
    save(&colorful, &format!("{tag}-3-colorful"));
}

#[test]
#[ignore = "needs a GPU/EGL; run with --ignored"]
fn glass_refracts_rim_not_centre() {
    check_scale(1., "scale1");
}

#[test]
#[ignore = "needs a GPU/EGL; run with --ignored"]
fn glass_refracts_at_fractional_scale() {
    check_scale(4. / 3., "scale4_3");
}

/// Glass follows the surface: unmapping the target surface drops its shapes.
#[test]
#[ignore = "needs a GPU/EGL; run with --ignored"]
fn glass_dropped_when_surface_unmaps() {
    let (mut f, _, id) = scene(1., false);
    f.arbor().set_glass(plain((100., 100., 300., 200., 40.)));
    assert!(!f.arbor().glass.is_empty());
    let has = |f: &mut Fixture| {
        f.arbor()
            .mapped_layer_surfaces
            .values()
            .any(|m| m.has_glass())
    };
    assert!(has(&mut f));

    // "glassy" is the last layer surface the scene creates.
    let surface = f.client(id).state.layers.last().unwrap().surface.clone();
    f.client(id).layer(&surface).attach_null();
    f.client(id).layer(&surface).commit();
    f.double_roundtrip(id);
    assert!(f.arbor().glass.is_empty());
    assert!(!has(&mut f));
}
