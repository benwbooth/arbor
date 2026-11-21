//! `org.kde.kglobalaccel`: KDE's global shortcut service, served by the compositor so that
//! KGlobalAccel clients (KDE/Qt apps, KDE's shortcut settings) work outside Plasma.
//!
//! Object `/kglobalaccel` implements `org.kde.KGlobalAccel` and each component gets
//! `/component/<name>` with `org.kde.kglobalaccel.Component` (signals `globalShortcutPressed` /
//! `globalShortcutReleased`). The state is the compositor's app shortcut store (see
//! `crate::kglobalaccel`); every call runs on the compositor thread through a channel.
//!
//! Not supported: shortcut contexts (`component|context` names map to the component), multi-key
//! sequences, and the context-taking overloads of `shortcutNames` / `allShortcutInfos`.

use futures_util::StreamExt;
use zbus::fdo::{self, RequestNameFlags};
use zbus::message::Header;
use zbus::names::UniqueName;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{NoneValue, OwnedObjectPath};
use zbus::{interface, ObjectServer};

use crate::arbor::State;
use crate::global_shortcuts::GlobalShortcuts;
use crate::kglobalaccel::keys::KeySeq;
use crate::kglobalaccel::{ActionId, KdeEvent, ShortcutInfo};

const BUS_NAME: &str = "org.kde.kglobalaccel";
const PATH: &str = "/kglobalaccel";
const COMPONENT_IFACE: &str = "org.kde.kglobalaccel.Component";

type Job = Box<dyn FnOnce(&mut State) + Send>;

/// QKeySequence on the bus: `(ai)` with four ints.
type DKeySeq = (Vec<i32>,);
/// KGlobalShortcutInfo: `(ssssssaiai)`.
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

#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "org.kde.kglobalaccel")]
enum KError {
    #[zbus(error)]
    ZBus(zbus::Error),
    NoSuchComponent(String),
    Failed(String),
}

fn to_seq(d: &DKeySeq) -> KeySeq {
    let mut seq = [0; 4];
    for (i, k) in d.0.iter().take(4).enumerate() {
        seq[i] = *k;
    }
    seq
}

fn from_seq(seq: &KeySeq) -> DKeySeq {
    (seq.to_vec(),)
}

fn from_seqs(seqs: &[KeySeq]) -> Vec<DKeySeq> {
    seqs.iter().map(from_seq).collect()
}

fn first_ints(seqs: &[KeySeq]) -> Vec<i32> {
    seqs.iter().map(|s| s[0]).collect()
}

fn info(i: ShortcutInfo) -> DInfo {
    (
        i.action,
        i.action_friendly,
        i.component,
        i.component_friendly,
        String::from("default"),
        String::from("Default Context"),
        first_ints(&i.keys),
        first_ints(&i.default_keys),
    )
}

/// kglobalacceld's Component::dbusPath.
pub fn component_path(name: &str) -> String {
    let sanitized: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("/component/{sanitized}")
}

/// Runs `f` with the store, the config binds and the mod key.
fn with_gs<T>(
    state: &mut State,
    f: impl FnOnce(&mut GlobalShortcuts, &[arbor_config::binds::Bind], arbor_config::ModKey) -> T,
) -> T {
    let config = state.arbor.config.borrow();
    let mod_key = state.backend.mod_key(&config);
    f(&mut state.arbor.global_shortcuts, &config.binds.0, mod_key)
}

#[derive(Clone)]
struct ToArbor(calloop::channel::Sender<Job>);

impl ToArbor {
    async fn call<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut State) -> T + Send + 'static,
    ) -> Result<T, KError> {
        let (tx, rx) = async_channel::bounded(1);
        self.0
            .send(Box::new(move |state| {
                let _ = tx.try_send(f(state));
            }))
            .map_err(|_| KError::Failed(String::from("compositor is gone")))?;
        rx.recv()
            .await
            .map_err(|_| KError::Failed(String::from("compositor did not reply")))
    }

    async fn gs<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut GlobalShortcuts, &[arbor_config::binds::Bind], arbor_config::ModKey) -> T
            + Send
            + 'static,
    ) -> Result<T, KError> {
        self.call(move |state| with_gs(state, f)).await
    }

    /// Exports component objects that aren't exported yet.
    async fn export(&self, server: &ObjectServer, names: Vec<String>) {
        for name in names {
            let obj = Component {
                to_arbor: self.clone(),
                name: name.clone(),
            };
            if let Err(err) = server.at(component_path(&name), obj).await {
                warn!("error exporting kglobalaccel component {name}: {err:?}");
            }
        }
    }
}

struct KGlobalAccel {
    to_arbor: ToArbor,
}

#[interface(name = "org.kde.KGlobalAccel")]
impl KGlobalAccel {
    #[zbus(name = "allComponents")]
    async fn all_components(
        &self,
        #[zbus(object_server)] server: &ObjectServer,
    ) -> Result<Vec<OwnedObjectPath>, KError> {
        let names: Vec<String> = self
            .to_arbor
            .gs(|gs, _, _| gs.kde_components().into_iter().map(|(n, _)| n).collect())
            .await?;
        self.to_arbor.export(server, names.clone()).await;
        Ok(names
            .iter()
            .filter_map(|n| OwnedObjectPath::try_from(component_path(n)).ok())
            .collect())
    }

    #[zbus(name = "allMainComponents")]
    async fn all_main_components(&self) -> Result<Vec<Vec<String>>, KError> {
        self.to_arbor
            .gs(|gs, _, _| {
                gs.kde_components()
                    .into_iter()
                    .map(|(n, f)| vec![n, f, String::new(), String::new()])
                    .collect()
            })
            .await
    }

    #[zbus(name = "allActionsForComponent")]
    async fn all_actions_for_component(
        &self,
        action_id: Vec<String>,
    ) -> Result<Vec<Vec<String>>, KError> {
        let Some(component) = action_id.first().cloned() else {
            return Ok(Vec::new());
        };
        self.to_arbor
            .gs(move |gs, _, m| gs.kde_actions(&component, m))
            .await
    }

    #[zbus(name = "action")]
    async fn action(&self, key: i32) -> Result<Vec<String>, KError> {
        self.to_arbor
            .gs(move |gs, _, m| gs.kde_action_for_key(&[key, 0, 0, 0], m))
            .await
    }

    #[zbus(name = "actionList")]
    async fn action_list(&self, key: DKeySeq) -> Result<Vec<String>, KError> {
        let seq = to_seq(&key);
        self.to_arbor
            .gs(move |gs, _, m| gs.kde_action_for_key(&seq, m))
            .await
    }

    #[zbus(name = "getComponent")]
    async fn get_component(
        &self,
        #[zbus(object_server)] server: &ObjectServer,
        component_unique: String,
    ) -> Result<OwnedObjectPath, KError> {
        let name = crate::kglobalaccel::component_name(&component_unique).to_owned();
        let n = name.clone();
        let exists = self.to_arbor.gs(move |gs, _, _| gs.kde_has_component(&n)).await?;
        if !exists {
            return Err(KError::NoSuchComponent(format!(
                "The component '{component_unique}' doesn't exist."
            )));
        }
        self.to_arbor.export(server, vec![name.clone()]).await;
        OwnedObjectPath::try_from(component_path(&name)).map_err(|e| KError::Failed(e.to_string()))
    }

    #[zbus(name = "shortcut")]
    async fn shortcut(&self, action_id: ActionId) -> Result<Vec<i32>, KError> {
        let keys = self
            .to_arbor
            .gs(move |gs, _, m| gs.kde_keys(&action_id, m))
            .await?;
        Ok(first_ints(&keys))
    }

    #[zbus(name = "defaultShortcut")]
    async fn default_shortcut(&self, action_id: ActionId) -> Result<Vec<i32>, KError> {
        let keys = self
            .to_arbor
            .gs(move |gs, _, m| gs.kde_default_keys(&action_id, m))
            .await?;
        Ok(first_ints(&keys))
    }

    #[zbus(name = "shortcutKeys")]
    async fn shortcut_keys(&self, action_id: ActionId) -> Result<Vec<DKeySeq>, KError> {
        let keys = self
            .to_arbor
            .gs(move |gs, _, m| gs.kde_keys(&action_id, m))
            .await?;
        Ok(from_seqs(&keys))
    }

    #[zbus(name = "defaultShortcutKeys")]
    async fn default_shortcut_keys(&self, action_id: ActionId) -> Result<Vec<DKeySeq>, KError> {
        let keys = self
            .to_arbor
            .gs(move |gs, _, m| gs.kde_default_keys(&action_id, m))
            .await?;
        Ok(from_seqs(&keys))
    }

    #[zbus(name = "setShortcut")]
    async fn set_shortcut(
        &self,
        action_id: ActionId,
        keys: Vec<i32>,
        flags: u32,
    ) -> Result<Vec<i32>, KError> {
        let keys: Vec<KeySeq> = keys.into_iter().map(|k| [k, 0, 0, 0]).collect();
        let got = self
            .to_arbor
            .gs(move |gs, b, m| gs.kde_set_keys(&action_id, &keys, flags, b, m))
            .await?;
        Ok(first_ints(&got))
    }

    #[zbus(name = "setShortcutKeys")]
    async fn set_shortcut_keys(
        &self,
        action_id: ActionId,
        keys: Vec<DKeySeq>,
        flags: u32,
    ) -> Result<Vec<DKeySeq>, KError> {
        let keys: Vec<KeySeq> = keys.iter().map(to_seq).collect();
        let got = self
            .to_arbor
            .gs(move |gs, b, m| gs.kde_set_keys(&action_id, &keys, flags, b, m))
            .await?;
        Ok(from_seqs(&got))
    }

    #[zbus(name = "setForeignShortcut")]
    async fn set_foreign_shortcut(&self, action_id: ActionId, keys: Vec<i32>) -> Result<(), KError> {
        let keys: Vec<KeySeq> = keys.into_iter().map(|k| [k, 0, 0, 0]).collect();
        self.to_arbor
            .gs(move |gs, b, m| gs.kde_set_foreign(&action_id, &keys, b, m))
            .await
    }

    #[zbus(name = "setForeignShortcutKeys")]
    async fn set_foreign_shortcut_keys(
        &self,
        action_id: ActionId,
        keys: Vec<DKeySeq>,
    ) -> Result<(), KError> {
        let keys: Vec<KeySeq> = keys.iter().map(to_seq).collect();
        self.to_arbor
            .gs(move |gs, b, m| gs.kde_set_foreign(&action_id, &keys, b, m))
            .await
    }

    #[zbus(name = "setInactive")]
    async fn set_inactive(&self, action_id: ActionId) -> Result<(), KError> {
        self.to_arbor
            .gs(move |gs, _, _| gs.kde_set_inactive(&action_id))
            .await
    }

    #[zbus(name = "doRegister")]
    async fn do_register(
        &self,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(object_server)] server: &ObjectServer,
        action_id: ActionId,
    ) -> Result<(), KError> {
        let owner = hdr.sender().map(|s| s.to_string());
        let Some(component) = action_id.first() else {
            return Ok(());
        };
        let component = crate::kglobalaccel::component_name(component).to_owned();
        self.to_arbor
            .gs(move |gs, _, _| gs.kde_register(&action_id, owner))
            .await?;
        self.to_arbor.export(server, vec![component]).await;
        Ok(())
    }

    #[zbus(name = "unRegister")]
    async fn un_register(&self, action_id: ActionId) -> Result<(), KError> {
        if action_id.len() < 2 {
            return Ok(());
        }
        self.to_arbor
            .gs(move |gs, _, _| {
                gs.kde_unregister(&action_id[0], &action_id[1]);
            })
            .await
    }

    #[zbus(name = "unregister")]
    async fn unregister(
        &self,
        component_unique: String,
        shortcut_unique: String,
    ) -> Result<bool, KError> {
        self.to_arbor
            .gs(move |gs, _, _| gs.kde_unregister(&component_unique, &shortcut_unique))
            .await
    }

    #[zbus(name = "activateGlobalShortcutContext")]
    async fn activate_global_shortcut_context(&self, _component: String, _context: String) {}

    #[zbus(name = "getGlobalShortcutsByKey")]
    async fn get_global_shortcuts_by_key(&self, key: i32) -> Result<Vec<DInfo>, KError> {
        let infos = self
            .to_arbor
            .gs(move |gs, _, m| gs.kde_infos_by_key(&[key, 0, 0, 0], m))
            .await?;
        Ok(infos.into_iter().map(info).collect())
    }

    #[zbus(name = "globalShortcutsByKey")]
    async fn global_shortcuts_by_key(
        &self,
        key: DKeySeq,
        _match_type: (i32,),
    ) -> Result<Vec<DInfo>, KError> {
        let seq = to_seq(&key);
        let infos = self
            .to_arbor
            .gs(move |gs, _, m| gs.kde_infos_by_key(&seq, m))
            .await?;
        Ok(infos.into_iter().map(info).collect())
    }

    #[zbus(name = "isGlobalShortcutAvailable")]
    async fn is_global_shortcut_available(
        &self,
        key: i32,
        component: String,
    ) -> Result<bool, KError> {
        self.to_arbor
            .gs(move |gs, b, m| gs.kde_available(&[key, 0, 0, 0], &component, b, m))
            .await
    }

    #[zbus(name = "globalShortcutAvailable")]
    async fn global_shortcut_available(
        &self,
        key: DKeySeq,
        component: String,
    ) -> Result<bool, KError> {
        let seq = to_seq(&key);
        self.to_arbor
            .gs(move |gs, b, m| gs.kde_available(&seq, &component, b, m))
            .await
    }

    #[zbus(name = "blockGlobalShortcuts")]
    async fn block_global_shortcuts(&self, block: bool) -> Result<(), KError> {
        self.to_arbor
            .gs(move |gs, _, _| gs.kde_set_blocked(block))
            .await
    }

    #[zbus(name = "setInverseShortcutActions")]
    async fn set_inverse_shortcut_actions(
        &self,
        _component_unique: String,
        _forward_action_unique: String,
        _backward_action_unique: String,
        _flags: u32,
    ) -> bool {
        false
    }

    #[zbus(signal, name = "yourShortcutGotChanged")]
    async fn your_shortcut_got_changed(
        emitter: &SignalEmitter<'_>,
        action_id: Vec<String>,
        new_keys: Vec<i32>,
    ) -> zbus::Result<()>;

    #[zbus(signal, name = "yourShortcutsChanged")]
    async fn your_shortcuts_changed(
        emitter: &SignalEmitter<'_>,
        action_id: Vec<String>,
        new_keys: Vec<DKeySeq>,
    ) -> zbus::Result<()>;
}

struct Component {
    to_arbor: ToArbor,
    name: String,
}

#[interface(name = "org.kde.kglobalaccel.Component")]
impl Component {
    #[zbus(property, name = "uniqueName")]
    async fn unique_name(&self) -> String {
        self.name.clone()
    }

    #[zbus(property, name = "friendlyName")]
    async fn friendly_name(&self) -> String {
        let name = self.name.clone();
        self.to_arbor
            .gs(move |gs, _, _| {
                gs.kde_components()
                    .into_iter()
                    .find(|(n, _)| *n == name)
                    .map(|(_, f)| f)
            })
            .await
            .ok()
            .flatten()
            .unwrap_or_else(|| self.name.clone())
    }

    #[zbus(name = "cleanUp")]
    async fn clean_up(&self) -> Result<bool, KError> {
        let name = self.name.clone();
        self.to_arbor.gs(move |gs, _, _| gs.kde_clean_up(&name)).await
    }

    #[zbus(name = "isActive")]
    async fn is_active(&self) -> Result<bool, KError> {
        let name = self.name.clone();
        self.to_arbor.gs(move |gs, _, _| gs.kde_is_active(&name)).await
    }

    #[zbus(name = "shortcutNames")]
    async fn shortcut_names(&self) -> Result<Vec<String>, KError> {
        let name = self.name.clone();
        let infos = self
            .to_arbor
            .gs(move |gs, _, m| gs.kde_infos(&name, m))
            .await?;
        Ok(infos.into_iter().map(|i| i.action).collect())
    }

    #[zbus(name = "allShortcutInfos")]
    async fn all_shortcut_infos(&self) -> Result<Vec<DInfo>, KError> {
        let name = self.name.clone();
        let infos = self
            .to_arbor
            .gs(move |gs, _, m| gs.kde_infos(&name, m))
            .await?;
        Ok(infos.into_iter().map(info).collect())
    }

    #[zbus(name = "getShortcutContexts")]
    async fn get_shortcut_contexts(&self) -> Vec<String> {
        vec![String::from("default")]
    }

    #[zbus(name = "invokeShortcut")]
    async fn invoke_shortcut(&self, action_name: String) -> Result<(), KError> {
        let name = self.name.clone();
        self.to_arbor
            .gs(move |gs, _, _| {
                gs.kde_invoke(&name, &action_name, 0);
            })
            .await
    }

    #[zbus(signal, name = "globalShortcutPressed")]
    async fn global_shortcut_pressed(
        emitter: &SignalEmitter<'_>,
        component_unique: &str,
        action_unique: &str,
        timestamp: i64,
    ) -> zbus::Result<()>;

    #[zbus(signal, name = "globalShortcutRepeated")]
    async fn global_shortcut_repeated(
        emitter: &SignalEmitter<'_>,
        component_unique: &str,
        action_unique: &str,
        timestamp: i64,
    ) -> zbus::Result<()>;

    #[zbus(signal, name = "globalShortcutReleased")]
    async fn global_shortcut_released(
        emitter: &SignalEmitter<'_>,
        component_unique: &str,
        action_unique: &str,
        timestamp: i64,
    ) -> zbus::Result<()>;
}

async fn emit(conn: &zbus::Connection, event: KdeEvent) -> zbus::Result<()> {
    match event {
        KdeEvent::Pressed {
            component,
            action,
            timestamp,
        } => {
            conn.emit_signal(
                None::<()>,
                component_path(&component),
                COMPONENT_IFACE,
                "globalShortcutPressed",
                &(component.as_str(), action.as_str(), timestamp),
            )
            .await
        }
        KdeEvent::Released {
            component,
            action,
            timestamp,
        } => {
            conn.emit_signal(
                None::<()>,
                component_path(&component),
                COMPONENT_IFACE,
                "globalShortcutReleased",
                &(component.as_str(), action.as_str(), timestamp),
            )
            .await
        }
        KdeEvent::ShortcutsChanged { action_id, keys } => {
            let emitter = SignalEmitter::new(conn, PATH)?;
            KGlobalAccel::your_shortcuts_changed(&emitter, action_id.clone(), from_seqs(&keys))
                .await?;
            KGlobalAccel::your_shortcut_got_changed(&emitter, action_id, first_ints(&keys)).await
        }
    }
}

/// Apps that leave the bus stop taking key presses.
async fn watch_owners(conn: zbus::Connection, to_arbor: ToArbor) -> anyhow::Result<()> {
    let proxy = fdo::DBusProxy::new(&conn).await?;
    let mut stream = proxy
        .receive_name_owner_changed_with_args(&[(2, UniqueName::null_value())])
        .await?;
    while let Some(signal) = stream.next().await {
        let args = signal.args()?;
        if args.new_owner().is_some() || !args.name().starts_with(':') {
            continue;
        }
        let name = args.name().to_string();
        let _ = to_arbor
            .gs(move |gs, _, _| gs.kde_owner_vanished(&name))
            .await;
    }
    Ok(())
}

/// Starts `org.kde.kglobalaccel` on the session bus and hooks it up to the compositor.
pub fn start(
    event_loop: &calloop::LoopHandle<'static, State>,
    global_shortcuts: &mut GlobalShortcuts,
) -> Option<zbus::blocking::Connection> {
    let (to_arbor, from_dbus) = calloop::channel::channel::<Job>();
    let (to_dbus, from_arbor) = async_channel::unbounded();
    global_shortcuts.set_kde_event_sender(to_dbus);
    // Load the store now (and do the one-off kglobalshortcutsrc import on first start) rather
    // than on the first key press.
    global_shortcuts.store();

    event_loop
        .insert_source(from_dbus, move |event, _, state| match event {
            calloop::channel::Event::Msg(job) => job(state),
            calloop::channel::Event::Closed => (),
        })
        .unwrap();

    match start_service(ToArbor(to_arbor), from_arbor) {
        Ok(conn) => Some(conn),
        Err(err) => {
            warn!("error starting {BUS_NAME}: {err:?}");
            None
        }
    }
}

fn start_service(
    to_arbor: ToArbor,
    from_arbor: async_channel::Receiver<KdeEvent>,
) -> anyhow::Result<zbus::blocking::Connection> {
    let conn = zbus::blocking::Connection::session()?;
    conn.object_server().at(
        PATH,
        KGlobalAccel {
            to_arbor: to_arbor.clone(),
        },
    )?;
    // arbor owns the keyboard: it is the only kglobalaccel there is in its session.
    conn.request_name_with_flags(
        BUS_NAME,
        RequestNameFlags::ReplaceExisting | RequestNameFlags::DoNotQueue,
    )?;

    let async_conn = conn.inner().clone();
    let emitter = {
        let conn = async_conn.clone();
        async move {
            while let Ok(event) = from_arbor.recv().await {
                if let Err(err) = emit(&conn, event).await {
                    warn!("error emitting kglobalaccel signal: {err:?}");
                }
            }
        }
    };
    async_conn
        .executor()
        .spawn(emitter, "emit kglobalaccel signals")
        .detach();

    let watcher = {
        let conn = async_conn.clone();
        async move {
            if let Err(err) = watch_owners(conn, to_arbor).await {
                warn!("error watching kglobalaccel clients: {err:?}");
            }
        }
    };
    async_conn
        .executor()
        .spawn(watcher, "watch kglobalaccel clients")
        .detach();

    Ok(conn)
}
