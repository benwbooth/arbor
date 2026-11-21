//! Drives the GlobalShortcuts portal backend over D-Bus.
//!
//! This registers well-known names, so it must only run on a private bus:
//! `ARBOR_TEST_PRIVATE_DBUS=1 dbus-run-session -- cargo test portal_global_shortcuts -- --ignored`

use std::collections::HashMap;
use std::time::{Duration, Instant};

use arbor_config::{Config, ModKey, Modifiers};
use smithay::input::keyboard::{Keycode, Keysym};
use zbus::zvariant::{ObjectPath, OwnedValue, Value};

use super::server::Server;
use crate::global_shortcuts::GlobalShortcuts;

const DEST: &str = "org.freedesktop.impl.portal.desktop.arbor";
const PATH: &str = "/org/freedesktop/portal/desktop";
const IFACE: &str = "org.freedesktop.impl.portal.GlobalShortcuts";

type Shortcuts = Vec<(String, HashMap<String, OwnedValue>)>;

fn run_until(server: &mut Server, mut done: impl FnMut(&mut Server) -> bool) {
    let start = Instant::now();
    while !done(server) {
        assert!(start.elapsed() < Duration::from_secs(10), "timed out");
        server.dispatch();
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
#[ignore = "needs a private session bus (dbus-run-session)"]
fn portal_global_shortcuts_over_dbus() {
    if std::env::var_os("ARBOR_TEST_PRIVATE_DBUS").is_none() {
        eprintln!("skipping: set ARBOR_TEST_PRIVATE_DBUS=1 and run under dbus-run-session");
        return;
    }

    let mut config = Config::default();
    config.debug.dbus_interfaces_in_non_session_instances = true;
    let mut server = Server::new(config);
    server.state.arbor.global_shortcuts = GlobalShortcuts::with_path(None);
    crate::dbus::DBusServers::start(&mut server.state, false);

    let client = std::thread::spawn(|| {
        let conn = zbus::blocking::Connection::session().unwrap();
        let session =
            ObjectPath::try_from("/org/freedesktop/portal/desktop/session/1_1/t").unwrap();
        let request =
            ObjectPath::try_from("/org/freedesktop/portal/desktop/request/1_1/t").unwrap();

        // Listen for Activated before anything can fire.
        let proxy = zbus::blocking::Proxy::new(&conn, DEST, PATH, IFACE).unwrap();
        let mut activated = proxy.receive_signal("Activated").unwrap();

        let reply = conn
            .call_method(
                Some(DEST),
                PATH,
                Some(IFACE),
                "CreateSession",
                &(
                    &request,
                    &session,
                    "org.example.App",
                    HashMap::<&str, Value>::new(),
                ),
            )
            .unwrap();
        let (response, _): (u32, HashMap<String, OwnedValue>) = reply.body().deserialize().unwrap();
        assert_eq!(response, 0);

        let mut props = HashMap::new();
        props.insert("description", Value::from("Push to talk"));
        props.insert("preferred_trigger", Value::from("CTRL+ALT+t"));
        let shortcuts = vec![("talk", props)];
        let reply = conn
            .call_method(
                Some(DEST),
                PATH,
                Some(IFACE),
                "BindShortcuts",
                &(
                    &request,
                    &session,
                    shortcuts,
                    "",
                    HashMap::<&str, Value>::new(),
                ),
            )
            .unwrap();
        let (response, results): (u32, HashMap<String, OwnedValue>) =
            reply.body().deserialize().unwrap();
        assert_eq!(response, 0);
        let bound: Shortcuts = results["shortcuts"]
            .try_clone()
            .unwrap()
            .try_into()
            .unwrap();
        assert_eq!(bound.len(), 1);
        assert_eq!(bound[0].0, "talk");
        let trigger: &str = (&bound[0].1["trigger_description"]).try_into().unwrap();
        assert_eq!(trigger, "Ctrl+Alt+T");

        let reply = conn
            .call_method(
                Some(DEST),
                PATH,
                Some(IFACE),
                "ListShortcuts",
                &(&request, &session),
            )
            .unwrap();
        let (_, results): (u32, HashMap<String, OwnedValue>) = reply.body().deserialize().unwrap();
        let listed: Shortcuts = results["shortcuts"]
            .try_clone()
            .unwrap()
            .try_into()
            .unwrap();
        assert_eq!(listed.len(), 1);

        // Tell the test thread to press the key, then wait for the signal.
        let msg = activated.next().unwrap();
        let body = msg.body();
        let (sess, id, ts, _): (ObjectPath, String, u64, HashMap<String, OwnedValue>) =
            body.deserialize().unwrap();
        assert_eq!(sess.as_str(), session.as_str());
        assert_eq!(id, "talk");
        assert_eq!(ts, 1234);

        // Closing the session removes the object.
        conn.call_method(
            Some(DEST),
            session.as_str(),
            Some("org.freedesktop.impl.portal.Session"),
            "Close",
            &(),
        )
        .unwrap();
    });

    // Wait until the session is bound, then press its trigger.
    run_until(&mut server, |s| {
        s.state
            .arbor
            .global_shortcuts
            .all()
            .iter()
            .any(|g| g.trigger.as_deref() == Some("Ctrl+Alt+T") && g.active)
    });
    // Give the client a moment to issue ListShortcuts.
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(200) {
        server.dispatch();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(server.state.arbor.global_shortcuts.press(
        Keycode::new(28 + 8),
        ModKey::Super,
        Keysym::t,
        Modifiers::CTRL | Modifiers::ALT,
        1234,
    ));
    run_until(&mut server, |_| client.is_finished());
    client.join().unwrap();
    run_until(&mut server, |s| {
        s.state
            .arbor
            .global_shortcuts
            .all()
            .iter()
            .all(|g| !g.active)
    });
}
