//! arbor: every app opens where it was last time.
//!
//! When a window closes, hides (unmaps) or arbor quits, its app's placement is stored in
//! the SQLite database `$XDG_STATE_HOME/arbor/windows.db` (`~/.local/state/arbor/windows.db`,
//! table `placements`; an older `windows.json` is imported once): the output, whether
//! it was docked, and for floating windows the position (relative to the working area's top
//! left) and the window size. When a window of that app opens again, the placement fills in the
//! window-rule fields the user's own rules left unset (`default-column-width`,
//! `default-window-height`, `default-floating-position`, `open-on-output`, `open-docked`), so the
//! initial configure already carries the saved size.
//!
//! Each window title also keeps its own placement (the last 16 per app), used when a window
//! opens with a title seen before (keyed by the title a window opened with, not the one it closed
//! with), so an app's settings window and main window each come back
//! to their own spot.
//!
//! Dialogs and other windows with a parent are neither recorded nor placed. When another window
//! of the same app is already open, only the size is restored, so the new window doesn't land
//! exactly on top of the old one. Maximized and fullscreen windows keep their previous record.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

use arbor_config::utils::FloatOrInt;
use arbor_config::{FloatingPosition, PresetSize, RelativeTo};
use rusqlite::{params, Connection, OptionalExtension as _};
use serde::{Deserialize, Serialize};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::compositor::with_states;

use crate::window::ResolvedWindowRules;

/// Where an app's window was.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Placement {
    /// Output (connector) name.
    pub output: String,
    /// Whether it was docked (in the dock tree) rather than floating.
    #[serde(default)]
    pub docked: bool,
    /// Floating: top-left of the tile, relative to the working area's top-left.
    #[serde(default)]
    pub pos: Option<(f64, f64)>,
    /// Floating: window size (without arbor's title bar and border).
    #[serde(default)]
    pub size: Option<(i32, i32)>,
}

/// How many window titles are remembered per app (most recent kept).
const TITLES_PER_APP: usize = 16;

/// The old JSON store (`windows.json`), imported once into the database.
#[derive(Debug, Default, Deserialize)]
struct JsonFile {
    #[serde(default)]
    apps: BTreeMap<String, Placement>,
    #[serde(default)]
    titles: BTreeMap<String, Vec<(String, Placement)>>,
}

/// The stored placements: an in-memory copy for lookups while windows open, written through to
/// SQLite on a background thread so the compositor never waits for the disk.
#[derive(Debug, Default)]
pub struct WindowMemory {
    writer: Option<mpsc::Sender<Write>>,
    writer_thread: Option<std::thread::JoinHandle<()>>,
    apps: BTreeMap<String, Placement>,
    /// Per app, the last placement of each window title, oldest first.
    titles: BTreeMap<String, Vec<(String, Placement)>>,
}

/// One change for the writer thread.
#[derive(Debug)]
enum Write {
    /// The app's last window (`title` empty) or one of its titles, at `seq` (recency).
    Upsert {
        app_id: String,
        title: String,
        placement: Placement,
        seq: i64,
    },
    /// Forget an app's title (dropped from the most recent `TITLES_PER_APP`).
    Delete { app_id: String, title: String },
}

/// `$XDG_STATE_HOME/arbor/windows.db`.
pub fn default_path() -> Option<PathBuf> {
    let state = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".local/state")))?;
    Some(state.join("arbor").join("windows.db"))
}

const SCHEMA: &str = "
    PRAGMA journal_mode = WAL;
    PRAGMA synchronous = NORMAL;
    CREATE TABLE IF NOT EXISTS placements (
        app_id TEXT NOT NULL,
        title  TEXT NOT NULL,          -- '' = the app's last window
        output TEXT NOT NULL,
        docked INTEGER NOT NULL,
        x REAL, y REAL,                -- floating: tile position in the working area
        w INTEGER, h INTEGER,          -- floating: window size
        seq INTEGER NOT NULL,          -- recency (higher = newer)
        PRIMARY KEY (app_id, title)
    );";

fn open(path: &Path) -> rusqlite::Result<Connection> {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let conn = Connection::open(path)?;
    conn.execute_batch(SCHEMA)?;
    Ok(conn)
}

fn row_placement(row: &rusqlite::Row) -> rusqlite::Result<Placement> {
    let x: Option<f64> = row.get("x")?;
    let y: Option<f64> = row.get("y")?;
    let w: Option<i32> = row.get("w")?;
    let h: Option<i32> = row.get("h")?;
    Ok(Placement {
        output: row.get("output")?,
        docked: row.get("docked")?,
        pos: x.zip(y),
        size: w.zip(h),
    })
}

fn upsert(
    conn: &Connection,
    app_id: &str,
    title: &str,
    p: &Placement,
    seq: i64,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO placements (app_id, title, output, docked, x, y, w, h, seq)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT (app_id, title) DO UPDATE SET
           output = excluded.output, docked = excluded.docked, x = excluded.x, y = excluded.y,
           w = excluded.w, h = excluded.h, seq = excluded.seq",
        params![
            app_id,
            title,
            p.output,
            p.docked,
            p.pos.map(|p| p.0),
            p.pos.map(|p| p.1),
            p.size.map(|s| s.0),
            p.size.map(|s| s.1),
            seq
        ],
    )?;
    Ok(())
}

/// Copies the old `windows.json` next to `db` into the database, once (it's renamed after).
fn import_json(conn: &Connection, db: &Path) {
    let json = db.with_extension("json");
    let Ok(data) = std::fs::read(&json) else {
        return;
    };
    let Ok(file) = serde_json::from_slice::<JsonFile>(&data) else {
        warn!("window memory: couldn't read {}", json.display());
        return;
    };
    let mut seq = 0;
    let res = (|| -> rusqlite::Result<()> {
        let tx = conn.unchecked_transaction()?;
        for (app_id, p) in &file.apps {
            upsert(&tx, app_id, "", p, seq)?;
        }
        for (app_id, list) in &file.titles {
            for (title, p) in list {
                seq += 1;
                upsert(&tx, app_id, title, p, seq)?;
            }
        }
        tx.commit()
    })();
    match res {
        Ok(()) => {
            info!("window memory: imported {}", json.display());
            let _ = std::fs::rename(&json, json.with_extension("json.imported"));
        }
        Err(err) => warn!("window memory: couldn't import {}: {err}", json.display()),
    }
}

fn read_all(conn: &Connection) -> rusqlite::Result<Vec<(String, String, Placement, i64)>> {
    let mut stmt = conn.prepare("SELECT * FROM placements ORDER BY seq")?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>("app_id")?,
            row.get::<_, String>("title")?,
            row_placement(row)?,
            row.get::<_, i64>("seq")?,
        ))
    })?;
    rows.collect()
}

impl WindowMemory {
    /// Opens the database at `path` (`None`: in memory only, for tests).
    pub fn load(path: Option<PathBuf>) -> Self {
        let mut mem = Self::default();
        let Some(path) = path else {
            return mem;
        };
        let conn = match open(&path) {
            Ok(conn) => conn,
            Err(err) => {
                warn!("window memory: couldn't open {}: {err}", path.display());
                return mem;
            }
        };
        let empty: bool = conn
            .query_row("SELECT 1 FROM placements LIMIT 1", [], |_| Ok(()))
            .optional()
            .map(|r| r.is_none())
            .unwrap_or(true);
        if empty {
            import_json(&conn, &path);
        }
        match read_all(&conn) {
            Ok(rows) => {
                for (app_id, title, placement, _) in rows {
                    if title.is_empty() {
                        mem.apps.insert(app_id, placement);
                    } else {
                        mem.titles.entry(app_id).or_default().push((title, placement));
                    }
                }
            }
            Err(err) => warn!("window memory: couldn't read {}: {err}", path.display()),
        }

        let (tx, rx) = mpsc::channel::<Write>();
        let spawned = std::thread::Builder::new()
            .name(String::from("window-memory"))
            .spawn(move || {
                for write in rx {
                    let res = match &write {
                        Write::Upsert {
                            app_id,
                            title,
                            placement,
                            seq,
                        } => upsert(&conn, app_id, title, placement, *seq),
                        Write::Delete { app_id, title } => conn
                            .execute(
                                "DELETE FROM placements WHERE app_id = ?1 AND title = ?2",
                                params![app_id, title],
                            )
                            .map(|_| ()),
                    };
                    if let Err(err) = res {
                        warn!("window memory: couldn't store {write:?}: {err}");
                    }
                }
            });
        match spawned {
            Ok(handle) => {
                mem.writer = Some(tx);
                mem.writer_thread = Some(handle);
            }
            Err(err) => warn!("window memory: couldn't start the writer thread: {err}"),
        }
        mem
    }

    /// Where `app_id`'s window titled `title` was, else where its last window was.
    pub fn get(&self, app_id: &str, title: Option<&str>) -> Option<&Placement> {
        title
            .filter(|t| !t.is_empty())
            .and_then(|title| {
                self.titles
                    .get(app_id)?
                    .iter()
                    .find(|(t, _)| t == title)
                    .map(|(_, p)| p)
            })
            .or_else(|| self.apps.get(app_id))
    }

    fn write(&self, write: Write) {
        if let Some(writer) = &self.writer {
            let _ = writer.send(write);
        }
    }

    /// Stores `placement` for `app_id` and its window `title` (written to the database when it
    /// changed).
    pub fn record(&mut self, app_id: &str, title: Option<&str>, placement: Placement) {
        if app_id.is_empty() {
            return;
        }
        // Recency for the titles: milliseconds since the epoch.
        let seq = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let mut changed = false;
        if self.apps.get(app_id) != Some(&placement) {
            self.apps.insert(app_id.to_owned(), placement.clone());
            self.write(Write::Upsert {
                app_id: app_id.to_owned(),
                title: String::new(),
                placement: placement.clone(),
                seq,
            });
            changed = true;
        }
        if let Some(title) = title.filter(|t| !t.is_empty()) {
            let list = self.titles.entry(app_id.to_owned()).or_default();
            let idx = list.iter().position(|(t, _)| t == title);
            if idx.map(|i| &list[i].1) != Some(&placement) || idx != Some(list.len() - 1) {
                if let Some(i) = idx {
                    list.remove(i);
                }
                list.push((title.to_owned(), placement.clone()));
                let mut dropped = vec![];
                while list.len() > TITLES_PER_APP {
                    dropped.push(list.remove(0).0);
                }
                self.write(Write::Upsert {
                    app_id: app_id.to_owned(),
                    title: title.to_owned(),
                    placement: placement.clone(),
                    seq,
                });
                for title in dropped {
                    self.write(Write::Delete {
                        app_id: app_id.to_owned(),
                        title,
                    });
                }
                changed = true;
            }
        }
        if changed {
            debug!("window memory: {app_id} {title:?} -> {placement:?}");
        }
    }

    /// Waits until everything recorded so far is in the database, then stops writing (arbor is
    /// quitting or restarting in place, which exits without running destructors).
    pub fn flush(&mut self) {
        drop(self.writer.take());
        if let Some(handle) = self.writer_thread.take() {
            let _ = handle.join();
        }
    }
}

/// The placement a new toplevel should open with, kept in its surface's data map so that every
/// window-rule computation (initial configure, later recomputes) sees it.
#[derive(Default)]
struct RememberedPlacement(RefCell<Option<Placement>>);

/// Attaches `placement` (or clears it with `None`) to a toplevel's surface.
pub fn set_for_surface(surface: &WlSurface, placement: Option<Placement>) {
    with_states(surface, |states| {
        let data = states.data_map.get_or_insert(RememberedPlacement::default);
        *data.0.borrow_mut() = placement;
    });
}

/// The title a toplevel had when it first opened. Its placement is stored under that title,
/// not the title it has when it closes: apps put the folder or command in the title (grove,
/// tessera), and the next window opens with its startup title again, so that's what lookups use.
#[derive(Default)]
struct OpeningTitle(RefCell<Option<String>>);

/// Remembers `title` as the toplevel's opening title, unless it already has one.
pub fn set_opening_title(surface: &WlSurface, title: Option<String>) {
    with_states(surface, |states| {
        let data = states.data_map.get_or_insert(OpeningTitle::default);
        let mut data = data.0.borrow_mut();
        if data.is_none() {
            *data = title;
        }
    });
}

/// The toplevel's opening title (see [`OpeningTitle`]).
pub fn opening_title(surface: &WlSurface) -> Option<String> {
    with_states(surface, |states| {
        states
            .data_map
            .get::<OpeningTitle>()
            .and_then(|data| data.0.borrow().clone())
    })
}

/// Fills the rule fields the user's window rules left unset from the surface's placement.
pub fn apply_to_rules(surface: &WlSurface, rules: &mut ResolvedWindowRules) {
    let placement = with_states(surface, |states| {
        states
            .data_map
            .get::<RememberedPlacement>()
            .and_then(|data| data.0.borrow().clone())
    });
    if let Some(placement) = placement {
        fill_rules(&placement, rules);
    }
}

fn fill_rules(placement: &Placement, rules: &mut ResolvedWindowRules) {
    if rules.open_on_output.is_none() && !placement.output.is_empty() {
        rules.open_on_output = Some(placement.output.clone());
    }
    if placement.docked {
        if rules.open_docked.is_none() && rules.open_floating.is_none() {
            rules.open_docked = Some(true);
        }
        return;
    }
    if let Some((w, h)) = placement.size {
        if rules.default_width.is_none() {
            rules.default_width = Some(Some(PresetSize::Fixed(w.max(1))));
        }
        if rules.default_height.is_none() {
            rules.default_height = Some(Some(PresetSize::Fixed(h.max(1))));
        }
    }
    if let Some((x, y)) = placement.pos {
        if rules.default_floating_position.is_none() {
            rules.default_floating_position = Some(FloatingPosition {
                x: FloatOrInt(x.clamp(-65535., 65535.)),
                y: FloatOrInt(y.clamp(-65535., 65535.)),
                relative_to: RelativeTo::TopLeft,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn floating() -> Placement {
        Placement {
            output: String::from("DP-1"),
            docked: false,
            pos: Some((100., 50.)),
            size: Some((800, 600)),
        }
    }

    #[test]
    fn fills_only_unset_fields() {
        let mut rules = ResolvedWindowRules {
            default_width: Some(Some(PresetSize::Proportion(0.5))),
            ..Default::default()
        };
        fill_rules(&floating(), &mut rules);
        assert_eq!(rules.default_width, Some(Some(PresetSize::Proportion(0.5))));
        assert_eq!(rules.default_height, Some(Some(PresetSize::Fixed(600))));
        assert_eq!(rules.open_on_output.as_deref(), Some("DP-1"));
        let pos = rules.default_floating_position.unwrap();
        assert_eq!((pos.x.0, pos.y.0), (100., 50.));
        assert_eq!(pos.relative_to, RelativeTo::TopLeft);
        assert_eq!(rules.open_docked, None);
    }

    #[test]
    fn docked_opens_docked() {
        let mut rules = ResolvedWindowRules::default();
        let p = Placement {
            docked: true,
            pos: None,
            size: None,
            ..floating()
        };
        fill_rules(&p, &mut rules);
        assert_eq!(rules.open_docked, Some(true));
        assert_eq!(rules.default_width, None);

        // A user rule saying floating wins.
        let mut rules = ResolvedWindowRules {
            open_floating: Some(true),
            ..Default::default()
        };
        fill_rules(&p, &mut rules);
        assert_eq!(rules.open_docked, None);
    }

    #[test]
    fn store_round_trip() {
        let dir = std::env::temp_dir().join(format!("arbor-wm-test-{}", std::process::id()));
        let path = dir.join("windows.db");
        let mut mem = WindowMemory::load(Some(path.clone()));
        mem.record("org.example.App", Some("Main"), floating());
        let settings = Placement {
            pos: Some((5., 5.)),
            ..floating()
        };
        mem.record("org.example.App", Some("Settings"), settings.clone());
        mem.flush();
        let again = WindowMemory::load(Some(path));
        // A known title gets its own place; anything else the app's last window's.
        assert_eq!(
            again.get("org.example.App", Some("Main")),
            Some(&floating())
        );
        assert_eq!(
            again.get("org.example.App", Some("Settings")),
            Some(&settings)
        );
        assert_eq!(again.get("org.example.App", Some("Other")), Some(&settings));
        assert_eq!(again.get("org.example.App", None), Some(&settings));
        assert_eq!(again.get("nope", None), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn imports_json_once() {
        let dir = std::env::temp_dir().join(format!("arbor-wm-json-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("windows.json"),
            r#"{"version":1,"apps":{"treetop":{"output":"DP-1","docked":false,"pos":[10.0,20.0],"size":[900,700]}},
               "titles":{"treetop":[["treetop",{"output":"DP-1","docked":true,"pos":null,"size":null}]]}}"#,
        )
        .unwrap();
        let mem = WindowMemory::load(Some(dir.join("windows.db")));
        assert_eq!(mem.get("treetop", None).unwrap().size, Some((900, 700)));
        assert!(mem.get("treetop", Some("treetop")).unwrap().docked);
        assert!(!dir.join("windows.json").exists());
        assert!(dir.join("windows.json.imported").exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn keeps_the_newest_titles() {
        let dir = std::env::temp_dir().join(format!("arbor-wm-titles-{}", std::process::id()));
        let path = dir.join("windows.db");
        let mut mem = WindowMemory::load(Some(path.clone()));
        for i in 0..(TITLES_PER_APP + 3) {
            mem.record("app", Some(&format!("t{i}")), floating());
        }
        mem.flush();
        let again = WindowMemory::load(Some(path));
        assert_eq!(again.titles["app"].len(), TITLES_PER_APP);
        assert_eq!(again.titles["app"][0].0, "t3");
        let _ = std::fs::remove_dir_all(dir);
    }
}

use smithay::desktop::Window;
use smithay::wayland::shell::xdg::ToplevelSurface;

use crate::arbor::State;
use crate::utils::with_toplevel_role;

fn app_id_of(toplevel: &ToplevelSurface) -> String {
    with_toplevel_role(toplevel, |role| role.app_id.clone()).unwrap_or_default()
}

fn title_of(toplevel: &ToplevelSurface) -> Option<String> {
    with_toplevel_role(toplevel, |role| role.title.clone())
}

impl State {
    /// Before a toplevel's initial configure: attach where its app's window was last time.
    pub fn window_memory_prepare(&mut self, toplevel: &ToplevelSurface) {
        let app_id = app_id_of(toplevel);
        let placement = if app_id.is_empty() || toplevel.parent().is_some() {
            None
        } else {
            let title = title_of(toplevel);
            set_opening_title(toplevel.wl_surface(), title.clone());
            self.arbor
                .window_memory
                .get(&app_id, title.as_deref())
                .cloned()
                .map(|mut p| {
                    // Another window of this app is open: only the size, or the new one would sit
                    // exactly on top of it.
                    let other_open = self.arbor.layout.windows().any(|(_, mapped)| {
                        mapped.toplevel() != toplevel && app_id_of(mapped.toplevel()) == app_id
                    });
                    if other_open {
                        p.pos = None;
                    }
                    p
                })
        };
        set_for_surface(toplevel.wl_surface(), placement);
    }

    /// Remembers where `window` is (before it closes or unmaps).
    pub fn window_memory_record(&mut self, window: &Window) {
        let Some(toplevel) = window.toplevel() else {
            return;
        };
        if toplevel.parent().is_some() {
            return;
        }
        let app_id = app_id_of(toplevel);
        if app_id.is_empty() {
            return;
        }
        let Some(mut placement) = self.arbor.layout.window_placement(window) else {
            return;
        };
        let title = opening_title(toplevel.wl_surface()).or_else(|| title_of(toplevel));
        // Docked now: keep the last floating geometry for when it floats again.
        if placement.docked {
            if let Some(prev) = self.arbor.window_memory.get(&app_id, title.as_deref()) {
                placement.pos = prev.pos;
                placement.size = prev.size;
            }
        }
        self.arbor
            .window_memory
            .record(&app_id, title.as_deref(), placement);
    }

    /// Remembers every open window (arbor is quitting or restarting).
    pub fn window_memory_record_all(&mut self) {
        let windows: Vec<Window> = self
            .arbor
            .layout
            .windows()
            .map(|(_, mapped)| mapped.window.clone())
            .collect();
        for window in windows {
            self.window_memory_record(&window);
        }
    }}
