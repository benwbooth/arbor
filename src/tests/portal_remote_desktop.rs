//! Drives arbor's RemoteDesktop portal backend over D-Bus: the permission dialog, restore data,
//! and keycodes/keysyms reaching the seat keyboard.
//!
//! This registers well-known names, so it must only run on a private bus:
//! `ARBOR_TEST_PRIVATE_DBUS=1 dbus-run-session -- cargo test portal_remote_desktop -- --ignored`

use std::collections::HashMap;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use arbor_config::Config;
use smithay::input::keyboard::{xkb, Keycode, Keysym};
use zbus::zvariant::{ObjectPath, OwnedValue, Value};

use super::server::Server;

const DEST: &str = "org.freedesktop.impl.portal.desktop.arbor";
const PATH: &str = "/org/freedesktop/portal/desktop";
const IFACE: &str = "org.freedesktop.impl.portal.RemoteDesktop";

const KEY_A: u32 = 30;
const KEY_LEFTSHIFT: u32 = 42;
const KEY_BACKSPACE: u32 = 14;

type Results = HashMap<String, OwnedValue>;

fn run_until(server: &mut Server, mut done: impl FnMut(&mut Server) -> bool) {
    let start = Instant::now();
    while !done(server) {
        assert!(start.elapsed() < Duration::from_secs(10), "timed out");
        server.dispatch();
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// What the client thread asks the compositor thread to check or do.
#[derive(Debug)]
enum Step {
    AnswerDialog(Keysym),
    /// A client pastes the remote session's selection (compositor side of the paste).
    Paste(String),
    ExpectPasted(String),
    /// arbor's own clipboard holds this text.
    SetLocalSelection(String),
    ExpectPressed(Vec<u32>),
    ExpectKeymapChanged(bool),
    Done,
}

fn call(
    conn: &zbus::blocking::Connection,
    iface: &str,
    method: &str,
    body: &(impl serde::Serialize + zbus::zvariant::DynamicType),
) -> zbus::Result<zbus::message::Message> {
    conn.call_method(Some(DEST), PATH, Some(iface), method, body)
}

fn start_session(
    conn: &zbus::blocking::Connection,
    n: u32,
    restore: Option<OwnedValue>,
) -> (ObjectPath<'static>, u32, Results) {
    start_session_with(conn, n, restore, false)
}

fn start_session_with(
    conn: &zbus::blocking::Connection,
    n: u32,
    restore: Option<OwnedValue>,
    clipboard: bool,
) -> (ObjectPath<'static>, u32, Results) {
    let session =
        ObjectPath::try_from(format!("/org/freedesktop/portal/desktop/session/1_1/rd{n}")).unwrap();
    let request =
        ObjectPath::try_from(format!("/org/freedesktop/portal/desktop/request/1_1/rd{n}")).unwrap();

    let reply = call(
        conn,
        IFACE,
        "CreateSession",
        &(
            &request,
            &session,
            "com.example.Typer",
            HashMap::<&str, Value>::new(),
        ),
    )
    .unwrap();
    let (response, _): (u32, Results) = reply.body().deserialize().unwrap();
    assert_eq!(response, 0);

    let mut options: HashMap<&str, Value> = HashMap::new();
    options.insert("types", Value::from(1u32));
    options.insert("persist_mode", Value::from(2u32));
    if let Some(restore) = restore {
        options.insert("restore_data", Value::from(restore));
    }
    let reply = call(
        conn,
        IFACE,
        "SelectDevices",
        &(&request, &session, "com.example.Typer", options),
    )
    .unwrap();
    let (response, _): (u32, Results) = reply.body().deserialize().unwrap();
    assert_eq!(response, 0);

    if clipboard {
        call(
            conn,
            "org.freedesktop.impl.portal.Clipboard",
            "RequestClipboard",
            &(&session, HashMap::<&str, Value>::new()),
        )
        .unwrap();
    }

    let reply = call(
        conn,
        IFACE,
        "Start",
        &(
            &request,
            &session,
            "com.example.Typer",
            "",
            HashMap::<&str, Value>::new(),
        ),
    )
    .unwrap();
    let (response, results): (u32, Results) = reply.body().deserialize().unwrap();
    (session, response, results)
}

fn notify(
    conn: &zbus::blocking::Connection,
    session: &ObjectPath,
    method: &str,
    code: i32,
    down: bool,
) {
    call(
        conn,
        IFACE,
        method,
        &(
            session,
            HashMap::<&str, Value>::new(),
            code,
            u32::from(down),
        ),
    )
    .unwrap();
}

/// Minimal libei sender: binds the keyboard, presses and releases `key` (evdev), calling
/// `check(true)` after the press and `check(false)` after the release.
fn eis_press_key(stream: std::os::unix::net::UnixStream, key: u32, check: &mut dyn FnMut(bool)) {
    use reis::{ei, PendingRequestResult};

    let context = ei::Context::new(stream).unwrap();
    let mut keyboard_devices: HashMap<ei::Device, Option<ei::Keyboard>> = HashMap::new();
    let start = Instant::now();
    loop {
        assert!(start.elapsed() < Duration::from_secs(10), "EIS timed out");
        context.read().unwrap();
        while let Some(result) = context.pending_event() {
            let PendingRequestResult::Request(event) = result else {
                continue;
            };
            match event {
                ei::Event::Handshake(handshake, ei::handshake::Event::HandshakeVersion { .. }) => {
                    handshake.handshake_version(1);
                    handshake.name("arbor-test");
                    handshake.context_type(ei::handshake::ContextType::Sender);
                    for (interface, version) in [
                        ("ei_callback", 1),
                        ("ei_connection", 1),
                        ("ei_seat", 1),
                        ("ei_device", 1),
                        ("ei_pingpong", 1),
                        ("ei_keyboard", 1),
                    ] {
                        handshake.interface_version(interface, version);
                    }
                    handshake.finish();
                }
                ei::Event::Connection(_, ei::connection::Event::Ping { ping }) => ping.done(0),
                ei::Event::Seat(seat, ei::seat::Event::Capability { mask, interface }) => {
                    if interface == "ei_keyboard" {
                        seat.bind(mask);
                    }
                }
                ei::Event::Seat(_, ei::seat::Event::Device { device }) => {
                    keyboard_devices.insert(device, None);
                }
                ei::Event::Device(device, ei::device::Event::Interface { object }) => {
                    if let Some(kb) = object.downcast::<ei::Keyboard>() {
                        keyboard_devices.insert(device, Some(kb));
                    }
                }
                ei::Event::Device(device, ei::device::Event::Resumed { serial }) => {
                    let last_serial = serial;
                    if let Some(Some(kb)) = keyboard_devices.get(&device) {
                        device.start_emulating(last_serial, 1);
                        kb.key(key, ei::keyboard::KeyState::Press);
                        device.frame(last_serial, 1);
                        context.flush().unwrap();
                        check(true);
                        kb.key(key, ei::keyboard::KeyState::Released);
                        device.frame(last_serial, 2);
                        device.stop_emulating(last_serial);
                        context.flush().unwrap();
                        check(false);
                        return;
                    }
                }
                _ => (),
            }
        }
        context.flush().unwrap();
    }
}

#[test]
#[ignore = "needs a private session bus (dbus-run-session)"]
fn portal_remote_desktop_over_dbus() {
    if std::env::var_os("ARBOR_TEST_PRIVATE_DBUS").is_none() {
        eprintln!("skipping: set ARBOR_TEST_PRIVATE_DBUS=1 and run under dbus-run-session");
        return;
    }

    let mut config = Config::default();
    config.debug.dbus_interfaces_in_non_session_instances = true;
    let mut server = Server::new(config);
    server.state.arbor.global_shortcuts = crate::global_shortcuts::GlobalShortcuts::with_path(None);
    crate::dbus::DBusServers::start(&mut server.state, false);

    let (steps_tx, steps_rx) = mpsc::channel::<Step>();
    let (ack_tx, ack_rx) = mpsc::channel::<()>();

    let client = std::thread::spawn(move || {
        let step = |s: Step| {
            steps_tx.send(s).unwrap();
            ack_rx.recv().unwrap();
        };
        let conn = zbus::blocking::Connection::session().unwrap();

        // First session: the dialog asks; Enter allows (Remember stays ticked).
        let dialog = std::thread::spawn({
            let steps_tx = steps_tx.clone();
            move || steps_tx.send(Step::AnswerDialog(Keysym::Return)).unwrap()
        });
        let (session, response, results) = start_session(&conn, 1, None);
        dialog.join().unwrap();
        ack_rx.recv().unwrap();
        assert_eq!(response, 0);
        let devices: u32 = (&results["devices"]).try_into().unwrap();
        assert_eq!(devices, 1);
        let persist: u32 = (&results["persist_mode"]).try_into().unwrap();
        assert_eq!(persist, 2);
        let restore = results["restore_data"].try_clone().unwrap();

        // A keycode.
        notify(
            &conn,
            &session,
            "NotifyKeyboardKeycode",
            KEY_BACKSPACE as i32,
            true,
        );
        step(Step::ExpectPressed(vec![KEY_BACKSPACE]));
        notify(
            &conn,
            &session,
            "NotifyKeyboardKeycode",
            KEY_BACKSPACE as i32,
            false,
        );
        step(Step::ExpectPressed(vec![]));

        // "A" is Shift+a on the default (us) layout.
        notify(&conn, &session, "NotifyKeyboardKeysym", 'A' as i32, true);
        step(Step::ExpectPressed(vec![KEY_LEFTSHIFT, KEY_A]));
        notify(&conn, &session, "NotifyKeyboardKeysym", 'A' as i32, false);
        step(Step::ExpectPressed(vec![]));

        // An em dash has no key: a temporary keymap types it and goes away afterwards.
        let em_dash = 0x0100_2014;
        notify(&conn, &session, "NotifyKeyboardKeysym", em_dash, true);
        step(Step::ExpectKeymapChanged(true));
        notify(&conn, &session, "NotifyKeyboardKeysym", em_dash, false);
        step(Step::ExpectKeymapChanged(false));
        step(Step::ExpectPressed(vec![]));

        // Pointer input was not granted.
        let err = call(
            &conn,
            IFACE,
            "NotifyPointerMotion",
            &(&session, HashMap::<&str, Value>::new(), 1.0f64, 1.0f64),
        );
        assert!(err.is_err());

        // A held key is released when the session closes.
        notify(&conn, &session, "NotifyKeyboardKeycode", KEY_A as i32, true);
        step(Step::ExpectPressed(vec![KEY_A]));
        conn.call_method(
            Some(DEST),
            session.as_str(),
            Some("org.freedesktop.impl.portal.Session"),
            "Close",
            &(),
        )
        .unwrap();
        step(Step::ExpectPressed(vec![]));

        // Second session with the restore data: no dialog.
        let (session, response, results) = start_session(&conn, 2, Some(restore));
        assert_eq!(response, 0);
        assert!(results.contains_key("restore_data"));
        notify(&conn, &session, "NotifyKeyboardKeycode", KEY_A as i32, true);
        step(Step::ExpectPressed(vec![KEY_A]));
        notify(
            &conn,
            &session,
            "NotifyKeyboardKeycode",
            KEY_A as i32,
            false,
        );
        step(Step::ExpectPressed(vec![]));

        // The same session over EIS: a libei sender types a key.
        let reply = call(
            &conn,
            IFACE,
            "ConnectToEIS",
            &(&session, "com.example.Typer", HashMap::<&str, Value>::new()),
        )
        .unwrap();
        let fd: zbus::zvariant::OwnedFd = reply.body().deserialize().unwrap();
        let stream = std::os::unix::net::UnixStream::from(std::os::fd::OwnedFd::from(fd));
        eis_press_key(stream, KEY_A, &mut |down| {
            step(Step::ExpectPressed(if down { vec![KEY_A] } else { vec![] }))
        });

        // Clipboard sharing.
        const CLIP: &str = "org.freedesktop.impl.portal.Clipboard";
        let clip_proxy = zbus::blocking::Proxy::new(&conn, DEST, PATH, CLIP).unwrap();
        let mut owner_changed = clip_proxy.receive_signal("SelectionOwnerChanged").unwrap();
        let mut transfer = clip_proxy.receive_signal("SelectionTransfer").unwrap();
        let dialog = std::thread::spawn({
            let steps_tx = steps_tx.clone();
            move || steps_tx.send(Step::AnswerDialog(Keysym::Return)).unwrap()
        });
        let (session, response, results) = start_session_with(&conn, 4, None, true);
        dialog.join().unwrap();
        ack_rx.recv().unwrap();
        assert_eq!(response, 0);
        let enabled: bool = (&results["clipboard_enabled"]).try_into().unwrap();
        assert!(enabled);
        // The current selection is announced on start.
        let msg = owner_changed.next().unwrap();
        let (_, _options): (ObjectPath, Results) = msg.body().deserialize().unwrap();

        // The remote side owns the clipboard...
        let mut options: HashMap<&str, Value> = HashMap::new();
        options.insert("mime_types", Value::from(vec!["text/plain;charset=utf-8"]));
        call(&conn, CLIP, "SetSelection", &(&session, options)).unwrap();
        let msg = owner_changed.next().unwrap();
        let (_, options): (ObjectPath, Results) = msg.body().deserialize().unwrap();
        let is_owner: bool = (&options["session_is_owner"]).try_into().unwrap();
        assert!(is_owner);

        // ...and a local paste is served by it.
        steps_tx
            .send(Step::Paste(String::from("text/plain;charset=utf-8")))
            .unwrap();
        ack_rx.recv().unwrap();
        let msg = transfer.next().unwrap();
        let (_, mime, serial): (ObjectPath, String, u32) = msg.body().deserialize().unwrap();
        assert_eq!(mime, "text/plain;charset=utf-8");
        let reply = call(&conn, CLIP, "SelectionWrite", &(&session, serial)).unwrap();
        let fd: zbus::zvariant::OwnedFd = reply.body().deserialize().unwrap();
        {
            use std::io::Write as _;
            let mut file = std::fs::File::from(std::os::fd::OwnedFd::from(fd));
            file.write_all(b"from the remote").unwrap();
        }
        // The reply message holds a copy of the fd; the paste ends only when every copy is closed.
        drop(reply);
        call(&conn, CLIP, "SelectionWriteDone", &(&session, serial, true)).unwrap();
        step(Step::ExpectPasted(String::from("from the remote")));

        // Reading arbor's clipboard.
        step(Step::SetLocalSelection(String::from("from arbor")));
        let reply = call(&conn, CLIP, "SelectionRead", &(&session, "text/plain")).unwrap();
        let fd: zbus::zvariant::OwnedFd = reply.body().deserialize().unwrap();
        let mut text = String::new();
        {
            use std::io::Read as _;
            std::fs::File::from(std::os::fd::OwnedFd::from(fd))
                .read_to_string(&mut text)
                .unwrap();
        }
        assert_eq!(text, "from arbor");

        // Third session: Escape cancels.
        let dialog = std::thread::spawn({
            let steps_tx = steps_tx.clone();
            move || steps_tx.send(Step::AnswerDialog(Keysym::Escape)).unwrap()
        });
        let (_, response, _) = start_session(&conn, 3, None);
        dialog.join().unwrap();
        ack_rx.recv().unwrap();
        assert_eq!(response, 1);

        steps_tx.send(Step::Done).unwrap();
    });

    let keymap_text = |server: &mut Server| {
        let keyboard = server.state.arbor.seat.get_keyboard().unwrap();
        let state = &mut server.state;
        keyboard.with_xkb_state(state, |ctx| {
            let xkb = ctx.xkb().lock().unwrap();
            unsafe { xkb.keymap() }.get_as_string(xkb::KEYMAP_FORMAT_TEXT_V1)
        })
    };
    let original_keymap = keymap_text(&mut server);
    let mut pasted: Option<std::os::fd::OwnedFd> = None;

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
            Step::AnswerDialog(key) => {
                run_until(&mut server, |s| s.state.arbor.portal_dialog.is_open());
                server.state.arbor.portal_dialog.key(key);
            }
            Step::ExpectPressed(codes) => {
                let mut expected: Vec<Keycode> =
                    codes.iter().map(|c| Keycode::new(c + 8)).collect();
                expected.sort_by_key(|k| k.raw());
                run_until(&mut server, |s| {
                    let mut pressed: Vec<Keycode> = s
                        .state
                        .arbor
                        .seat
                        .get_keyboard()
                        .unwrap()
                        .pressed_keys()
                        .into_iter()
                        .collect();
                    pressed.sort_by_key(|k| k.raw());
                    pressed == expected
                });
            }
            Step::ExpectKeymapChanged(changed) => {
                run_until(&mut server, |s| {
                    (keymap_text(s) != original_keymap) == changed
                });
            }
            Step::Paste(mime) => {
                use smithay::wayland::selection::data_device::current_data_device_selection_userdata;
                use smithay::wayland::selection::{SelectionHandler as _, SelectionTarget};
                let seat = server.state.arbor.seat.clone();
                let data = current_data_device_selection_userdata(&seat)
                    .expect("remote selection")
                    .clone();
                let (read, write) = smithay::reexports::rustix::pipe::pipe().unwrap();
                server
                    .state
                    .send_selection(SelectionTarget::Clipboard, mime, write, seat, &data);
                pasted = Some(read);
            }
            Step::ExpectPasted(text) => {
                use std::io::Read as _;
                let mut got = String::new();
                std::fs::File::from(pasted.take().unwrap())
                    .read_to_string(&mut got)
                    .unwrap();
                assert_eq!(got, text);
            }
            Step::SetLocalSelection(text) => {
                let dh = server.state.arbor.display_handle.clone();
                let seat = server.state.arbor.seat.clone();
                let data = crate::clipboard_persist::SelectionData::single(
                    "text/plain",
                    std::sync::Arc::from(text.into_bytes()),
                );
                smithay::wayland::selection::data_device::set_data_device_selection(
                    &dh,
                    &seat,
                    data.mime_types(),
                    data,
                );
            }
            Step::Done => break,
        }
        ack_tx.send(()).unwrap();
    }
    client.join().unwrap();
}
