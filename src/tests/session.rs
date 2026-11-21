//! arbor: session save and restore puts windows back in their dock slots and floating spots.

use smithay::desktop::Window;
use wayland_client::protocol::wl_surface::WlSurface;

use super::client::ClientId;
use super::*;
use crate::layout::dock::DockShape;
use crate::utils::with_toplevel_role;

fn open(f: &mut Fixture, id: ClientId, app_id: &str) -> WlSurface {
    let window = f.client(id).create_window();
    let surface = window.surface.clone();
    window.xdg_toplevel.set_app_id(app_id.to_owned());
    window.commit();
    f.roundtrip(id);
    let window = f.client(id).window(&surface);
    window.attach_new_buffer();
    window.set_size(300, 200);
    window.ack_last_and_commit();
    f.double_roundtrip(id);
    surface
}

fn window_by_app(f: &mut Fixture, app_id: &str) -> Window {
    let mut found = None;
    f.arbor().layout.with_windows(|mapped, _, _, _| {
        let id = with_toplevel_role(mapped.toplevel(), |role| role.app_id.clone());
        if id.as_deref() == Some(app_id) {
            found = Some(mapped.window.clone());
        }
    });
    found.unwrap_or_else(|| panic!("no window {app_id}"))
}

fn shape_by_app(f: &mut Fixture) -> Option<DockShape<String>> {
    let ws = f.arbor().layout.active_workspace().unwrap();
    let shape = ws.dock_shape()?;
    shape.filter_map(&|w: &Window| {
        with_toplevel_role(w.toplevel().unwrap(), |role| role.app_id.clone())
    })
}

fn approx(a: &DockShape<String>, b: &DockShape<String>) -> bool {
    match (a, b) {
        (
            DockShape::Leaf { tabs, active },
            DockShape::Leaf {
                tabs: t2,
                active: a2,
            },
        ) => tabs == t2 && active == a2,
        (
            DockShape::Split {
                vertical,
                fractions,
                children,
            },
            DockShape::Split {
                vertical: v2,
                fractions: f2,
                children: c2,
            },
        ) => {
            vertical == v2
                && fractions.len() == f2.len()
                && fractions.iter().zip(f2).all(|(x, y)| (x - y).abs() < 0.01)
                && children.iter().zip(c2).all(|(x, y)| approx(x, y))
        }
        _ => false,
    }
}

fn leaf(tabs: &[&str], active: usize) -> DockShape<String> {
    DockShape::Leaf {
        tabs: tabs.iter().map(|s| s.to_string()).collect(),
        active,
    }
}

#[test]
fn save_and_restore_dock_tree_tabs_and_floating() {
    let mut config = arbor_config::Config::default();
    config.hotkey_overlay.skip_at_startup = true;
    let mut f = Fixture::with_config(config);
    f.add_output(1, (1920, 1080));
    let id = f.add_client();

    let mut surfaces = Vec::new();
    for app in ["a", "b", "c", "e"] {
        surfaces.push(open(&mut f, id, app));
        f.arbor().layout.toggle_window_floating(None);
        f.double_roundtrip(id);
    }
    surfaces.push(open(&mut f, id, "d"));

    // H[ a 0.3 | V[ tabs(b, c; c shown) 0.4 / e 0.6 ] 0.7 ]
    let want = DockShape::Split {
        vertical: false,
        fractions: vec![0.3, 0.7],
        children: vec![
            leaf(&["a"], 0),
            DockShape::Split {
                vertical: true,
                fractions: vec![0.4, 0.6],
                children: vec![leaf(&["b", "c"], 1), leaf(&["e"], 0)],
            },
        ],
    };
    let by_app: Vec<(String, Window)> = ["a", "b", "c", "e"]
        .iter()
        .map(|a| (a.to_string(), window_by_app(&mut f, a)))
        .collect();
    let shape_w = want
        .filter_map(&|app: &String| {
            by_app
                .iter()
                .find(|(a, _)| a == app)
                .map(|(_, w)| w.clone())
        })
        .unwrap();
    let a = window_by_app(&mut f, "a");
    f.arbor().layout.apply_dock_shape_for(&a, &shape_w);
    f.double_roundtrip(id);
    let got = shape_by_app(&mut f).unwrap();
    assert!(approx(&got, &want), "apply_shape: {got:?}");

    // Floating "d" somewhere specific.
    let d = window_by_app(&mut f, "d");
    f.arbor().layout.move_floating_window(
        Some(&d),
        arbor_ipc::PositionChange::SetFixed(400.),
        arbor_ipc::PositionChange::SetFixed(250.),
        false,
    );
    f.double_roundtrip(id);

    let file = f.arbor_state().session_collect();
    assert_eq!(file.windows.len(), 5);
    let saved_d = file.windows.iter().find(|w| w.app_id == "d").unwrap();
    let geo = saved_d.floating.expect("d floats");
    assert!(file.windows.iter().filter(|w| w.floating.is_none()).count() == 4);
    assert_eq!(file.workspaces.len(), 1);

    // Close everything.
    for surface in &surfaces {
        let window = f.client(id).window(surface);
        window.xdg_toplevel.destroy();
        window.xdg_surface.destroy();
        window.surface.destroy();
    }
    f.double_roundtrip(id);
    assert!(shape_by_app(&mut f).is_none());

    // Restore (without launching), then the "apps" open their windows in another order.
    f.arbor_state().session_restore_from(file, false);
    for app in ["e", "d", "c", "a", "b"] {
        open(&mut f, id, app);
    }
    f.double_roundtrip(id);
    assert!(
        f.arbor().session_restore.is_none(),
        "restore should be complete"
    );

    let got = shape_by_app(&mut f).unwrap();
    assert!(approx(&got, &want), "restored: {got:?}");

    let state = f.arbor_state();
    let d_now = state.session_collect();
    let d_now = d_now.windows.iter().find(|w| w.app_id == "d").unwrap();
    let now = d_now.floating.expect("d floats again");
    assert!(
        (now.x - geo.x).abs() < 1. && (now.y - geo.y).abs() < 1.,
        "{now:?} vs {geo:?}"
    );
}

#[test]
fn excluded_apps_are_not_saved() {
    let mut config = arbor_config::Config::default();
    config.session_restore.exclude_app_ids = vec!["secret".to_owned()];
    let mut f = Fixture::with_config(config);
    f.add_output(1, (1920, 1080));
    let id = f.add_client();
    open(&mut f, id, "secret");
    open(&mut f, id, "kept");
    let file = f.arbor_state().session_collect();
    let apps: Vec<_> = file.windows.iter().map(|w| w.app_id.as_str()).collect();
    assert_eq!(apps, ["kept"]);
}
