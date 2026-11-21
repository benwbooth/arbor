//! Qt key codes as KGlobalAccel speaks them: an `int` per key press (`Qt::Key | modifiers`),
//! up to four of them in a `QKeySequence`, and the "PortableText" strings
//! (`Meta+Ctrl+Alt+Shift+F12`, `Volume Mute`) stored in `kglobalshortcutsrc`.

use arbor_config::binds::{Key, Modifiers, Trigger};
use smithay::input::keyboard::{xkb, Keysym};

use super::qt_keys::{QT_KEY_NAMES, XKB_TO_QT};

pub const SHIFT: i32 = 0x0200_0000;
pub const CTRL: i32 = 0x0400_0000;
pub const ALT: i32 = 0x0800_0000;
pub const META: i32 = 0x1000_0000;
pub const KEYPAD: i32 = 0x2000_0000;
pub const MODIFIER_MASK: i32 = 0xfe00_0000_u32 as i32;

pub const KEY_TAB: i32 = 0x0100_0001;
pub const KEY_SHIFT: i32 = 0x0100_0020;
pub const KEY_CONTROL: i32 = 0x0100_0021;
pub const KEY_META: i32 = 0x0100_0022;
pub const KEY_ALT: i32 = 0x0100_0023;
pub const KEY_SUPER_L: i32 = 0x0100_0053;
pub const KEY_SUPER_R: i32 = 0x0100_0054;
const KEY_F1: i32 = 0x0100_0030;
const KEY_F35: i32 = 0x0100_0052;

/// A `QKeySequence`: up to four key presses, unused ones 0.
pub type KeySeq = [i32; 4];

pub fn seq_len(seq: &KeySeq) -> usize {
    seq.iter().take_while(|k| **k != 0).count()
}

pub fn seq_is_empty(seq: &KeySeq) -> bool {
    seq[0] == 0
}

/// Whether the Qt key (without modifiers) is a modifier key itself.
pub fn is_modifier_key(key: i32) -> bool {
    matches!(
        key & !MODIFIER_MASK,
        KEY_SHIFT | KEY_CONTROL | KEY_META | KEY_ALT | KEY_SUPER_L | KEY_SUPER_R
    )
}

/// Qt::Key for an xkb keysym, as QXkbCommon::keysymToQtKey computes it (no modifiers).
pub fn keysym_to_qt(sym: Keysym) -> i32 {
    let raw = sym.raw();
    if (Keysym::F1.raw()..=Keysym::F35.raw()).contains(&raw) {
        return KEY_F1 + (raw - Keysym::F1.raw()) as i32;
    }
    if (Keysym::KP_0.raw()..=Keysym::KP_9.raw()).contains(&raw) {
        return '0' as i32 + (raw - Keysym::KP_0.raw()) as i32;
    }
    if raw < 0x100 {
        // Latin-1: Qt keys are the upper-case form where that stays in Latin-1.
        let c = char::from_u32(raw).unwrap_or('\0');
        let mut upper = c.to_uppercase();
        let u = match (upper.next(), upper.next()) {
            (Some(u), None) if (u as u32) < 0x100 => u as u32,
            _ => raw,
        };
        return u as i32;
    }
    if let Some((_, qt)) = XKB_TO_QT.iter().find(|(x, _)| *x == raw) {
        return *qt as i32;
    }
    let ucs = xkb::keysym_to_utf32(sym);
    if ucs != 0 {
        if let Some(c) = char::from_u32(ucs) {
            let mut upper = c.to_uppercase();
            if let (Some(u), None) = (upper.next(), upper.next()) {
                return u as i32;
            }
            return c as i32;
        }
    }
    0
}

/// Keysym for a Qt key (no modifiers), the inverse of [`keysym_to_qt`]: letters become their
/// lower-case keysym (what arbor binds match), keypad digits `KP_n`.
pub fn qt_to_keysym(key: i32, keypad: bool) -> Option<Keysym> {
    if key <= 0 {
        return None;
    }
    if (KEY_F1..=KEY_F35).contains(&key) {
        return Some(Keysym::new(Keysym::F1.raw() + (key - KEY_F1) as u32));
    }
    if keypad && ('0' as i32..='9' as i32).contains(&key) {
        return Some(Keysym::new(Keysym::KP_0.raw() + (key - '0' as i32) as u32));
    }
    if key < 0x0100_0000 {
        let c = char::from_u32(key as u32)?;
        let mut lower = c.to_lowercase();
        let c = match (lower.next(), lower.next()) {
            (Some(l), None) => l,
            _ => c,
        };
        let sym = xkb::utf32_to_keysym(c as u32);
        return (sym.raw() != 0).then_some(sym);
    }
    XKB_TO_QT
        .iter()
        .find(|(_, q)| *q as i32 == key)
        .map(|(x, _)| Keysym::new(*x))
}

/// An arbor key for one Qt key press. `None` for modifier-only presses and keys arbor can't bind.
pub fn qt_to_key(combined: i32) -> Option<Key> {
    let key = combined & !MODIFIER_MASK;
    if is_modifier_key(key) {
        return None;
    }
    let sym = qt_to_keysym(key, combined & KEYPAD != 0)?;
    let mut modifiers = Modifiers::empty();
    if combined & CTRL != 0 {
        modifiers |= Modifiers::CTRL;
    }
    if combined & SHIFT != 0 {
        modifiers |= Modifiers::SHIFT;
    }
    if combined & ALT != 0 {
        modifiers |= Modifiers::ALT;
    }
    if combined & META != 0 {
        modifiers |= Modifiers::SUPER;
    }
    Some(Key {
        trigger: Trigger::Keysym(sym),
        modifiers,
    })
}

/// The Qt key press for an arbor key (`Mod` must already be expanded). 0 when it has no Qt
/// equivalent.
pub fn key_to_qt(key: &Key) -> i32 {
    let Trigger::Keysym(sym) = key.trigger else {
        return 0;
    };
    let mut combined = keysym_to_qt(sym);
    if combined == 0 {
        return 0;
    }
    let m = key.modifiers;
    if m.contains(Modifiers::CTRL) {
        combined |= CTRL;
    }
    if m.contains(Modifiers::SHIFT) {
        combined |= SHIFT;
    }
    if m.contains(Modifiers::ALT) {
        combined |= ALT;
    }
    if m.contains(Modifiers::SUPER) {
        combined |= META;
    }
    if (Keysym::KP_0.raw()..=Keysym::KP_9.raw()).contains(&sym.raw()) {
        combined |= KEYPAD;
    }
    combined
}

/// Name of a single Qt key (no modifiers) in PortableText.
fn key_name(key: i32) -> String {
    if (KEY_F1..=KEY_F35).contains(&key) {
        return format!("F{}", key - KEY_F1 + 1);
    }
    if let Some((_, name)) = QT_KEY_NAMES.iter().find(|(k, _)| *k as i32 == key) {
        return (*name).to_owned();
    }
    if key > 0 && key < 0x0100_0000 {
        if let Some(c) = char::from_u32(key as u32) {
            return c.to_string();
        }
    }
    format!("0x{key:x}")
}

/// One key press (`Qt::Key | modifiers`) as PortableText, e.g. `Meta+Ctrl+Esc`.
pub fn combined_to_string(combined: i32) -> String {
    let mut s = String::new();
    if combined & META != 0 {
        s.push_str("Meta+");
    }
    if combined & CTRL != 0 {
        s.push_str("Ctrl+");
    }
    if combined & ALT != 0 {
        s.push_str("Alt+");
    }
    if combined & SHIFT != 0 {
        s.push_str("Shift+");
    }
    if combined & KEYPAD != 0 {
        s.push_str("Num+");
    }
    s.push_str(&key_name(combined & !MODIFIER_MASK));
    s
}

/// A sequence as PortableText (`Ctrl+X, Ctrl+C`).
pub fn seq_to_string(seq: &KeySeq) -> String {
    seq.iter()
        .take_while(|k| **k != 0)
        .map(|k| combined_to_string(*k))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Parse one key press like `Ctrl+Alt+Shift+/` or `Meta++`. `None` when unknown.
pub fn parse_combined(s: &str) -> Option<i32> {
    let mut rest = s.trim();
    let mut mods = 0;
    loop {
        let lower = rest.to_ascii_lowercase();
        let found = [
            ("ctrl+", CTRL),
            ("control+", CTRL),
            ("shift+", SHIFT),
            ("alt+", ALT),
            ("meta+", META),
            ("super+", META),
            ("num+", KEYPAD),
        ]
        .into_iter()
        .find(|(p, _)| lower.len() > p.len() && lower.starts_with(p));
        match found {
            Some((p, m)) => {
                mods |= m;
                rest = &rest[p.len()..];
            }
            None => break,
        }
    }

    let key = parse_key_name(rest)?;
    Some(key | mods)
}

fn parse_key_name(name: &str) -> Option<i32> {
    if name.is_empty() {
        return None;
    }
    let mut chars = name.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        let mut upper = c.to_uppercase();
        return Some(match (upper.next(), upper.next()) {
            (Some(u), None) => u as i32,
            _ => c as i32,
        });
    }
    if let Some(n) = name
        .strip_prefix(['F', 'f'])
        .and_then(|n| n.parse::<i32>().ok())
    {
        if (1..=35).contains(&n) {
            return Some(KEY_F1 + n - 1);
        }
    }
    QT_KEY_NAMES
        .iter()
        .find(|(_, n)| n.eq_ignore_ascii_case(name))
        .map(|(k, _)| *k as i32)
}

/// Parse a sequence like `Ctrl+X, Ctrl+C` (at most four presses).
pub fn parse_seq(s: &str) -> Option<KeySeq> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let mut parts: Vec<&str> = Vec::new();
    let mut rest = s;
    // ", " separates presses, but "Ctrl+," is a press of its own.
    while let Some(i) = rest.find(", ") {
        if i == 0 || rest[..i].ends_with('+') {
            // "Ctrl+, X" -> "Ctrl+," then "X"
            let end = i + 1;
            parts.push(&rest[..end]);
            rest = rest[end..].trim_start();
            continue;
        }
        parts.push(&rest[..i]);
        rest = &rest[i + 2..];
    }
    if !rest.is_empty() {
        parts.push(rest);
    }
    if parts.is_empty() || parts.len() > 4 {
        return None;
    }
    let mut seq = [0; 4];
    for (i, p) in parts.iter().enumerate() {
        seq[i] = parse_combined(p)?;
    }
    Some(seq)
}

/// Keys from a `kglobalshortcutsrc` field: tab-separated sequences, `none` for no keys. Unknown
/// sequences become empty ones, as QKeySequence::fromString does.
pub fn keys_from_string(s: &str) -> Vec<KeySeq> {
    if s == "none" {
        return Vec::new();
    }
    s.split('\t')
        .map(|part| parse_seq(part).unwrap_or([0; 4]))
        .collect()
}

pub fn keys_to_string(keys: &[KeySeq]) -> String {
    if keys.is_empty() {
        return String::from("none");
    }
    keys.iter()
        .map(seq_to_string)
        .collect::<Vec<_>>()
        .join("\t")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_portable_text() {
        for s in [
            "Meta+Ctrl+Alt+Shift+F12",
            "Ctrl+Alt+Shift+/",
            "Ctrl+Alt+Shift+,",
            "Meta++",
            "Meta+`",
            "Volume Mute",
            "Monitor Brightness Up",
            "Shift+Monitor Brightness Down",
            "Meta+PgDown",
            "Media Play",
            "Microphone Mute",
            "Meta+Ctrl+Esc",
            "Pause",
            "Ctrl+X, Ctrl+C",
        ] {
            let seq = parse_seq(s).unwrap_or_else(|| panic!("{s} should parse"));
            assert_eq!(seq_to_string(&seq), s);
        }
    }

    #[test]
    fn parses_fields() {
        let keys = keys_from_string("Meta+Tab\tAlt+Tab");
        assert_eq!(keys.len(), 2);
        assert_eq!(keys[0][0], META | KEY_TAB);
        assert_eq!(keys[1][0], ALT | KEY_TAB);
        assert!(keys_from_string("none").is_empty());
        assert_eq!(keys_to_string(&[]), "none");
        // Empty field: one empty sequence (KDE writes `=,Alt+Space,...` for a cleared key).
        assert_eq!(keys_from_string(""), vec![[0; 4]]);
        assert_eq!(keys_to_string(&keys), "Meta+Tab\tAlt+Tab");
    }

    #[test]
    fn keysyms_map_like_qt() {
        assert_eq!(keysym_to_qt(Keysym::a), 'A' as i32);
        assert_eq!(keysym_to_qt(Keysym::A), 'A' as i32);
        assert_eq!(keysym_to_qt(Keysym::slash), '/' as i32);
        assert_eq!(keysym_to_qt(Keysym::F5), KEY_F1 + 4);
        assert_eq!(keysym_to_qt(Keysym::Escape), 0x0100_0000);
        assert_eq!(
            keysym_to_qt(Keysym::XF86_AudioMute),
            parse_combined("Volume Mute").unwrap()
        );
        assert_eq!(keysym_to_qt(Keysym::odiaeresis), 'Ö' as i32);
        assert_eq!(keysym_to_qt(Keysym::Cyrillic_a), 'А' as i32);
    }

    #[test]
    fn converts_to_arbor_keys() {
        for (qt, arbor) in [
            ("Ctrl+Alt+Shift+/", "Ctrl+Alt+Shift+slash"),
            ("Ctrl+Alt+Shift+C", "Ctrl+Alt+Shift+c"),
            ("Meta+H", "Super+h"),
            ("Pause", "Pause"),
            ("Volume Mute", "XF86AudioMute"),
            ("Meta+F5", "Super+F5"),
            ("Ctrl+Alt+Shift+,", "Ctrl+Alt+Shift+comma"),
        ] {
            let combined = parse_combined(qt).unwrap();
            let key = qt_to_key(combined).unwrap_or_else(|| panic!("{qt}"));
            assert_eq!(key, arbor.parse::<Key>().unwrap(), "{qt}");
            assert_eq!(combined_to_string(key_to_qt(&key)), qt);
        }
        assert_eq!(qt_to_key(META | KEY_META), None);
    }
}
