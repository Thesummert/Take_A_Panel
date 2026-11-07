#[cfg(target_os = "macos")]
// This standalone harness doesn't collect the imported modules' unit tests.
#[allow(dead_code, unused_imports)]
#[path = "../src/settings.rs"]
mod settings;
#[cfg(target_os = "macos")]
#[allow(unused_imports)]
#[path = "../src/shortcuts.rs"]
mod shortcuts;

#[cfg(target_os = "macos")]
fn main() -> anyhow::Result<()> {
    use global_hotkey::hotkey::HotKey;
    use std::sync::atomic::Ordering;

    let mut random = [0; 8];
    getrandom::fill(&mut random).unwrap();
    let directory = std::env::temp_dir().join(format!(
        "take-a-panel-hotkey-runtime-{:016x}",
        u64::from_ne_bytes(random)
    ));
    let settings = settings::Settings::new(directory.clone());
    let result = (|| -> anyhow::Result<()> {
        let mut shortcuts = shortcuts::Shortcuts::default();
        // Unusual combinations avoid taking over shortcuts used by applications.
        let first = "Ctrl+Alt+Shift+Super+F13";
        let second = "Ctrl+Alt+Shift+Super+F14";
        shortcuts.apply(first, || settings.save_hotkey(first))?;
        assert!(shortcuts.enabled());

        // Reproduce the stale ENOENT that the dependency reports when a second
        // macOS manager installs the same Carbon event handler.
        let _ = std::fs::metadata(directory.join("missing"));
        shortcuts.apply(second, || settings.save_hotkey(second))?;
        assert_eq!(settings.load_hotkey()?.as_deref(), Some(second));
        assert_eq!(
            shortcuts.active_id.load(Ordering::Acquire),
            u64::from(second.parse::<HotKey>()?.id()) + 1
        );
        // Saving the active key and switching back must both succeed.
        shortcuts.apply(second, || settings.save_hotkey(second))?;
        shortcuts.apply(first, || settings.save_hotkey(first))?;

        // A regular file in the destination path makes the actual settings
        // writer fail without changing permissions or touching user settings.
        let invalid_settings = settings::Settings::new(directory.join("hotkey/nested"));
        let failure = shortcuts.apply(second, || invalid_settings.save_hotkey(second));
        assert!(format!("{:#}", failure.unwrap_err()).contains("无法创建设置目录"));
        assert_eq!(settings.load_hotkey()?.as_deref(), Some(first));
        assert_eq!(
            shortcuts.active_id.load(Ordering::Acquire),
            u64::from(first.parse::<HotKey>()?.id()) + 1
        );
        // A failed save must release the candidate so it can be retried.
        shortcuts.apply(second, || settings.save_hotkey(second))?;
        drop(shortcuts);

        // Shutdown releases the native handler and registered shortcut.
        let mut restarted = shortcuts::Shortcuts::default();
        restarted.apply(second, || settings.save_hotkey(second))?;
        Ok(())
    })();
    if directory.exists() {
        std::fs::remove_dir_all(directory)?;
    }
    result?;
    println!("macOS hotkey switching, persistence, rollback, and restart: passed");
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn main() {
    println!("macOS native hotkey regression test: skipped on this platform");
}
