//! `org.freedesktop.impl.portal.Inhibit`: the Inhibit portal backend, part of arbor's portal
//! (`org.freedesktop.impl.portal.desktop.arbor`).
//!
//! The KDE backend forwards inhibitions to PowerDevil and the GNOME one to gnome-session, neither
//! of which runs under arbor. Here the "idle" flag stops arbor's idle notifications (what
//! swayidle and other ext-idle-notify clients listen to), like `org.freedesktop.ScreenSaver`
//! inhibitors and Wayland idle inhibitors do. The logout, user switch and suspend flags have
//! nothing to act on and are accepted without effect. Session state monitoring (CreateMonitor)
//! is not implemented.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use futures_util::StreamExt;
use zbus::fdo;
use zbus::message::Header;
use zbus::names::UniqueName;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{NoneValue, OwnedObjectPath, OwnedValue};
use zbus::{interface, ObjectServer};

const FLAG_IDLE: u32 = 8;

/// Active idle inhibitions: request handle → the portal frontend's bus name.
#[derive(Clone)]
struct Inhibitors {
    active: Arc<Mutex<HashMap<String, String>>>,
    is_inhibited: Arc<AtomicBool>,
    wake: Option<calloop::ping::Ping>,
}

impl Inhibitors {
    fn update(&self, active: &HashMap<String, String>) {
        let inhibited = !active.is_empty();
        if self.is_inhibited.swap(inhibited, Ordering::SeqCst) != inhibited {
            if let Some(wake) = &self.wake {
                wake.ping();
            }
        }
    }

    fn add(&self, handle: String, owner: String) {
        let mut active = self.active.lock().unwrap();
        active.insert(handle, owner);
        self.update(&active);
    }

    fn remove(&self, handle: &str) {
        let mut active = self.active.lock().unwrap();
        active.remove(handle);
        self.update(&active);
    }

    fn remove_owner(&self, owner: &str) {
        let mut active = self.active.lock().unwrap();
        active.retain(|_, o| o != owner);
        self.update(&active);
    }
}

pub struct Inhibit {
    inhibitors: Inhibitors,
}

/// `org.freedesktop.impl.portal.Request` at the inhibition's handle: closing it ends it.
struct Request {
    inhibitors: Inhibitors,
}

#[interface(name = "org.freedesktop.impl.portal.Request")]
impl Request {
    async fn close(
        &self,
        #[zbus(object_server)] server: &ObjectServer,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) {
        self.inhibitors.remove(emitter.path().as_str());
        let _ = server.remove::<Request, _>(emitter.path()).await;
    }
}

#[interface(name = "org.freedesktop.impl.portal.Inhibit")]
impl Inhibit {
    async fn inhibit(
        &self,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(object_server)] server: &ObjectServer,
        handle: OwnedObjectPath,
        app_id: String,
        _window: String,
        flags: u32,
        _options: HashMap<String, OwnedValue>,
    ) -> fdo::Result<()> {
        debug!(app_id, flags, "Inhibit portal");
        if flags & FLAG_IDLE == 0 {
            return Ok(());
        }
        let owner = hdr.sender().map(|s| s.to_string()).unwrap_or_default();
        let request = Request {
            inhibitors: self.inhibitors.clone(),
        };
        server
            .at(&handle, request)
            .await
            .map_err(|err| fdo::Error::Failed(err.to_string()))?;
        self.inhibitors.add(handle.as_str().to_owned(), owner);
        Ok(())
    }
}

async fn watch_owners(conn: zbus::Connection, inhibitors: Inhibitors) -> anyhow::Result<()> {
    let proxy = fdo::DBusProxy::new(&conn).await?;
    let mut stream = proxy
        .receive_name_owner_changed_with_args(&[(2, UniqueName::null_value())])
        .await?;
    while let Some(signal) = stream.next().await {
        let args = signal.args()?;
        if args.new_owner().is_none() {
            inhibitors.remove_owner(args.name());
        }
    }
    Ok(())
}

/// Adds the Inhibit interface to arbor's portal backend connection.
pub fn register(
    conn: &zbus::blocking::Connection,
    is_inhibited: Arc<AtomicBool>,
    wake: Option<calloop::ping::Ping>,
) -> anyhow::Result<()> {
    let inhibitors = Inhibitors {
        active: Arc::default(),
        is_inhibited,
        wake,
    };
    conn.object_server().at(
        "/org/freedesktop/portal/desktop",
        Inhibit {
            inhibitors: inhibitors.clone(),
        },
    )?;

    let async_conn = conn.inner().clone();
    let task = {
        let conn = async_conn.clone();
        async move {
            if let Err(err) = watch_owners(conn, inhibitors).await {
                warn!("error watching Inhibit portal clients: {err:?}");
            }
        }
    };
    async_conn
        .executor()
        .spawn(task, "watch Inhibit portal clients")
        .detach();
    Ok(())
}
