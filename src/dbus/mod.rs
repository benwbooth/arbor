use zbus::blocking::Connection;
use zbus::object_server::Interface;

use crate::arbor::State;

pub mod freedesktop_a11y;
pub mod freedesktop_locale1;
pub mod freedesktop_login1;
pub mod freedesktop_screensaver;
pub mod gnome_shell_introspect;
pub mod gnome_shell_screenshot;
pub mod kglobalaccel;
pub mod mutter_display_config;
pub mod mutter_service_channel;
#[cfg(feature = "xdp-gnome-screencast")]
pub mod portal_casting;
pub mod portal_global_shortcuts;
pub mod portal_inhibit;
pub mod portal_input_capture;
pub mod portal_screenshot;
pub mod portal_wallpaper;

#[cfg(feature = "xdp-gnome-screencast")]
pub mod mutter_screen_cast;
#[cfg(feature = "xdp-gnome-screencast")]
use mutter_screen_cast::ScreenCast;

use self::freedesktop_a11y::KeyboardMonitor;
use self::freedesktop_screensaver::ScreenSaver;
use self::gnome_shell_introspect::Introspect;
use self::mutter_display_config::DisplayConfig;
use self::mutter_service_channel::ServiceChannel;

pub(crate) trait Start: Interface {
    fn start(self) -> anyhow::Result<zbus::blocking::Connection>;
}

#[derive(Default)]
pub struct DBusServers {
    pub conn_service_channel: Option<Connection>,
    pub conn_display_config: Option<Connection>,
    pub conn_screen_saver: Option<Connection>,
    pub conn_screen_shot: Option<Connection>,
    pub conn_introspect: Option<Connection>,
    #[cfg(feature = "xdp-gnome-screencast")]
    pub conn_screen_cast: Option<Connection>,
    pub conn_login1: Option<Connection>,
    pub conn_locale1: Option<Connection>,
    pub conn_keyboard_monitor: Option<Connection>,
    pub conn_global_shortcuts: Option<Connection>,
    pub conn_kglobalaccel: Option<Connection>,
}

impl DBusServers {
    pub fn start(state: &mut State, is_session_instance: bool) {
        let _span = tracy_client::span!("DBusServers::start");

        let backend = &state.backend;
        let arbor = &mut state.arbor;
        let config = arbor.config.borrow();

        let mut dbus = Self::default();

        if is_session_instance {
            let (to_arbor, from_service_channel) = calloop::channel::channel();
            let service_channel = ServiceChannel::new(to_arbor);
            arbor
                .event_loop
                .insert_source(from_service_channel, move |event, _, state| match event {
                    calloop::channel::Event::Msg(new_client) => {
                        state.arbor.insert_client(new_client);
                    }
                    calloop::channel::Event::Closed => (),
                })
                .unwrap();
            dbus.conn_service_channel = try_start(service_channel);
        }

        if is_session_instance || config.debug.dbus_interfaces_in_non_session_instances {
            let (to_arbor, from_display_config) = calloop::channel::channel();
            let display_config = DisplayConfig::new(to_arbor, backend.ipc_outputs());
            arbor
                .event_loop
                .insert_source(from_display_config, move |event, _, state| match event {
                    calloop::channel::Event::Msg(new_conf) => {
                        for (name, conf) in new_conf {
                            state.modify_output_config(&name, move |output| {
                                if let Some(new_output) = conf {
                                    *output = new_output;
                                } else {
                                    output.off = true;
                                }
                            });
                        }
                        state.reload_output_config();
                    }
                    calloop::channel::Event::Closed => (),
                })
                .unwrap();
            dbus.conn_display_config = try_start(display_config);

            let screen_saver =
                ScreenSaver::new(arbor.is_fdo_idle_inhibited.clone(), idle_wake(arbor));
            dbus.conn_screen_saver = try_start(screen_saver);

            let (to_arbor, from_screenshot) = calloop::channel::channel();
            let (to_screenshot, from_arbor) = async_channel::unbounded();
            arbor
                .event_loop
                .insert_source(from_screenshot, move |event, _, state| match event {
                    calloop::channel::Event::Msg(msg) => {
                        state.on_screen_shot_msg(&to_screenshot, msg)
                    }
                    calloop::channel::Event::Closed => (),
                })
                .unwrap();
            let screenshot = gnome_shell_screenshot::Screenshot::new(to_arbor, from_arbor);
            dbus.conn_screen_shot = try_start(screenshot);

            let (to_arbor, from_introspect) = calloop::channel::channel();
            let (to_introspect, from_arbor) = async_channel::unbounded();
            arbor
                .event_loop
                .insert_source(from_introspect, move |event, _, state| match event {
                    calloop::channel::Event::Msg(msg) => {
                        state.on_introspect_msg(&to_introspect, msg)
                    }
                    calloop::channel::Event::Closed => (),
                })
                .unwrap();
            let introspect = Introspect::new(to_arbor, from_arbor);
            dbus.conn_introspect = try_start(introspect);

            #[cfg(feature = "xdp-gnome-screencast")]
            {
                let (to_arbor, from_screen_cast) = calloop::channel::channel();
                arbor
                    .event_loop
                    .insert_source(from_screen_cast, {
                        move |event, _, state| match event {
                            calloop::channel::Event::Msg(msg) => state.on_screen_cast_msg(msg),
                            calloop::channel::Event::Closed => (),
                        }
                    })
                    .unwrap();
                let screen_cast = ScreenCast::new(backend.ipc_outputs(), to_arbor);
                dbus.conn_screen_cast = try_start(screen_cast);
            }

            dbus.conn_global_shortcuts =
                portal_global_shortcuts::start(&arbor.event_loop, &mut arbor.global_shortcuts);
            // The rest of arbor's portal backend lives on the same connection.
            if let Some(conn) = &dbus.conn_global_shortcuts {
                if let Err(err) = portal_wallpaper::register(conn, &arbor.event_loop) {
                    warn!("error starting the Wallpaper portal: {err:?}");
                }
                let inhibited = arbor.is_portal_idle_inhibited.clone();
                if let Err(err) = portal_inhibit::register(conn, inhibited, idle_wake(arbor)) {
                    warn!("error starting the Inhibit portal: {err:?}");
                }
                #[cfg(feature = "xdp-gnome-screencast")]
                if let Err(err) =
                    portal_casting::register(conn, &arbor.event_loop, &mut arbor.casting)
                {
                    warn!("error starting the ScreenCast/RemoteDesktop portal: {err:?}");
                }
                if let Err(err) =
                    portal_input_capture::register(conn, &arbor.event_loop, &mut arbor.input_capture)
                {
                    warn!("error starting the InputCapture portal: {err:?}");
                }
                if let Err(err) = portal_screenshot::register(conn, &arbor.event_loop) {
                    warn!("error starting the Screenshot portal: {err:?}");
                }
            }

            dbus.conn_kglobalaccel =
                kglobalaccel::start(&arbor.event_loop, &mut arbor.global_shortcuts);

            let keyboard_monitor = KeyboardMonitor::new();
            if let Some(x) = try_start(keyboard_monitor.clone()) {
                dbus.conn_keyboard_monitor = Some(x);
                arbor.a11y_keyboard_monitor = Some(keyboard_monitor);
            }
        }

        let (to_arbor, from_login1) = calloop::channel::channel();
        arbor
            .event_loop
            .insert_source(from_login1, move |event, _, state| match event {
                calloop::channel::Event::Msg(msg) => state.on_login1_msg(msg),
                calloop::channel::Event::Closed => (),
            })
            .unwrap();
        match freedesktop_login1::start(to_arbor) {
            Ok(conn) => {
                dbus.conn_login1 = Some(conn);
            }
            Err(err) => {
                warn!("error starting login1 watcher: {err:?}");
            }
        }

        let (to_arbor, from_locale1) = calloop::channel::channel();
        arbor
            .event_loop
            .insert_source(from_locale1, move |event, _, state| match event {
                calloop::channel::Event::Msg(msg) => state.on_locale1_msg(msg),
                calloop::channel::Event::Closed => (),
            })
            .unwrap();
        match freedesktop_locale1::start(to_arbor) {
            Ok(conn) => {
                dbus.conn_locale1 = Some(conn);
            }
            Err(err) => {
                warn!("error starting locale1 watcher: {err:?}");
            }
        }

        arbor.dbus = Some(dbus);
    }
}

/// A ping that wakes the compositor loop (its refresh then picks up D-Bus idle inhibitors).
fn idle_wake(arbor: &crate::arbor::Arbor) -> Option<calloop::ping::Ping> {
    let (ping, source) = calloop::ping::make_ping().ok()?;
    arbor.event_loop.insert_source(source, |_, _, _| ()).ok()?;
    Some(ping)
}

fn try_start<I: Start>(iface: I) -> Option<Connection> {
    match iface.start() {
        Ok(conn) => Some(conn),
        Err(err) => {
            warn!("error starting {}: {err:?}", I::name());
            None
        }
    }
}
