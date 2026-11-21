//! `org.freedesktop.impl.portal.Wallpaper`: the Wallpaper portal backend, part of arbor's portal
//! (`org.freedesktop.impl.portal.desktop.arbor`).
//!
//! arbor doesn't draw wallpapers itself, so setting one runs a command from the config, e.g. for
//! hearth:
//!
//! ```kdl
//! portal {
//!     wallpaper-command "hearth-ipc" "call" "wallpaper" "setImage"
//! }
//! ```
//!
//! The image is first copied (or downloaded with curl) to `$XDG_CACHE_HOME/arbor/wallpaper/`, so
//! it stays valid after the app (and its document portal grant) goes away; its path is appended
//! to the command. `set-on` picks `wallpaper-command`, `lockscreen-wallpaper-command` or both.
//! There is no preview dialog: `show-preview` is ignored.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use zbus::interface;
use zbus::zvariant::{OwnedObjectPath, OwnedValue};

use crate::arbor::State;

type Job = Box<dyn FnOnce(&mut State) + Send>;

const RESPONSE_SUCCESS: u32 = 0;
const RESPONSE_OTHER: u32 = 2;

pub struct Wallpaper {
    to_arbor: calloop::channel::Sender<Job>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SetOn {
    Background,
    Lockscreen,
    Both,
}

fn percent_decode(s: &str) -> Vec<u8> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

/// `file:///a%20b.png` → `/a b.png`.
fn file_uri_path(uri: &str) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    let rest = uri.strip_prefix("file://")?;
    // Skip an authority ("localhost").
    let path = &rest[rest.find('/')?..];
    Some(PathBuf::from(std::ffi::OsString::from_vec(percent_decode(path))))
}

fn cache_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".cache")))?;
    Some(base.join("arbor").join("wallpaper"))
}

fn extension(name: &str) -> String {
    let name = name.split(['?', '#']).next().unwrap_or(name);
    Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .filter(|e| e.len() <= 5 && e.chars().all(|c| c.is_ascii_alphanumeric()))
        .map(|e| format!(".{}", e.to_ascii_lowercase()))
        .unwrap_or_default()
}

/// Copies or downloads the image into the cache (a new name each time, so that watchers of the
/// path notice; older copies are removed). Blocking.
fn fetch(uri: &str) -> anyhow::Result<PathBuf> {
    let dir = cache_dir().ok_or_else(|| anyhow::anyhow!("no cache directory"))?;
    std::fs::create_dir_all(&dir)?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or_default();
    let dest = dir.join(format!("wallpaper-{stamp}{}", extension(uri)));

    if let Some(path) = file_uri_path(uri).or_else(|| uri.starts_with('/').then(|| uri.into())) {
        std::fs::copy(&path, &dest)?;
    } else if uri.starts_with("http://") || uri.starts_with("https://") {
        let status = Command::new("curl")
            .args(["-fsSL", "--max-time", "60", "-o"])
            .arg(&dest)
            .arg(uri)
            .stdin(Stdio::null())
            .status()?;
        anyhow::ensure!(status.success(), "curl failed: {status}");
    } else {
        anyhow::bail!("unsupported URI");
    }

    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            if entry.path() != dest {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    Ok(dest)
}

fn run(command: &[String], image: &Path) -> anyhow::Result<()> {
    let (program, args) = command
        .split_first()
        .ok_or_else(|| anyhow::anyhow!("empty command"))?;
    let status = Command::new(program)
        .args(args)
        .arg(image)
        .stdin(Stdio::null())
        .status()?;
    anyhow::ensure!(status.success(), "{program} failed: {status}");
    Ok(())
}

/// The whole request, blocking (runs on its own thread).
fn set_wallpaper(
    uri: &str,
    set_on: SetOn,
    background: Option<Vec<String>>,
    lockscreen: Option<Vec<String>>,
) -> anyhow::Result<()> {
    let commands: Vec<Vec<String>> = match set_on {
        SetOn::Background => background.into_iter().collect(),
        SetOn::Lockscreen => lockscreen.into_iter().collect(),
        SetOn::Both => background.into_iter().chain(lockscreen).collect(),
    };
    anyhow::ensure!(
        !commands.is_empty(),
        "no portal {{ wallpaper-command }} configured for {set_on:?}"
    );
    let image = fetch(uri)?;
    for command in &commands {
        run(command, &image)?;
    }
    Ok(())
}

#[interface(name = "org.freedesktop.impl.portal.Wallpaper")]
impl Wallpaper {
    #[zbus(name = "SetWallpaperURI")]
    async fn set_wallpaper_uri(
        &self,
        _handle: OwnedObjectPath,
        app_id: String,
        _parent_window: String,
        uri: String,
        options: HashMap<String, OwnedValue>,
    ) -> u32 {
        let set_on = match options.get("set-on").and_then(|v| <&str>::try_from(v).ok()) {
            Some("lockscreen") => SetOn::Lockscreen,
            Some("both") => SetOn::Both,
            _ => SetOn::Background,
        };
        debug!(app_id, uri, ?set_on, "Wallpaper SetWallpaperURI");

        let (tx, rx) = async_channel::bounded(1);
        let sent = self.to_arbor.send(Box::new(move |state| {
            let portal = state.arbor.config.borrow().portal.clone();
            let _ = tx.try_send((portal.wallpaper_command, portal.lockscreen_wallpaper_command));
        }));
        if sent.is_err() {
            return RESPONSE_OTHER;
        }
        let Ok((background, lockscreen)) = rx.recv().await else {
            return RESPONSE_OTHER;
        };

        let (tx, rx) = async_channel::bounded(1);
        let spawned = std::thread::Builder::new()
            .name(String::from("portal wallpaper"))
            .spawn(move || {
                let _ = tx.send_blocking(set_wallpaper(&uri, set_on, background, lockscreen));
            });
        if spawned.is_err() {
            return RESPONSE_OTHER;
        }
        match rx.recv().await {
            Ok(Ok(())) => RESPONSE_SUCCESS,
            Ok(Err(err)) => {
                warn!("error setting the wallpaper from {app_id:?}: {err:?}");
                RESPONSE_OTHER
            }
            Err(_) => RESPONSE_OTHER,
        }
    }
}

/// Adds the Wallpaper interface to arbor's portal backend connection (the one that owns
/// `org.freedesktop.impl.portal.desktop.arbor`).
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
    conn.object_server()
        .at("/org/freedesktop/portal/desktop", Wallpaper { to_arbor })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_file_uris() {
        assert_eq!(
            file_uri_path("file:///home/me/My%20Pictures/a.png"),
            Some(PathBuf::from("/home/me/My Pictures/a.png"))
        );
        assert_eq!(
            file_uri_path("file://localhost/tmp/x.jpg"),
            Some(PathBuf::from("/tmp/x.jpg"))
        );
        assert_eq!(file_uri_path("https://x/y.png"), None);
        assert_eq!(extension("https://x/y.JPG?size=4k"), ".jpg");
        assert_eq!(extension("file:///a/b"), "");
    }

    #[test]
    fn copies_and_runs_the_command() {
        let dir = std::env::temp_dir().join(format!("arbor-wallpaper-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("pic one.png");
        std::fs::write(&src, b"png").unwrap();
        let out = dir.join("out");
        // Runs `sh -c 'cp "$0" out' <image>`.
        let command = vec![
            String::from("sh"),
            String::from("-c"),
            format!("cp \"$0\" '{}'", out.display()),
        ];
        // Keep the cache inside the test directory.
        let cache = dir.join("cache");
        temp_env(&cache, || {
            let uri = format!("file://{}", src.display()).replace(' ', "%20");
            set_wallpaper(&uri, SetOn::Background, Some(command.clone()), None).unwrap();
            assert!(set_wallpaper(&uri, SetOn::Lockscreen, Some(command), None).is_err());
        });
        assert_eq!(std::fs::read(&out).unwrap(), b"png");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn temp_env(cache: &Path, f: impl FnOnce()) {
        // SAFETY: only this test reads XDG_CACHE_HOME.
        let old = std::env::var_os("XDG_CACHE_HOME");
        unsafe { std::env::set_var("XDG_CACHE_HOME", cache) };
        f();
        match old {
            Some(v) => unsafe { std::env::set_var("XDG_CACHE_HOME", v) },
            None => unsafe { std::env::remove_var("XDG_CACHE_HOME") },
        }
    }
}
