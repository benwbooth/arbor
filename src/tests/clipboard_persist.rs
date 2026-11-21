//! arbor: the clipboard outlives the source that set it.

use std::io::Write as _;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use smithay::wayland::selection::data_device::current_data_device_selection_userdata;
use smithay::wayland::selection::SelectionTarget;
use wayland_client::protocol::wl_data_device::{self, WlDataDevice};
use wayland_client::protocol::wl_data_device_manager::WlDataDeviceManager;
use wayland_client::protocol::wl_data_offer::{self, WlDataOffer};
use wayland_client::protocol::wl_data_source::{self, WlDataSource};
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::protocol::wl_seat::{self, WlSeat};
use wayland_client::{event_created_child, Connection, Dispatch, Proxy, QueueHandle};

use super::client::{ClientId, State};
use super::*;

#[derive(Default)]
struct Globals {
    ddm: Option<WlDataDeviceManager>,
    seat: Option<WlSeat>,
}

type G = Arc<Mutex<Globals>>;

/// What the source hands out, per mime type.
type Content = Arc<Vec<(String, Vec<u8>)>>;

impl Dispatch<WlRegistry, G> for State {
    fn event(
        _: &mut Self,
        registry: &WlRegistry,
        event: wl_registry::Event,
        data: &G,
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            let mut g = data.lock().unwrap();
            if interface == WlDataDeviceManager::interface().name {
                g.ddm = Some(registry.bind(name, version.min(3), qh, ()));
            } else if interface == WlSeat::interface().name {
                g.seat = Some(registry.bind(name, 1, qh, ()));
            }
        }
    }
}

impl Dispatch<WlDataDeviceManager, ()> for State {
    fn event(
        _: &mut Self,
        _: &WlDataDeviceManager,
        _: <WlDataDeviceManager as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlSeat, ()> for State {
    fn event(
        _: &mut Self,
        _: &WlSeat,
        _: wl_seat::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlDataDevice, ()> for State {
    fn event(
        _: &mut Self,
        _: &WlDataDevice,
        _: wl_data_device::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }

    event_created_child!(State, WlDataDevice, [
        wl_data_device::EVT_DATA_OFFER_OPCODE => (WlDataOffer, ()),
    ]);
}

impl Dispatch<WlDataOffer, ()> for State {
    fn event(
        _: &mut Self,
        _: &WlDataOffer,
        _: wl_data_offer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlDataSource, Content> for State {
    fn event(
        _: &mut Self,
        _: &WlDataSource,
        event: wl_data_source::Event,
        content: &Content,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_data_source::Event::Send { mime_type, fd } = event {
            if let Some((_, data)) = content.iter().find(|(m, _)| *m == mime_type) {
                let _ = std::fs::File::from(fd).write_all(data);
            }
        }
    }
}

fn setup() -> (Fixture, ClientId, G) {
    let mut f = Fixture::new();
    f.add_output(1, (1920, 1080));
    let id = f.add_client();
    let g = G::default();
    {
        let client = f.client(id);
        client.display.get_registry(&client.qh, g.clone());
    }
    f.double_roundtrip(id);

    // A focused window, so that the client may set the selection.
    let window = f.client(id).create_window();
    let surface = window.surface.clone();
    window.commit();
    f.roundtrip(id);
    let window = f.client(id).window(&surface);
    window.attach_new_buffer();
    window.set_size(100, 100);
    window.ack_last_and_commit();
    f.double_roundtrip(id);
    (f, id, g)
}

fn set_selection(f: &mut Fixture, id: ClientId, g: &G, content: Content) -> WlDataSource {
    let (ddm, seat) = {
        let g = g.lock().unwrap();
        (g.ddm.clone().unwrap(), g.seat.clone().unwrap())
    };
    let client = f.client(id);
    let device = ddm.get_data_device(&seat, &client.qh, ());
    let source = ddm.create_data_source(&client.qh, content.clone());
    for (mime, _) in content.iter() {
        source.offer(mime.clone());
    }
    // The keyboard-focus serial isn't checked against this; any serial works in arbor.
    device.set_selection(Some(&source), 0);
    f.double_roundtrip(id);
    source
}

/// Runs both sides until `done` or a timeout (the reads happen on a thread and a timer).
fn pump(f: &mut Fixture, id: ClientId, mut done: impl FnMut(&mut Fixture) -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(5) {
        f.roundtrip(id);
        if done(f) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

fn kept_text(f: &mut Fixture) -> Option<Vec<u8>> {
    f.arbor()
        .clipboard_persist
        .kept(SelectionTarget::Clipboard)
        .and_then(|d| d.get("text/plain"))
        .map(|d| d.to_vec())
}

fn compositor_text(f: &mut Fixture) -> Option<Vec<u8>> {
    let seat = f.arbor().seat.clone();
    let data = current_data_device_selection_userdata::<crate::arbor::State>(&seat);
    data.and_then(|d| d.get("text/plain")).map(|d| d.to_vec())
}

#[test]
fn clipboard_survives_source_destruction() {
    let (mut f, id, g) = setup();
    let content: Content = Arc::new(vec![
        ("text/plain".to_owned(), b"hello".to_vec()),
        ("text/html".to_owned(), b"<b>hello</b>".to_vec()),
    ]);
    let source = set_selection(&mut f, id, &g, content);

    assert!(
        pump(&mut f, id, |f| kept_text(f).is_some()),
        "selection was never read"
    );
    assert_eq!(kept_text(&mut f).as_deref(), Some(&b"hello"[..]));
    // While the source lives, arbor doesn't take over.
    assert_eq!(compositor_text(&mut f), None);

    // The app goes away (destroying its source clears the selection, as quitting does).
    source.destroy();
    assert!(
        pump(&mut f, id, |f| compositor_text(f).is_some()),
        "kept selection was not offered"
    );
    assert_eq!(compositor_text(&mut f).as_deref(), Some(&b"hello"[..]));
    let seat = f.arbor().seat.clone();
    let html = current_data_device_selection_userdata::<crate::arbor::State>(&seat)
        .and_then(|d| d.get("text/html"));
    assert_eq!(html.as_deref(), Some(&b"<b>hello</b>"[..]));
}

#[test]
fn password_manager_selection_is_not_kept() {
    let (mut f, id, g) = setup();
    let content: Content = Arc::new(vec![
        ("text/plain".to_owned(), b"hunter2".to_vec()),
        ("x-kde-passwordManagerHint".to_owned(), b"secret".to_vec()),
    ]);
    let source = set_selection(&mut f, id, &g, content);
    for _ in 0..10 {
        std::thread::sleep(Duration::from_millis(20));
        f.roundtrip(id);
    }
    assert_eq!(kept_text(&mut f), None);
    source.destroy();
    f.double_roundtrip(id);
    std::thread::sleep(Duration::from_millis(200));
    f.double_roundtrip(id);
    assert_eq!(compositor_text(&mut f), None);
}

#[test]
fn persist_off_keeps_nothing() {
    let (mut f, id, g) = setup();
    f.arbor().config.borrow_mut().clipboard.persist = false;
    let content: Content = Arc::new(vec![("text/plain".to_owned(), b"x".to_vec())]);
    let source = set_selection(&mut f, id, &g, content);
    std::thread::sleep(Duration::from_millis(100));
    f.double_roundtrip(id);
    assert_eq!(kept_text(&mut f), None);
    source.destroy();
    f.double_roundtrip(id);
    assert_eq!(compositor_text(&mut f), None);
}
