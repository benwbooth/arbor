//! `org.freedesktop.impl.portal.InputCapture`: arbor's InputCapture portal backend, part of
//! arbor's portal (`org.freedesktop.impl.portal.desktop.arbor`). Used by keyboard-and-mouse
//! sharing servers (Deskflow, Input Leap). See `crate::input_capture`.
//!
//! CreateSession asks the user (arbor-drawn dialog). Zones are the screens in global logical
//! coordinates; barriers must lie on their outer edges.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use zbus::object_server::SignalEmitter;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};
use zbus::{fdo, interface, ObjectServer};

use crate::arbor::State;
use crate::input_capture::{barrier_is_valid, Barrier, CaptureEvent, CAP_KEYBOARD, CAP_POINTER};
use crate::ui::portal_dialog::{app_name, DialogAnswer, DialogRequest};

const PATH: &str = "/org/freedesktop/portal/desktop";
const RESPONSE_SUCCESS: u32 = 0;
const RESPONSE_CANCELLED: u32 = 1;

type Job = Box<dyn FnOnce(&mut State) + Send>;
type Results = HashMap<String, OwnedValue>;

#[derive(Clone)]
struct Backend {
    to_arbor: calloop::channel::Sender<Job>,
    /// Session handle → id.
    sessions: Arc<Mutex<(HashMap<String, u64>, u64)>>,
}

pub struct InputCapturePortal(Backend);

struct CaptureSessionObject {
    handle: String,
    backend: Backend,
}

fn owned(value: Value<'_>) -> OwnedValue {
    OwnedValue::try_from(value).expect("value has no fds")
}

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

    fn id(&self, handle: &str) -> fdo::Result<u64> {
        self.sessions
            .lock()
            .unwrap()
            .0
            .get(handle)
            .copied()
            .ok_or_else(|| fdo::Error::InvalidArgs(String::from("unknown session")))
    }

    async fn zones(&self) -> fdo::Result<Vec<(u32, u32, i32, i32)>> {
        self.ask(|state, tx| {
            let zones = state
                .input_capture_zones()
                .into_iter()
                .map(|z| (z.size.w as u32, z.size.h as u32, z.loc.x, z.loc.y))
                .collect();
            let _ = tx.try_send(zones);
        })
        .await
    }
}

/// A zone set id that changes when the screens change.
fn zone_set(zones: &[(u32, u32, i32, i32)]) -> u32 {
    let mut h: u32 = 2166136261;
    for &(w, hh, x, y) in zones {
        for v in [w, hh, x as u32, y as u32] {
            h = (h ^ v).wrapping_mul(16777619);
        }
    }
    h
}

#[interface(name = "org.freedesktop.impl.portal.InputCapture")]
impl InputCapturePortal {
    #[allow(clippy::too_many_arguments)]
    async fn create_session(
        &self,
        #[zbus(object_server)] server: &ObjectServer,
        _handle: OwnedObjectPath,
        session_handle: OwnedObjectPath,
        app_id: String,
        _parent_window: String,
        options: HashMap<String, OwnedValue>,
    ) -> fdo::Result<(u32, Results)> {
        let wanted = options
            .get("capabilities")
            .and_then(|v| u32::try_from(v).ok())
            .unwrap_or(CAP_KEYBOARD | CAP_POINTER)
            & (CAP_KEYBOARD | CAP_POINTER);
        if wanted == 0 {
            return Ok((RESPONSE_CANCELLED, Results::new()));
        }

        let name = app_name(&app_id);
        let title = format!("{name} wants to take over your keyboard and mouse");
        let body = format!(
            "When the pointer is pushed past a screen edge that {name} chooses, your keyboard \
             and mouse control {name} (for example another computer) until it gives them back. \
             Super+Shift+Escape always takes them back."
        );
        let answer = self
            .0
            .ask(move |state, tx| {
                let (dialog_tx, dialog_rx) = async_channel::bounded(1);
                state.arbor.portal_dialog.show(DialogRequest {
                    title,
                    body,
                    choices: vec![String::from("Allow")],
                    remember: None,
                    reply: dialog_tx,
                });
                state.arbor.queue_redraw_all();
                let _ = tx.try_send(dialog_rx);
            })
            .await?
            .recv()
            .await
            .unwrap_or(DialogAnswer::Cancelled);
        if answer == DialogAnswer::Cancelled {
            return Ok((RESPONSE_CANCELLED, Results::new()));
        }

        let handle = session_handle.as_str().to_owned();
        let id = {
            let mut sessions = self.0.sessions.lock().unwrap();
            sessions.1 += 1;
            let id = sessions.1;
            sessions.0.insert(handle.clone(), id);
            id
        };
        let handle_ = handle.clone();
        self.0.run(move |state| {
            state.arbor.input_capture.add_session(id, handle_, wanted);
        })?;
        server
            .at(
                &session_handle,
                CaptureSessionObject {
                    handle,
                    backend: self.0.clone(),
                },
            )
            .await
            .map_err(|err| fdo::Error::Failed(err.to_string()))?;
        Ok((
            RESPONSE_SUCCESS,
            HashMap::from([(String::from("capabilities"), OwnedValue::from(wanted))]),
        ))
    }

    async fn get_zones(
        &self,
        _handle: OwnedObjectPath,
        session_handle: OwnedObjectPath,
        _app_id: String,
        _options: HashMap<String, OwnedValue>,
    ) -> fdo::Result<(u32, Results)> {
        self.0.id(session_handle.as_str())?;
        let zones = self.0.zones().await?;
        let set = zone_set(&zones);
        Ok((
            RESPONSE_SUCCESS,
            HashMap::from([
                (String::from("zones"), owned(Value::from(zones))),
                (String::from("zone_set"), OwnedValue::from(set)),
            ]),
        ))
    }

    #[allow(clippy::too_many_arguments)]
    async fn set_pointer_barriers(
        &self,
        _handle: OwnedObjectPath,
        session_handle: OwnedObjectPath,
        _app_id: String,
        _options: HashMap<String, OwnedValue>,
        barriers: Vec<HashMap<String, OwnedValue>>,
        _zone_set: u32,
    ) -> fdo::Result<(u32, Results)> {
        let id = self.0.id(session_handle.as_str())?;
        let zones = self.0.zones().await?;
        let zone_rects: Vec<_> = zones
            .iter()
            .map(|&(w, h, x, y)| {
                smithay::utils::Rectangle::new((x, y).into(), (w as i32, h as i32).into())
            })
            .collect();

        let mut ok = Vec::new();
        let mut failed = Vec::new();
        for b in &barriers {
            let Some(bid) = b.get("barrier_id").and_then(|v| u32::try_from(v).ok()) else {
                continue;
            };
            let pos = b
                .get("position")
                .and_then(|v| <(i32, i32, i32, i32)>::try_from(v.try_clone().ok()?).ok());
            let Some((x1, y1, x2, y2)) = pos else {
                failed.push(bid);
                continue;
            };
            let barrier = Barrier {
                id: bid,
                x1,
                y1,
                x2,
                y2,
            };
            if barrier_is_valid(&barrier, &zone_rects) {
                ok.push(barrier);
            } else {
                failed.push(bid);
            }
        }
        self.0.run(move |state| {
            if let Some(s) = state.arbor.input_capture.sessions.get_mut(&id) {
                s.barriers = ok;
            }
        })?;
        Ok((
            RESPONSE_SUCCESS,
            HashMap::from([(String::from("failed_barriers"), owned(Value::from(failed)))]),
        ))
    }

    async fn enable(
        &self,
        session_handle: OwnedObjectPath,
        _app_id: String,
        _options: HashMap<String, OwnedValue>,
    ) -> fdo::Result<(u32, Results)> {
        let id = self.0.id(session_handle.as_str())?;
        self.0.run(move |state| {
            if let Some(s) = state.arbor.input_capture.sessions.get_mut(&id) {
                s.enabled = true;
            }
        })?;
        Ok((RESPONSE_SUCCESS, Results::new()))
    }

    async fn disable(
        &self,
        session_handle: OwnedObjectPath,
        _app_id: String,
        _options: HashMap<String, OwnedValue>,
    ) -> fdo::Result<(u32, Results)> {
        let id = self.0.id(session_handle.as_str())?;
        self.0.run(move |state| {
            if state
                .arbor
                .input_capture
                .active
                .is_some_and(|(a, _)| a == id)
            {
                state.input_capture_deactivate(None);
            }
            if let Some(s) = state.arbor.input_capture.sessions.get_mut(&id) {
                s.enabled = false;
            }
        })?;
        Ok((RESPONSE_SUCCESS, Results::new()))
    }

    async fn release(
        &self,
        session_handle: OwnedObjectPath,
        _app_id: String,
        options: HashMap<String, OwnedValue>,
    ) -> fdo::Result<(u32, Results)> {
        let id = self.0.id(session_handle.as_str())?;
        let activation = options
            .get("activation_id")
            .and_then(|v| u32::try_from(v).ok());
        let cursor = options
            .get("cursor_position")
            .and_then(|v| <(f64, f64)>::try_from(v.try_clone().ok()?).ok());
        self.0.run(move |state| {
            let Some((active, act_id)) = state.arbor.input_capture.active else {
                return;
            };
            if active != id || activation.is_some_and(|a| a != act_id) {
                return;
            }
            state.input_capture_deactivate(cursor.map(|(x, y)| (x, y).into()));
        })?;
        Ok((RESPONSE_SUCCESS, Results::new()))
    }

    async fn connect_to_e_i_s(
        &self,
        session_handle: OwnedObjectPath,
        _app_id: String,
        _options: HashMap<String, OwnedValue>,
    ) -> fdo::Result<zbus::zvariant::OwnedFd> {
        let id = self.0.id(session_handle.as_str())?;
        let fd = self
            .0
            .ask(move |state, tx| {
                let _ = tx.try_send(
                    state
                        .input_capture_connect_eis(id)
                        .map_err(|err| err.to_string()),
                );
            })
            .await?
            .map_err(fdo::Error::Failed)?;
        Ok(zbus::zvariant::OwnedFd::from(fd))
    }

    #[zbus(signal)]
    async fn disabled(
        ctxt: &SignalEmitter<'_>,
        session_handle: OwnedObjectPath,
        options: HashMap<String, OwnedValue>,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn activated(
        ctxt: &SignalEmitter<'_>,
        session_handle: OwnedObjectPath,
        options: HashMap<String, OwnedValue>,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn deactivated(
        ctxt: &SignalEmitter<'_>,
        session_handle: OwnedObjectPath,
        options: HashMap<String, OwnedValue>,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn zones_changed(
        ctxt: &SignalEmitter<'_>,
        session_handle: OwnedObjectPath,
        options: HashMap<String, OwnedValue>,
    ) -> zbus::Result<()>;

    #[zbus(property)]
    fn supported_capabilities(&self) -> u32 {
        CAP_KEYBOARD | CAP_POINTER
    }

    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        1
    }
}

#[interface(name = "org.freedesktop.impl.portal.Session")]
impl CaptureSessionObject {
    async fn close(
        &self,
        #[zbus(object_server)] server: &ObjectServer,
        #[zbus(signal_context)] ctxt: SignalEmitter<'_>,
    ) {
        let id = self.backend.sessions.lock().unwrap().0.remove(&self.handle);
        if let Some(id) = id {
            let _ = self.backend.run(move |state| state.input_capture_close(id));
        }
        let _ = server.remove::<CaptureSessionObject, _>(ctxt.path()).await;
    }

    #[zbus(signal)]
    async fn closed(ctxt: &SignalEmitter<'_>) -> zbus::Result<()>;

    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        1
    }
}

/// Adds the InputCapture interface to arbor's portal backend connection.
pub fn register(
    conn: &zbus::blocking::Connection,
    event_loop: &calloop::LoopHandle<'static, State>,
    input_capture: &mut crate::input_capture::InputCapture,
) -> anyhow::Result<()> {
    let (to_arbor, from_dbus) = calloop::channel::channel::<Job>();
    event_loop
        .insert_source(from_dbus, move |event, _, state| match event {
            calloop::channel::Event::Msg(job) => job(state),
            calloop::channel::Event::Closed => (),
        })
        .map_err(|err| anyhow::anyhow!("{err}"))?;

    let (events_tx, events_rx) = async_channel::unbounded::<CaptureEvent>();
    input_capture.events = Some(events_tx);
    let async_conn = conn.inner().clone();
    let future = async move {
        while let Ok(event) = events_rx.recv().await {
            let Ok(emitter) = SignalEmitter::new(&async_conn, PATH) else {
                continue;
            };
            let res = match event {
                CaptureEvent::Activated {
                    handle,
                    activation_id,
                    cursor,
                    barrier_id,
                } => {
                    let Ok(path) = OwnedObjectPath::try_from(handle) else {
                        continue;
                    };
                    let options = HashMap::from([
                        (
                            String::from("activation_id"),
                            OwnedValue::from(activation_id),
                        ),
                        (String::from("cursor_position"), owned(Value::from(cursor))),
                        (String::from("barrier_id"), OwnedValue::from(barrier_id)),
                    ]);
                    InputCapturePortal::activated(&emitter, path, options).await
                }
                CaptureEvent::Deactivated {
                    handle,
                    activation_id,
                    cursor,
                } => {
                    let Ok(path) = OwnedObjectPath::try_from(handle) else {
                        continue;
                    };
                    let options = HashMap::from([
                        (
                            String::from("activation_id"),
                            OwnedValue::from(activation_id),
                        ),
                        (String::from("cursor_position"), owned(Value::from(cursor))),
                    ]);
                    InputCapturePortal::deactivated(&emitter, path, options).await
                }
                CaptureEvent::Disabled { handle } => {
                    let Ok(path) = OwnedObjectPath::try_from(handle) else {
                        continue;
                    };
                    InputCapturePortal::disabled(&emitter, path, HashMap::new()).await
                }
            };
            if let Err(err) = res {
                warn!("error emitting an InputCapture signal: {err:?}");
            }
        }
    };
    conn.inner()
        .executor()
        .spawn(future, "emit InputCapture signals")
        .detach();

    conn.object_server().at(
        PATH,
        InputCapturePortal(Backend {
            to_arbor,
            sessions: Arc::new(Mutex::new((HashMap::new(), 0))),
        }),
    )?;
    Ok(())
}
