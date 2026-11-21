use super::*;
use crate::ipc::server::make_ipc_window;

fn window_app_menu(f: &mut Fixture) -> Option<arbor_ipc::AppMenu> {
    let mut menus = Vec::new();
    f.niri().layout.with_windows(|mapped, _, ws_id, layout| {
        menus.push(make_ipc_window(mapped, ws_id, layout).app_menu)
    });
    assert_eq!(menus.len(), 1);
    menus.pop().unwrap()
}

#[test]
fn set_address_and_release() {
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

    assert_eq!(window_app_menu(&mut f), None);

    let client = f.client(id);
    let manager = client.state.appmenu_manager.clone().unwrap();
    let appmenu = manager.create(&surface, &client.qh, ());
    appmenu.set_address(":1.42".to_owned(), "/MenuBar/1".to_owned());
    f.roundtrip(id);

    assert_eq!(
        window_app_menu(&mut f),
        Some(arbor_ipc::AppMenu {
            service_name: ":1.42".to_owned(),
            object_path: "/MenuBar/1".to_owned(),
        })
    );

    appmenu.set_address(":1.42".to_owned(), "/MenuBar/2".to_owned());
    f.roundtrip(id);
    assert_eq!(window_app_menu(&mut f).unwrap().object_path, "/MenuBar/2");

    appmenu.release();
    f.roundtrip(id);
    assert_eq!(window_app_menu(&mut f), None);
}
