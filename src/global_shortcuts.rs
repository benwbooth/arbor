//! Global shortcuts that apps register through the
//! `org.freedesktop.portal.GlobalShortcuts` portal.
//!
//! arbor implements the portal backend itself (see `dbus/portal_global_shortcuts.rs`) because it
//! owns the keyboard. This module holds the compositor-side state: the assignments persisted per
//! app id in `$XDG_CONFIG_HOME/arbor/global-shortcuts.json`, the open portal sessions, and the
//! keys currently held down for an activated shortcut.
//!
//! Config binds always win: a portal shortcut is only assigned a trigger that no config bind
//! uses, and at runtime it is only checked after no config bind matched.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use arbor_config::binds::{Bind, Key, Modifiers, Trigger};
use arbor_config::ModKey;
use serde::{Deserialize, Serialize};
use smithay::input::keyboard::{xkb, Keycode, Keysym};

/// A shortcut as the portal reports it back to apps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShortcutInfo {
    pub id: String,
    pub description: String,
    /// Human-readable trigger, empty when unassigned.
    pub trigger_description: String,
}

/// Events from the compositor to the portal D-Bus service.
#[derive(Debug, Clone)]
pub enum GlobalShortcutsEvent {
    Activated {
        session: String,
        id: String,
        timestamp: u64,
    },
    Deactivated {
        session: String,
        id: String,
        timestamp: u64,
    },
    ShortcutsChanged {
        session: String,
        shortcuts: Vec<ShortcutInfo>,
    },
}

/// One persisted shortcut.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredShortcut {
    #[serde(default)]
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_trigger: Option<String>,
    /// Assigned trigger in arbor key syntax (e.g. `Ctrl+Alt+R`), `None` when unassigned.
    #[serde(default)]
    pub trigger: Option<String>,
    /// The user cleared the trigger on purpose: don't assign the preferred trigger again.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub user_cleared: bool,
}

/// The persisted store: app id → shortcut id → shortcut.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Store {
    #[serde(default)]
    pub apps: BTreeMap<String, BTreeMap<String, StoredShortcut>>,
}

impl Store {
    pub fn load(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => match serde_json::from_str(&text) {
                Ok(store) => store,
                Err(err) => {
                    warn!("error parsing {path:?}: {err}; starting with no portal shortcuts");
                    Self::default()
                }
            },
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(err) => {
                warn!("error reading {path:?}: {err}");
                Self::default()
            }
        }
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(self)?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, text + "\n")?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
}

/// Error from changing an assignment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssignError {
    NoSuchShortcut,
    InvalidTrigger(String),
    /// Clashes with a config bind.
    ConfigBind(String),
    /// Clashes with another portal shortcut (app id, shortcut id).
    OtherShortcut(String, String),
}

impl std::fmt::Display for AssignError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AssignError::NoSuchShortcut => write!(f, "no such portal shortcut"),
            AssignError::InvalidTrigger(t) => write!(f, "invalid trigger: {t}"),
            AssignError::ConfigBind(k) => write!(f, "{k} is already bound in the arbor config"),
            AssignError::OtherShortcut(app, id) => {
                write!(f, "already assigned to shortcut \"{id}\" of app \"{app}\"")
            }
        }
    }
}

/// Expands the `Mod` modifier into the actual mod key so that keys can be compared.
pub fn normalized_modifiers(modifiers: Modifiers, mod_key: ModKey) -> Modifiers {
    if modifiers.contains(Modifiers::COMPOSITOR) {
        (modifiers - Modifiers::COMPOSITOR) | mod_key.to_modifiers()
    } else {
        modifiers
    }
}

/// Whether `key` is triggered by `trigger` with `pressed` modifiers (taken from the XKB state, so
/// never containing `COMPOSITOR`).
pub fn key_matches(key: &Key, mod_key: ModKey, trigger: Trigger, pressed: Modifiers) -> bool {
    key.trigger == trigger
        && normalized_modifiers(key.modifiers, mod_key) == normalized_modifiers(pressed, mod_key)
}

fn keys_equal(a: &Key, b: &Key, mod_key: ModKey) -> bool {
    a.trigger == b.trigger
        && normalized_modifiers(a.modifiers, mod_key) == normalized_modifiers(b.modifiers, mod_key)
}

/// Parses a trigger, either in the XDG shortcuts spec format (`CTRL+ALT+r`, `LOGO+F1`) or in
/// arbor's key syntax (`Ctrl+Alt+R`, `Mod+Shift+T`). Only keyboard keys are accepted.
pub fn parse_trigger(s: &str) -> Option<Key> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }

    let mut parts: Vec<&str> = s.split('+').map(str::trim).collect();
    let key = parts.pop()?;
    if key.is_empty() {
        return None;
    }

    let mut normalized = Vec::new();
    for part in parts {
        let name = match part.to_ascii_uppercase().as_str() {
            "CTRL" | "CONTROL" => "Ctrl",
            "ALT" => "Alt",
            "SHIFT" => "Shift",
            "LOGO" | "SUPER" | "META" | "WIN" => "Super",
            "MOD" => "Mod",
            // NumLock as a modifier can't be expressed in arbor binds.
            "NUM" => return None,
            _ => part,
        };
        normalized.push(name);
    }
    normalized.push(key);

    let parsed: Key = normalized.join("+").parse().ok()?;
    match parsed.trigger {
        Trigger::Keysym(_) => Some(parsed),
        _ => None,
    }
}

/// Formats a key in arbor key syntax (which [`parse_trigger`] reads back).
pub fn format_key(key: &Key) -> String {
    let mut out = String::new();
    let m = key.modifiers;
    for (flag, name) in [
        (Modifiers::COMPOSITOR, "Mod"),
        (Modifiers::CTRL, "Ctrl"),
        (Modifiers::ALT, "Alt"),
        (Modifiers::SHIFT, "Shift"),
        (Modifiers::SUPER, "Super"),
        (Modifiers::ISO_LEVEL3_SHIFT, "ISO_Level3_Shift"),
        (Modifiers::ISO_LEVEL5_SHIFT, "ISO_Level5_Shift"),
    ] {
        if m.contains(flag) {
            out.push_str(name);
            out.push('+');
        }
    }
    let name = match key.trigger {
        Trigger::Keysym(sym) => {
            let name = xkb::keysym_get_name(sym);
            if name.len() == 1 {
                name.to_ascii_uppercase()
            } else {
                name
            }
        }
        _ => String::from("?"),
    };
    out.push_str(&name);
    out
}

#[derive(Debug)]
struct Session {
    app_id: String,
    /// Shortcut ids this session listens to.
    active: Vec<String>,
}

pub struct GlobalShortcuts {
    path: Option<PathBuf>,
    store: Option<Store>,
    sessions: HashMap<String, Session>,
    /// Parsed triggers of all assigned shortcuts: (key, app id, shortcut id).
    parsed: Option<Vec<(Key, String, String)>>,
    /// Keys held down for an activated shortcut → (session, shortcut id).
    pressed: HashMap<Keycode, Vec<(String, String)>>,
    events: Option<async_channel::Sender<GlobalShortcutsEvent>>,
}

impl Default for GlobalShortcuts {
    fn default() -> Self {
        let path = directories::ProjectDirs::from("", "", "arbor")
            .map(|dirs| dirs.config_dir().join("global-shortcuts.json"));
        Self::with_path(path)
    }
}

impl GlobalShortcuts {
    /// Creates the state with the given store location (`None`: don't persist).
    pub fn with_path(path: Option<PathBuf>) -> Self {
        Self {
            path,
            store: None,
            sessions: HashMap::new(),
            parsed: None,
            pressed: HashMap::new(),
            events: None,
        }
    }

    pub fn set_event_sender(&mut self, sender: async_channel::Sender<GlobalShortcutsEvent>) {
        self.events = Some(sender);
    }

    fn store(&mut self) -> &mut Store {
        let path = self.path.clone();
        self.store
            .get_or_insert_with(|| path.map(|p| Store::load(&p)).unwrap_or_default())
    }

    fn save(&mut self) {
        self.parsed = None;
        let Some(path) = self.path.clone() else {
            return;
        };
        let store = self.store().clone();
        if let Err(err) = store.save(&path) {
            warn!("error saving portal shortcuts to {path:?}: {err:?}");
        }
    }

    fn send(&self, event: GlobalShortcutsEvent) {
        if let Some(events) = &self.events {
            let _ = events.try_send(event);
        }
    }

    fn info(&mut self, app_id: &str, id: &str) -> ShortcutInfo {
        let stored = self
            .store()
            .apps
            .get(app_id)
            .and_then(|app| app.get(id))
            .cloned()
            .unwrap_or_default();
        ShortcutInfo {
            id: id.to_owned(),
            description: stored.description,
            trigger_description: stored.trigger.unwrap_or_default(),
        }
    }

    /// Finds what `key` would clash with, ignoring the shortcut (`app_id`, `id`) itself.
    pub fn find_conflict<'a>(
        &mut self,
        key: &Key,
        app_id: &str,
        id: &str,
        binds: impl IntoIterator<Item = &'a Bind>,
        mod_key: ModKey,
    ) -> Option<AssignError> {
        for bind in binds {
            if keys_equal(&bind.key, key, mod_key) {
                return Some(AssignError::ConfigBind(format_key(key)));
            }
        }
        for (app, shortcuts) in &self.store().apps {
            for (other_id, shortcut) in shortcuts {
                if app == app_id && other_id == id {
                    continue;
                }
                let Some(other) = shortcut.trigger.as_deref().and_then(parse_trigger) else {
                    continue;
                };
                if keys_equal(&other, key, mod_key) {
                    return Some(AssignError::OtherShortcut(app.clone(), other_id.clone()));
                }
            }
        }
        None
    }

    /// A portal session was created: it listens to all shortcuts persisted for the app.
    pub fn create_session(&mut self, session: String, app_id: String) {
        let active = self
            .store()
            .apps
            .get(&app_id)
            .map(|app| app.keys().cloned().collect())
            .unwrap_or_default();
        self.sessions.insert(session, Session { app_id, active });
    }

    pub fn close_session(&mut self, session: &str) {
        self.sessions.remove(session);
        for held in self.pressed.values_mut() {
            held.retain(|(s, _)| s != session);
        }
        self.pressed.retain(|_, held| !held.is_empty());
    }

    /// BindShortcuts: (id, description, preferred trigger). Returns `None` for an unknown session.
    pub fn bind<'a>(
        &mut self,
        session: &str,
        shortcuts: Vec<(String, String, Option<String>)>,
        binds: impl IntoIterator<Item = &'a Bind> + Clone,
        mod_key: ModKey,
    ) -> Option<Vec<ShortcutInfo>> {
        let app_id = self.sessions.get(session)?.app_id.clone();

        for (id, description, preferred) in &shortcuts {
            let entry = self
                .store()
                .apps
                .entry(app_id.clone())
                .or_default()
                .entry(id.clone())
                .or_default();
            entry.description.clone_from(description);
            entry.preferred_trigger = preferred.clone().filter(|p| !p.is_empty());
            let want = (entry.trigger.is_none() && !entry.user_cleared)
                .then(|| entry.preferred_trigger.as_deref().and_then(parse_trigger))
                .flatten();

            if let Some(key) = want {
                if let Some(conflict) =
                    self.find_conflict(&key, &app_id, id, binds.clone(), mod_key)
                {
                    debug!("not assigning {id} of {app_id:?}: {conflict}");
                } else {
                    let entry = self
                        .store()
                        .apps
                        .get_mut(&app_id)
                        .and_then(|app| app.get_mut(id))
                        .unwrap();
                    entry.trigger = Some(format_key(&key));
                }
            }
        }
        self.save();

        let ids: Vec<String> = shortcuts.into_iter().map(|(id, _, _)| id).collect();
        let infos = ids.iter().map(|id| self.info(&app_id, id)).collect();
        self.sessions.get_mut(session).unwrap().active = ids;
        Some(infos)
    }

    /// ListShortcuts. Returns `None` for an unknown session.
    pub fn list(&mut self, session: &str) -> Option<Vec<ShortcutInfo>> {
        let session = self.sessions.get(session)?;
        let app_id = session.app_id.clone();
        let ids = session.active.clone();
        Some(ids.iter().map(|id| self.info(&app_id, id)).collect())
    }

    /// All persisted shortcuts for IPC.
    pub fn all(&mut self) -> Vec<arbor_ipc::GlobalShortcut> {
        let active: Vec<(String, String)> = self
            .sessions
            .values()
            .flat_map(|s| s.active.iter().map(|id| (s.app_id.clone(), id.clone())))
            .collect();
        let mut out = Vec::new();
        for (app_id, shortcuts) in &self.store().apps {
            for (id, s) in shortcuts {
                out.push(arbor_ipc::GlobalShortcut {
                    app_id: app_id.clone(),
                    id: id.clone(),
                    description: s.description.clone(),
                    trigger: s.trigger.clone(),
                    preferred_trigger: s.preferred_trigger.clone(),
                    active: active.contains(&(app_id.clone(), id.clone())),
                });
            }
        }
        out
    }

    /// Changes or clears (`trigger: None`) an assignment and notifies the app's sessions.
    pub fn set_trigger<'a>(
        &mut self,
        app_id: &str,
        id: &str,
        trigger: Option<&str>,
        binds: impl IntoIterator<Item = &'a Bind>,
        mod_key: ModKey,
    ) -> Result<(), AssignError> {
        if !self
            .store()
            .apps
            .get(app_id)
            .is_some_and(|app| app.contains_key(id))
        {
            return Err(AssignError::NoSuchShortcut);
        }

        let new = match trigger {
            Some(t) => {
                let key =
                    parse_trigger(t).ok_or_else(|| AssignError::InvalidTrigger(t.to_owned()))?;
                if let Some(conflict) = self.find_conflict(&key, app_id, id, binds, mod_key) {
                    return Err(conflict);
                }
                Some(format_key(&key))
            }
            None => None,
        };

        let entry = self
            .store()
            .apps
            .get_mut(app_id)
            .unwrap()
            .get_mut(id)
            .unwrap();
        entry.user_cleared = new.is_none();
        entry.trigger = new;
        self.save();

        let sessions: Vec<(String, Vec<String>)> = self
            .sessions
            .iter()
            .filter(|(_, s)| s.app_id == app_id && s.active.iter().any(|a| a == id))
            .map(|(handle, s)| (handle.clone(), s.active.clone()))
            .collect();
        for (session, active) in sessions {
            let shortcuts = active.iter().map(|a| self.info(app_id, a)).collect();
            self.send(GlobalShortcutsEvent::ShortcutsChanged { session, shortcuts });
        }
        Ok(())
    }

    /// Sessions that should get Activated for this key press.
    fn matching(
        &mut self,
        mod_key: ModKey,
        sym: Keysym,
        modifiers: Modifiers,
    ) -> Vec<(String, String)> {
        if self.sessions.is_empty() {
            return Vec::new();
        }
        if self.parsed.is_none() {
            let parsed = self
                .store()
                .apps
                .iter()
                .flat_map(|(app, shortcuts)| {
                    shortcuts.iter().filter_map(move |(id, s)| {
                        let key = s.trigger.as_deref().and_then(parse_trigger)?;
                        Some((key, app.clone(), id.clone()))
                    })
                })
                .collect();
            self.parsed = Some(parsed);
        }

        let trigger = Trigger::Keysym(sym);
        let mut out = Vec::new();
        for (key, app, id) in self.parsed.as_ref().unwrap() {
            if !key_matches(key, mod_key, trigger, modifiers) {
                continue;
            }
            for (handle, session) in &self.sessions {
                if &session.app_id == app && session.active.contains(id) {
                    out.push((handle.clone(), id.clone()));
                }
            }
        }
        out
    }

    /// A key was pressed and no config bind took it. Returns whether a portal shortcut did (the
    /// key must then be suppressed).
    pub fn press(
        &mut self,
        key_code: Keycode,
        mod_key: ModKey,
        sym: Keysym,
        modifiers: Modifiers,
        time_msec: u32,
    ) -> bool {
        let matched = self.matching(mod_key, sym, modifiers);
        if matched.is_empty() {
            return false;
        }
        for (session, id) in &matched {
            self.send(GlobalShortcutsEvent::Activated {
                session: session.clone(),
                id: id.clone(),
                timestamp: u64::from(time_msec),
            });
        }
        self.pressed.insert(key_code, matched);
        true
    }

    /// A key was released. Returns whether it had activated a portal shortcut.
    pub fn release(&mut self, key_code: Keycode, time_msec: u32) -> bool {
        let Some(held) = self.pressed.remove(&key_code) else {
            return false;
        };
        for (session, id) in held {
            self.send(GlobalShortcutsEvent::Deactivated {
                session,
                id,
                timestamp: u64::from(time_msec),
            });
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use arbor_config::Action;

    use super::*;

    fn key(s: &str) -> Key {
        s.parse().unwrap()
    }

    fn bind(s: &str) -> Bind {
        Bind {
            key: key(s),
            action: Action::Screenshot(true, None),
            repeat: true,
            cooldown: None,
            allow_when_locked: false,
            allow_inhibiting: true,
            hotkey_overlay_title: None,
        }
    }

    fn temp_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "arbor-gs-test-{}-{name}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        dir.join("global-shortcuts.json")
    }

    #[test]
    fn parses_xdg_format() {
        assert_eq!(parse_trigger("CTRL+ALT+r"), Some(key("Ctrl+Alt+R")));
        assert_eq!(parse_trigger("LOGO+SHIFT+F1"), Some(key("Super+Shift+F1")));
        assert_eq!(parse_trigger("ctrl+Return"), Some(key("Ctrl+Return")));
        assert_eq!(parse_trigger("Pause"), Some(key("Pause")));
    }

    #[test]
    fn parses_arbor_format() {
        assert_eq!(parse_trigger("Mod+Shift+T"), Some(key("Mod+Shift+T")));
        assert_eq!(parse_trigger(" Super + space "), Some(key("Super+space")));
    }

    #[test]
    fn rejects_invalid_triggers() {
        assert_eq!(parse_trigger(""), None);
        assert_eq!(parse_trigger("CTRL+"), None);
        assert_eq!(parse_trigger("NUM+KP_1"), None);
        assert_eq!(parse_trigger("HYPER+a"), None);
        assert_eq!(parse_trigger("Ctrl+NotAKeyName"), None);
        assert_eq!(parse_trigger("Mod+MouseLeft"), None);
    }

    #[test]
    fn format_round_trips() {
        for s in [
            "Ctrl+Alt+R",
            "Mod+Shift+T",
            "Super+F1",
            "Ctrl+Return",
            "Alt+space",
        ] {
            let k = key(s);
            assert_eq!(format_key(&k), s);
            assert_eq!(parse_trigger(&format_key(&k)), Some(k));
        }
    }

    #[test]
    fn key_matching_handles_mod_key() {
        let a = Keysym::a;
        assert!(key_matches(
            &key("Mod+A"),
            ModKey::Super,
            Trigger::Keysym(a),
            Modifiers::SUPER
        ));
        assert!(key_matches(
            &key("Super+A"),
            ModKey::Super,
            Trigger::Keysym(a),
            Modifiers::SUPER
        ));
        assert!(!key_matches(
            &key("Mod+A"),
            ModKey::Alt,
            Trigger::Keysym(a),
            Modifiers::SUPER
        ));
        assert!(!key_matches(
            &key("Ctrl+A"),
            ModKey::Super,
            Trigger::Keysym(a),
            Modifiers::CTRL | Modifiers::SHIFT
        ));
        assert!(!key_matches(
            &key("Ctrl+A"),
            ModKey::Super,
            Trigger::Keysym(Keysym::b),
            Modifiers::CTRL
        ));
    }

    #[test]
    fn persistence_round_trip() {
        let path = temp_path("persist");
        let mut gs = GlobalShortcuts::with_path(Some(path.clone()));
        gs.create_session("/s/1".into(), "org.example.App".into());
        let infos = gs
            .bind(
                "/s/1",
                vec![
                    (
                        "talk".into(),
                        "Push to talk".into(),
                        Some("CTRL+ALT+t".into()),
                    ),
                    ("mute".into(), "Mute".into(), None),
                ],
                &[] as &[Bind],
                ModKey::Super,
            )
            .unwrap();
        assert_eq!(infos[0].trigger_description, "Ctrl+Alt+T");
        assert_eq!(infos[1].trigger_description, "");

        // A fresh instance reads the same assignments back, and a new session of that app gets
        // all of them.
        let mut gs2 = GlobalShortcuts::with_path(Some(path.clone()));
        gs2.create_session("/s/2".into(), "org.example.App".into());
        let listed = gs2.list("/s/2").unwrap();
        assert_eq!(listed.len(), 2);
        let talk = listed.iter().find(|i| i.id == "talk").unwrap();
        assert_eq!(talk.description, "Push to talk");
        assert_eq!(talk.trigger_description, "Ctrl+Alt+T");

        // Clearing sticks, even when the app asks for its preferred trigger again.
        gs2.set_trigger(
            "org.example.App",
            "talk",
            None,
            &[] as &[Bind],
            ModKey::Super,
        )
        .unwrap();
        let infos = gs2
            .bind(
                "/s/2",
                vec![(
                    "talk".into(),
                    "Push to talk".into(),
                    Some("CTRL+ALT+t".into()),
                )],
                &[] as &[Bind],
                ModKey::Super,
            )
            .unwrap();
        assert_eq!(infos[0].trigger_description, "");
        let reloaded = Store::load(&path);
        assert!(reloaded.apps["org.example.App"]["talk"].user_cleared);

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn config_bind_conflict_leaves_unassigned() {
        let mut gs = GlobalShortcuts::with_path(None);
        gs.create_session("/s/1".into(), "app".into());
        let binds = [bind("Mod+T")];
        let infos = gs
            .bind(
                "/s/1",
                vec![("x".into(), "X".into(), Some("LOGO+t".into()))],
                &binds,
                ModKey::Super,
            )
            .unwrap();
        assert_eq!(infos[0].trigger_description, "");
        assert!(matches!(
            gs.set_trigger("app", "x", Some("Super+T"), &binds, ModKey::Super),
            Err(AssignError::ConfigBind(_))
        ));
        // With Alt as the mod key, Super+T is free.
        gs.set_trigger("app", "x", Some("Super+T"), &binds, ModKey::Alt)
            .unwrap();
    }

    #[test]
    fn other_app_conflict_leaves_unassigned() {
        let mut gs = GlobalShortcuts::with_path(None);
        gs.create_session("/s/1".into(), "one".into());
        gs.create_session("/s/2".into(), "two".into());
        let no_binds: &[Bind] = &[];
        let a = gs
            .bind(
                "/s/1",
                vec![("a".into(), "A".into(), Some("CTRL+F5".into()))],
                no_binds,
                ModKey::Super,
            )
            .unwrap();
        assert_eq!(a[0].trigger_description, "Ctrl+F5");
        let b = gs
            .bind(
                "/s/2",
                vec![("b".into(), "B".into(), Some("Ctrl+F5".into()))],
                no_binds,
                ModKey::Super,
            )
            .unwrap();
        assert_eq!(b[0].trigger_description, "");
        assert_eq!(
            gs.set_trigger("two", "b", Some("CTRL+F5"), no_binds, ModKey::Super),
            Err(AssignError::OtherShortcut("one".into(), "a".into()))
        );
        assert_eq!(
            gs.set_trigger("two", "nope", Some("CTRL+F6"), no_binds, ModKey::Super),
            Err(AssignError::NoSuchShortcut)
        );
    }

    #[test]
    fn press_and_release_emit_events() {
        let (tx, rx) = async_channel::unbounded();
        let mut gs = GlobalShortcuts::with_path(None);
        gs.set_event_sender(tx);
        gs.create_session("/s/1".into(), "app".into());
        gs.bind(
            "/s/1",
            vec![("x".into(), "X".into(), Some("CTRL+ALT+x".into()))],
            &[] as &[Bind],
            ModKey::Super,
        );

        let code = Keycode::new(53);
        assert!(!gs.press(code, ModKey::Super, Keysym::x, Modifiers::CTRL, 10));
        assert!(gs.press(
            code,
            ModKey::Super,
            Keysym::x,
            Modifiers::CTRL | Modifiers::ALT,
            10
        ));
        assert!(matches!(
            rx.try_recv().unwrap(),
            GlobalShortcutsEvent::Activated { ref session, ref id, timestamp: 10 }
                if session == "/s/1" && id == "x"
        ));
        assert!(gs.release(code, 20));
        assert!(matches!(
            rx.try_recv().unwrap(),
            GlobalShortcutsEvent::Deactivated { timestamp: 20, .. }
        ));
        assert!(!gs.release(code, 30));

        // Changing the assignment notifies the session.
        gs.set_trigger("app", "x", Some("Ctrl+F9"), &[] as &[Bind], ModKey::Super)
            .unwrap();
        assert!(matches!(
            rx.try_recv().unwrap(),
            GlobalShortcutsEvent::ShortcutsChanged { ref shortcuts, .. }
                if shortcuts[0].trigger_description == "Ctrl+F9"
        ));

        // Closed sessions get nothing.
        gs.close_session("/s/1");
        assert!(!gs.press(code, ModKey::Super, Keysym::F9, Modifiers::CTRL, 40));
    }
}
