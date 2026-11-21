//! Desktop files for KDE "service" shortcuts: `kglobalshortcutsrc`'s `[services][foo.desktop]`
//! groups launch an app (`_launch`) or one of its `[Desktop Action ...]`s.

use std::path::{Path, PathBuf};

use super::kconfig::KConfig;

/// XDG data directories, most important first.
pub fn data_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    match std::env::var_os("XDG_DATA_HOME") {
        Some(home) => dirs.push(PathBuf::from(home)),
        None => {
            if let Some(home) = std::env::var_os("HOME") {
                dirs.push(Path::new(&home).join(".local/share"));
            }
        }
    }
    let system =
        std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".to_owned());
    dirs.extend(
        system
            .split(':')
            .filter(|d| !d.is_empty())
            .map(PathBuf::from),
    );
    dirs
}

/// Finds a desktop file by its storage id (`org.kde.konsole.desktop`) in `applications/`
/// (where `a-b.desktop` may also live at `a/b.desktop`) or in kglobalaccel's own directory.
pub fn find(storage_id: &str) -> Option<PathBuf> {
    let dirs = data_dirs();
    for dir in &dirs {
        let apps = dir.join("applications");
        let path = apps.join(storage_id);
        if path.is_file() {
            return Some(path);
        }
        let nested = apps.join(storage_id.replacen('-', "/", 1));
        if storage_id.contains('-') && nested.is_file() {
            return Some(nested);
        }
    }
    dirs.iter()
        .map(|dir| dir.join("kglobalaccel").join(storage_id))
        .find(|p| p.is_file())
}

/// The command line for the app (`action == "_launch"`) or one of its actions, `None` if the
/// desktop file or action doesn't exist.
pub fn command(storage_id: &str, action: &str) -> Option<Vec<String>> {
    let path = find(storage_id)?;
    let text = std::fs::read_to_string(&path).ok()?;
    let config = KConfig::parse(&text);
    let entry = config.group(&["Desktop Entry"])?;
    let group = if action == "_launch" {
        entry
    } else {
        config.group(&[&format!("Desktop Action {action}")])?
    };
    let exec = group.get("Exec")?;
    let name = group.get("Name").or_else(|| entry.get("Name"));
    let argv = exec_argv(exec, name.map(String::as_str), &path);
    (!argv.is_empty()).then_some(argv)
}

/// Splits an `Exec=` value per the desktop entry spec and expands its field codes (file and URL
/// codes expand to nothing: nothing is being opened).
pub fn exec_argv(exec: &str, name: Option<&str>, path: &Path) -> Vec<String> {
    let mut args = Vec::new();
    let mut cur = String::new();
    let mut has_cur = false;
    let mut in_quotes = false;
    let mut chars = exec.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                in_quotes = !in_quotes;
                has_cur = true;
            }
            '\\' if in_quotes => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            ' ' | '\t' if !in_quotes => {
                if has_cur {
                    args.push(std::mem::take(&mut cur));
                    has_cur = false;
                }
            }
            c => {
                cur.push(c);
                has_cur = true;
            }
        }
    }
    if has_cur {
        args.push(cur);
    }

    args.into_iter()
        .filter_map(|arg| {
            match arg.as_str() {
                "%f" | "%F" | "%u" | "%U" | "%d" | "%D" | "%n" | "%N" | "%v" | "%m" | "%i" => {
                    return None
                }
                "%c" => return name.map(str::to_owned),
                "%k" => return Some(path.to_string_lossy().into_owned()),
                _ => (),
            }
            let mut out = String::new();
            let mut it = arg.chars();
            while let Some(c) = it.next() {
                if c != '%' {
                    out.push(c);
                    continue;
                }
                match it.next() {
                    Some('%') => out.push('%'),
                    Some('c') => out.push_str(name.unwrap_or_default()),
                    Some('k') => out.push_str(&path.to_string_lossy()),
                    _ => (),
                }
            }
            Some(out)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_exec() {
        let p = Path::new("/x/a.desktop");
        assert_eq!(
            exec_argv("dsnote --action start %U", None, p),
            ["dsnote", "--action", "start"]
        );
        assert_eq!(
            exec_argv(r#"sh -c "echo \"hi\" 100%%" %f"#, None, p),
            ["sh", "-c", "echo \"hi\" 100%"]
        );
        assert_eq!(
            exec_argv("app --name %c", Some("App"), p),
            ["app", "--name", "App"]
        );
    }
}
