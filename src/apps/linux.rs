#[cfg(target_os = "linux")]
use super::run_helper;
use super::{Application, Catalog, LaunchTarget};
#[cfg(target_os = "linux")]
use anyhow::Result;
#[cfg(target_os = "linux")]
use std::process::Command;
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};
use walkdir::WalkDir;

pub fn discover() -> Catalog {
    let mut roots = Vec::new();
    if let Some(data_home) = std::env::var_os("XDG_DATA_HOME").filter(|v| !v.is_empty()) {
        roots.push(PathBuf::from(data_home).join("applications"));
    } else if let Some(home) = std::env::var_os("HOME") {
        roots.push(PathBuf::from(home).join(".local/share/applications"));
    }
    let dirs = std::env::var_os("XDG_DATA_DIRS")
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    roots.extend(std::env::split_paths(&dirs).map(|p| p.join("applications")));
    let locale = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .into_iter()
        .find_map(|key| std::env::var(key).ok().filter(|s| !s.is_empty()))
        .unwrap_or_default();
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    discover_in(&roots, &locale, &desktop)
}

fn discover_in(roots: &[PathBuf], locale: &str, desktop: &str) -> Catalog {
    let mut catalog = Catalog::default();
    let mut seen = HashSet::new();
    for root in roots {
        if !root.exists() {
            continue;
        }
        for entry in WalkDir::new(root).max_depth(12).into_iter() {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    catalog.warnings.push(error.to_string());
                    continue;
                }
            };
            let path = entry.path();
            // Flatpak exports and some distro packages use symlinked desktop
            // files. Follow file links without recursing through linked dirs.
            if !(entry.file_type().is_file() || entry.file_type().is_symlink() && path.is_file())
                || path.extension().is_none_or(|e| e != "desktop")
            {
                continue;
            }
            let id = desktop_id(root, path);
            // User entries override system entries, even when Hidden=true.
            if !seen.insert(id) {
                continue;
            }
            match std::fs::read_to_string(path) {
                Ok(text) => {
                    if let Some(app) = parse_entry(&text, path, locale, desktop) {
                        catalog.apps.push(app);
                    }
                }
                Err(error) => catalog
                    .warnings
                    .push(format!("{}: {error}", path.display())),
            }
        }
    }
    catalog
}

fn desktop_id(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("-")
}

fn parse_entry(text: &str, path: &Path, locale: &str, desktop: &str) -> Option<Application> {
    let mut in_entry = false;
    let mut values = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if in_entry && let Some((key, value)) = line.split_once('=') {
            values.insert(key.trim().to_owned(), unescape(value));
        }
    }
    let get = |key: &str| values.get(key).map(String::as_str).unwrap_or("");
    if get("Type") != "Application" || get("Hidden") == "true" || get("NoDisplay") == "true" {
        return None;
    }
    let desktops: Vec<_> = desktop.split(':').filter(|s| !s.is_empty()).collect();
    let listed = |list: &str| list.split(';').any(|s| desktops.contains(&s));
    if (!get("OnlyShowIn").is_empty() && !listed(get("OnlyShowIn"))) || listed(get("NotShowIn")) {
        return None;
    }
    if !get("TryExec").is_empty() && !executable_exists(get("TryExec")) {
        return None;
    }
    if get("Exec").is_empty() && get("DBusActivatable") != "true" {
        return None;
    }
    let localized = |key: &str| {
        for suffix in locale_candidates(locale) {
            if let Some(value) = values.get(&format!("{key}[{suffix}]")) {
                return value.clone();
            }
        }
        get(key).to_owned()
    };
    let name = localized("Name");
    if name.trim().is_empty() {
        return None;
    }
    let comment = localized("Comment");
    Some(Application {
        name,
        detail: if comment.is_empty() {
            path.display().to_string()
        } else {
            comment
        },
        keywords: format!(
            "{} {} {} {}",
            get("Name"),
            localized("GenericName"),
            localized("Keywords"),
            get("Exec")
        ),
        target: LaunchTarget::File(path.to_owned()),
    })
}

fn locale_candidates(locale: &str) -> Vec<String> {
    // language_COUNTRY.encoding@modifier -> language_COUNTRY@modifier, ...
    let (base, modifier) = locale.split_once('@').unwrap_or((locale, ""));
    let base = base.split('.').next().unwrap_or(base);
    let language = base.split('_').next().unwrap_or(base);
    let mut result = Vec::new();
    if !modifier.is_empty() {
        result.push(format!("{base}@{modifier}"));
    }
    result.push(base.into());
    if base != language {
        if !modifier.is_empty() {
            result.push(format!("{language}@{modifier}"));
        }
        result.push(language.into());
    }
    result
}

fn unescape(text: &str) -> String {
    let mut result = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            result.push(c);
            continue;
        }
        match chars.next() {
            Some('s') => result.push(' '),
            Some('n') => result.push('\n'),
            Some('t') => result.push('\t'),
            Some('r') => result.push('\r'),
            Some('\\') => result.push('\\'),
            Some(other) => {
                result.push('\\');
                result.push(other);
            }
            None => result.push('\\'),
        }
    }
    result
}

fn executable_exists(name: &str) -> bool {
    fn executable(path: &Path) -> bool {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            path.metadata()
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        }
        #[cfg(not(unix))]
        {
            path.is_file()
        }
    }
    if name.contains('/') {
        return executable(Path::new(name));
    }
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|p| executable(&p.join(name))))
}

#[cfg(target_os = "linux")]
pub fn launch(app: &Application) -> Result<()> {
    let LaunchTarget::File(path) = &app.target;
    // GLib handles Exec quoting/field codes, DBus activation, working directories,
    // and Terminal=true. Never feed desktop-entry contents to a shell.
    if !executable_exists("gio") {
        anyhow::bail!("缺少 gio，请安装 GLib 工具（Ubuntu/Debian: libglib2.0-bin）");
    }
    run_helper(Command::new("gio").arg("launch").arg(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str, locale: &str, desktop: &str) -> Option<Application> {
        parse_entry(text, Path::new("/apps/demo.desktop"), locale, desktop)
    }

    #[test]
    fn localizes_and_ignores_action_sections() {
        let app = parse("[Desktop Entry]\nType=Application\nName=Editor\nName[zh_CN]=编辑器\nExec=editor %U\nKeywords=code;\n[Desktop Action New]\nName=Wrong\n", "zh_CN.UTF-8", "GNOME").unwrap();
        assert_eq!(app.name, "编辑器");
        assert!(app.keywords.contains("Editor"));
        assert!(app.keywords.contains("code"));
    }

    #[test]
    fn respects_visibility_and_desktop_environment() {
        let base = "[Desktop Entry]\nType=Application\nName=Demo\nExec=demo\n";
        for value in [
            "Hidden=true",
            "NoDisplay=true",
            "OnlyShowIn=KDE;",
            "NotShowIn=GNOME;",
            "TryExec=/absolutely/missing/take-a-panel",
        ] {
            assert!(
                parse(&format!("{base}{value}\n"), "en_US", "GNOME").is_none(),
                "{value}"
            );
        }
        assert!(parse(&format!("{base}OnlyShowIn=GNOME;\n"), "en", "Unity:GNOME").is_some());
        assert!(
            parse(
                "[Desktop Entry]\nType=Application\nName=DBus\nDBusActivatable=true",
                "en",
                ""
            )
            .is_some()
        );
        assert!(
            parse(
                "[Desktop Entry]\nType=Application\nName=Missing exec",
                "en",
                ""
            )
            .is_none()
        );
    }

    #[test]
    fn locale_fallback_and_escapes() {
        assert_eq!(
            locale_candidates("sr_RS.UTF-8@latin"),
            vec!["sr_RS@latin", "sr_RS", "sr@latin", "sr"]
        );
        assert_eq!(
            unescape(r"Hello\sworld\nnext\\line"),
            "Hello world\nnext\\line"
        );
        assert_eq!(
            desktop_id(Path::new("/apps"), Path::new("/apps/vendor/editor.desktop")),
            "vendor-editor.desktop"
        );
    }

    #[test]
    fn hidden_user_entry_masks_system_entry() {
        let base = std::env::temp_dir().join(format!("take-a-panel-test-{}", std::process::id()));
        let user = base.join("user");
        let system = base.join("system");
        std::fs::create_dir_all(&user).unwrap();
        std::fs::create_dir_all(&system).unwrap();
        std::fs::write(user.join("demo.desktop"), "[Desktop Entry]\nHidden=true").unwrap();
        std::fs::write(
            system.join("demo.desktop"),
            "[Desktop Entry]\nType=Application\nName=Demo\nExec=demo",
        )
        .unwrap();
        assert!(discover_in(&[user, system], "en", "").apps.is_empty());
        std::fs::remove_dir_all(base).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn discovers_symlinked_desktop_files() {
        let base =
            std::env::temp_dir().join(format!("take-a-panel-link-test-{}", std::process::id()));
        let root = base.join("applications");
        std::fs::create_dir_all(&root).unwrap();
        let target = base.join("export.desktop");
        std::fs::write(
            &target,
            "[Desktop Entry]\nType=Application\nName=Exported App\nExec=demo",
        )
        .unwrap();
        std::os::unix::fs::symlink(&target, root.join("demo.desktop")).unwrap();
        let catalog = discover_in(&[root], "en", "");
        assert_eq!(catalog.apps.len(), 1);
        assert_eq!(catalog.apps[0].name, "Exported App");
        std::fs::remove_dir_all(base).unwrap();
    }
}
