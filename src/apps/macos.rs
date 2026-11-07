use super::{Application, Catalog, LaunchTarget, run_helper};
use anyhow::Result;
use std::{collections::HashSet, path::Path, process::Command};
use walkdir::WalkDir;

pub fn discover() -> Catalog {
    let mut roots = vec![
        "/Applications".into(),
        "/System/Applications".into(),
        "/System/Library/CoreServices/Applications".into(),
    ];
    if let Some(home) = std::env::var_os("HOME") {
        roots.push(std::path::PathBuf::from(home).join("Applications"));
    }
    discover_in(&roots)
}

fn discover_in(roots: &[std::path::PathBuf]) -> Catalog {
    let mut catalog = Catalog::default();
    let mut seen = HashSet::new();
    for root in roots {
        if !root.exists() {
            continue;
        }
        // Do not descend into bundles: embedded helpers are not user apps.
        let mut entries = WalkDir::new(root)
            .max_depth(6)
            .sort_by_file_name()
            .into_iter();
        while let Some(entry) = entries.next() {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    catalog.warnings.push(error.to_string());
                    continue;
                }
            };
            let path = entry.path();
            if path.extension().is_none_or(|ext| ext != "app") {
                continue;
            }
            if entry.file_type().is_dir() {
                // Only real directories are on WalkDir's traversal stack.
                // Pruning a symlink would pop its parent (e.g. /Applications)
                // and silently skip all remaining sibling apps.
                entries.skip_current_dir();
            } else if !entry.file_type().is_symlink() || !path.is_dir() {
                continue;
            }
            let canonical = path.canonicalize().unwrap_or_else(|_| path.to_owned());
            if seen.insert(canonical) {
                catalog.apps.push(read_bundle(path));
            }
        }
    }
    catalog
}

fn read_bundle(path: &Path) -> Application {
    let info = plist::Value::from_file(path.join("Contents/Info.plist")).ok();
    let string = |key: &str| {
        info.as_ref()
            .and_then(plist::Value::as_dictionary)
            .and_then(|dict| dict.get(key))
            .and_then(plist::Value::as_string)
            .filter(|s| !s.is_empty())
    };
    let fallback = path.file_stem().unwrap_or_default().to_string_lossy();
    let name = string("CFBundleDisplayName")
        .or_else(|| string("CFBundleName"))
        .unwrap_or(&fallback)
        .to_owned();
    Application {
        name,
        detail: path.to_string_lossy().into_owned(),
        keywords: format!(
            "{} {}",
            fallback,
            string("CFBundleIdentifier").unwrap_or("")
        ),
        target: LaunchTarget::File(path.to_owned()),
        icon_name: None,
    }
}

pub fn launch(app: &Application) -> Result<()> {
    let LaunchTarget::File(path) = &app.target;
    run_helper(Command::new("/usr/bin/open").arg("-a").arg(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovers_named_bundles_deduplicates_roots_and_skips_embedded_helpers() {
        let root =
            std::env::temp_dir().join(format!("take-a-panel-macos-test-{}", std::process::id()));
        let app = root.join("Vendor/Editor.app");
        std::fs::create_dir_all(app.join("Contents/Helpers/Hidden.app")).unwrap();
        let mut info = plist::Dictionary::new();
        info.insert("CFBundleDisplayName".into(), "编辑器".into());
        info.insert("CFBundleName".into(), "Editor".into());
        info.insert("CFBundleIdentifier".into(), "com.example.editor".into());
        plist::Value::Dictionary(info)
            .to_file_xml(app.join("Contents/Info.plist"))
            .unwrap();
        let catalog = discover_in(&[root.clone(), root.clone()]);
        assert_eq!(catalog.apps.len(), 1);
        assert_eq!(catalog.apps[0].name, "编辑器");
        assert!(catalog.apps[0].keywords.contains("com.example.editor"));
        assert!(catalog.apps[0].keywords.contains("Editor"));
        assert!(catalog.warnings.is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn symlinked_app_does_not_skip_sibling_apps() {
        let base = std::env::temp_dir().join(format!(
            "take-a-panel-macos-link-test-{}",
            std::process::id()
        ));
        let root = base.join("Applications");
        let linked_app = base.join("Elsewhere/Safari.app");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(linked_app.join("Contents")).unwrap();
        std::os::unix::fs::symlink(&linked_app, root.join("Safari.app")).unwrap();
        std::fs::create_dir_all(root.join("Zen.app/Contents")).unwrap();
        let catalog = discover_in(&[root, base.join("Elsewhere")]);
        let names: HashSet<_> = catalog.apps.iter().map(|app| app.name.as_str()).collect();
        assert_eq!(names, HashSet::from(["Safari", "Zen"]));
        assert_eq!(
            catalog.apps.len(),
            2,
            "the linked app should only appear once"
        );
        assert!(catalog.warnings.is_empty());
        assert_eq!(crate::search::search(&catalog.apps, "zen").len(), 1);
        std::fs::remove_dir_all(base).unwrap();
    }
}
