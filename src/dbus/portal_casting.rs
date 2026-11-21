//! `org.freedesktop.impl.portal.ScreenCast` and `org.freedesktop.impl.portal.RemoteDesktop`:
//! arbor's own screen sharing and remote control portal backends, part of arbor's portal
//! (`org.freedesktop.impl.portal.desktop.arbor`). No xdg-desktop-portal-gnome or Mutter D-Bus
//! round trip: streams come straight from arbor's PipeWire casting and input goes straight into
//! arbor's input pipeline (`crate::remote_input`).
//!
//! The two interfaces share sessions: a RemoteDesktop session can also select screen-cast
//! sources (the portal frontend calls `ScreenCast.SelectSources` on it). `Start` shows an
//! arbor-drawn dialog (`crate::ui::portal_dialog`) to pick the screen or window and to allow
//! remote control; "A window" opens the overview to click one. When the app asks for
//! persistence and the user leaves "Remember" ticked, the choice comes back as restore data, so
//! the next session starts without asking (the frontend keeps the token).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use zbus::object_server::SignalEmitter;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, StructureBuilder, Value};
use zbus::{fdo, interface, ObjectServer};

use crate::arbor::State;
use crate::dbus::mutter_screen_cast::CursorMode;
use crate::layout::LayoutElement as _;
use crate::remote_input::{AbsTarget, RemoteClipboardEvent, RemoteInput, ScrollSource};
use crate::screencasting::{PortalCastTarget, PortalStreamMeta};
use crate::ui::portal_dialog::{DialogAnswer, DialogRequest};
use crate::utils::CastSessionId;

const PATH: &str = "/org/freedesktop/portal/desktop";

const RESPONSE_SUCCESS: u32 = 0;
const RESPONSE_CANCELLED: u32 = 1;
const RESPONSE_OTHER: u32 = 2;

const SOURCE_MONITOR: u32 = 1;
const SOURCE_WINDOW: u32 = 2;

const DEVICE_KEYBOARD: u32 = 1;
const DEVICE_POINTER: u32 = 2;
const DEVICE_TOUCHSCREEN: u32 = 4;
const ALL_DEVICES: u32 = DEVICE_KEYBOARD | DEVICE_POINTER | DEVICE_TOUCHSCREEN;

const CURSOR_HIDDEN: u32 = 1;
const CURSOR_EMBEDDED: u32 = 2;
const CURSOR_METADATA: u32 = 4;

const RESTORE_VENDOR: &str = "arbor";
const RESTORE_VERSION: u32 = 1;

/// How long to wait for PipeWire to set up a stream.
const STREAM_TIMEOUT: Duration = Duration::from_secs(10);

type Job = Box<dyn FnOnce(&mut State) + Send>;
type Results = HashMap<String, OwnedValue>;

/// What a session restores without asking.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RestoreChoice {
    pub output: Option<String>,
    pub window_app_id: Option<String>,
    pub window_title: Option<String>,
    pub devices: u32,
    pub clipboard: bool,
}

#[derive(Debug, Clone, Default)]
struct SourceOptions {
    types: u32,
    cursor_mode: u32,
    persist_mode: u32,
    restore: Option<RestoreChoice>,
}

#[derive(Debug, Clone, Default)]
struct DeviceOptions {
    types: u32,
    persist_mode: u32,
    restore: Option<RestoreChoice>,
}

#[derive(Debug, Clone)]
struct StreamEntry {
    node_id: u32,
    target: AbsTarget,
}

#[derive(Debug)]
struct SessionData {
    /// Remote input source id.
    id: u64,
    app_id: String,
    remote: bool,
    sources: Option<SourceOptions>,
    devices: Option<DeviceOptions>,
    started: bool,
    granted_devices: u32,
    cast_session: Option<CastSessionId>,
    streams: Vec<StreamEntry>,
    clipboard_requested: bool,
    clipboard_enabled: bool,
}

#[derive(Default)]
struct Shared {
    sessions: HashMap<String, SessionData>,
    next_id: u64,
}

#[derive(Clone)]
struct Backend {
    shared: Arc<Mutex<Shared>>,
    to_arbor: calloop::channel::Sender<Job>,
}

pub struct ScreenCastPortal(Backend);
pub struct RemoteDesktopPortal(Backend);

/// `org.freedesktop.impl.portal.Session` at each session handle.
struct CastSession {
    handle: String,
    backend: Backend,
}

// ---------------------------------------------------------------------------------------------
// Restore data: (s vendor, u version, v a{sv}).

fn encode_restore(choice: &RestoreChoice) -> OwnedValue {
    let mut dict: HashMap<String, Value<'static>> = HashMap::new();
    if let Some(o) = &choice.output {
        dict.insert(String::from("output"), Value::from(o.clone()));
    }
    if let Some(a) = &choice.window_app_id {
        dict.insert(String::from("window-app-id"), Value::from(a.clone()));
    }
    if let Some(t) = &choice.window_title {
        dict.insert(String::from("window-title"), Value::from(t.clone()));
    }
    dict.insert(String::from("devices"), Value::from(choice.devices));
    dict.insert(String::from("clipboard"), Value::from(choice.clipboard));
    let structure = StructureBuilder::new()
        .add_field(String::from(RESTORE_VENDOR))
        .add_field(RESTORE_VERSION)
        .append_field(Value::Value(Box::new(Value::from(dict))))
        .build()
        .expect("restore data structure");
    OwnedValue::try_from(Value::Structure(structure)).expect("restore data has no fds")
}

fn decode_restore(value: &Value<'_>) -> Option<RestoreChoice> {
    let Value::Structure(s) = value else {
        return None;
    };
    let [vendor, version, data] = s.fields() else {
        return None;
    };
    if <&str>::try_from(vendor).ok()? != RESTORE_VENDOR
        || u32::try_from(version).ok()? != RESTORE_VERSION
    {
        return None;
    }
    let data = match data {
        Value::Value(inner) => inner.as_ref(),
        other => other,
    };
    let dict = HashMap::<String, OwnedValue>::try_from(data.try_clone().ok()?).ok()?;
    let string = |key: &str| {
        dict.get(key)
            .and_then(|v| <&str>::try_from(v).ok())
            .map(str::to_owned)
    };
    Some(RestoreChoice {
        output: string("output"),
        window_app_id: string("window-app-id"),
        window_title: string("window-title"),
        devices: dict
            .get("devices")
            .and_then(|v| u32::try_from(v).ok())
            .unwrap_or(0),
        clipboard: dict
            .get("clipboard")
            .and_then(|v| bool::try_from(v).ok())
            .unwrap_or(false),
    })
}

fn opt_u32(options: &HashMap<String, OwnedValue>, key: &str) -> Option<u32> {
    options.get(key).and_then(|v| u32::try_from(v).ok())
}

fn opt_bool(options: &HashMap<String, OwnedValue>, key: &str) -> Option<bool> {
    options.get(key).and_then(|v| bool::try_from(v).ok())
}

fn opt_restore(options: &HashMap<String, OwnedValue>) -> Option<RestoreChoice> {
    options.get("restore_data").and_then(|v| decode_restore(v))
}

fn cursor_mode(portal_mode: u32) -> CursorMode {
    match portal_mode {
        CURSOR_EMBEDDED => CursorMode::Embedded,
        CURSOR_METADATA => CursorMode::Metadata,
        _ => CursorMode::Hidden,
    }
}

pub use crate::ui::portal_dialog::app_name;

fn devices_text(devices: u32) -> String {
    let mut parts = Vec::new();
    if devices & DEVICE_KEYBOARD != 0 {
        parts.push("keyboard");
    }
    if devices & DEVICE_POINTER != 0 {
        parts.push("mouse");
    }
    if devices & DEVICE_TOUCHSCREEN != 0 {
        parts.push("touchscreen");
    }
    match parts.len() {
        0 => String::from("keyboard and mouse"),
        1 => parts[0].to_owned(),
        _ => {
            let last = parts.pop().unwrap();
            format!("{} and {last}", parts.join(", "))
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Compositor-side information.

#[derive(Debug, Clone)]
pub struct OutputChoice {
    pub name: String,
    pub description: String,
    pub size: (i32, i32),
}

#[derive(Debug, Clone)]
pub struct WindowChoice {
    pub id: u64,
    pub app_id: String,
    pub title: String,
}

impl State {
    fn portal_outputs(&self) -> Vec<OutputChoice> {
        let mut outputs: Vec<_> = self
            .arbor
            .global_space
            .outputs()
            .filter_map(|output| {
                let geo = self.arbor.global_space.output_geometry(output)?;
                let props = output.physical_properties();
                let model = format!("{} {}", props.make, props.model);
                let model = model.trim();
                let description = if model.is_empty() || model == "Unknown Unknown" {
                    output.name()
                } else {
                    format!("{model} ({})", output.name())
                };
                Some((
                    geo.loc.x,
                    geo.loc.y,
                    OutputChoice {
                        name: output.name(),
                        description,
                        size: (geo.size.w, geo.size.h),
                    },
                ))
            })
            .collect();
        outputs.sort_by_key(|(x, y, _)| (*x, *y));
        outputs.into_iter().map(|(_, _, o)| o).collect()
    }

    fn portal_window_choice(&self, id: u64) -> Option<WindowChoice> {
        self.arbor
            .layout
            .windows()
            .find(|(_, m)| m.id().get() == id)
            .map(|(_, m)| WindowChoice {
                id,
                app_id: m.app_id().unwrap_or_default(),
                title: m.title().unwrap_or_default(),
            })
    }

    /// The window to restore: same app id, preferring the same title.
    fn portal_find_window(&self, app_id: &str, title: Option<&str>) -> Option<WindowChoice> {
        let mut candidates = self
            .arbor
            .layout
            .windows()
            .filter(|(_, m)| m.app_id().as_deref() == Some(app_id))
            .map(|(_, m)| m.id().get())
            .collect::<Vec<_>>();
        candidates.sort();
        let best = candidates
            .iter()
            .copied()
            .find(|&id| {
                title.is_some()
                    && self.portal_window_choice(id).map(|w| w.title).as_deref() == title
            })
            .or_else(|| candidates.first().copied())?;
        self.portal_window_choice(best)
    }

    fn portal_pick_window(&mut self, reply: async_channel::Sender<Option<WindowChoice>>) {
        // Cancel an earlier pick.
        if let Some(tx) = self.arbor.portal_window_pick.take() {
            let _ = tx.try_send(None);
        }
        let (tx, rx) = async_channel::bounded(1);
        self.arbor.portal_window_pick = Some(tx);
        self.arbor.layout.open_overview();
        self.arbor.queue_redraw_all();

        // Resolve the id to window info once picked.
        let (job_tx, job_rx) = calloop::channel::channel::<Option<u64>>();
        let _ = self
            .arbor
            .event_loop
            .insert_source(job_rx, move |event, _, state| {
                if let calloop::channel::Event::Msg(id) = event {
                    let choice = id.and_then(|id| state.portal_window_choice(id));
                    let _ = reply.try_send(choice);
                }
            });
        std::thread::spawn(move || {
            let id = rx.recv_blocking().ok().flatten().map(|id| id.get());
            let _ = job_tx.send(id);
        });
    }
}

// ---------------------------------------------------------------------------------------------
// Shared backend logic.

impl Backend {
    fn run(&self, job: impl FnOnce(&mut State) + Send + 'static) -> fdo::Result<()> {
        self.to_arbor
            .send(Box::new(job))
            .map_err(|_| fdo::Error::Failed(String::from("compositor is gone")))
    }

    async fn ask<T: Send + 'static>(
        &self,
        job: impl FnOnce(&mut State, async_channel::Sender<T>) + Send + 'static,
    ) -> fdo::Result<T> {
        let (tx, rx) = async_channel::bounded(1);
        self.run(move |state| job(state, tx))?;
        rx.recv()
            .await
            .map_err(|_| fdo::Error::Failed(String::from("compositor did not reply")))
    }

    async fn create_session(
        &self,
        server: &ObjectServer,
        session_handle: OwnedObjectPath,
        app_id: String,
        remote: bool,
    ) -> fdo::Result<(u32, Results)> {
        let handle = session_handle.as_str().to_owned();
        {
            let mut shared = self.shared.lock().unwrap();
            shared.next_id += 1;
            let id = shared.next_id;
            shared.sessions.insert(
                handle.clone(),
                SessionData {
                    id,
                    app_id,
                    remote,
                    sources: None,
                    devices: None,
                    started: false,
                    granted_devices: 0,
                    cast_session: None,
                    streams: Vec::new(),
                    clipboard_requested: false,
                    clipboard_enabled: false,
                },
            );
        }
        let obj = CastSession {
            handle: handle.clone(),
            backend: self.clone(),
        };
        if let Err(err) = server.at(&session_handle, obj).await {
            self.shared.lock().unwrap().sessions.remove(&handle);
            return Err(fdo::Error::Failed(format!(
                "error exporting the session: {err}"
            )));
        }
        Ok((RESPONSE_SUCCESS, Results::new()))
    }

    /// Ends a session on arbor's side: stops its casts and releases its input.
    fn end_session(&self, handle: &str) -> Option<SessionData> {
        let data = self.shared.lock().unwrap().sessions.remove(handle)?;
        let cast = data.cast_session;
        let id = data.id;
        let _ = self.run(move |state| {
            if let Some(cast) = cast {
                state.arbor.stop_cast(cast);
            }
            state.close_eis(id);
            state.remote_clipboard_enable(id, false);
            state.remote_input_source_gone(id);
        });
        Some(data)
    }

    async fn start(&self, handle: &str) -> fdo::Result<(u32, Results)> {
        let (app_id, remote, sources, devices, clipboard) = {
            let shared = self.shared.lock().unwrap();
            let Some(data) = shared.sessions.get(handle) else {
                return Err(fdo::Error::InvalidArgs(String::from("unknown session")));
            };
            if data.started {
                return Err(fdo::Error::Failed(String::from("session already started")));
            }
            (
                data.app_id.clone(),
                data.remote,
                data.sources.clone(),
                data.devices.clone(),
                data.remote && data.clipboard_requested,
            )
        };

        let wanted_devices = if remote {
            let types = devices.as_ref().map(|d| d.types).unwrap_or(0);
            if types == 0 {
                ALL_DEVICES
            } else {
                types & ALL_DEVICES
            }
        } else {
            0
        };
        let persist_mode = sources
            .as_ref()
            .map(|s| s.persist_mode)
            .into_iter()
            .chain(devices.as_ref().map(|d| d.persist_mode))
            .max()
            .unwrap_or(0);
        let restore = devices
            .as_ref()
            .and_then(|d| d.restore.clone())
            .or_else(|| sources.as_ref().and_then(|s| s.restore.clone()));

        // 1. What to share: restored, or asked.
        let mut target: Option<PortalCastTarget> = None;
        let mut target_restore = RestoreChoice::default();
        let mut remember = false;
        let mut restored = false;

        if let Some(restore) = &restore {
            restored = self
                .try_restore(
                    restore,
                    &sources,
                    wanted_devices,
                    &mut target,
                    &mut target_restore,
                )
                .await?;
            remember = restored;
        }

        if !restored {
            let outputs = self
                .ask(|state, tx| {
                    let _ = tx.try_send(state.portal_outputs());
                })
                .await?;

            let name = app_name(&app_id);
            let types = sources.as_ref().map(|s| {
                if s.types == 0 {
                    SOURCE_MONITOR
                } else {
                    s.types
                }
            });

            enum Choice {
                Output(String),
                Window,
                Allow,
            }
            let mut choices: Vec<(String, Choice)> = Vec::new();
            let prefix = if remote { "Allow, and share " } else { "" };
            if let Some(types) = types {
                if types & SOURCE_MONITOR != 0 {
                    for o in &outputs {
                        choices.push((
                            format!("{prefix}the entire screen: {}", o.description),
                            Choice::Output(o.name.clone()),
                        ));
                    }
                }
                if types & SOURCE_WINDOW != 0 {
                    choices.push((
                        format!("{prefix}one window (click it in the overview)"),
                        Choice::Window,
                    ));
                }
                if choices.is_empty() {
                    return Ok((RESPONSE_OTHER, Results::new()));
                }
            } else {
                choices.push((String::from("Allow"), Choice::Allow));
            }

            let (title, body) = match (types.is_some(), remote) {
                (true, false) => (
                    format!("{name} wants to share your screen"),
                    format!(
                        "Choose what {name} may see. It sees only what you pick, until it \
                         stops sharing."
                    ),
                ),
                (true, true) => (
                    format!(
                        "{name} wants to see your screen and use your {}{}",
                        devices_text(wanted_devices),
                        if clipboard { " and clipboard" } else { "" }
                    ),
                    format!(
                        "{name} will see what you pick and can type and click as if it were \
                         you, until the session ends."
                    ),
                ),
                (false, _) => (
                    format!(
                        "{name} wants to use your {}{}",
                        devices_text(wanted_devices),
                        if clipboard { " and clipboard" } else { "" }
                    ),
                    format!(
                        "{name} will be able to type and click as if it were you, until the \
                         session ends."
                    ),
                ),
            };
            let remember_label =
                (persist_mode > 0).then(|| (format!("Remember, and don't ask {name} again"), true));

            let labels = choices.iter().map(|(l, _)| l.clone()).collect();
            let answer = self
                .ask(move |state, tx| {
                    let (dialog_tx, dialog_rx) = async_channel::bounded(1);
                    state.arbor.portal_dialog.show(DialogRequest {
                        title,
                        body,
                        choices: labels,
                        remember: remember_label,
                        reply: dialog_tx,
                    });
                    state.arbor.queue_redraw_all();
                    // Hand the dialog's answer channel back.
                    let _ = tx.try_send(dialog_rx);
                })
                .await?
                .recv()
                .await
                .unwrap_or(DialogAnswer::Cancelled);

            let (index, remember_ticked) = match answer {
                DialogAnswer::Chosen { index, remember } => (index, remember),
                DialogAnswer::Cancelled => return Ok((RESPONSE_CANCELLED, Results::new())),
            };
            remember = remember_ticked;

            match &choices[index].1 {
                Choice::Output(name) => {
                    target = Some(PortalCastTarget::Output(name.clone()));
                    target_restore.output = Some(name.clone());
                }
                Choice::Window => {
                    let picked = self.ask(|state, tx| state.portal_pick_window(tx)).await?;
                    let Some(window) = picked else {
                        return Ok((RESPONSE_CANCELLED, Results::new()));
                    };
                    target = Some(PortalCastTarget::Window(window.id));
                    target_restore.window_app_id = Some(window.app_id);
                    target_restore.window_title = Some(window.title);
                }
                Choice::Allow => (),
            }
        }

        // 2. Start the stream.
        let mut streams = Vec::new();
        let mut cast_session = None;
        if let Some(target) = target {
            let session_id = CastSessionId::next();
            let mode = cursor_mode(sources.as_ref().map(|s| s.cursor_mode).unwrap_or(0));
            let (ready_tx, ready_rx) = async_channel::bounded(1);
            let target_ = target.clone();
            let meta: Result<PortalStreamMeta, String> = self
                .ask(move |state, tx| {
                    let res = state
                        .start_portal_cast(session_id, &target_, mode, ready_tx)
                        .map_err(|err| format!("{err:#}"));
                    let _ = tx.try_send(res);
                })
                .await?;
            let meta = match meta {
                Ok(meta) => meta,
                Err(err) => {
                    warn!("error starting a portal screencast: {err}");
                    return Ok((RESPONSE_OTHER, Results::new()));
                }
            };
            cast_session = Some(session_id);
            let node = async { futures_or_timeout(ready_rx.recv(), STREAM_TIMEOUT).await }.await;
            let Some(node_id) = node else {
                warn!("portal screencast stream did not start");
                let _ = self.run(move |state| state.arbor.stop_cast(session_id));
                return Ok((RESPONSE_OTHER, Results::new()));
            };
            let abs = match &target {
                PortalCastTarget::Output(name) => AbsTarget::Output(name.clone()),
                PortalCastTarget::Window(id) => AbsTarget::Window(*id),
            };
            streams.push((node_id, meta, abs));
        }

        // 3. Results.
        let mut results = Results::new();
        if sources.is_some() {
            let value: Vec<(u32, HashMap<String, OwnedValue>)> = streams
                .iter()
                .map(|(node, meta, _)| {
                    let mut props = HashMap::new();
                    props.insert(String::from("position"), owned(Value::from(meta.position)));
                    props.insert(String::from("size"), owned(Value::from(meta.size)));
                    props.insert(
                        String::from("source_type"),
                        OwnedValue::from(meta.source_type),
                    );
                    (*node, props)
                })
                .collect();
            results.insert(String::from("streams"), owned(Value::from(value)));
        }
        if remote {
            results.insert(String::from("devices"), OwnedValue::from(wanted_devices));
            results.insert(
                String::from("clipboard_enabled"),
                OwnedValue::from(clipboard),
            );
        }
        if remember && persist_mode > 0 {
            target_restore.devices = wanted_devices;
            target_restore.clipboard = clipboard;
            results.insert(
                String::from("restore_data"),
                encode_restore(&target_restore),
            );
            results.insert(String::from("persist_mode"), OwnedValue::from(persist_mode));
        }

        let mut shared = self.shared.lock().unwrap();
        let Some(data) = shared.sessions.get_mut(handle) else {
            // Closed while we were asking.
            drop(shared);
            if let Some(cast) = cast_session {
                let _ = self.run(move |state| state.arbor.stop_cast(cast));
            }
            return Ok((RESPONSE_CANCELLED, Results::new()));
        };
        data.started = true;
        data.granted_devices = wanted_devices;
        data.clipboard_enabled = clipboard;
        if clipboard {
            let id = data.id;
            let _ = self.run(move |state| state.remote_clipboard_enable(id, true));
        }
        data.cast_session = cast_session;
        data.streams = streams
            .into_iter()
            .map(|(node_id, _, target)| StreamEntry { node_id, target })
            .collect();
        Ok((RESPONSE_SUCCESS, results))
    }

    /// Fills `target` from restore data. Returns false if the user must be asked.
    async fn try_restore(
        &self,
        restore: &RestoreChoice,
        sources: &Option<SourceOptions>,
        wanted_devices: u32,
        target: &mut Option<PortalCastTarget>,
        target_restore: &mut RestoreChoice,
    ) -> fdo::Result<bool> {
        // More devices than were allowed before need asking again.
        if wanted_devices & !restore.devices != 0 {
            return Ok(false);
        }
        if sources.is_none() {
            return Ok(true);
        }
        if let Some(output) = &restore.output {
            let outputs = self
                .ask(|state, tx| {
                    let _ = tx.try_send(state.portal_outputs());
                })
                .await?;
            if outputs.iter().any(|o| o.name == *output) {
                *target = Some(PortalCastTarget::Output(output.clone()));
                target_restore.output = Some(output.clone());
                return Ok(true);
            }
            return Ok(false);
        }
        if let Some(app_id) = restore.window_app_id.clone() {
            let title = restore.window_title.clone();
            let window = self
                .ask(move |state, tx| {
                    let _ = tx.try_send(state.portal_find_window(&app_id, title.as_deref()));
                })
                .await?;
            if let Some(window) = window {
                *target = Some(PortalCastTarget::Window(window.id));
                target_restore.window_app_id = Some(window.app_id);
                target_restore.window_title = Some(window.title);
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Validates an input call and injects it.
    fn input(
        &self,
        handle: &str,
        device: u32,
        event: impl FnOnce(&SessionData) -> Option<RemoteInput>,
    ) -> fdo::Result<()> {
        let (id, event) = {
            let shared = self.shared.lock().unwrap();
            let Some(data) = shared.sessions.get(handle) else {
                return Err(fdo::Error::InvalidArgs(String::from("unknown session")));
            };
            if !data.remote || !data.started {
                return Err(fdo::Error::Failed(String::from("session not started")));
            }
            if data.granted_devices & device == 0 {
                return Err(fdo::Error::AccessDenied(String::from("device not allowed")));
            }
            let Some(event) = event(data) else {
                return Err(fdo::Error::InvalidArgs(String::from("unknown stream")));
            };
            (data.id, event)
        };
        self.run(move |state| state.remote_input(id, event))
    }
}

fn stream_target(data: &SessionData, node_id: u32) -> Option<AbsTarget> {
    data.streams
        .iter()
        .find(|s| s.node_id == node_id)
        .map(|s| s.target.clone())
}

fn owned(value: Value<'_>) -> OwnedValue {
    OwnedValue::try_from(value).expect("value has no fds")
}

/// Waits for `fut` at most `timeout`.
async fn futures_or_timeout<T>(
    fut: impl std::future::Future<Output = Result<T, async_channel::RecvError>>,
    timeout: Duration,
) -> Option<T> {
    use futures_util::future::{select, Either};
    let timer = async_io::Timer::after(timeout);
    let fut = std::pin::pin!(fut);
    match select(fut, timer).await {
        Either::Left((res, _)) => res.ok(),
        Either::Right(_) => None,
    }
}

// ---------------------------------------------------------------------------------------------
// D-Bus interfaces.

#[interface(name = "org.freedesktop.impl.portal.ScreenCast")]
impl ScreenCastPortal {
    async fn create_session(
        &self,
        #[zbus(object_server)] server: &ObjectServer,
        _handle: OwnedObjectPath,
        session_handle: OwnedObjectPath,
        app_id: String,
        _options: HashMap<String, OwnedValue>,
    ) -> fdo::Result<(u32, Results)> {
        debug!(?session_handle, app_id, "ScreenCast CreateSession");
        self.0
            .create_session(server, session_handle, app_id, false)
            .await
    }

    async fn select_sources(
        &self,
        _handle: OwnedObjectPath,
        session_handle: OwnedObjectPath,
        _app_id: String,
        options: HashMap<String, OwnedValue>,
    ) -> fdo::Result<(u32, Results)> {
        let mut shared = self.0.shared.lock().unwrap();
        let Some(data) = shared.sessions.get_mut(session_handle.as_str()) else {
            return Err(fdo::Error::InvalidArgs(String::from("unknown session")));
        };
        if data.started {
            return Err(fdo::Error::Failed(String::from("session already started")));
        }
        if opt_bool(&options, "multiple") == Some(true) {
            debug!("ScreenCast: multiple sources requested; arbor shares one");
        }
        data.sources = Some(SourceOptions {
            types: opt_u32(&options, "types").unwrap_or(SOURCE_MONITOR),
            cursor_mode: opt_u32(&options, "cursor_mode").unwrap_or(CURSOR_HIDDEN),
            persist_mode: opt_u32(&options, "persist_mode").unwrap_or(0),
            restore: opt_restore(&options),
        });
        Ok((RESPONSE_SUCCESS, Results::new()))
    }

    async fn start(
        &self,
        _handle: OwnedObjectPath,
        session_handle: OwnedObjectPath,
        _app_id: String,
        _parent_window: String,
        _options: HashMap<String, OwnedValue>,
    ) -> fdo::Result<(u32, Results)> {
        self.0.start(session_handle.as_str()).await
    }

    #[zbus(property)]
    fn available_source_types(&self) -> u32 {
        SOURCE_MONITOR | SOURCE_WINDOW
    }

    #[zbus(property)]
    fn available_cursor_modes(&self) -> u32 {
        CURSOR_HIDDEN | CURSOR_EMBEDDED | CURSOR_METADATA
    }

    #[zbus(property)]
    fn version(&self) -> u32 {
        5
    }
}

#[interface(name = "org.freedesktop.impl.portal.RemoteDesktop")]
impl RemoteDesktopPortal {
    async fn create_session(
        &self,
        #[zbus(object_server)] server: &ObjectServer,
        _handle: OwnedObjectPath,
        session_handle: OwnedObjectPath,
        app_id: String,
        _options: HashMap<String, OwnedValue>,
    ) -> fdo::Result<(u32, Results)> {
        debug!(?session_handle, app_id, "RemoteDesktop CreateSession");
        self.0
            .create_session(server, session_handle, app_id, true)
            .await
    }

    async fn select_devices(
        &self,
        _handle: OwnedObjectPath,
        session_handle: OwnedObjectPath,
        _app_id: String,
        options: HashMap<String, OwnedValue>,
    ) -> fdo::Result<(u32, Results)> {
        let mut shared = self.0.shared.lock().unwrap();
        let Some(data) = shared.sessions.get_mut(session_handle.as_str()) else {
            return Err(fdo::Error::InvalidArgs(String::from("unknown session")));
        };
        if data.started {
            return Err(fdo::Error::Failed(String::from("session already started")));
        }
        data.devices = Some(DeviceOptions {
            types: opt_u32(&options, "types").unwrap_or(ALL_DEVICES),
            persist_mode: opt_u32(&options, "persist_mode").unwrap_or(0),
            restore: opt_restore(&options),
        });
        Ok((RESPONSE_SUCCESS, Results::new()))
    }

    async fn start(
        &self,
        _handle: OwnedObjectPath,
        session_handle: OwnedObjectPath,
        _app_id: String,
        _parent_window: String,
        _options: HashMap<String, OwnedValue>,
    ) -> fdo::Result<(u32, Results)> {
        self.0.start(session_handle.as_str()).await
    }

    fn notify_pointer_motion(
        &self,
        session_handle: OwnedObjectPath,
        _options: HashMap<String, OwnedValue>,
        dx: f64,
        dy: f64,
    ) -> fdo::Result<()> {
        self.0.input(session_handle.as_str(), DEVICE_POINTER, |_| {
            Some(RemoteInput::PointerMotion { dx, dy })
        })
    }

    fn notify_pointer_motion_absolute(
        &self,
        session_handle: OwnedObjectPath,
        _options: HashMap<String, OwnedValue>,
        stream: u32,
        x: f64,
        y: f64,
    ) -> fdo::Result<()> {
        self.0
            .input(session_handle.as_str(), DEVICE_POINTER, |data| {
                Some(RemoteInput::PointerMotionAbsolute {
                    target: stream_target(data, stream)?,
                    x,
                    y,
                })
            })
    }

    fn notify_pointer_button(
        &self,
        session_handle: OwnedObjectPath,
        _options: HashMap<String, OwnedValue>,
        button: i32,
        state: u32,
    ) -> fdo::Result<()> {
        let button = u32::try_from(button)
            .map_err(|_| fdo::Error::InvalidArgs(String::from("bad button")))?;
        self.0.input(session_handle.as_str(), DEVICE_POINTER, |_| {
            Some(RemoteInput::PointerButton {
                button,
                pressed: state != 0,
            })
        })
    }

    fn notify_pointer_axis(
        &self,
        session_handle: OwnedObjectPath,
        options: HashMap<String, OwnedValue>,
        dx: f64,
        dy: f64,
    ) -> fdo::Result<()> {
        let finish = opt_bool(&options, "finish").unwrap_or(false);
        self.0.input(session_handle.as_str(), DEVICE_POINTER, |_| {
            Some(RemoteInput::PointerAxis {
                dx,
                dy,
                source: if finish {
                    ScrollSource::Finger
                } else {
                    ScrollSource::Continuous
                },
                finish,
            })
        })
    }

    fn notify_pointer_axis_discrete(
        &self,
        session_handle: OwnedObjectPath,
        _options: HashMap<String, OwnedValue>,
        axis: u32,
        steps: i32,
    ) -> fdo::Result<()> {
        self.0.input(session_handle.as_str(), DEVICE_POINTER, |_| {
            Some(RemoteInput::PointerAxisDiscrete {
                horizontal: axis == 1,
                steps,
            })
        })
    }

    fn notify_keyboard_keycode(
        &self,
        session_handle: OwnedObjectPath,
        _options: HashMap<String, OwnedValue>,
        keycode: i32,
        state: u32,
    ) -> fdo::Result<()> {
        let keycode = u32::try_from(keycode)
            .map_err(|_| fdo::Error::InvalidArgs(String::from("bad keycode")))?;
        self.0.input(session_handle.as_str(), DEVICE_KEYBOARD, |_| {
            Some(RemoteInput::Key {
                keycode,
                pressed: state != 0,
            })
        })
    }

    fn notify_keyboard_keysym(
        &self,
        session_handle: OwnedObjectPath,
        _options: HashMap<String, OwnedValue>,
        keysym: i32,
        state: u32,
    ) -> fdo::Result<()> {
        self.0.input(session_handle.as_str(), DEVICE_KEYBOARD, |_| {
            Some(RemoteInput::Keysym {
                keysym: keysym as u32,
                pressed: state != 0,
            })
        })
    }

    fn notify_touch_down(
        &self,
        session_handle: OwnedObjectPath,
        _options: HashMap<String, OwnedValue>,
        stream: u32,
        slot: u32,
        x: f64,
        y: f64,
    ) -> fdo::Result<()> {
        self.0
            .input(session_handle.as_str(), DEVICE_TOUCHSCREEN, |data| {
                Some(RemoteInput::TouchDown {
                    target: stream_target(data, stream)?,
                    slot,
                    x,
                    y,
                })
            })
    }

    fn notify_touch_motion(
        &self,
        session_handle: OwnedObjectPath,
        _options: HashMap<String, OwnedValue>,
        stream: u32,
        slot: u32,
        x: f64,
        y: f64,
    ) -> fdo::Result<()> {
        self.0
            .input(session_handle.as_str(), DEVICE_TOUCHSCREEN, |data| {
                Some(RemoteInput::TouchMotion {
                    target: stream_target(data, stream)?,
                    slot,
                    x,
                    y,
                })
            })
    }

    fn notify_touch_up(
        &self,
        session_handle: OwnedObjectPath,
        _options: HashMap<String, OwnedValue>,
        slot: u32,
    ) -> fdo::Result<()> {
        self.0
            .input(session_handle.as_str(), DEVICE_TOUCHSCREEN, |_| {
                Some(RemoteInput::TouchUp { slot })
            })
    }

    async fn connect_to_e_i_s(
        &self,
        session_handle: OwnedObjectPath,
        _app_id: String,
        _options: HashMap<String, OwnedValue>,
    ) -> fdo::Result<zbus::zvariant::OwnedFd> {
        let (id, devices) = {
            let shared = self.0.shared.lock().unwrap();
            let Some(data) = shared.sessions.get(session_handle.as_str()) else {
                return Err(fdo::Error::InvalidArgs(String::from("unknown session")));
            };
            if !data.remote || !data.started {
                return Err(fdo::Error::Failed(String::from("session not started")));
            }
            (data.id, data.granted_devices)
        };
        let fd = self
            .0
            .ask(move |state, tx| {
                let _ = tx.try_send(
                    state
                        .start_eis_sender(id, devices)
                        .map_err(|err| err.to_string()),
                );
            })
            .await?
            .map_err(fdo::Error::Failed)?;
        Ok(zbus::zvariant::OwnedFd::from(fd))
    }

    #[zbus(property)]
    fn available_device_types(&self) -> u32 {
        ALL_DEVICES
    }

    #[zbus(property)]
    fn version(&self) -> u32 {
        2
    }
}

pub struct ClipboardPortal(Backend);

impl Backend {
    /// The input source id of a started remote-desktop session with the clipboard enabled.
    fn clipboard_source(&self, handle: &str) -> fdo::Result<u64> {
        let shared = self.shared.lock().unwrap();
        let data = shared
            .sessions
            .get(handle)
            .ok_or_else(|| fdo::Error::InvalidArgs(String::from("unknown session")))?;
        if !data.clipboard_enabled {
            return Err(fdo::Error::AccessDenied(String::from(
                "clipboard not enabled",
            )));
        }
        Ok(data.id)
    }
}

#[interface(name = "org.freedesktop.impl.portal.Clipboard")]
impl ClipboardPortal {
    fn request_clipboard(
        &self,
        session_handle: OwnedObjectPath,
        _options: HashMap<String, OwnedValue>,
    ) -> fdo::Result<()> {
        let mut shared = self.0.shared.lock().unwrap();
        let data = shared
            .sessions
            .get_mut(session_handle.as_str())
            .ok_or_else(|| fdo::Error::InvalidArgs(String::from("unknown session")))?;
        if data.started {
            return Err(fdo::Error::Failed(String::from("session already started")));
        }
        data.clipboard_requested = true;
        Ok(())
    }

    fn set_selection(
        &self,
        session_handle: OwnedObjectPath,
        options: HashMap<String, OwnedValue>,
    ) -> fdo::Result<()> {
        let id = self.0.clipboard_source(session_handle.as_str())?;
        let mime_types: Vec<String> = options
            .get("mime_types")
            .and_then(|v| Vec::<String>::try_from(v.try_clone().ok()?).ok())
            .unwrap_or_default();
        self.0
            .run(move |state| state.remote_clipboard_set(id, mime_types))
    }

    async fn selection_write(
        &self,
        session_handle: OwnedObjectPath,
        serial: u32,
    ) -> fdo::Result<zbus::zvariant::OwnedFd> {
        let id = self.0.clipboard_source(session_handle.as_str())?;
        let fd = self
            .0
            .ask(move |state, tx| {
                let _ = tx.try_send(state.remote_clipboard_take_transfer(id, serial));
            })
            .await?
            .ok_or_else(|| fdo::Error::InvalidArgs(String::from("unknown transfer")))?;
        Ok(zbus::zvariant::OwnedFd::from(fd))
    }

    fn selection_write_done(
        &self,
        session_handle: OwnedObjectPath,
        _serial: u32,
        _success: bool,
    ) -> fdo::Result<()> {
        // The paste target's fd was handed over in SelectionWrite; closing it ends the paste.
        self.0.clipboard_source(session_handle.as_str()).map(|_| ())
    }

    async fn selection_read(
        &self,
        session_handle: OwnedObjectPath,
        mime_type: String,
    ) -> fdo::Result<zbus::zvariant::OwnedFd> {
        let id = self.0.clipboard_source(session_handle.as_str())?;
        let fd = self
            .0
            .ask(move |state, tx| {
                let _ = tx.try_send(state.remote_clipboard_read(id, mime_type));
            })
            .await?
            .map_err(fdo::Error::Failed)?;
        Ok(zbus::zvariant::OwnedFd::from(fd))
    }

    #[zbus(signal)]
    async fn selection_owner_changed(
        ctxt: &SignalEmitter<'_>,
        session_handle: OwnedObjectPath,
        options: HashMap<String, OwnedValue>,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn selection_transfer(
        ctxt: &SignalEmitter<'_>,
        session_handle: OwnedObjectPath,
        mime_type: String,
        serial: u32,
    ) -> zbus::Result<()>;

    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        1
    }
}

#[interface(name = "org.freedesktop.impl.portal.Session")]
impl CastSession {
    async fn close(
        &self,
        #[zbus(object_server)] server: &ObjectServer,
        #[zbus(signal_context)] ctxt: SignalEmitter<'_>,
    ) {
        debug!(
            handle = self.handle,
            "portal cast session closed by the frontend"
        );
        self.backend.end_session(&self.handle);
        let _ = server.remove::<CastSession, _>(ctxt.path()).await;
    }

    #[zbus(signal)]
    async fn closed(ctxt: &SignalEmitter<'_>) -> zbus::Result<()>;

    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        1
    }
}

/// Adds the ScreenCast and RemoteDesktop interfaces to arbor's portal backend connection.
pub fn register(
    conn: &zbus::blocking::Connection,
    event_loop: &calloop::LoopHandle<'static, State>,
    casting: &mut crate::screencasting::Screencasting,
    remote_clipboard: &mut crate::remote_input::RemoteClipboard,
) -> anyhow::Result<()> {
    let (to_arbor, from_dbus) = calloop::channel::channel::<Job>();
    event_loop
        .insert_source(from_dbus, move |event, _, state| match event {
            calloop::channel::Event::Msg(job) => job(state),
            calloop::channel::Event::Closed => (),
        })
        .map_err(|err| anyhow::anyhow!("{err}"))?;

    let backend = Backend {
        shared: Arc::new(Mutex::new(Shared::default())),
        to_arbor,
    };

    // Casts that stop on arbor's side (window closed, screen unplugged, PipeWire error) close
    // their portal session.
    let (stopped_tx, stopped_rx) = async_channel::unbounded::<CastSessionId>();
    casting.portal_cast_stopped = Some(stopped_tx);
    let async_conn = conn.inner().clone();
    let backend_ = backend.clone();
    let future = async move {
        while let Ok(cast) = stopped_rx.recv().await {
            let handle = {
                let shared = backend_.shared.lock().unwrap();
                shared
                    .sessions
                    .iter()
                    .find(|(_, d)| d.cast_session == Some(cast))
                    .map(|(h, _)| h.clone())
            };
            let Some(handle) = handle else {
                continue;
            };
            backend_.end_session(&handle);
            let Ok(path) = OwnedObjectPath::try_from(handle.clone()) else {
                continue;
            };
            if let Ok(emitter) = SignalEmitter::new(&async_conn, path.clone()) {
                let _ = CastSession::closed(&emitter).await;
            }
            let _ = async_conn
                .object_server()
                .remove::<CastSession, _>(&path)
                .await;
        }
    };
    conn.inner()
        .executor()
        .spawn(future, "close stopped portal casts")
        .detach();

    // Clipboard events from arbor become signals on the session's behalf.
    let (clip_tx, clip_rx) = async_channel::unbounded::<RemoteClipboardEvent>();
    remote_clipboard.events = Some(clip_tx);
    let async_conn = conn.inner().clone();
    let backend_ = backend.clone();
    let future = async move {
        while let Ok(event) = clip_rx.recv().await {
            let source = match &event {
                RemoteClipboardEvent::OwnerChanged { source, .. }
                | RemoteClipboardEvent::Transfer { source, .. } => *source,
            };
            let handle = {
                let shared = backend_.shared.lock().unwrap();
                shared
                    .sessions
                    .iter()
                    .find(|(_, d)| d.id == source)
                    .map(|(h, _)| h.clone())
            };
            let Some(Ok(path)) = handle.map(OwnedObjectPath::try_from) else {
                continue;
            };
            let Ok(emitter) = SignalEmitter::new(&async_conn, PATH) else {
                continue;
            };
            let res = match event {
                RemoteClipboardEvent::OwnerChanged {
                    mime_types,
                    is_owner,
                    ..
                } => {
                    let options = HashMap::from([
                        (String::from("mime_types"), owned(Value::from(mime_types))),
                        (String::from("session_is_owner"), OwnedValue::from(is_owner)),
                    ]);
                    ClipboardPortal::selection_owner_changed(&emitter, path, options).await
                }
                RemoteClipboardEvent::Transfer {
                    mime_type, serial, ..
                } => ClipboardPortal::selection_transfer(&emitter, path, mime_type, serial).await,
            };
            if let Err(err) = res {
                warn!("error emitting a Clipboard signal: {err:?}");
            }
        }
    };
    conn.inner()
        .executor()
        .spawn(future, "emit Clipboard signals")
        .detach();

    conn.object_server()
        .at(PATH, ClipboardPortal(backend.clone()))?;
    conn.object_server()
        .at(PATH, ScreenCastPortal(backend.clone()))?;
    conn.object_server()
        .at(PATH, RemoteDesktopPortal(backend))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restore_data_round_trips() {
        let choice = RestoreChoice {
            output: Some(String::from("DP-1")),
            window_app_id: None,
            window_title: None,
            devices: DEVICE_KEYBOARD,
            clipboard: true,
        };
        let value = encode_restore(&choice);
        assert_eq!(value.value_signature().to_string(), "(suv)");
        assert_eq!(decode_restore(&value), Some(choice));

        let window = RestoreChoice {
            output: None,
            window_app_id: Some(String::from("org.example.App")),
            window_title: Some(String::from("Doc — Example")),
            devices: 0,
            clipboard: false,
        };
        assert_eq!(decode_restore(&encode_restore(&window)), Some(window));
    }

    #[test]
    fn foreign_restore_data_is_ignored() {
        let structure = StructureBuilder::new()
            .add_field(String::from("GNOME"))
            .add_field(1u32)
            .append_field(Value::Value(Box::new(Value::from(0u32))))
            .build()
            .unwrap();
        assert_eq!(decode_restore(&Value::Structure(structure)), None);
    }

    #[test]
    fn readable_names() {
        assert_eq!(app_name(""), "An app");
        assert_eq!(app_name("org.mozilla.firefox"), "Firefox");
        assert_eq!(app_name("zoom"), "Zoom");
        assert_eq!(devices_text(DEVICE_KEYBOARD), "keyboard");
        assert_eq!(devices_text(ALL_DEVICES), "keyboard, mouse and touchscreen");
    }
}
