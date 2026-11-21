//! One-off import of KDE Plasma's global shortcut assignments (`kglobalshortcutsrc`) into arbor's
//! app shortcut store. arbor never writes that file and doesn't read it again afterwards.

use std::path::PathBuf;

use super::kconfig::{split_list, KConfig};
use super::keys::{keys_from_string, qt_to_key, seq_len};
use crate::global_shortcuts::{format_key, Source, Store, StoredShortcut};

/// Components that belong to Plasma itself (KWin, plasmashell, the session, Plasma's media,
/// volume, brightness and layout handlers, ...). Nothing registers them under arbor; arbor's
/// config binds and hearth do those jobs.
const SKIP_COMPONENTS: &[&str] = &[
    "kwin",
    "plasmashell",
    "ksmserver",
    "mediacontrol",
    "kmix",
    "org_kde_powerdevil",
    "KDE Keyboard Layout Switcher",
    "kaccess",
    "ActivityManager",
    "khotkeys",
    "kded5",
    "kcm_touchpad",
    "org.kde.kglobalaccel",
];

/// Plasma launchers in `[services]` that make no sense outside Plasma.
const SKIP_SERVICES: &[&str] = &[
    "org.kde.krunner.desktop",
    "org.kde.kscreen.desktop",
    "org.kde.touchpadshortcuts.desktop",
    "org.kde.plasma-systemmonitor.desktop",
];

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ImportReport {
    pub imported: u32,
    pub skipped: u32,
}

pub fn kglobalshortcutsrc_path() -> Option<PathBuf> {
    let dir = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(dir.join("kglobalshortcutsrc"))
}

/// Imports `$XDG_CONFIG_HOME/kglobalshortcutsrc` if it exists.
pub fn import_kde_file(store: &mut Store, overwrite: bool) -> ImportReport {
    let Some(path) = kglobalshortcutsrc_path() else {
        return ImportReport::default();
    };
    match std::fs::read_to_string(&path) {
        Ok(text) => import_kde(store, &text, overwrite, &|id| service_name(id)),
        Err(_) => ImportReport::default(),
    }
}

fn service_name(storage_id: &str) -> Option<String> {
    let path = super::desktop::find(storage_id)?;
    let text = std::fs::read_to_string(path).ok()?;
    KConfig::parse(&text)
        .group(&["Desktop Entry"])?
        .get("Name")
        .cloned()
}

/// Shortcut ids that Chromium/Electron apps register through the GlobalShortcuts portal
/// (`<32 hex digits>-<name>`): xdg-desktop-portal-kde stores those as kglobalaccel components
/// named after the app id, so they move to the portal side of the store.
fn is_portal_shortcut_id(id: &str) -> bool {
    id.len() > 33 && id.as_bytes()[32] == b'-' && id[..32].bytes().all(|b| b.is_ascii_hexdigit())
}

/// The first assigned key arbor can bind, in arbor key syntax.
fn first_key(field: &str) -> Option<String> {
    keys_from_string(field)
        .iter()
        .filter(|seq| seq_len(seq) == 1)
        .find_map(|seq| qt_to_key(seq[0]))
        .map(|key| format_key(&key))
}

pub fn import_kde(
    store: &mut Store,
    text: &str,
    overwrite: bool,
    desktop_name: &dyn Fn(&str) -> Option<String>,
) -> ImportReport {
    let config = KConfig::parse(text);
    let mut report = ImportReport::default();

    // (app, id, shortcut)
    let mut found: Vec<(String, String, StoredShortcut)> = Vec::new();

    for (path, entries) in &config.groups {
        // Regular components are top-level groups; their subgroups are shortcut contexts, which
        // arbor doesn't have.
        if path.len() != 1 || path[0] == "services" || path[0].ends_with(".desktop") {
            continue;
        }
        let component = &path[0];
        let ids: Vec<&String> = entries
            .keys()
            .filter(|k| *k != "_k_friendly_name")
            .collect();
        if SKIP_COMPONENTS.contains(&component.as_str()) {
            report.skipped += ids.len() as u32;
            continue;
        }
        let portal = !ids.is_empty() && ids.iter().all(|id| is_portal_shortcut_id(id));
        let app_name = entries.get("_k_friendly_name").cloned().unwrap_or_default();

        for id in ids {
            let fields = split_list(&entries[id]);
            if fields.len() != 3 {
                continue;
            }
            let Some(trigger) = first_key(&fields[0]) else {
                // Unassigned (or nothing arbor can bind): the app registers it again when it
                // runs.
                continue;
            };
            found.push((
                component.clone(),
                id.clone(),
                StoredShortcut {
                    source: if portal { Source::Portal } else { Source::Kde },
                    app_name: if portal {
                        String::new()
                    } else {
                        app_name.clone()
                    },
                    description: fields[2].clone(),
                    preferred_trigger: first_key(&fields[1]),
                    trigger: Some(trigger),
                    user_cleared: false,
                },
            ));
        }
    }

    // Launchers: only the keys that differ from the desktop file's default are stored.
    let services = vec![String::from("services")];
    for service in config.subgroups(&services) {
        let entries = &config.groups[&vec![String::from("services"), service.clone()]];
        if SKIP_SERVICES.contains(&service.as_str()) {
            report.skipped += entries.len() as u32;
            continue;
        }
        let app_name = desktop_name(&service).unwrap_or_default();
        for (id, value) in entries {
            let Some(trigger) = first_key(value) else {
                continue;
            };
            let description = if id == "_launch" {
                format!(
                    "Launch {}",
                    if app_name.is_empty() {
                        &service
                    } else {
                        &app_name
                    }
                )
            } else {
                id.clone()
            };
            found.push((
                service.clone(),
                id.clone(),
                StoredShortcut {
                    source: Source::Kde,
                    app_name: app_name.clone(),
                    description,
                    preferred_trigger: None,
                    trigger: Some(trigger),
                    user_cleared: false,
                },
            ));
        }
    }

    for (app, id, shortcut) in found {
        let exists = store.apps.get(&app).is_some_and(|a| a.contains_key(&id));
        if exists && !overwrite {
            report.skipped += 1;
            continue;
        }
        // One key, one shortcut: the first one imported keeps it.
        let taken = store.apps.iter().any(|(a, shortcuts)| {
            shortcuts.iter().any(|(i, s)| {
                !(a == &app && i == &id) && s.trigger.is_some() && s.trigger == shortcut.trigger
            })
        });
        if taken {
            report.skipped += 1;
            continue;
        }
        store.apps.entry(app).or_default().insert(id, shortcut);
        report.imported += 1;
    }

    report
}

#[cfg(test)]
mod tests {
    use super::*;

    const RC: &str = r"[kwin]
Walk Through Windows=Meta+Tab\tAlt+Tab,Meta+Tab\tAlt+Tab,Walk Through Windows
_k_friendly_name=KWin

[com.anthropic.Claude]
9A5B5DFD45E495F50E70BF35E7BC53B8-Alt+Space=,Alt+Space,Claude shortcut: Alt+Space
B02769BC012D3AC0FFA3D2001DEED136-Ctrl+Alt+Space=Ctrl+Alt+Space,Ctrl+Alt+Space,Claude shortcut: Ctrl+Alt+Space
_k_friendly_name=Claude

[net.mkiol.SpeechNote]
_k_friendly_name=Speech Note
cancel=Ctrl+Alt+Shift+C,Ctrl+Alt+Shift+C,Cancel
start-listening-translate-clipboard=Ctrl+Alt+Shift+\\,,Ctrl+Alt+Shift+\\,,Start listening\\, always translate\\, text to clipboard
stop-listening=none,Ctrl+Alt+Shift+S,Stop listening

[org.kde.kdeconnect.daemon]
_k_friendly_name=KDE Connect
ring=Meta+Alt+R,none,Ring my phone

[services][net.local.whisper-toggle.desktop]
_launch=Pause

[services][org.kde.krunner.desktop]
_launch=Alt+Space

[services][org.kde.spectacle.desktop]
CurrentMonitorScreenShot=
RectangularRegionScreenShot=Meta+Shift+S
";

    #[test]
    fn imports_app_components_and_launchers() {
        let mut store = Store::default();
        let report = import_kde(&mut store, RC, false, &|id| {
            (id == "org.kde.spectacle.desktop").then(|| String::from("Spectacle"))
        });

        assert!(!store.apps.contains_key("kwin"));
        assert!(!store.apps.contains_key("org.kde.krunner.desktop"));

        let sn = &store.apps["net.mkiol.SpeechNote"];
        assert_eq!(sn["cancel"].trigger.as_deref(), Some("Ctrl+Alt+Shift+C"));
        assert_eq!(sn["cancel"].source, Source::Kde);
        assert_eq!(sn["cancel"].app_name, "Speech Note");
        assert_eq!(
            sn["start-listening-translate-clipboard"].trigger.as_deref(),
            Some("Ctrl+Alt+Shift+comma")
        );
        assert_eq!(
            sn["start-listening-translate-clipboard"].description,
            "Start listening, always translate, text to clipboard"
        );
        assert!(!sn.contains_key("stop-listening"));

        // Portal-made (xdg-desktop-portal-kde) components keep their app id and ids.
        let claude = &store.apps["com.anthropic.Claude"];
        assert_eq!(claude.len(), 1);
        let s = &claude["B02769BC012D3AC0FFA3D2001DEED136-Ctrl+Alt+Space"];
        assert_eq!(s.source, Source::Portal);
        assert_eq!(s.trigger.as_deref(), Some("Ctrl+Alt+space"));

        assert_eq!(
            store.apps["org.kde.kdeconnect.daemon"]["ring"]
                .trigger
                .as_deref(),
            Some("Alt+Super+R")
        );
        assert_eq!(
            store.apps["net.local.whisper-toggle.desktop"]["_launch"]
                .trigger
                .as_deref(),
            Some("Pause")
        );
        let spectacle = &store.apps["org.kde.spectacle.desktop"];
        assert_eq!(spectacle.len(), 1);
        assert_eq!(
            spectacle["RectangularRegionScreenShot"].trigger.as_deref(),
            Some("Shift+Super+S")
        );
        assert_eq!(
            spectacle["RectangularRegionScreenShot"].app_name,
            "Spectacle"
        );
        assert_eq!(report.imported, 6);

        // Importing again changes nothing unless asked to overwrite.
        let before = store.clone();
        let again = import_kde(&mut store, RC, false, &|_| None);
        assert_eq!(again.imported, 0);
        assert_eq!(store, before);
    }
}
