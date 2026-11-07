use anyhow::{Context, Result, bail};
use global_hotkey::{GlobalHotKeyManager, hotkey::HotKey};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

#[derive(Default)]
pub struct Shortcuts {
    // macOS allows only one of the library's event handlers per process.
    manager: Option<GlobalHotKeyManager>,
    hotkey: Option<HotKey>,
    // Zero means disabled; adding one also allows a hotkey whose ID is zero.
    pub active_id: Arc<AtomicU64>,
}

impl Shortcuts {
    pub fn enabled(&self) -> bool {
        self.hotkey.is_some()
    }

    pub fn apply(&mut self, value: &str, persist: impl FnOnce() -> Result<()>) -> Result<()> {
        let hotkey: HotKey = value
            .parse()
            .context("快捷键格式错误，请输入 Ctrl+Alt+Space、Alt+Space 等组合键")?;
        #[cfg(target_os = "linux")]
        if std::env::var_os("WAYLAND_DISPLAY").is_some()
            || std::env::var("XDG_SESSION_TYPE").is_ok_and(|v| v == "wayland")
        {
            bail!("Wayland 暂不支持全局快捷键，请在桌面设置中绑定 take-a-panel --toggle");
        }
        // These combinations must reach the panel's own keyboard handlers.
        let reserved = [
            "Ctrl+R",
            "Ctrl+Q",
            "Enter",
            "Shift+Enter",
            "Escape",
            "Up",
            "Down",
        ];
        if reserved
            .iter()
            .any(|key| key.parse::<HotKey>().ok() == Some(hotkey))
        {
            bail!("该快捷键用于面板内操作，请选择其他组合键");
        }
        if self.hotkey == Some(hotkey) {
            return persist();
        }
        if self.manager.is_none() {
            self.manager = Some(GlobalHotKeyManager::new().context("无法初始化全局快捷键")?);
        }
        let manager = self.manager.as_ref().unwrap();
        manager
            .register(hotkey)
            .context("无法注册全局快捷键，可能已被其他程序占用")?;
        // Keep the old binding until the new binding has been registered and
        // saved. A failed save releases only the candidate shortcut.
        if let Err(error) = persist() {
            if let Err(cleanup) = manager.unregister(hotkey) {
                return Err(error.context(format!("同时无法释放未保存的快捷键：{cleanup}")));
            }
            return Err(error);
        }
        let previous = self.hotkey.replace(hotkey);
        self.active_id
            .store(u64::from(hotkey.id()) + 1, Ordering::Release);
        if let Some(previous) = previous
            && let Err(error) = manager.unregister(previous)
        {
            // The new binding is already active and persisted. Report cleanup
            // separately rather than telling the UI that saving failed.
            eprintln!("新快捷键已生效，但无法释放原快捷键 {previous}：{error}");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn invalid_or_reserved_shortcuts_are_not_saved_or_activated() {
        let mut shortcuts = Shortcuts::default();
        let saved = Cell::new(false);
        for value in [
            "", "Ctrl+", "Ctrl+A+B", "Ctrl+R", "Ctrl+Q", "Enter", "Escape",
        ] {
            assert!(
                shortcuts
                    .apply(value, || {
                        saved.set(true);
                        Ok(())
                    })
                    .is_err()
            );
            assert!(!saved.get());
            assert!(!shortcuts.enabled());
            assert_eq!(shortcuts.active_id.load(Ordering::Acquire), 0);
        }
    }
}
