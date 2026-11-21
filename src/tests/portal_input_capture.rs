//! Drives arbor's InputCapture portal backend over D-Bus with a libei receiver: barrier,
//! activation, captured motion and keys, release.
//!
//! This registers well-known names, so it must only run on a private bus:
//! `ARBOR_TEST_PRIVATE_DBUS=1 dbus-run-session -- cargo test portal_ -- --ignored --test-threads=1`

use std::collections::HashMap;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use arbor_config::Config;
use smithay::input::keyboard::Keysym;
use zbus::zvariant::{ObjectPath, OwnedValue, Value};

use super::server::Server;
use crate::remote_input::{AbsTarget, RemoteInput};

const DEST: &str = "org.freedesktop.impl.portal.desktop.arbor";
const PATH: &str = "/org/freedesktop/portal/desktop";
const IFACE: &str = "org.freedesktop.impl.portal.InputCapture";

type Results = HashMap<String, OwnedValue>;

#[derive(Debug)]
enum Step {
    AnswerDialog,
    Push,
    PressKey,
    ExpectPointerAt(f64, f64),
    Done,
}

fn run_until(server: &mut Server, mut done: impl FnMut(&mut Server) -> bool) {
    let start = Instant::now();
    while !done(server) {
        assert!(start.elapsed() < Duration::from_secs(10), "timed out");
        server.dispatch();
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Reads EIS events until `want` returns true for one.
fn eis_until(context: &reis::ei::Context, want: &mut dyn FnMut(reis::ei::Event) -> bool) {
    use reis::PendingRequestResult;
    let start = Instant::now();
    loop {
        assert!(start.elapsed() < Duration::from_secs(10), "EIS timed out");
        while let Some(result) = context.pending_event() {
            if let PendingRequestResult::Request(event) = result {
                if want(event) {
                    let _ = context.flush();
                    return;
                }
            }
        }
        let _ = context.flush();
        context.read().unwrap();
    }
}

#[test]
#[ignore = "needs a private session bus (dbus-run-session)"]
fn portal_input_capture_over_dbus() {
    if std::env::var_os("ARBOR_TEST_PRIVATE_DBUS").is_none() {
        eprintln!("skipping: set ARBOR_TEST_PRIVATE_DBUS=1 and run under dbus-run-session");
        return;
    }

    let mut config = Config::default();
    config.debug.dbus_interfaces_in_non_session_instances = true;
    let mut server = Server::new(config);
    server.state.arbor.global_shortcuts = crate::global_shortcuts::GlobalShortcuts::with_path(None);
    {
        let state = &mut server.state;
        state
            .backend
            .headless()
            .add_output(&mut state.arbor, 1, (1920, 1080));
    }
    crate::dbus::DBusServers::start(&mut server.state, false);

    let (steps_tx, steps_rx) = mpsc::channel::<Step>();
    let (ack_tx, ack_rx) = mpsc::channel::<()>();

    let client = std::thread::spawn(move || {
        use reis::ei;

        let step = |s: Step| {
            steps_tx.send(s).unwrap();
            ack_rx.recv().unwrap();
        };
        let conn = zbus::blocking::Connection::session().unwrap();
        let proxy = zbus::blocking::Proxy::new(&conn, DEST, PATH, IFACE).unwrap();
        let mut activated = proxy.receive_signal("Activated").unwrap();
        let mut deactivated = proxy.receive_signal("Deactivated").unwrap();

        let session =
            ObjectPath::try_from("/org/freedesktop/portal/desktop/session/1_1/ic").unwrap();
        let request =
            ObjectPath::try_from("/org/freedesktop/portal/desktop/request/1_1/ic").unwrap();

        let dialog = std::thread::spawn({
            let steps_tx = steps_tx.clone();
            move || steps_tx.send(Step::AnswerDialog).unwrap()
        });
        let mut options: HashMap<&str, Value> = HashMap::new();
        options.insert("capabilities", Value::from(3u32));
        let reply = conn
            .call_method(
                Some(DEST),
                PATH,
                Some(IFACE),
                "CreateSession",
                &(&request, &session, "org.deskflow.deskflow", "", options),
            )
            .unwrap();
        dialog.join().unwrap();
        ack_rx.recv().unwrap();
        let (response, results): (u32, Results) = reply.body().deserialize().unwrap();
        assert_eq!(response, 0);
        let caps: u32 = (&results["capabilities"]).try_into().unwrap();
        assert_eq!(caps, 3);

        let reply = conn
            .call_method(
                Some(DEST),
                PATH,
                Some(IFACE),
                "GetZones",
                &(&request, &session, "", HashMap::<&str, Value>::new()),
            )
            .unwrap();
        let (_, results): (u32, Results) = reply.body().deserialize().unwrap();
        let zones: Vec<(u32, u32, i32, i32)> =
            results["zones"].try_clone().unwrap().try_into().unwrap();
        assert_eq!(zones, vec![(1920, 1080, 0, 0)]);
        let zone_set: u32 = (&results["zone_set"]).try_into().unwrap();

        let mut right: HashMap<&str, Value> = HashMap::new();
        right.insert("barrier_id", Value::from(1u32));
        right.insert("position", Value::from((1920i32, 0i32, 1920i32, 1079i32)));
        let mut inside: HashMap<&str, Value> = HashMap::new();
        inside.insert("barrier_id", Value::from(2u32));
        inside.insert("position", Value::from((500i32, 0i32, 500i32, 1079i32)));
        let reply = conn
            .call_method(
                Some(DEST),
                PATH,
                Some(IFACE),
                "SetPointerBarriers",
                &(
                    &request,
                    &session,
                    "",
                    HashMap::<&str, Value>::new(),
                    vec![right, inside],
                    zone_set,
                ),
            )
            .unwrap();
        let (_, results): (u32, Results) = reply.body().deserialize().unwrap();
        let failed: Vec<u32> = results["failed_barriers"]
            .try_clone()
            .unwrap()
            .try_into()
            .unwrap();
        assert_eq!(failed, vec![2]);

        // libei receiver.
        let reply = conn
            .call_method(
                Some(DEST),
                PATH,
                Some(IFACE),
                "ConnectToEIS",
                &(&session, "", HashMap::<&str, Value>::new()),
            )
            .unwrap();
        let fd: zbus::zvariant::OwnedFd = reply.body().deserialize().unwrap();
        let stream = std::os::unix::net::UnixStream::from(std::os::fd::OwnedFd::from(fd));
        let context = ei::Context::new(stream).unwrap();
        let mut resumed = 0;
        let mut mask = 0u64;
        eis_until(&context, &mut |event| {
            match event {
                ei::Event::Handshake(h, ei::handshake::Event::HandshakeVersion { .. }) => {
                    h.handshake_version(1);
                    h.name("arbor-test-capture");
                    h.context_type(ei::handshake::ContextType::Receiver);
                    for (i, v) in [
                        ("ei_callback", 1),
                        ("ei_connection", 1),
                        ("ei_seat", 1),
                        ("ei_device", 1),
                        ("ei_pingpong", 1),
                        ("ei_keyboard", 1),
                        ("ei_pointer", 1),
                        ("ei_button", 1),
                        ("ei_scroll", 1),
                    ] {
                        h.interface_version(i, v);
                    }
                    h.finish();
                }
                ei::Event::Connection(_, ei::connection::Event::Ping { ping }) => ping.done(0),
                ei::Event::Seat(_, ei::seat::Event::Capability { mask: m, .. }) => mask |= m,
                ei::Event::Seat(seat, ei::seat::Event::Done) => seat.bind(mask),
                ei::Event::Device(_, ei::device::Event::Resumed { .. }) => resumed += 1,
                _ => (),
            }
            resumed == 2
        });

        conn.call_method(
            Some(DEST),
            PATH,
            Some(IFACE),
            "Enable",
            &(&session, "", HashMap::<&str, Value>::new()),
        )
        .unwrap();

        // Push the pointer through the right edge.
        step(Step::Push);
        let msg = activated.next().unwrap();
        let (_, options): (ObjectPath, Results) = msg.body().deserialize().unwrap();
        let activation_id: u32 = (&options["activation_id"]).try_into().unwrap();
        let barrier_id: u32 = (&options["barrier_id"]).try_into().unwrap();
        assert_eq!(barrier_id, 1);

        let mut started = false;
        eis_until(&context, &mut |event| match event {
            ei::Event::Device(_, ei::device::Event::StartEmulating { sequence, .. }) => {
                assert_eq!(sequence, activation_id);
                started = true;
                false
            }
            ei::Event::Pointer(_, ei::pointer::Event::MotionRelative { x, .. }) => {
                assert!(started);
                assert!(x > 0.);
                true
            }
            _ => false,
        });

        // Keys go to the receiver, not to windows.
        step(Step::PressKey);
        eis_until(&context, &mut |event| {
            matches!(
                event,
                ei::Event::Keyboard(
                    _,
                    ei::keyboard::Event::Key {
                        key: 30,
                        state: ei::keyboard::KeyState::Press
                    }
                )
            )
        });

        // Release, putting the pointer at (100, 100).
        let mut options: HashMap<&str, Value> = HashMap::new();
        options.insert("activation_id", Value::from(activation_id));
        options.insert("cursor_position", Value::from((100.0f64, 100.0f64)));
        conn.call_method(
            Some(DEST),
            PATH,
            Some(IFACE),
            "Release",
            &(&session, "", options),
        )
        .unwrap();
        let msg = deactivated.next().unwrap();
        let (_, options): (ObjectPath, Results) = msg.body().deserialize().unwrap();
        let id: u32 = (&options["activation_id"]).try_into().unwrap();
        assert_eq!(id, activation_id);
        step(Step::ExpectPointerAt(100., 100.));

        conn.call_method(
            Some(DEST),
            session.as_str(),
            Some("org.freedesktop.impl.portal.Session"),
            "Close",
            &(),
        )
        .unwrap();
        steps_tx.send(Step::Done).unwrap();
    });

    loop {
        let step = loop {
            match steps_rx.try_recv() {
                Ok(step) => break step,
                Err(mpsc::TryRecvError::Empty) => {
                    server.dispatch();
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(mpsc::TryRecvError::Disconnected) => panic!("client thread failed"),
            }
        };
        match step {
            Step::AnswerDialog => {
                run_until(&mut server, |s| s.state.arbor.portal_dialog.is_open());
                server.state.arbor.portal_dialog.key(Keysym::Return);
            }
            Step::Push => {
                run_until(&mut server, |s| {
                    s.state
                        .arbor
                        .input_capture
                        .sessions
                        .values()
                        .any(|s| s.enabled && !s.barriers.is_empty())
                });
                server.state.remote_input(
                    999,
                    RemoteInput::PointerMotionAbsolute {
                        target: AbsTarget::Global,
                        x: 1915.,
                        y: 500.,
                    },
                );
                server
                    .state
                    .remote_input(999, RemoteInput::PointerMotion { dx: 20., dy: 0. });
                assert!(server.state.arbor.input_capture.is_capturing());
                // Motion while capturing goes to the receiver.
                server
                    .state
                    .remote_input(999, RemoteInput::PointerMotion { dx: 5., dy: 0. });
            }
            Step::PressKey => {
                server.state.remote_input(
                    999,
                    RemoteInput::Key {
                        keycode: 30,
                        pressed: true,
                    },
                );
                server.state.remote_input(
                    999,
                    RemoteInput::Key {
                        keycode: 30,
                        pressed: false,
                    },
                );
                // Windows never saw it.
                assert!(server
                    .state
                    .arbor
                    .seat
                    .get_keyboard()
                    .unwrap()
                    .pressed_keys()
                    .is_empty());
            }
            Step::ExpectPointerAt(x, y) => {
                run_until(&mut server, |s| {
                    let p = s.state.arbor.seat.get_pointer().unwrap().current_location();
                    (p.x - x).abs() < 0.5 && (p.y - y).abs() < 0.5
                });
                assert!(!server.state.arbor.input_capture.is_capturing());
            }
            Step::Done => break,
        }
        ack_tx.send(()).unwrap();
    }
    client.join().unwrap();
}
