//! `org.freedesktop.impl.portal.GlobalShortcuts`: the GlobalShortcuts portal backend.
//!
//! xdg-desktop-portal forwards apps' GlobalShortcuts calls here when `arbor-portals.conf` (or
//! NixOS `xdg.portal.config.arbor`) sets `org.freedesktop.impl.portal.GlobalShortcuts=arbor`.
//! The state lives in the compositor (`crate::global_shortcuts`); this side only translates D-Bus
//! calls into messages and compositor events into signals.

use std::collections::HashMap;

use zbus::fdo::{self, RequestNameFlags};
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};
use zbus::{interface, ObjectServer};

use super::Start;
use crate::arbor::State;
use crate::global_shortcuts::{GlobalShortcutsEvent, ShortcutInfo};

const BUS_NAME: &str = "org.freedesktop.impl.portal.desktop.arbor";
const PATH: &str = "/org/freedesktop/portal/desktop";

type Reply = async_channel::Sender<Option<Vec<ShortcutInfo>>>;

pub enum GlobalShortcutsToArbor {
    CreateSession {
        session: String,
        app_id: String,
    },
    CloseSession {
        session: String,
    },
    Bind {
        session: String,
        /// (id, description, preferred trigger)
        shortcuts: Vec<(String, String, Option<String>)>,
        reply: Reply,
    },
    List {
        session: String,
        reply: Reply,
    },
}

#[derive(Clone)]
pub struct GlobalShortcutsPortal {
    to_arbor: calloop::channel::Sender<GlobalShortcutsToArbor>,
    from_arbor: async_channel::Receiver<GlobalShortcutsEvent>,
}

/// `org.freedesktop.impl.portal.Session` exported at each session handle.
struct PortalSession {
    handle: String,
    to_arbor: calloop::channel::Sender<GlobalShortcutsToArbor>,
}

type Results = HashMap<String, OwnedValue>;

fn shortcuts_value(shortcuts: Vec<ShortcutInfo>) -> Vec<(String, HashMap<String, OwnedValue>)> {
    shortcuts
        .into_iter()
        .map(|s| {
            let mut props = HashMap::new();
            props.insert(
                String::from("description"),
                OwnedValue::from(zbus::zvariant::Str::from(s.description)),
            );
            props.insert(
                String::from("trigger_description"),
                OwnedValue::from(zbus::zvariant::Str::from(s.trigger_description)),
            );
            (s.id, props)
        })
        .collect()
}

fn results_with_shortcuts(shortcuts: Vec<ShortcutInfo>) -> fdo::Result<Results> {
    let value = Value::from(shortcuts_value(shortcuts));
    let value = OwnedValue::try_from(value).map_err(|err| fdo::Error::Failed(err.to_string()))?;
    Ok(HashMap::from([(String::from("shortcuts"), value)]))
}

fn str_option(props: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    props
        .get(key)
        .and_then(|v| <&str>::try_from(v).ok())
        .map(str::to_owned)
}

impl GlobalShortcutsPortal {
    async fn ask(
        &self,
        make: impl FnOnce(Reply) -> GlobalShortcutsToArbor,
    ) -> fdo::Result<Vec<ShortcutInfo>> {
        let (tx, rx) = async_channel::bounded(1);
        self.to_arbor
            .send(make(tx))
            .map_err(|_| fdo::Error::Failed(String::from("compositor is gone")))?;
        match rx.recv().await {
            Ok(Some(shortcuts)) => Ok(shortcuts),
            Ok(None) => Err(fdo::Error::InvalidArgs(String::from("unknown session"))),
            Err(_) => Err(fdo::Error::Failed(String::from("compositor did not reply"))),
        }
    }
}

#[interface(name = "org.freedesktop.impl.portal.GlobalShortcuts")]
impl GlobalShortcutsPortal {
    async fn create_session(
        &self,
        #[zbus(object_server)] server: &ObjectServer,
        _handle: OwnedObjectPath,
        session_handle: OwnedObjectPath,
        app_id: String,
        _options: HashMap<String, OwnedValue>,
    ) -> fdo::Result<(u32, Results)> {
        let session = session_handle.as_str().to_owned();
        debug!(session, app_id, "GlobalShortcuts CreateSession");

        let obj = PortalSession {
            handle: session.clone(),
            to_arbor: self.to_arbor.clone(),
        };
        server
            .at(&session_handle, obj)
            .await
            .map_err(|err| fdo::Error::Failed(format!("error exporting session: {err}")))?;

        self.to_arbor
            .send(GlobalShortcutsToArbor::CreateSession { session, app_id })
            .map_err(|_| fdo::Error::Failed(String::from("compositor is gone")))?;

        Ok((0, HashMap::new()))
    }

    async fn bind_shortcuts(
        &self,
        _handle: OwnedObjectPath,
        session_handle: OwnedObjectPath,
        shortcuts: Vec<(String, HashMap<String, OwnedValue>)>,
        _parent_window: String,
        _options: HashMap<String, OwnedValue>,
    ) -> fdo::Result<(u32, Results)> {
        let session = session_handle.as_str().to_owned();
        let shortcuts = shortcuts
            .into_iter()
            .map(|(id, props)| {
                let description = str_option(&props, "description").unwrap_or_default();
                let preferred = str_option(&props, "preferred_trigger");
                (id, description, preferred)
            })
            .collect();
        let result = self
            .ask(|reply| GlobalShortcutsToArbor::Bind {
                session,
                shortcuts,
                reply,
            })
            .await?;
        Ok((0, results_with_shortcuts(result)?))
    }

    async fn list_shortcuts(
        &self,
        _handle: OwnedObjectPath,
        session_handle: OwnedObjectPath,
    ) -> fdo::Result<(u32, Results)> {
        let session = session_handle.as_str().to_owned();
        let result = self
            .ask(|reply| GlobalShortcutsToArbor::List { session, reply })
            .await?;
        Ok((0, results_with_shortcuts(result)?))
    }

    #[zbus(signal)]
    async fn activated(
        emitter: &SignalEmitter<'_>,
        session_handle: ObjectPath<'_>,
        shortcut_id: &str,
        timestamp: u64,
        options: HashMap<&str, Value<'_>>,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn deactivated(
        emitter: &SignalEmitter<'_>,
        session_handle: ObjectPath<'_>,
        shortcut_id: &str,
        timestamp: u64,
        options: HashMap<&str, Value<'_>>,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn shortcuts_changed(
        emitter: &SignalEmitter<'_>,
        session_handle: ObjectPath<'_>,
        shortcuts: Vec<(String, HashMap<String, OwnedValue>)>,
    ) -> zbus::Result<()>;

    #[zbus(property)]
    async fn version(&self) -> u32 {
        1
    }
}

#[interface(name = "org.freedesktop.impl.portal.Session")]
impl PortalSession {
    async fn close(
        &self,
        #[zbus(object_server)] server: &ObjectServer,
        #[zbus(signal_context)] ctxt: SignalEmitter<'_>,
    ) {
        debug!(session = self.handle, "GlobalShortcuts session closed");
        let _ = self.to_arbor.send(GlobalShortcutsToArbor::CloseSession {
            session: self.handle.clone(),
        });
        let _ = server.remove::<PortalSession, _>(ctxt.path()).await;
    }

    #[zbus(signal)]
    async fn closed(ctxt: &SignalEmitter<'_>) -> zbus::Result<()>;

    #[zbus(property, name = "version")]
    async fn version(&self) -> u32 {
        1
    }
}

async fn emit(conn: &zbus::Connection, event: GlobalShortcutsEvent) -> zbus::Result<()> {
    let emitter = SignalEmitter::new(conn, PATH)?;
    let path = |s: &str| ObjectPath::try_from(s.to_owned());
    match event {
        GlobalShortcutsEvent::Activated {
            session,
            id,
            timestamp,
        } => {
            GlobalShortcutsPortal::activated(
                &emitter,
                path(&session)?,
                &id,
                timestamp,
                HashMap::new(),
            )
            .await
        }
        GlobalShortcutsEvent::Deactivated {
            session,
            id,
            timestamp,
        } => {
            GlobalShortcutsPortal::deactivated(
                &emitter,
                path(&session)?,
                &id,
                timestamp,
                HashMap::new(),
            )
            .await
        }
        // Handled in start().
        GlobalShortcutsEvent::Restore { .. } => Ok(()),
        GlobalShortcutsEvent::ShortcutsChanged { session, shortcuts } => {
            GlobalShortcutsPortal::shortcuts_changed(
                &emitter,
                path(&session)?,
                shortcuts_value(shortcuts),
            )
            .await
        }
    }
}

impl Start for GlobalShortcutsPortal {
    fn start(self) -> anyhow::Result<zbus::blocking::Connection> {
        let from_arbor = self.from_arbor.clone();
        let to_arbor = self.to_arbor.clone();

        let conn = zbus::blocking::Connection::session()?;
        let flags = RequestNameFlags::AllowReplacement
            | RequestNameFlags::ReplaceExisting
            | RequestNameFlags::DoNotQueue;

        conn.object_server().at(PATH, self)?;
        conn.request_name_with_flags(BUS_NAME, flags)?;

        // Signals are emitted from the connection's executor so the compositor never blocks on
        // D-Bus.
        let async_conn = conn.inner().clone();
        let future = {
            let conn = async_conn.clone();
            async move {
                while let Ok(event) = from_arbor.recv().await {
                    if let GlobalShortcutsEvent::Restore { session } = &event {
                        let obj = PortalSession {
                            handle: session.clone(),
                            to_arbor: to_arbor.clone(),
                        };
                        let path = match ObjectPath::try_from(session.clone()) {
                            Ok(p) => p,
                            Err(_) => continue,
                        };
                        if let Err(err) = conn.object_server().at(path, obj).await {
                            warn!("error exporting a restored GlobalShortcuts session: {err:?}");
                        }
                        continue;
                    }
                    if let Err(err) = emit(&conn, event).await {
                        warn!("error emitting GlobalShortcuts signal: {err:?}");
                    }
                }
            }
        };
        async_conn
            .executor()
            .spawn(future, "emit GlobalShortcuts signals")
            .detach();

        Ok(conn)
    }
}

/// Starts the portal backend and hooks it up to the compositor.
pub fn start(
    event_loop: &calloop::LoopHandle<'static, State>,
    global_shortcuts: &mut crate::global_shortcuts::GlobalShortcuts,
) -> Option<zbus::blocking::Connection> {
    let (to_arbor, from_portal) = calloop::channel::channel();
    let (to_portal, from_arbor) = async_channel::unbounded();
    global_shortcuts.set_event_sender(to_portal);

    event_loop
        .insert_source(from_portal, move |event, _, state| match event {
            calloop::channel::Event::Msg(msg) => state.on_global_shortcuts_msg(msg),
            calloop::channel::Event::Closed => (),
        })
        .unwrap();

    super::try_start(GlobalShortcutsPortal {
        to_arbor,
        from_arbor,
    })
}

impl State {
    fn on_global_shortcuts_msg(&mut self, msg: GlobalShortcutsToArbor) {
        let gs = &mut self.arbor.global_shortcuts;
        match msg {
            GlobalShortcutsToArbor::CreateSession { session, app_id } => {
                gs.create_session(session, app_id);
            }
            GlobalShortcutsToArbor::CloseSession { session } => gs.close_session(&session),
            GlobalShortcutsToArbor::Bind {
                session,
                shortcuts,
                reply,
            } => {
                let config = self.arbor.config.borrow();
                let mod_key = self.backend.mod_key(&config);
                let result = gs.bind(&session, shortcuts, &config.binds.0, mod_key);
                let _ = reply.try_send(result);
            }
            GlobalShortcutsToArbor::List { session, reply } => {
                let _ = reply.try_send(gs.list(&session));
            }
        }
    }
}
