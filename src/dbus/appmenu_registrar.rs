//! Watches `com.canonical.AppMenu.Registrar` on the session bus.
//!
//! Its owner (Plasma's kded, hearth's menu bar) draws global menus. Qt apps check for it
//! themselves; GTK apps learn it from us through the `gtk_shell1` `global_menu_bar` capability,
//! so that both hide their in-window menu bars at the same moments.

use futures_util::StreamExt;
use zbus::fdo;

const REGISTRAR: &str = "com.canonical.AppMenu.Registrar";

pub fn start(
    to_arbor: calloop::channel::Sender<bool>,
) -> anyhow::Result<zbus::blocking::Connection> {
    let conn = zbus::blocking::Connection::session()?;

    let async_conn = conn.inner().clone();
    let future = async move {
        let proxy = match fdo::DBusProxy::new(&async_conn).await {
            Ok(x) => x,
            Err(err) => {
                warn!("error creating DBusProxy: {err:?}");
                return;
            }
        };

        let mut changes = match proxy
            .receive_name_owner_changed_with_args(&[(0, REGISTRAR)])
            .await
        {
            Ok(x) => x,
            Err(err) => {
                warn!("error subscribing to NameOwnerChanged: {err:?}");
                return;
            }
        };

        let name = zbus::names::BusName::try_from(REGISTRAR).unwrap();
        let owned = proxy.name_has_owner(name).await.unwrap_or(false);
        if to_arbor.send(owned).is_err() {
            return;
        }

        while let Some(change) = changes.next().await {
            let Ok(args) = change.args() else { continue };
            if args.name() != REGISTRAR {
                continue;
            }
            if to_arbor.send(args.new_owner().is_some()).is_err() {
                return;
            }
        }
    };

    let task = conn
        .inner()
        .executor()
        .spawn(future, "watch com.canonical.AppMenu.Registrar");
    task.detach();

    Ok(conn)
}
