//! arbor: every app opens where it was last time.
//!
//! When a window closes, hides (unmaps) or arbor quits, its app's placement is stored in
//! `$XDG_STATE_HOME/arbor/windows.json` (`~/.local/state/arbor/windows.json`): the output, whether
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

use arbor_config::utils::FloatOrInt;
use arbor_config::{FloatingPosition, PresetSize, RelativeTo};
use serde::{Deserialize, Serialize};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::compositor::with_states;

use crate::window::ResolvedWindowRules;

const VERSION: u32 = 1;

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

#[derive(Debug, Default, Serialize, Deserialize)]
struct File {
    version: u32,
    #[serde(default)]
    apps: BTreeMap<String, Placement>,
    /// Per app, the last placement of each window title, oldest first.
    #[serde(default)]
    titles: BTreeMap<String, Vec<(String, Placement)>>,
}

/// The stored placements.
#[derive(Debug, Default)]
pub struct WindowMemory {
    path: Option<PathBuf>,
    apps: BTreeMap<String, Placement>,
    titles: BTreeMap<String, Vec<(String, Placement)>>,
}

/// `$XDG_STATE_HOME/arbor/windows.json`.
pub fn default_path() -> Option<PathBuf> {
    let state = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".local/state")))?;
    Some(state.join("arbor").join("windows.json"))
}

impl WindowMemory {
    /// Loads the store at `path` (`None`: in memory only, for tests).
    pub fn load(path: Option<PathBuf>) -> Self {
        let file = path
            .as_deref()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|data| match serde_json::from_slice::<File>(&data) {
                Ok(file) if file.version == VERSION => Some(file),
                Ok(file) => {
                    warn!("window memory: unsupported version {}", file.version);
                    None
                }
                Err(err) => {
                    warn!("window memory: couldn't read the store: {err}");
                    None
                }
            })
            .unwrap_or_default();
        Self {
            path,
            apps: file.apps,
            titles: file.titles,
        }
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

    /// Stores `placement` for `app_id` and its window `title` (and writes the file when it
    /// changed).
    pub fn record(&mut self, app_id: &str, title: Option<&str>, placement: Placement) {
        if app_id.is_empty() {
            return;
        }
        let mut changed = false;
        if self.apps.get(app_id) != Some(&placement) {
            self.apps.insert(app_id.to_owned(), placement.clone());
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
                if list.len() > TITLES_PER_APP {
                    list.remove(0);
                }
                changed = true;
            }
        }
        if changed {
            debug!("window memory: {app_id} {title:?} -> {placement:?}");
            self.save();
        }
    }

    fn save(&self) {
        let Some(path) = &self.path else {
            return;
        };
        let file = File {
            version: VERSION,
            apps: self.apps.clone(),
            titles: self.titles.clone(),
        };
        let res = (|| -> anyhow::Result<()> {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let tmp = path.with_extension("json.tmp");
            std::fs::write(&tmp, serde_json::to_vec_pretty(&file)?)?;
            std::fs::rename(&tmp, path)?;
            Ok(())
        })();
        if let Err(err) = res {
            warn!("window memory: couldn't write {}: {err}", path.display());
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
        let path = dir.join("windows.json");
        let mut mem = WindowMemory::load(Some(path.clone()));
        mem.record("org.example.App", Some("Main"), floating());
        let settings = Placement {
            pos: Some((5., 5.)),
            ..floating()
        };
        mem.record("org.example.App", Some("Settings"), settings.clone());
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
    }
}
