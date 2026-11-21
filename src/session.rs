//! Plasma-style session restore.
//!
//! On quit (and every few minutes while running) arbor saves the running apps to
//! `$XDG_STATE_HOME/arbor/session.json` (`~/.local/state/arbor/session.json`): for each window
//! its app id, how to launch it (the app's desktop entry, or the process command line), its
//! workspace, its dock tree slot (the whole tree shape with splits, fractions and tab groups) or
//! floating geometry, and maximized/fullscreen/minimized. At the next session start
//! (`session-restore { on; }`, the default) the apps are launched again, one launch per saved
//! process (an app with several windows in one process gets one launch and is expected to
//! reopen its windows itself, as browsers do), and each window that maps is matched by app id,
//! in saved order, and put back in its place. Windows that don't show up within
//! `RESTORE_TIMEOUT` are forgotten.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use arbor_ipc::PositionChange;
use calloop::timer::{TimeoutAction, Timer};
use serde::{Deserialize, Serialize};
use smithay::desktop::Window;
use smithay::output::Output;

use crate::arbor::State;
use crate::ipc::server::make_ipc_window;
use crate::layout::dock::DockShape;
use crate::layout::{ActivateWindow, LayoutElement as _};
use crate::utils::spawning::spawn;
use crate::utils::with_toplevel_role;

pub const SESSION_VERSION: u32 = 1;
/// How long relaunched apps get to map their windows.
const RESTORE_TIMEOUT: Duration = Duration::from_secs(90);
/// Delay after start before relaunching, so that the shell and portals come up first.
const STARTUP_DELAY: Duration = Duration::from_secs(2);
const AUTOSAVE_INTERVAL: Duration = Duration::from_secs(180);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedWorkspaceRef {
    /// Output (connector) name.
    pub output: Option<String>,
    /// Index of the workspace on its output, from 0.
    pub index: usize,
    /// Name of a named workspace.
    pub name: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SavedGeometry {
    pub x: f64,
    pub y: f64,
    pub width: i32,
    pub height: i32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedWindow {
    /// Id of this entry within the file (dock trees refer to it).
    pub slot: u32,
    pub app_id: String,
    pub title: Option<String>,
    /// Windows with the same `launch_group` came from one process and share one launch.
    pub launch_group: u32,
    /// Command to launch the app (desktop entry `Exec`, or the process command line).
    pub command: Vec<String>,
    /// Desktop entry id the command came from, if any.
    pub desktop_entry: Option<String>,
    pub workspace: SavedWorkspaceRef,
    /// Floating geometry (workspace view coordinates); `None` for docked windows.
    pub floating: Option<SavedGeometry>,
    pub maximized: bool,
    pub fullscreen: bool,
    pub minimized: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedWorkspace {
    pub workspace: SavedWorkspaceRef,
    /// Dock tree with window slots as leaves.
    pub tree: Option<DockShape<u32>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct SessionFile {
    pub version: u32,
    pub windows: Vec<SavedWindow>,
    pub workspaces: Vec<SavedWorkspace>,
}

impl SessionFile {
    /// The launches to do: one per launch group, in saved order.
    pub fn launch_commands(&self) -> Vec<Vec<String>> {
        let mut seen = Vec::new();
        let mut out = Vec::new();
        for win in &self.windows {
            if win.command.is_empty() || seen.contains(&win.launch_group) {
                continue;
            }
            seen.push(win.launch_group);
            out.push(win.command.clone());
        }
        out
    }
}

/// A restore in progress: saved windows waiting for their app to map them.
#[derive(Debug)]
pub struct PendingRestore {
    pub file: SessionFile,
    pub deadline: Instant,
    /// Slots taken by mapped windows.
    pub claimed: Vec<(u32, Window)>,
}

impl PendingRestore {
    /// Claims the first unclaimed slot for `app_id`.
    pub fn claim(&mut self, app_id: &str, window: &Window) -> Option<SavedWindow> {
        let saved = self
            .file
            .windows
            .iter()
            .find(|w| w.app_id == app_id && !self.claimed.iter().any(|(s, _)| *s == w.slot))?
            .clone();
        self.claimed.push((saved.slot, window.clone()));
        Some(saved)
    }

    pub fn is_complete(&self) -> bool {
        self.claimed.len() >= self.file.windows.len()
    }
}

/// `$XDG_STATE_HOME/arbor/session.json`.
pub fn default_session_path() -> Option<PathBuf> {
    let state = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".local/state")))?;
    Some(state.join("arbor").join("session.json"))
}

pub fn load(path: &Path) -> anyhow::Result<SessionFile> {
    let data = std::fs::read(path)?;
    let file: SessionFile = serde_json::from_slice(&data)?;
    anyhow::ensure!(
        file.version == SESSION_VERSION,
        "unsupported session file version {}",
        file.version
    );
    Ok(file)
}

pub fn store(path: &Path, file: &SessionFile) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(file)?)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Splits a desktop entry `Exec=` value into arguments, dropping field codes (`%f`, `%U`, ...).
pub fn parse_exec(exec: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut has_arg = false;
    let mut chars = exec.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                in_quotes = !in_quotes;
                has_arg = true;
            }
            '\\' if in_quotes => {
                if let Some(next) = chars.next() {
                    cur.push(next);
                }
            }
            ' ' | '\t' if !in_quotes => {
                if has_arg {
                    args.push(std::mem::take(&mut cur));
                    has_arg = false;
                }
            }
            '%' => match chars.next() {
                Some('%') => {
                    cur.push('%');
                    has_arg = true;
                }
                // Field codes expand to nothing for a plain launch.
                Some(_) => (),
                None => (),
            },
            c => {
                cur.push(c);
                has_arg = true;
            }
        }
    }
    if has_arg && !cur.is_empty() {
        args.push(cur);
    }
    args
}

/// Finds the desktop entry for an app id: `<app-id>.desktop` (as is, lowercase) in the
/// application dirs. Returns the entry id and its parsed `Exec`.
pub fn desktop_entry_for(app_id: &str, data_dirs: &[PathBuf]) -> Option<(String, Vec<String>)> {
    let candidates = [app_id.to_owned(), app_id.to_lowercase()];
    for dir in data_dirs {
        for name in &candidates {
            let path = dir.join("applications").join(format!("{name}.desktop"));
            let Ok(contents) = std::fs::read_to_string(&path) else {
                continue;
            };
            let mut in_entry = false;
            for line in contents.lines() {
                let line = line.trim();
                if line.starts_with('[') {
                    in_entry = line == "[Desktop Entry]";
                } else if in_entry {
                    if let Some(exec) = line.strip_prefix("Exec=") {
                        let args = parse_exec(exec.trim());
                        if !args.is_empty() {
                            return Some((name.clone(), args));
                        }
                    }
                }
            }
        }
    }
    None
}

fn proc_cmdline(pid: i32) -> Vec<String> {
    let Ok(data) = std::fs::read(format!("/proc/{pid}/cmdline")) else {
        return Vec::new();
    };
    data.split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect()
}

fn output_name(output: &Output) -> String {
    output.name()
}

impl State {
    /// Collects the current session.
    pub fn session_collect(&self) -> SessionFile {
        let config = self.arbor.config.borrow();
        let excluded = &config.session_restore.exclude_app_ids;
        let layout = &self.arbor.layout;
        let data_dirs = crate::layout::icon_tile::data_dirs();

        // Workspace id -> reference.
        let mut ws_refs = HashMap::new();
        for (mon, idx, ws) in layout.workspaces() {
            ws_refs.insert(
                ws.id(),
                SavedWorkspaceRef {
                    output: mon.map(|mon| output_name(mon.output())),
                    index: idx,
                    name: ws.name().cloned(),
                },
            );
        }

        let mut windows = Vec::new();
        let mut slots: Vec<(Window, u32)> = Vec::new();
        let mut groups: HashMap<i32, u32> = HashMap::new();
        let mut next_group = 0;
        layout.with_windows(|mapped, _output, ws_id, win_layout| {
            let has_parent = with_toplevel_role(mapped.toplevel(), |role| role.parent.is_some());
            if has_parent {
                // Dialogs come back with their app, if at all.
                return;
            }
            let ipc = make_ipc_window(mapped, ws_id, win_layout);
            let Some(app_id) = ipc.app_id.clone().filter(|a| !a.is_empty()) else {
                return;
            };
            if excluded.contains(&app_id) {
                return;
            }
            let Some(workspace) = ws_id.and_then(|id| ws_refs.get(&id)).cloned() else {
                return;
            };

            let (desktop_entry, command) = match desktop_entry_for(&app_id, &data_dirs) {
                Some((id, exec)) => (Some(id), exec),
                None => (None, ipc.pid.map(proc_cmdline).unwrap_or_default()),
            };
            let launch_group = match ipc.pid {
                Some(pid) => *groups.entry(pid).or_insert_with(|| {
                    next_group += 1;
                    next_group
                }),
                None => {
                    next_group += 1;
                    next_group
                }
            };

            let floating = ipc.is_floating.then(|| {
                let (x, y) = ipc.layout.tile_pos_in_workspace_view.unwrap_or_default();
                SavedGeometry {
                    x,
                    y,
                    width: ipc.layout.window_size.0,
                    height: ipc.layout.window_size.1,
                }
            });
            let mode = mapped.pending_sizing_mode();
            let slot = windows.len() as u32;
            slots.push((mapped.window.clone(), slot));
            windows.push(SavedWindow {
                slot,
                app_id,
                title: ipc.title.clone(),
                launch_group,
                command,
                desktop_entry,
                workspace,
                floating,
                maximized: mode.is_maximized(),
                fullscreen: mode.is_fullscreen(),
                minimized: ipc.is_minimized,
            });
        });

        let mut workspaces = Vec::new();
        for (_, _, ws) in layout.workspaces() {
            let Some(shape) = ws.dock_shape() else {
                continue;
            };
            let tree = shape.filter_map(&|win: &Window| {
                slots.iter().find(|(w, _)| w == win).map(|(_, slot)| *slot)
            });
            if tree.is_none() {
                continue;
            }
            let Some(workspace) = ws_refs.get(&ws.id()).cloned() else {
                continue;
            };
            workspaces.push(SavedWorkspace { workspace, tree });
        }

        SessionFile {
            version: SESSION_VERSION,
            windows,
            workspaces,
        }
    }

    /// Saves the session now (`session save`), even if empty.
    pub fn session_save_now(&mut self) {
        let file = self.session_collect();
        self.session_write(&file);
    }

    fn session_write(&self, file: &SessionFile) {
        let Some(path) = self.arbor.session_path.clone() else {
            return;
        };
        match store(&path, file) {
            Ok(()) => debug!(
                "saved session ({} windows) to {}",
                file.windows.len(),
                path.display()
            ),
            Err(err) => warn!("error saving session to {}: {err:?}", path.display()),
        }
    }

    /// Saves the session automatically (on quit, periodically): only with session restore on,
    /// not while a restore is still placing windows, and never overwriting a saved session with
    /// an empty one (at logout the apps may be gone before arbor).
    pub fn session_autosave(&mut self) {
        if self.arbor.config.borrow().session_restore.off || self.arbor.session_restore.is_some() {
            return;
        }
        let file = self.session_collect();
        if file.windows.is_empty() {
            return;
        }
        self.session_write(&file);
    }

    /// Deletes the saved session (`session clear`).
    pub fn session_clear(&mut self) {
        self.arbor.session_restore = None;
        if let Some(path) = &self.arbor.session_path {
            match std::fs::remove_file(path) {
                Ok(()) => info!("cleared saved session"),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => (),
                Err(err) => warn!("error removing {}: {err:?}", path.display()),
            }
        }
    }

    /// Restores the saved session now (`session restore`).
    pub fn session_restore_now(&mut self) {
        let Some(path) = self.arbor.session_path.clone() else {
            return;
        };
        match load(&path) {
            Ok(file) => self.session_restore_from(file, true),
            Err(err) => warn!("error loading session from {}: {err:?}", path.display()),
        }
    }

    /// Starts restoring `file`: relaunches its apps (if `launch`) and places windows as they map.
    pub fn session_restore_from(&mut self, file: SessionFile, launch: bool) {
        let excluded = self
            .arbor
            .config
            .borrow()
            .session_restore
            .exclude_app_ids
            .clone();
        let mut file = file;
        file.windows.retain(|w| !excluded.contains(&w.app_id));
        info!("restoring session: {} windows", file.windows.len());
        if launch {
            for command in file.launch_commands() {
                spawn(command, None);
            }
        }
        self.arbor.session_restore = Some(PendingRestore {
            file,
            deadline: Instant::now() + RESTORE_TIMEOUT,
            claimed: Vec::new(),
        });
    }

    /// At session start: restore after a short delay, then autosave periodically.
    pub fn session_start(&mut self) {
        let off = self.arbor.config.borrow().session_restore.off;
        let _ = self.arbor.event_loop.insert_source(
            Timer::from_duration(STARTUP_DELAY),
            move |_, _, state| {
                if !off {
                    if let Some(path) = state.arbor.session_path.clone() {
                        if path.exists() {
                            state.session_restore_now();
                        }
                    }
                }
                TimeoutAction::Drop
            },
        );
        let _ = self.arbor.event_loop.insert_source(
            Timer::from_duration(AUTOSAVE_INTERVAL),
            |_, _, state| {
                state.session_autosave();
                TimeoutAction::ToDuration(AUTOSAVE_INTERVAL)
            },
        );
    }

    /// A window just mapped: if it belongs to the restoring session, put it in its place.
    pub fn session_restore_place(&mut self, window: &Window) {
        let Some(pending) = &mut self.arbor.session_restore else {
            return;
        };
        if Instant::now() > pending.deadline {
            debug!("session restore timed out");
            self.arbor.session_restore = None;
            return;
        }
        let Some(toplevel) = window.toplevel() else {
            return;
        };
        let (app_id, has_parent) = with_toplevel_role(toplevel, |role| {
            (role.app_id.clone(), role.parent.is_some())
        });
        let Some(app_id) = app_id else { return };
        if has_parent {
            return;
        }
        let Some(saved) = pending.claim(&app_id, window) else {
            return;
        };
        let complete = pending.is_complete();
        self.session_place_window(window, &saved);
        if complete {
            debug!("session restore complete");
            self.arbor.session_restore = None;
        }
    }

    fn session_place_window(&mut self, window: &Window, saved: &SavedWindow) {
        let layout = &mut self.arbor.layout;

        // Workspace: by name, else output + index (clamped; the last one is the empty one).
        let target = layout
            .workspaces()
            .find(|(_, _, ws)| {
                saved.workspace.name.is_some() && ws.name() == saved.workspace.name.as_ref()
            })
            .and_then(|(mon, idx, _)| mon.map(|mon| (mon.output().clone(), idx)))
            .or_else(|| {
                let mon = layout
                    .monitors()
                    .find(|mon| Some(output_name(mon.output())) == saved.workspace.output)
                    .or_else(|| layout.active_monitor_ref())?;
                let count = layout
                    .workspaces()
                    .filter(|(m, _, _)| m.is_some_and(|m| m.output() == mon.output()))
                    .count();
                Some((
                    mon.output().clone(),
                    saved.workspace.index.min(count.saturating_sub(1)),
                ))
            });
        let current = layout
            .workspaces()
            .find(|(_, _, ws)| ws.has_window(window))
            .and_then(|(mon, idx, _)| mon.map(|mon| (mon.output().clone(), idx)));
        if let (Some((output, idx)), Some((cur_output, cur_idx))) = (target, current) {
            if output != cur_output {
                layout.move_to_output(Some(window), &output, Some(idx), ActivateWindow::No);
            } else if idx != cur_idx {
                layout.move_to_workspace(Some(window), idx, ActivateWindow::No);
            }
        }

        match saved.floating {
            Some(geo) => {
                layout.set_window_floating(Some(window), true);
                layout.set_window_width(Some(window), arbor_ipc::SizeChange::SetFixed(geo.width));
                layout.set_window_height(Some(window), arbor_ipc::SizeChange::SetFixed(geo.height));
                layout.move_floating_window(
                    Some(window),
                    PositionChange::SetFixed(geo.x),
                    PositionChange::SetFixed(geo.y),
                    false,
                );
                // Positions are set relative to the working area but reported relative to the
                // view: correct by the difference once.
                let mut pos = None;
                layout.with_windows(|mapped, _, ws_id, l| {
                    if mapped.window == *window {
                        pos = make_ipc_window(mapped, ws_id, l)
                            .layout
                            .tile_pos_in_workspace_view;
                    }
                });
                if let Some((x, y)) = pos {
                    let (dx, dy) = (geo.x - x, geo.y - y);
                    if dx.abs() > 0.5 || dy.abs() > 0.5 {
                        layout.move_floating_window(
                            Some(window),
                            PositionChange::AdjustFixed(dx),
                            PositionChange::AdjustFixed(dy),
                            false,
                        );
                    }
                }
            }
            None => {
                layout.set_window_floating(Some(window), false);
                self.session_apply_trees();
            }
        }

        let layout = &mut self.arbor.layout;
        if saved.fullscreen {
            layout.set_fullscreen(window, true);
        } else if saved.maximized {
            layout.set_maximized(window, true);
        }
        if saved.minimized {
            layout.minimize_window(Some(window));
        }
        self.arbor.queue_redraw_all();
    }

    /// Rebuilds the saved dock trees with the windows claimed so far.
    fn session_apply_trees(&mut self) {
        let Some(pending) = &self.arbor.session_restore else {
            return;
        };
        let claimed = pending.claimed.clone();
        let trees: Vec<DockShape<Window>> = pending
            .file
            .workspaces
            .iter()
            .filter_map(|ws| ws.tree.as_ref())
            .filter_map(|tree| {
                tree.filter_map(&|slot| {
                    claimed
                        .iter()
                        .find(|(s, _)| s == slot)
                        .map(|(_, w)| w.clone())
                })
            })
            .collect();
        for tree in trees {
            let mut ids = Vec::new();
            tree.ids(&mut ids);
            let Some(first) = ids.first() else { continue };
            self.arbor.layout.apply_dock_shape_for(first, &tree);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_parsing() {
        assert_eq!(parse_exec("dolphin %u"), ["dolphin"]);
        assert_eq!(
            parse_exec(r#"env FOO=1 "/opt/My App/app" --flag %F"#),
            ["env", "FOO=1", "/opt/My App/app", "--flag"]
        );
        assert_eq!(parse_exec("echo 100%%"), ["echo", "100%"]);
        assert_eq!(
            parse_exec(r#"sh -c "echo \"hi\"""#),
            ["sh", "-c", r#"echo "hi""#]
        );
    }

    #[test]
    fn launch_commands_once_per_group() {
        let win = |slot, group, cmd: &str| SavedWindow {
            slot,
            app_id: format!("app{group}"),
            title: None,
            launch_group: group,
            command: vec![cmd.to_owned()],
            desktop_entry: None,
            workspace: SavedWorkspaceRef {
                output: None,
                index: 0,
                name: None,
            },
            floating: None,
            maximized: false,
            fullscreen: false,
            minimized: false,
        };
        let file = SessionFile {
            version: SESSION_VERSION,
            windows: vec![win(0, 1, "a"), win(1, 1, "a"), win(2, 2, "b")],
            workspaces: vec![],
        };
        assert_eq!(file.launch_commands(), [vec!["a"], vec!["b"]]);

        let json = serde_json::to_string(&file).unwrap();
        let back: SessionFile = serde_json::from_str(&json).unwrap();
        assert_eq!(back, file);
    }

    #[test]
    fn desktop_entry_lookup() {
        let dir = std::env::temp_dir().join(format!("arbor-session-test-{}", std::process::id()));
        let apps = dir.join("applications");
        std::fs::create_dir_all(&apps).unwrap();
        std::fs::write(
            apps.join("org.kde.dolphin.desktop"),
            "[Desktop Entry]\nName=Dolphin\nExec=dolphin %u\n[Desktop Action new]\nExec=dolphin --new-window\n",
        )
        .unwrap();
        let got = desktop_entry_for("org.kde.dolphin", std::slice::from_ref(&dir));
        assert_eq!(
            got,
            Some(("org.kde.dolphin".to_owned(), vec!["dolphin".to_owned()]))
        );
        assert_eq!(desktop_entry_for("nope", std::slice::from_ref(&dir)), None);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
