//! KDE global shortcuts (`org.kde.kglobalaccel`) as a compatibility layer over arbor's app
//! shortcut store (`crate::global_shortcuts`).
//!
//! KDE/Qt apps register their global actions with KGlobalAccel and wait for
//! `globalShortcutPressed`. Under Plasma, kglobalacceld (inside KWin) grabs the keys; under arbor
//! the compositor owns the keyboard, so it serves that D-Bus API itself (`dbus/kglobalaccel.rs`).
//! Registrations land in the same store as GlobalShortcuts portal shortcuts, as entries with
//! `source: kde` keyed by the KGlobalAccel component name. Keys come from the store, or from the
//! app's default when it has none yet and the key is free. Config binds always win.
//!
//! KGlobalAccel's model, kept here: an action is *fresh* until keys were set for it once (then
//! the app's later "set my shortcut" calls just load the stored keys back), and *present* while
//! the app runs (only present actions take key presses). Components ending in `.desktop` are
//! launchers: their actions start that desktop file (or one of its `[Desktop Action]`s) and are
//! always present. Shortcut contexts and multi-key sequences are not supported.

use std::collections::{HashMap, HashSet};

use arbor_config::binds::{Bind, Key, Modifiers, Trigger};
use arbor_config::ModKey;
use smithay::input::keyboard::{Keycode, Keysym};

use crate::global_shortcuts::{
    format_key, key_matches, normalized_modifiers, parse_trigger, GlobalShortcuts, Source,
    StoredShortcut,
};

pub mod desktop;
pub mod import;
mod kconfig;
pub mod keys;
mod qt_keys;

use keys::{qt_to_key, seq_len, KeySeq};

/// KGlobalAccel's setShortcutKeys flags.
pub const SET_PRESENT: u32 = 2;
pub const NO_AUTOLOADING: u32 = 4;
pub const IS_DEFAULT: u32 = 8;

/// `[component unique, action unique, component friendly, action friendly]`.
pub type ActionId = Vec<String>;

/// KGlobalShortcutInfo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShortcutInfo {
    pub action: String,
    pub action_friendly: String,
    pub component: String,
    pub component_friendly: String,
    pub keys: Vec<KeySeq>,
    pub default_keys: Vec<KeySeq>,
}

/// Events from the compositor to the kglobalaccel D-Bus service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KdeEvent {
    Pressed {
        component: String,
        action: String,
        timestamp: i64,
    },
    Released {
        component: String,
        action: String,
        timestamp: i64,
    },
    /// `yourShortcutsChanged`: the keys of an app's action changed from outside the app.
    ShortcutsChanged { action_id: ActionId, keys: Vec<KeySeq> },
}

#[derive(Default)]
pub struct KdeState {
    present: HashSet<(String, String)>,
    fresh: HashSet<(String, String)>,
    /// Component → D-Bus unique name of the app that registered it.
    owners: HashMap<String, String>,
    blocked: bool,
    pressed: HashMap<Keycode, (String, String)>,
    events: Option<async_channel::Sender<KdeEvent>>,
}

impl KdeState {
    pub fn is_present(&self, component: &str, action: &str) -> bool {
        component.ends_with(".desktop")
            || self
                .present
                .contains(&(component.to_owned(), action.to_owned()))
    }

    fn send(&self, event: KdeEvent) {
        if let Some(events) = &self.events {
            let _ = events.try_send(event);
        }
    }
}

/// `component|context` → `component`.
pub fn component_name(name: &str) -> &str {
    name.split_once('|').map_or(name, |(c, _)| c)
}

fn trigger_keys(trigger: Option<&str>, mod_key: ModKey) -> Vec<KeySeq> {
    trigger
        .and_then(parse_trigger)
        .map(|key| {
            let key = Key {
                trigger: key.trigger,
                modifiers: normalized_modifiers(key.modifiers, mod_key),
            };
            keys::key_to_qt(&key)
        })
        .filter(|k| *k != 0)
        .map(|k| vec![[k, 0, 0, 0]])
        .unwrap_or_default()
}

/// The first key of a KGlobalAccel key list that arbor can bind.
fn first_key(keys: &[KeySeq]) -> Option<Key> {
    keys.iter()
        .filter(|seq| seq_len(seq) == 1)
        .find_map(|seq| qt_to_key(seq[0]))
}

impl GlobalShortcuts {
    pub fn set_kde_event_sender(&mut self, sender: async_channel::Sender<KdeEvent>) {
        self.kde.events = Some(sender);
    }

    fn kde_entry(&mut self, component: &str, action: &str) -> Option<&mut StoredShortcut> {
        self.store()
            .apps
            .get_mut(component)
            .and_then(|app| app.get_mut(action))
            .filter(|s| s.source == Source::Kde)
    }

    fn kde_entry_ref(&mut self, component: &str, action: &str) -> Option<StoredShortcut> {
        self.kde_entry(component, action).map(|s| s.clone())
    }

    /// KDE components in the store: (unique name, friendly name).
    pub fn kde_components(&mut self) -> Vec<(String, String)> {
        self.store()
            .apps
            .iter()
            .filter_map(|(name, shortcuts)| {
                let mut kde = shortcuts.values().filter(|s| s.source == Source::Kde);
                let first = kde.next()?;
                let friendly = if first.app_name.is_empty() {
                    name.clone()
                } else {
                    first.app_name.clone()
                };
                Some((name.clone(), friendly))
            })
            .collect()
    }

    pub fn kde_has_component(&mut self, component: &str) -> bool {
        let component = component_name(component);
        self.store()
            .apps
            .get(component)
            .is_some_and(|app| app.values().any(|s| s.source == Source::Kde))
    }

    /// doRegister. Returns whether this created a new component.
    pub fn kde_register(&mut self, id: &ActionId, owner: Option<String>) -> bool {
        if id.len() < 4 {
            return false;
        }
        let component = component_name(&id[0]).to_owned();
        let action = id[1].clone();
        let new_component = !self.kde_has_component(&component);
        if let Some(owner) = owner {
            self.kde.owners.insert(component.clone(), owner);
        }

        let key = (component.clone(), action.clone());
        let exists = self.kde_entry(&component, &action).is_some();
        if exists {
            let mut changed = false;
            let entry = self.kde_entry(&component, &action).unwrap();
            if !id[3].is_empty() && entry.description != id[3] {
                entry.description.clone_from(&id[3]);
                changed = true;
            }
            if !id[2].is_empty() && entry.app_name != id[2] {
                entry.app_name.clone_from(&id[2]);
                changed = true;
            }
            if changed {
                self.save();
            }
        } else {
            // A portal entry with the same app and id stays a portal entry.
            let taken = self
                .store()
                .apps
                .get(&component)
                .is_some_and(|app| app.contains_key(&action));
            if taken {
                return false;
            }
            self.store()
                .apps
                .entry(component.clone())
                .or_default()
                .insert(
                    action,
                    StoredShortcut {
                        source: Source::Kde,
                        app_name: id[2].clone(),
                        description: id[3].clone(),
                        ..Default::default()
                    },
                );
            self.kde.fresh.insert(key);
            self.save();
        }
        new_component
    }

    /// setShortcutKeys (and the deprecated setShortcut).
    pub fn kde_set_keys<'a>(
        &mut self,
        id: &ActionId,
        keys: &[KeySeq],
        flags: u32,
        binds: impl IntoIterator<Item = &'a Bind>,
        mod_key: ModKey,
    ) -> Vec<KeySeq> {
        if id.len() < 2 {
            return Vec::new();
        }
        let component = component_name(&id[0]).to_owned();
        let action = id[1].clone();
        let Some(entry) = self.kde_entry_ref(&component, &action) else {
            return Vec::new();
        };
        let key = (component.clone(), action.clone());
        let fresh = self.kde.fresh.contains(&key);

        if flags & IS_DEFAULT != 0 {
            let default = first_key(keys).map(|k| format_key(&k));
            let mut changed = entry.preferred_trigger != default;
            let mut assigned = None;
            // Unassigned so far: take the app's default if nobody else has it.
            if entry.trigger.is_none() && !entry.user_cleared {
                if let Some(k) = first_key(keys) {
                    if self
                        .find_conflict(&k, &component, &action, binds, mod_key)
                        .is_none()
                    {
                        assigned = Some(format_key(&k));
                        changed = true;
                    }
                }
            }
            if changed {
                let newly_assigned = assigned.is_some();
                let e = self.kde_entry(&component, &action).unwrap();
                e.preferred_trigger = default;
                if newly_assigned {
                    e.trigger = assigned;
                }
                self.save();
                if newly_assigned && self.kde.present.contains(&key) {
                    self.kde_notify_changed(&component, &action, mod_key);
                }
            }
            return keys.to_vec();
        }

        if flags & NO_AUTOLOADING == 0 && !fresh {
            if flags & SET_PRESENT != 0 {
                self.kde.present.insert(key);
            }
            return trigger_keys(entry.trigger.as_deref(), mod_key);
        }

        let new = first_key(keys).filter(|k| {
            self.find_conflict(k, &component, &action, binds, mod_key)
                .is_none()
        });
        let e = self.kde_entry(&component, &action).unwrap();
        e.trigger = new.map(|k| format_key(&k));
        let trigger = e.trigger.clone();
        self.kde.fresh.remove(&key);
        if flags & SET_PRESENT != 0 {
            self.kde.present.insert(key);
        }
        self.save();
        trigger_keys(trigger.as_deref(), mod_key)
    }

    /// setForeignShortcutKeys: someone else (a settings UI) changes an app's keys; the app is
    /// told with yourShortcutsChanged.
    pub fn kde_set_foreign<'a>(
        &mut self,
        id: &ActionId,
        keys: &[KeySeq],
        binds: impl IntoIterator<Item = &'a Bind>,
        mod_key: ModKey,
    ) {
        if id.len() < 2 || self.kde_entry(component_name(&id[0]), &id[1]).is_none() {
            return;
        }
        let new = self.kde_set_keys(id, keys, NO_AUTOLOADING, binds, mod_key);
        if let Some(e) = self.kde_entry(component_name(&id[0]), &id[1]) {
            // Cleared by the user: the app's default doesn't come back.
            e.user_cleared = new.is_empty();
        }
        self.save();
        self.kde.send(KdeEvent::ShortcutsChanged {
            action_id: id.clone(),
            keys: new,
        });
    }

    /// Tells a running app that the keys of its action changed.
    pub(crate) fn kde_notify_changed(&mut self, component: &str, action: &str, mod_key: ModKey) {
        let Some(entry) = self.kde_entry_ref(component, action) else {
            return;
        };
        self.kde.fresh.remove(&(component.to_owned(), action.to_owned()));
        let keys = trigger_keys(entry.trigger.as_deref(), mod_key);
        self.kde.send(KdeEvent::ShortcutsChanged {
            action_id: vec![
                component.to_owned(),
                action.to_owned(),
                entry.app_name.clone(),
                entry.description.clone(),
            ],
            keys,
        });
    }

    pub fn kde_keys(&mut self, id: &ActionId, mod_key: ModKey) -> Vec<KeySeq> {
        if id.len() < 2 {
            return Vec::new();
        }
        self.kde_entry_ref(component_name(&id[0]), &id[1])
            .map(|e| trigger_keys(e.trigger.as_deref(), mod_key))
            .unwrap_or_default()
    }

    pub fn kde_default_keys(&mut self, id: &ActionId, mod_key: ModKey) -> Vec<KeySeq> {
        if id.len() < 2 {
            return Vec::new();
        }
        self.kde_entry_ref(component_name(&id[0]), &id[1])
            .map(|e| trigger_keys(e.preferred_trigger.as_deref(), mod_key))
            .unwrap_or_default()
    }

    pub fn kde_set_inactive(&mut self, id: &ActionId) {
        if id.len() >= 2 {
            self.kde
                .present
                .remove(&(component_name(&id[0]).to_owned(), id[1].clone()));
        }
    }

    /// unregister: forget an action.
    pub fn kde_unregister(&mut self, component: &str, action: &str) -> bool {
        let component = component_name(component);
        if self.kde_entry(component, action).is_none() {
            return false;
        }
        let app = self.store().apps.get_mut(component).unwrap();
        app.remove(action);
        if app.is_empty() {
            self.store().apps.remove(component);
        }
        let key = (component.to_owned(), action.to_owned());
        self.kde.present.remove(&key);
        self.kde.fresh.remove(&key);
        self.save();
        true
    }

    fn kde_info(&mut self, component: &str, action: &str, mod_key: ModKey) -> Option<ShortcutInfo> {
        let entry = self.kde_entry_ref(component, action)?;
        let friendly = if entry.app_name.is_empty() {
            component.to_owned()
        } else {
            entry.app_name.clone()
        };
        Some(ShortcutInfo {
            action: action.to_owned(),
            action_friendly: entry.description.clone(),
            component: component.to_owned(),
            component_friendly: friendly,
            keys: trigger_keys(entry.trigger.as_deref(), mod_key),
            default_keys: trigger_keys(entry.preferred_trigger.as_deref(), mod_key),
        })
    }

    fn kde_actions_of(&mut self, component: &str) -> Vec<String> {
        self.store()
            .apps
            .get(component)
            .map(|app| {
                app.iter()
                    .filter(|(_, s)| s.source == Source::Kde)
                    .map(|(id, _)| id.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// allShortcutInfos of a component.
    pub fn kde_infos(&mut self, component: &str, mod_key: ModKey) -> Vec<ShortcutInfo> {
        let component = component_name(component);
        self.kde_actions_of(component)
            .iter()
            .filter_map(|a| self.kde_info(component, a, mod_key))
            .collect()
    }

    /// allActionsForComponent: action ids of a component's actions that have been set up.
    pub fn kde_actions(&mut self, component: &str, mod_key: ModKey) -> Vec<ActionId> {
        let component = component_name(component).to_owned();
        let fresh = self.kde.fresh.clone();
        self.kde_infos(&component, mod_key)
            .into_iter()
            .filter(|i| !fresh.contains(&(i.component.clone(), i.action.clone())))
            .map(|i| vec![i.component, i.action, i.component_friendly, i.action_friendly])
            .collect()
    }

    /// The KDE action that has `seq` (single key presses only).
    fn kde_find_by_key(&mut self, seq: &KeySeq, mod_key: ModKey) -> Option<(String, String)> {
        if seq_len(seq) != 1 {
            return None;
        }
        let key = qt_to_key(seq[0])?;
        let Trigger::Keysym(sym) = key.trigger else {
            return None;
        };
        self.ensure_parsed();
        let parsed = self.parsed.clone().unwrap_or_default();
        parsed
            .into_iter()
            .find(|(k, app, id)| {
                key_matches(k, mod_key, Trigger::Keysym(sym), key.modifiers)
                    && self
                        .store
                        .as_ref()
                        .and_then(|s| s.apps.get(app))
                        .and_then(|a| a.get(id))
                        .is_some_and(|s| s.source == Source::Kde)
            })
            .map(|(_, app, id)| (app, id))
    }

    /// actionList: the action id owning a key, empty if none.
    pub fn kde_action_for_key(&mut self, seq: &KeySeq, mod_key: ModKey) -> ActionId {
        let Some((component, action)) = self.kde_find_by_key(seq, mod_key) else {
            return Vec::new();
        };
        match self.kde_info(&component, &action, mod_key) {
            Some(i) => vec![i.component, i.action, i.component_friendly, i.action_friendly],
            None => Vec::new(),
        }
    }

    /// globalShortcutsByKey (exact matches).
    pub fn kde_infos_by_key(&mut self, seq: &KeySeq, mod_key: ModKey) -> Vec<ShortcutInfo> {
        self.kde_find_by_key(seq, mod_key)
            .and_then(|(c, a)| self.kde_info(&c, &a, mod_key))
            .into_iter()
            .collect()
    }

    /// globalShortcutAvailable: free for `component` (its own actions don't count).
    pub fn kde_available<'a>(
        &mut self,
        seq: &KeySeq,
        component: &str,
        binds: impl IntoIterator<Item = &'a Bind>,
        mod_key: ModKey,
    ) -> bool {
        if seq_len(seq) != 1 {
            return true;
        }
        let Some(key) = qt_to_key(seq[0]) else {
            return true;
        };
        let component = component_name(component);
        match self.find_conflict(&key, "", "", binds, mod_key) {
            None => true,
            Some(crate::global_shortcuts::AssignError::OtherShortcut(app, _)) => app == component,
            Some(_) => false,
        }
    }

    pub fn kde_is_active(&mut self, component: &str) -> bool {
        let component = component_name(component);
        if component.ends_with(".desktop") {
            return self.kde_has_component(component);
        }
        self.kde.present.iter().any(|(c, _)| c == component)
    }

    /// cleanUp: forget the component's actions that the running app didn't register.
    pub fn kde_clean_up(&mut self, component: &str) -> bool {
        let component = component_name(component).to_owned();
        let gone: Vec<String> = self
            .kde_actions_of(&component)
            .into_iter()
            .filter(|a| !self.kde.is_present(&component, a))
            .collect();
        for action in &gone {
            self.kde_unregister(&component, action);
        }
        !gone.is_empty()
    }

    pub fn kde_set_blocked(&mut self, blocked: bool) {
        self.kde.blocked = blocked;
    }

    /// The app that registered these components left the bus: they stop taking key presses.
    pub fn kde_owner_vanished(&mut self, owner: &str) {
        let components: Vec<String> = self
            .kde
            .owners
            .iter()
            .filter(|(_, o)| *o == owner)
            .map(|(c, _)| c.clone())
            .collect();
        for component in components {
            self.kde.owners.remove(&component);
            self.kde.present.retain(|(c, _)| *c != component);
        }
    }

    /// invokeShortcut, and `arbor msg` / IPC triggering.
    pub fn kde_invoke(&mut self, component: &str, action: &str, timestamp: i64) -> bool {
        let component = component_name(component).to_owned();
        if self.kde_entry(&component, action).is_none() {
            return false;
        }
        self.kde_fire(&component, action, timestamp);
        true
    }

    fn kde_fire(&mut self, component: &str, action: &str, timestamp: i64) {
        if component.ends_with(".desktop") {
            launch(component, action);
            return;
        }
        self.kde.send(KdeEvent::Pressed {
            component: component.to_owned(),
            action: action.to_owned(),
            timestamp,
        });
    }

    /// A key was pressed and no config bind or portal shortcut took it. Returns whether a KDE
    /// shortcut did (the key must then be suppressed).
    pub fn kde_press(
        &mut self,
        key_code: Keycode,
        mod_key: ModKey,
        sym: Keysym,
        modifiers: Modifiers,
        time_msec: u32,
    ) -> bool {
        if self.kde.blocked {
            return false;
        }
        self.ensure_parsed();
        let trigger = Trigger::Keysym(sym);
        let hit = self
            .parsed
            .as_ref()
            .unwrap()
            .iter()
            .filter(|(key, _, _)| key_matches(key, mod_key, trigger, modifiers))
            .find(|(_, app, id)| {
                self.kde.is_present(app, id)
                    && self
                        .store
                        .as_ref()
                        .and_then(|s| s.apps.get(app))
                        .and_then(|a| a.get(id))
                        .is_some_and(|s| s.source == Source::Kde)
            })
            .map(|(_, app, id)| (app.clone(), id.clone()));
        let Some((component, action)) = hit else {
            return false;
        };
        self.kde_fire(&component, &action, i64::from(time_msec));
        self.kde.pressed.insert(key_code, (component, action));
        true
    }

    /// A key was released. Returns whether it had activated a KDE shortcut.
    pub fn kde_release(&mut self, key_code: Keycode, time_msec: u32) -> bool {
        let Some((component, action)) = self.kde.pressed.remove(&key_code) else {
            return false;
        };
        if !component.ends_with(".desktop") {
            self.kde.send(KdeEvent::Released {
                component,
                action,
                timestamp: i64::from(time_msec),
            });
        }
        true
    }

    /// Replaces the whole store (after `arbor msg import-kde-shortcuts`) and saves it.
    pub fn replace_store(&mut self, store: crate::global_shortcuts::Store) {
        *self.store() = store;
        self.save();
    }
}

/// Starts a launcher shortcut's desktop file or action (looked up off the compositor thread).
fn launch(storage_id: &str, action: &str) {
    let storage_id = storage_id.to_owned();
    let action = action.to_owned();
    let res = std::thread::Builder::new()
        .name(String::from("kglobalaccel launch"))
        .spawn(move || match desktop::command(&storage_id, &action) {
            Some(argv) => crate::utils::spawning::spawn(argv, None),
            None => warn!("no desktop file or action for shortcut {storage_id} {action}"),
        });
    if let Err(err) = res {
        warn!("error spawning launcher thread: {err:?}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(c: &str, a: &str) -> ActionId {
        vec![c.into(), a.into(), "App".into(), format!("Do {a}")]
    }

    fn qt(s: &str) -> KeySeq {
        keys::parse_seq(s).unwrap()
    }

    const NO_BINDS: &[Bind] = &[];

    #[test]
    fn kglobalaccel_registration_flow() {
        let (tx, rx) = async_channel::unbounded();
        let mut gs = GlobalShortcuts::with_path(None);
        gs.set_kde_event_sender(tx);
        let m = ModKey::Super;

        // What KGlobalAccel::setShortcut does for a new action: register, set the active keys
        // (autoloading, present), then the defaults.
        assert!(gs.kde_register(&id("app", "ring"), Some(":1.5".into())));
        let got = gs.kde_set_keys(&id("app", "ring"), &[qt("Meta+Alt+R")], SET_PRESENT, NO_BINDS, m);
        assert_eq!(got, vec![qt("Meta+Alt+R")]);
        gs.kde_set_keys(&id("app", "ring"), &[qt("Meta+Alt+R")], IS_DEFAULT, NO_BINDS, m);
        assert_eq!(gs.kde_default_keys(&id("app", "ring"), m), vec![qt("Meta+Alt+R")]);

        let code = Keycode::new(27);
        assert!(gs.kde_press(code, m, Keysym::r, Modifiers::SUPER | Modifiers::ALT, 5));
        assert_eq!(
            rx.try_recv().unwrap(),
            KdeEvent::Pressed { component: "app".into(), action: "ring".into(), timestamp: 5 }
        );
        assert!(gs.kde_release(code, 6));
        assert!(matches!(rx.try_recv().unwrap(), KdeEvent::Released { .. }));

        // Next run of the app: autoloading returns the stored keys, not what the app asks for.
        gs.kde_owner_vanished(":1.5");
        assert!(!gs.kde_press(code, m, Keysym::r, Modifiers::SUPER | Modifiers::ALT, 7));
        assert!(!gs.kde_register(&id("app", "ring"), Some(":1.6".into())));
        let got = gs.kde_set_keys(&id("app", "ring"), &[qt("Meta+X")], SET_PRESENT, NO_BINDS, m);
        assert_eq!(got, vec![qt("Meta+Alt+R")]);

        // A settings UI changes it: the app hears about it.
        gs.kde_set_foreign(&id("app", "ring"), &[qt("Ctrl+F9")], NO_BINDS, m);
        assert!(matches!(
            rx.try_recv().unwrap(),
            KdeEvent::ShortcutsChanged { keys, .. } if keys == vec![qt("Ctrl+F9")]
        ));

        // IPC change goes through set_trigger and notifies too.
        gs.set_trigger("app", "ring", Some("Ctrl+F10"), NO_BINDS, m).unwrap();
        assert!(matches!(
            rx.try_recv().unwrap(),
            KdeEvent::ShortcutsChanged { keys, .. } if keys == vec![qt("Ctrl+F10")]
        ));
        let all = gs.all();
        assert_eq!(all[0].source, "kde");
        assert!(all[0].active);
    }

    #[test]
    fn config_binds_and_other_shortcuts_win() {
        let mut gs = GlobalShortcuts::with_path(None);
        let m = ModKey::Super;
        let binds = [Bind {
            key: "Mod+T".parse().unwrap(),
            action: arbor_config::Action::Screenshot(true, None),
            repeat: true,
            cooldown: None,
            allow_when_locked: false,
            allow_inhibiting: true,
            hotkey_overlay_title: None,
        }];
        gs.kde_register(&id("a", "x"), None);
        let got = gs.kde_set_keys(&id("a", "x"), &[qt("Meta+T")], SET_PRESENT, &binds, m);
        assert!(got.is_empty());
        assert!(!gs.kde_available(&qt("Meta+T"), "b", &binds, m));

        gs.kde_register(&id("a", "y"), None);
        gs.kde_set_keys(&id("a", "y"), &[qt("Ctrl+F1")], SET_PRESENT, &binds, m);
        gs.kde_register(&id("b", "z"), None);
        let got = gs.kde_set_keys(&id("b", "z"), &[qt("Ctrl+F1")], SET_PRESENT, &binds, m);
        assert!(got.is_empty());
        assert!(gs.kde_available(&qt("Ctrl+F1"), "a", &binds, m));
        assert!(!gs.kde_available(&qt("Ctrl+F1"), "b", &binds, m));
        assert_eq!(gs.kde_action_for_key(&qt("Ctrl+F1"), m)[..2], ["a", "y"]);

        // Unassigned: the default arrives later and is free, so it's taken.
        gs.kde_register(&id("c", "w"), None);
        gs.kde_set_keys(&id("c", "w"), &[], SET_PRESENT, &binds, m);
        gs.kde_set_keys(&id("c", "w"), &[qt("Ctrl+F2")], IS_DEFAULT, &binds, m);
        assert_eq!(gs.kde_keys(&id("c", "w"), m), vec![qt("Ctrl+F2")]);

        // cleanUp forgets what the running app didn't register.
        gs.kde_set_inactive(&id("a", "x"));
        assert!(gs.kde_clean_up("a"));
        assert_eq!(gs.kde_infos("a", m).len(), 1);
    }
}
