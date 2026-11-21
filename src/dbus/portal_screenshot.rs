//! `org.freedesktop.impl.portal.Screenshot`: arbor's Screenshot portal backend, part of arbor's
//! portal (`org.freedesktop.impl.portal.desktop.arbor`).
//!
//! - Non-interactive requests capture every screen into one image right away. (The portal frontend
//!   asks the user for permission first through the Access portal and remembers it.)
//! - Interactive requests open arbor's screenshot UI: pick a screen, window or region and confirm
//!   (Space/Enter), or press Escape to cancel.
//! - PickColor uses arbor's color picker (click a pixel, Escape cancels).
//!
//! Files go where `screenshot-path` in the config says, or to `$XDG_CACHE_HOME/arbor/screenshots`
//! when screenshots are not saved.

use std::collections::HashMap;
use std::path::PathBuf;

use zbus::interface;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

use crate::arbor::State;
use crate::utils::make_screenshot_path;

type Job = Box<dyn FnOnce(&mut State) + Send>;
type Results = HashMap<String, OwnedValue>;

const RESPONSE_SUCCESS: u32 = 0;
const RESPONSE_CANCELLED: u32 = 1;
const RESPONSE_OTHER: u32 = 2;

pub struct ScreenshotPortal {
    to_arbor: calloop::channel::Sender<Job>,
}

/// `file://` URI for a path, percent-encoding what needs it.
pub fn file_uri(path: &std::path::Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut uri = String::from("file://");
    for &b in path.as_os_str().as_bytes() {
        if b.is_ascii_alphanumeric() || b"/-_.~".contains(&b) {
            uri.push(b as char);
        } else {
            uri.push_str(&format!("%{b:02X}"));
        }
    }
    uri
}

fn portal_screenshot_path(state: &State) -> PathBuf {
    let configured = make_screenshot_path(&state.arbor.config.borrow())
        .ok()
        .flatten();
    configured.unwrap_or_else(|| {
        let dir = directories::BaseDirs::new()
            .map(|d| d.cache_dir().join("arbor/screenshots"))
            .unwrap_or_else(std::env::temp_dir);
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        dir.join(format!("Screenshot-{stamp}.png"))
    })
}

impl ScreenshotPortal {
    async fn ask<T: Send + 'static>(
        &self,
        job: impl FnOnce(&mut State, async_channel::Sender<T>) + Send + 'static,
    ) -> Option<T> {
        let (tx, rx) = async_channel::bounded(1);
        self.to_arbor
            .send(Box::new(move |state| job(state, tx)))
            .ok()?;
        rx.recv().await.ok()
    }
}

#[interface(name = "org.freedesktop.impl.portal.Screenshot")]
impl ScreenshotPortal {
    async fn screenshot(
        &self,
        _handle: OwnedObjectPath,
        app_id: String,
        _parent_window: String,
        options: HashMap<String, OwnedValue>,
    ) -> (u32, Results) {
        let interactive = options
            .get("interactive")
            .and_then(|v| bool::try_from(v).ok())
            .unwrap_or(false);
        debug!(app_id, interactive, "Screenshot portal");

        let path: Option<Option<String>> = if interactive {
            self.ask(|state, tx| {
                if state.arbor.is_locked() || state.arbor.screenshot_ui.is_open() {
                    let _ = tx.try_send(None);
                    return;
                }
                let path = portal_screenshot_path(state);
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                // An earlier request loses.
                if let Some(old) = state.arbor.portal_screenshot.take() {
                    let _ = old.try_send(None);
                }
                state.arbor.portal_screenshot = Some(tx);
                state.arbor.portal_screenshot_confirmed = false;
                state.open_screenshot_ui(false, Some(path.to_string_lossy().into_owned()));
                if !state.arbor.screenshot_ui.is_open() {
                    if let Some(tx) = state.arbor.portal_screenshot.take() {
                        let _ = tx.try_send(None);
                    }
                }
            })
            .await
        } else {
            self.ask(|state, tx| {
                let path = portal_screenshot_path(state);
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let res = state.backend.with_primary_renderer(|renderer| {
                    let tx_ = tx.clone();
                    state
                        .arbor
                        .screenshot_all_outputs(renderer, false, move |saved| {
                            // Move it where the portal expects it.
                            let final_path = if std::fs::rename(&saved, &path).is_ok() {
                                path
                            } else {
                                saved
                            };
                            let _ =
                                tx_.send_blocking(Some(final_path.to_string_lossy().into_owned()));
                        })
                });
                match res {
                    Some(Ok(())) => (),
                    Some(Err(err)) => {
                        warn!("error taking a portal screenshot: {err:?}");
                        let _ = tx.try_send(None);
                    }
                    None => {
                        let _ = tx.try_send(None);
                    }
                }
            })
            .await
        };

        match path {
            Some(Some(path)) => {
                let uri = file_uri(std::path::Path::new(&path));
                let results = HashMap::from([(
                    String::from("uri"),
                    OwnedValue::try_from(Value::from(uri)).unwrap(),
                )]);
                (RESPONSE_SUCCESS, results)
            }
            Some(None) if interactive => (RESPONSE_CANCELLED, Results::new()),
            _ => (RESPONSE_OTHER, Results::new()),
        }
    }

    async fn pick_color(
        &self,
        _handle: OwnedObjectPath,
        _app_id: String,
        _parent_window: String,
        _options: HashMap<String, OwnedValue>,
    ) -> (u32, Results) {
        let color = self
            .ask(|state, tx| state.handle_pick_color(tx))
            .await
            .flatten();
        match color {
            Some(color) => {
                let [r, g, b] = color.rgb;
                let value = Value::from((r, g, b));
                let results =
                    HashMap::from([(String::from("color"), OwnedValue::try_from(value).unwrap())]);
                (RESPONSE_SUCCESS, results)
            }
            None => (RESPONSE_CANCELLED, Results::new()),
        }
    }

    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        2
    }
}

/// Adds the Screenshot interface to arbor's portal backend connection.
pub fn register(
    conn: &zbus::blocking::Connection,
    event_loop: &calloop::LoopHandle<'static, State>,
) -> anyhow::Result<()> {
    let (to_arbor, from_dbus) = calloop::channel::channel::<Job>();
    event_loop
        .insert_source(from_dbus, move |event, _, state| match event {
            calloop::channel::Event::Msg(job) => job(state),
            calloop::channel::Event::Closed => (),
        })
        .map_err(|err| anyhow::anyhow!("{err}"))?;
    conn.object_server().at(
        "/org/freedesktop/portal/desktop",
        ScreenshotPortal { to_arbor },
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uris_are_escaped() {
        assert_eq!(
            file_uri(std::path::Path::new(
                "/home/me/Pictures/Screenshot from 2026.png"
            )),
            "file:///home/me/Pictures/Screenshot%20from%202026.png"
        );
    }
}
