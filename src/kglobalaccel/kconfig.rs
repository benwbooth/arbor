//! Just enough of KConfig's INI dialect to read `kglobalshortcutsrc` (for the one-off import)
//! and desktop files: nested groups (`[services][foo.desktop]`), KConfig value escapes and
//! comma-separated string lists.

use std::collections::BTreeMap;

/// A group path, e.g. `["services", "foo.desktop"]`. The empty path holds keys before the
/// first group header.
pub type GroupPath = Vec<String>;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct KConfig {
    /// Groups with their entries (values unescaped), sorted like KConfig writes them.
    pub groups: BTreeMap<GroupPath, BTreeMap<String, String>>,
}

impl KConfig {
    pub fn parse(text: &str) -> Self {
        let mut config = Self::default();
        let mut current: GroupPath = Vec::new();
        for line in text.lines() {
            let line = line.trim_start();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if line.starts_with('[') {
                current = parse_group_header(line);
                config.groups.entry(current.clone()).or_default();
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let key = unescape(key.trim());
            let value = unescape(value.trim_start());
            config
                .groups
                .entry(current.clone())
                .or_default()
                .insert(key, value);
        }
        config
    }

    pub fn group(&self, path: &[&str]) -> Option<&BTreeMap<String, String>> {
        let path: GroupPath = path.iter().map(|s| (*s).to_owned()).collect();
        self.groups.get(&path)
    }

    /// Paths of the direct subgroups of `parent`.
    pub fn subgroups(&self, parent: &[String]) -> Vec<String> {
        self.groups
            .keys()
            .filter(|p| p.len() == parent.len() + 1 && p.starts_with(parent))
            .map(|p| p[parent.len()].clone())
            .collect()
    }
}

fn parse_group_header(line: &str) -> GroupPath {
    let mut path = Vec::new();
    let mut rest = line.trim_end();
    while let Some(stripped) = rest.strip_prefix('[') {
        let Some(end) = stripped.find(']') else {
            break;
        };
        let segment = &stripped[..end];
        rest = &stripped[end + 1..];
        // `[$i]` and friends are KConfig flags, not groups.
        if segment.starts_with('$') {
            continue;
        }
        path.push(unescape(segment));
    }
    path
}

/// KConfig's printableToString.
pub fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('s') => out.push(' '),
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('\\') => out.push('\\'),
            Some('x') => {
                let hex: String = chars.clone().take(2).collect();
                match u8::from_str_radix(&hex, 16) {
                    Ok(b) if hex.len() == 2 => {
                        chars.next();
                        chars.next();
                        out.push(b as char);
                    }
                    _ => out.push_str("\\x"),
                }
            }
            // Unknown escapes (`\,` `\;` for lists) stay as they are.
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// KConfigGroup::readEntry(key, QStringList): `,` separates, `\,` and `\\` escape.
pub fn split_list(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    if s.is_empty() {
        return out;
    }
    let mut cur = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some(n) => cur.push(n),
                None => cur.push('\\'),
            },
            ',' => out.push(std::mem::take(&mut cur)),
            c => cur.push(c),
        }
    }
    out.push(cur);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r"[kwin]
Walk Through Windows=Meta+Tab\tAlt+Tab,Meta+Tab\tAlt+Tab,Walk Through Windows
_k_friendly_name=KWin

[net.mkiol.SpeechNote]
_k_friendly_name=Speech Note
start-listening-clipboard=Ctrl+Alt+Shift+J,Ctrl+Alt+Shift+J,Start listening\\, text to clipboard
start-listening-translate-clipboard=Ctrl+Alt+Shift+\\,,Ctrl+Alt+Shift+\\,,Start listening\\, always translate\\, text to clipboard

[services][net.local.whisper-toggle.desktop]
_launch=Pause
";

    #[test]
    fn reads_lists_and_nested_groups() {
        let c = KConfig::parse(SAMPLE);
        let kwin = c.group(&["kwin"]).unwrap();
        assert_eq!(
            split_list(&kwin["Walk Through Windows"]),
            [
                "Meta+Tab\tAlt+Tab",
                "Meta+Tab\tAlt+Tab",
                "Walk Through Windows"
            ]
        );
        let sn = c.group(&["net.mkiol.SpeechNote"]).unwrap();
        assert_eq!(
            split_list(&sn["start-listening-translate-clipboard"]),
            [
                "Ctrl+Alt+Shift+,",
                "Ctrl+Alt+Shift+,",
                "Start listening, always translate, text to clipboard"
            ]
        );
        assert_eq!(
            c.group(&["services", "net.local.whisper-toggle.desktop"])
                .unwrap()["_launch"],
            "Pause"
        );
        assert_eq!(c.subgroups(&["services".to_owned()]).len(), 1);
    }
}
