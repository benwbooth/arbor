//! arbor: headless checks of the Mission Control-style overview (exposé).

use smithay::backend::allocator::Fourcc;
use smithay::reexports::wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::Layer;
use smithay::reexports::wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::Anchor;
use smithay::utils::{Point, Scale, Transform};
use wayland_client::protocol::wl_surface::WlSurface;

use super::client::{ClientId, LayerConfigureProps};
use super::*;
use crate::layout::expose::ExposeHit;
use crate::render_helpers::{render_to_vec, RenderCtx, RenderTarget};

fn color_window(
    f: &mut Fixture,
    id: ClientId,
    (w, h): (u16, u16),
    rgb: (f32, f32, f32),
    title: &str,
) -> WlSurface {
    let window = f.client(id).create_window();
    let surface = window.surface.clone();
    window.set_title(title);
    window.commit();
    f.roundtrip(id);

    let window = f.client(id).window(&surface);
    let c = |v: f32| (v * u32::MAX as f32) as u32;
    let buffer =
        window
            .spbm
            .create_u32_rgba_buffer(c(rgb.0), c(rgb.1), c(rgb.2), u32::MAX, &window.qh, ());
    window.surface.attach(Some(&buffer), 0, 0);
    window.set_size(w, h);
    window.ack_last_and_commit();
    f.double_roundtrip(id);
    surface
}

fn wallpaper(f: &mut Fixture, id: ClientId) {
    let layer = f
        .client(id)
        .create_layer(None, Layer::Background, "wallpaper");
    let surface = layer.surface.clone();
    layer.set_configure_props(LayerConfigureProps {
        anchor: Some(Anchor::Left | Anchor::Right | Anchor::Top | Anchor::Bottom),
        ..Default::default()
    });
    layer.commit();
    f.roundtrip(id);

    let layer = f.client(id).layer(&surface);
    let buffer = layer.spbm.create_u32_rgba_buffer(
        u32::MAX / 5,
        u32::MAX / 3,
        u32::MAX / 2,
        u32::MAX,
        &layer.qh,
        (),
    );
    layer.surface.attach(Some(&buffer), 0, 0);
    layer.set_size(1920, 1080);
    layer.ack_last_and_commit();
    f.double_roundtrip(id);
}

/// Renders output 1 and returns RGBA pixels.
fn render(f: &mut Fixture) -> Vec<u8> {
    let output = f.niri_output(1);
    let state = f.niri_state();
    let niri = &mut state.niri;
    niri.update_render_elements(Some(&output));
    state
        .backend
        .headless()
        .with_primary_renderer(|renderer| {
            let ctx = RenderCtx {
                renderer: &mut *renderer,
                target: RenderTarget::ScreenCapture,
                xray: None,
            };
            let elements = niri.render_to_vec(ctx, &output, false);
            render_to_vec(
                renderer,
                (1920, 1080).into(),
                Scale::from(1.),
                Transform::Normal,
                Fourcc::Abgr8888,
                elements.iter().rev(),
            )
            .unwrap()
        })
        .unwrap()
}

fn save(pixels: &[u8], name: &str) {
    let Ok(dir) = std::env::var("ARBOR_EXPOSE_SHOT_DIR") else {
        return;
    };
    let path = std::path::Path::new(&dir).join(format!("{name}.png"));
    let file = std::fs::File::create(path).unwrap();
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), 1920, 1080);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .unwrap()
        .write_image_data(pixels)
        .unwrap();
}

fn pixel(pixels: &[u8], x: usize, y: usize) -> [u8; 4] {
    let i = (y * 1920 + x) * 4;
    pixels[i..i + 4].try_into().unwrap()
}

/// Opens three windows (two docked, one floating), takes the overview and checks that windows
/// shrink into slots, the wallpaper dims, and hit testing finds them. Needs EGL (surfaceless
/// Mesa); set `ARBOR_EXPOSE_SHOT_DIR` to also write the frames as PNGs.
#[test]
#[ignore = "needs a GPU/EGL; run with --ignored"]
fn expose_renders_and_hits() {
    let mut config = niri_config::Config::default();
    config.hotkey_overlay.skip_at_startup = true;
    let mut f = Fixture::with_config(config);
    f.niri_state().backend.headless().add_renderer().unwrap();
    f.add_output(1, (1920, 1080));
    let id = f.add_client();
    wallpaper(&mut f, id);

    let red = color_window(&mut f, id, (800, 600), (0.85, 0.2, 0.2), "Red docked");
    f.niri().layout.toggle_window_floating(None);
    f.double_roundtrip(id);
    let green = color_window(&mut f, id, (800, 600), (0.2, 0.75, 0.3), "Green docked");
    f.niri().layout.toggle_window_floating(None);
    f.double_roundtrip(id);
    let _blue = color_window(&mut f, id, (640, 400), (0.2, 0.4, 0.9), "Blue floating");
    for surface in [&red, &green] {
        f.client(id).window(surface).ack_last_and_commit();
    }
    f.double_roundtrip(id);
    f.niri_complete_animations();

    let closed = render(&mut f);
    save(&closed, "0-closed");

    // Half way through opening.
    f.niri().layout.overview_gesture_begin();
    f.niri()
        .layout
        .overview_gesture_update(150., std::time::Duration::ZERO);
    let half = render(&mut f);
    save(&half, "1-half");
    f.niri().layout.overview_gesture_end();
    f.niri_complete_animations();
    if !f.niri().layout.is_overview_open() {
        f.niri().layout.toggle_overview();
        f.niri_complete_animations();
    }
    assert!(f.niri().layout.is_overview_open());

    let open = render(&mut f);
    save(&open, "2-open");

    // The wallpaper in the corner is darker with the overview open.
    let (c0, c1) = (pixel(&closed, 1900, 1070), pixel(&open, 1900, 1070));
    assert!(c1[2] < c0[2], "wallpaper not dimmed: {c0:?} -> {c1:?}");

    // Every window is somewhere in the exposé area and hit testing finds each one at the
    // centre of its slot.
    let output = f.niri_output(1);
    let mut found = 0;
    for y in (200..1080).step_by(20) {
        for x in (0..1920).step_by(20) {
            if let Some(ExposeHit::Window { .. }) = f
                .niri()
                .layout
                .expose_hit(&output, Point::from((x as f64, y as f64)))
            {
                found += 1;
            }
        }
    }
    assert!(found > 0);
    let windows: Vec<_> = f
        .niri()
        .layout
        .windows()
        .map(|(_, m)| m.window.clone())
        .collect();
    assert_eq!(windows.len(), 3);
    for window in &windows {
        let mut hit = false;
        'outer: for y in (100..1080).step_by(10) {
            for x in (0..1920).step_by(10) {
                if let Some(ExposeHit::Window { id, .. }) = f
                    .niri()
                    .layout
                    .expose_hit(&output, Point::from((x as f64, y as f64)))
                {
                    if &id == window {
                        hit = true;
                        break 'outer;
                    }
                }
            }
        }
        assert!(hit, "window not shown in the exposé");
    }

    // The workspace bar is along the top.
    let thumbs: Vec<_> = (0..1920)
        .step_by(10)
        .filter_map(|x| {
            match f
                .niri()
                .layout
                .expose_hit(&output, Point::from((x as f64, 60.)))
            {
                Some(ExposeHit::Workspace(idx)) => Some(idx),
                _ => None,
            }
        })
        .collect();
    // The workspace with the windows and the empty one after it.
    assert!(thumbs.contains(&0) && thumbs.contains(&1));

    // Hover a window: outline and close button.
    let target = (0..1920)
        .step_by(10)
        .flat_map(|x| (300..1000).step_by(10).map(move |y| (x, y)))
        .find(|&(x, y)| {
            matches!(
                f.niri()
                    .layout
                    .expose_hit(&output, Point::from((x as f64, y as f64))),
                Some(ExposeHit::Window { close: false, .. })
            )
        })
        .unwrap();
    f.niri().layout.expose_set_pointer(Some((
        &output,
        Point::from((target.0 as f64 + 40., target.1 as f64 + 40.)),
    )));
    let hovered = render(&mut f);
    save(&hovered, "3-hover");

    // Closing animates back to the real layout.
    f.niri().layout.toggle_overview();
    f.niri_complete_animations();
    let back = render(&mut f);
    save(&back, "4-closed-again");
    assert_eq!(pixel(&back, 1900, 1070), c0);
}
