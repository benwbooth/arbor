//! arbor: screen zoom renders the frame scaled around the pointer.

use smithay::backend::allocator::Fourcc;
use smithay::utils::{Point, Scale, Transform};

use super::*;
use crate::render_helpers::{render_to_vec, RenderCtx, RenderTarget};

fn render(f: &mut Fixture) -> Vec<u8> {
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
                target: RenderTarget::Output,
                xray: None,
            };
            let elements = arbor.render_to_vec(ctx, &output, false);
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

fn is_red(pixels: &[u8], x: i32, y: i32) -> bool {
    let i = ((y * 1920 + x) * 4) as usize;
    let p = &pixels[i..i + 4];
    p[0] > 200 && p[1] < 60 && p[2] < 60
}

/// Needs EGL (surfaceless Mesa).
#[test]
#[ignore = "needs a GPU/EGL; run with --ignored"]
fn zoom_scales_around_pointer() {
    let mut config = arbor_config::Config::default();
    config.hotkey_overlay.skip_at_startup = true;
    config.animations.off = true;
    let mut f = Fixture::with_config(config);
    f.arbor_state().backend.headless().add_renderer().unwrap();
    f.add_output(1, (1920, 1080));
    let id = f.add_client();

    let window = f.client(id).create_window();
    let surface = window.surface.clone();
    window.commit();
    f.roundtrip(id);
    let window = f.client(id).window(&surface);
    let buffer = window
        .spbm
        .create_u32_rgba_buffer(u32::MAX, 0, 0, u32::MAX, &window.qh, ());
    window.surface.attach(Some(&buffer), 0, 0);
    window.set_size(300, 200);
    window.ack_last_and_commit();
    f.double_roundtrip(id);

    // Window position on screen.
    let mut pos = None;
    f.arbor().layout.with_windows(|mapped, _, ws_id, l| {
        let ipc = crate::ipc::server::make_ipc_window(mapped, ws_id, l);
        pos = ipc.layout.tile_pos_in_workspace_view;
    });
    let (wx, wy) = pos.unwrap();
    let (wx, wy) = (wx as i32, wy as i32);

    // Pointer at the window's top left corner.
    f.arbor_state()
        .move_cursor(Point::from((f64::from(wx), f64::from(wy))));

    let plain = render(&mut f);
    assert!(is_red(&plain, wx + 10, wy + 10));
    assert!(!is_red(&plain, wx + 350, wy + 250));

    f.arbor().zoom_action(|z, _, _, anim| z.set(2., 16., anim));
    assert!(f.arbor().screen_zoom.is_active());
    let zoomed = render(&mut f);
    // (wx + 350, wy + 250) on screen shows window point (175, 125): inside.
    assert!(is_red(&zoomed, wx + 350, wy + 250));
    // Window point (310, 0) is outside; on screen at wx + 620.
    assert!(!is_red(&zoomed, wx + 630, wy + 20));
    // The pointer spot stays put.
    assert!(is_red(&zoomed, wx + 4, wy + 4));

    f.arbor().zoom_action(|z, _, _, anim| z.reset(anim));
    assert!(!f.arbor().screen_zoom.is_active());
}
