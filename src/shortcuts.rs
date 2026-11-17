use anyhow::{Context, Result, bail};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, hotkey::HotKey};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

#[cfg(target_os = "windows")]
#[path = "shortcuts/windows.rs"]
mod windows;

type EventHandler = Arc<dyn Fn(GlobalHotKeyEvent) + Send + Sync>;

pub fn parse_hotkey(value: &str) -> Result<HotKey> {
    // global-hotkey calls the Windows key "Super" and has no Win alias.
    #[cfg(target_os = "windows")]
    let value = value
        .split('+')
        .map(|part| {
            if part.trim().eq_ignore_ascii_case("win") {
                "Super"
            } else {
                part
            }
        })
        .collect::<Vec<_>>()
        .join("+");
    Ok(value.parse::<HotKey>()?)
}

#[derive(Default)]
pub struct Shortcuts {
    // macOS allows only one of the library's event handlers per process.
    manager: Option<GlobalHotKeyManager>,
    hotkey: Option<HotKey>,
    #[cfg(target_os = "windows")]
    win_space: Option<windows::WinSpaceHook>,
    #[cfg(target_os = "windows")]
    event_handler: Option<EventHandler>,
    // Zero means disabled; adding one also allows a hotkey whose ID is zero.
    pub active_id: Arc<AtomicU64>,
}

impl Shortcuts {
    pub fn set_event_handler(
        &mut self,
        handler: impl Fn(GlobalHotKeyEvent) + Send + Sync + 'static,
    ) {
        let handler: EventHandler = Arc::new(handler);
        #[cfg(target_os = "windows")]
        {
            self.event_handler = Some(handler.clone());
        }
        GlobalHotKeyEvent::set_event_handler(Some(move |event| handler(event)));
    }

    pub fn enabled(&self) -> bool {
        self.hotkey.is_some()
    }

    pub fn apply(&mut self, value: &str, persist: impl FnOnce() -> Result<()>) -> Result<()> {
        let hotkey = parse_hotkey(value)
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
        #[cfg(target_os = "windows")]
        let candidate_hook = if windows::is_win_space(hotkey) {
            Some(windows::WinSpaceHook::new(
                hotkey.id(),
                self.event_handler.clone(),
            )?)
        } else {
            None
        };
        #[cfg(target_os = "windows")]
        let native = candidate_hook.is_none();
        #[cfg(not(target_os = "windows"))]
        let native = true;
        if native {
            if self.manager.is_none() {
                self.manager = Some(GlobalHotKeyManager::new().context("无法初始化全局快捷键")?);
            }
            self.manager
                .as_ref()
                .unwrap()
                .register(hotkey)
                .context("无法注册全局快捷键，可能已被其他程序占用")?;
        }
        // Keep the old binding until the new binding has been registered and
        // saved. A failed save releases only the candidate shortcut.
        if let Err(error) = persist() {
            if native && let Err(cleanup) = self.manager.as_ref().unwrap().unregister(hotkey) {
                return Err(error.context(format!("同时无法释放未保存的快捷键：{cleanup}")));
            }
            return Err(error);
        }
        let previous = self.hotkey.replace(hotkey);
        self.active_id
            .store(u64::from(hotkey.id()) + 1, Ordering::Release);
        #[cfg(target_os = "windows")]
        {
            if let Some(hook) = candidate_hook.as_ref() {
                hook.enable();
            }
            self.win_space = candidate_hook;
        }
        if let Some(previous) = previous
            && let Err(error) = self.unregister_native(previous)
        {
            // The new binding is already active and persisted. Report cleanup
            // separately rather than telling the UI that saving failed.
            eprintln!("新快捷键已生效，但无法释放原快捷键 {previous}：{error}");
        }
        Ok(())
    }

    fn unregister_native(&self, hotkey: HotKey) -> Result<()> {
        #[cfg(target_os = "windows")]
        if windows::is_win_space(hotkey) {
            return Ok(());
        }
        if let Some(manager) = &self.manager {
            manager.unregister(hotkey)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_key_alias_matches_super_and_preserves_validation() {
        for (value, canonical) in [
            ("Win+Space", "Super+Space"),
            ("Ctrl+ win +K", "Ctrl+Super+K"),
            ("Shift+WIN+F13", "Shift+Super+F13"),
            ("Super+Space", "Super+Space"),
        ] {
            assert_eq!(
                parse_hotkey(value).unwrap(),
                canonical.parse::<HotKey>().unwrap()
            );
        }
        for value in ["Win", "Win+", "Win+A+B", "A+Win", "Win++Space"] {
            assert!(parse_hotkey(value).is_err(), "accepted {value:?}");
        }
    }

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
