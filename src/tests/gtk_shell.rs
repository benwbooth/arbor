use super::client::gtk_shell::gtk_shell1::Capability;
use super::*;
use crate::ipc::server::make_ipc_window;

fn window_gtk_menu(f: &mut Fixture) -> Option<arbor_ipc::GtkMenu> {
    let mut menus = Vec::new();
    f.arbor().layout.with_windows(|mapped, _, ws_id, layout| {
        menus.push(make_ipc_window(mapped, ws_id, layout).gtk_menu.map(|m| *m))
    });
    assert_eq!(menus.len(), 1);
    menus.pop().unwrap()
}

#[test]
fn set_dbus_properties_and_release() {
    let mut f = Fixture::new();
    f.add_output(1, (1920, 1080));

    let id = f.add_client();
    let window = f.client(id).create_window();
    let surface = window.surface.clone();
    window.commit();
    f.roundtrip(id);

    let window = f.client(id).window(&surface);
    window.attach_new_buffer();
    window.set_size(100, 100);
    window.ack_last_and_commit();
    f.double_roundtrip(id);

    assert_eq!(window_gtk_menu(&mut f), None);

    let client = f.client(id);
    // No registrar on the bus: GTK keeps its menu bars in the window.
    assert_eq!(client.state.gtk_shell_capabilities, Some(0));
    let shell = client.state.gtk_shell.clone().unwrap();
    let gtk_surface = shell.get_gtk_surface(&surface, &client.qh, ());
    gtk_surface.set_dbus_properties(
        Some("org.example.App".to_owned()),
        None,
        Some("/org/example/App/menus/menubar".to_owned()),
        Some("/org/example/App/window/1".to_owned()),
        Some("/org/example/App".to_owned()),
        Some(":1.42".to_owned()),
    );
    f.roundtrip(id);

    assert_eq!(
        window_gtk_menu(&mut f),
        Some(arbor_ipc::GtkMenu {
            application_id: "org.example.App".to_owned(),
            unique_bus_name: ":1.42".to_owned(),
            menubar_path: "/org/example/App/menus/menubar".to_owned(),
            app_menu_path: String::new(),
            window_object_path: "/org/example/App/window/1".to_owned(),
            application_object_path: "/org/example/App".to_owned(),
        })
    );

    // A global menu bar appears: every bound gtk_shell1 hears about it.
    f.arbor_state()
        .arbor
        .gtk_shell_state
        .set_global_menu_bar(true);
    f.roundtrip(id);
    let caps = Capability::GlobalMenuBar as u32 | Capability::GlobalAppMenu as u32;
    assert_eq!(f.client(id).state.gtk_shell_capabilities, Some(caps));

    // Without a menu bar path nor app menu there's nothing to show.
    gtk_surface.set_dbus_properties(None, None, None, None, None, Some(":1.42".to_owned()));
    f.roundtrip(id);
    assert_eq!(window_gtk_menu(&mut f), None);

    // No menus but the app's actions (libadwaita): kept, for the app menu.
    gtk_surface.set_dbus_properties(
        Some("org.example.App".to_owned()),
        None,
        None,
        Some("/org/example/App/window/1".to_owned()),
        Some("/org/example/App".to_owned()),
        Some(":1.42".to_owned()),
    );
    f.roundtrip(id);
    let menu = window_gtk_menu(&mut f).expect("actions-only address kept");
    assert_eq!(menu.menubar_path, "");
    assert_eq!(menu.application_object_path, "/org/example/App");

    gtk_surface.set_dbus_properties(
        None,
        Some("/org/example/App/menus/appmenu".to_owned()),
        None,
        None,
        None,
        Some(":1.42".to_owned()),
    );
    f.roundtrip(id);
    assert!(window_gtk_menu(&mut f).is_some());

    gtk_surface.release();
    f.roundtrip(id);
    assert_eq!(window_gtk_menu(&mut f), None);
}
