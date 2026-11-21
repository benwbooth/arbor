//! `org.kde.kglobalaccel` end to end on a real (private) session bus: a client registers an
//! action the way KGlobalAccel does, a key press in the compositor emits `globalShortcutPressed`.
//!
//! Needs a session bus of its own, so it is ignored by default:
//! `dbus-run-session -- cargo test --lib tests::kglobalaccel -- --ignored`

use std::time::{Duration, Instant};

use arbor_config::binds::Modifiers;
use arbor_config::{Config, ModKey};
use futures_util::StreamExt;
use smithay::input::keyboard::{Keycode, Keysym};

use super::server::Server;
use crate::global_shortcuts::GlobalShortcuts;
use crate::kglobalaccel::keys::parse_seq;

type DKeySeq = (Vec<i32>,);
type DInfo = (
    String,
    String,
    String,
    String,
    String,
    String,
    Vec<i32>,
    Vec<i32>,
);

#[zbus::proxy(
    interface = "org.kde.KGlobalAccel",
    default_service = "org.kde.kglobalaccel",
    default_path = "/kglobalaccel"
)]
trait KGlobalAccel {
    #[zbus(name = "doRegister")]
    fn do_register(&self, action_id: &[&str]) -> zbus::Result<()>;
    #[zbus(name = "setShortcutKeys")]
    fn set_shortcut_keys(
        &self,
        action_id: &[&str],
        keys: &[DKeySeq],
        flags: u32,
    ) -> zbus::Result<Vec<DKeySeq>>;
    #[zbus(name = "getComponent")]
    fn get_component(&self, name: &str) -> zbus::Result<zbus::zvariant::OwnedObjectPath>;
    #[zbus(name = "allMainComponents")]
    fn all_main_components(&self) -> zbus::Result<Vec<Vec<String>>>;
}

#[zbus::proxy(
    interface = "org.kde.kglobalaccel.Component",
    default_service = "org.kde.kglobalaccel"
)]
trait Component {
    #[zbus(signal, name = "globalShortcutPressed")]
    fn global_shortcut_pressed(
        &self,
        component: String,
        action: String,
        ts: i64,
    ) -> zbus::Result<()>;
    #[zbus(name = "allShortcutInfos")]
    fn all_shortcut_infos(&self) -> zbus::Result<Vec<DInfo>>;
    #[zbus(property, name = "friendlyName")]
    fn friendly_name(&self) -> zbus::Result<String>;
}

/// Dispatches the compositor until `done` or a timeout.
fn pump(server: &mut Server, done: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < deadline, "timed out");
        server.dispatch();
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
#[ignore = "needs a private session bus (dbus-run-session)"]
fn kglobalaccel_over_dbus() {
    if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_none() {
        return;
    }

    let mut server = Server::new(Config::default());
    let state = &mut server.state;
    state.arbor.global_shortcuts = GlobalShortcuts::with_path(None);
    let handle = state.arbor.event_loop.clone();
    let _conn = crate::dbus::kglobalaccel::start(&handle, &mut state.arbor.global_shortcuts)
        .expect("service should start");

    let ctrl_alt_k = parse_seq("Ctrl+Alt+K").unwrap();

    // The client side runs on its own thread: its calls are answered by the compositor loop.
    let (tx, rx) = std::sync::mpsc::channel();
    let client = std::thread::spawn(move || {
        async_io::block_on(async move {
            let conn = zbus::Connection::session().await.unwrap();
            let kga = KGlobalAccelProxy::new(&conn).await.unwrap();
            let id = ["org.example.app", "say-hi", "Example", "Say hi"];
            kga.do_register(&id).await.unwrap();
            let got = kga
                .set_shortcut_keys(&id, &[(ctrl_alt_k.to_vec(),)], 2)
                .await
                .unwrap();
            assert_eq!(got, vec![(ctrl_alt_k.to_vec(),)]);

            let path = kga.get_component("org.example.app").await.unwrap();
            assert_eq!(path.as_str(), "/component/org_example_app");
            let comps = kga.all_main_components().await.unwrap();
            assert!(comps
                .iter()
                .any(|c| c[0] == "org.example.app" && c[1] == "Example"));

            let component = ComponentProxy::builder(&conn)
                .path(path)
                .unwrap()
                .build()
                .await
                .unwrap();
            assert_eq!(component.friendly_name().await.unwrap(), "Example");
            let infos = component.all_shortcut_infos().await.unwrap();
            assert_eq!(infos[0].0, "say-hi");
            assert_eq!(infos[0].6, vec![ctrl_alt_k[0]]);

            // Signatures as KF6's KGlobalAccel expects them (if busctl is around).
            let addr = std::env::var("DBUS_SESSION_BUS_ADDRESS").unwrap();
            if let Ok(out) = std::process::Command::new("busctl")
                .args([
                    &format!("--address={addr}"),
                    "introspect",
                    "org.kde.kglobalaccel",
                ])
                .args(["/kglobalaccel", "org.kde.KGlobalAccel"])
                .output()
            {
                let text = String::from_utf8_lossy(&out.stdout);
                for (member, sig) in [
                    (".setShortcutKeys", "asa(ai)u"),
                    (".globalShortcutsByKey", "(ai)(i)"),
                    (".yourShortcutsChanged", "asa(ai)"),
                    (".getGlobalShortcutsByKey", "a(ssssssaiai)"),
                ] {
                    let line = text.lines().find(|l| l.starts_with(member));
                    assert!(
                        line.is_some_and(|l| l.contains(sig)),
                        "{member} {sig}: {line:?}"
                    );
                }
            }

            let mut pressed = component.receive_global_shortcut_pressed().await.unwrap();
            tx.send(()).unwrap();
            let signal = pressed.next().await.unwrap();
            let args = signal.args().unwrap();
            (args.component().clone(), args.action().clone())
        })
    });

    pump(&mut server, || rx.try_recv().is_ok());

    // The client listens now: press Ctrl+Alt+K in the compositor.
    let gs = &mut server.state.arbor.global_shortcuts;
    let code = Keycode::new(45);
    assert!(gs.kde_press(
        code,
        ModKey::Super,
        Keysym::k,
        Modifiers::CTRL | Modifiers::ALT,
        1
    ));
    assert!(gs.kde_release(code, 2));

    pump(&mut server, || client.is_finished());
    let (component, action) = client.join().unwrap();
    assert_eq!(component, "org.example.app");
    assert_eq!(action, "say-hi");
}

/// `org.freedesktop.ScreenSaver.Inhibit` and the Inhibit portal reach the idle notifier (which
/// swayidle listens to), and un-inhibiting does too without any other event.
#[test]
#[ignore = "needs a private session bus (dbus-run-session)"]
fn idle_inhibit_over_dbus() {
    use crate::dbus::freedesktop_screensaver::ScreenSaver;
    use crate::dbus::Start as _;

    if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_none() {
        return;
    }

    let mut server = Server::new(Config::default());
    let arbor = &mut server.state.arbor;
    let (ping, source) = calloop::ping::make_ping().unwrap();
    arbor
        .event_loop
        .insert_source(source, |_, _, _| ())
        .unwrap();
    let _ss = ScreenSaver::new(arbor.is_fdo_idle_inhibited.clone(), Some(ping.clone()))
        .start()
        .unwrap();

    let portal = zbus::blocking::Connection::session().unwrap();
    portal
        .request_name("org.freedesktop.impl.portal.desktop.arbor")
        .unwrap();
    crate::dbus::portal_inhibit::register(
        &portal,
        arbor.is_portal_idle_inhibited.clone(),
        Some(ping),
    )
    .unwrap();

    let client = zbus::blocking::Connection::session().unwrap();
    let inhibited = |server: &mut Server| {
        server.dispatch();
        server.state.arbor.idle_notifier_state.is_inhibited()
    };

    let cookie: u32 = client
        .call_method(
            Some("org.freedesktop.ScreenSaver"),
            "/org/freedesktop/ScreenSaver",
            Some("org.freedesktop.ScreenSaver"),
            "Inhibit",
            &("mpv", "playing"),
        )
        .unwrap()
        .body()
        .deserialize()
        .unwrap();
    assert!(inhibited(&mut server));
    client
        .call_method(
            Some("org.freedesktop.ScreenSaver"),
            "/org/freedesktop/ScreenSaver",
            Some("org.freedesktop.ScreenSaver"),
            "UnInhibit",
            &(cookie,),
        )
        .unwrap();
    assert!(!inhibited(&mut server));

    let handle =
        zbus::zvariant::ObjectPath::try_from("/org/freedesktop/portal/desktop/request/1_1/t")
            .unwrap();
    let options: std::collections::HashMap<&str, zbus::zvariant::Value> = Default::default();
    client
        .call_method(
            Some("org.freedesktop.impl.portal.desktop.arbor"),
            "/org/freedesktop/portal/desktop",
            Some("org.freedesktop.impl.portal.Inhibit"),
            "Inhibit",
            &(&handle, "org.example.Player", "", 8u32, options),
        )
        .unwrap();
    assert!(inhibited(&mut server));
    client
        .call_method(
            Some("org.freedesktop.impl.portal.desktop.arbor"),
            &handle,
            Some("org.freedesktop.impl.portal.Request"),
            "Close",
            &(),
        )
        .unwrap();
    assert!(!inhibited(&mut server));
}
