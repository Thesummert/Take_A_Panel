use crate::LauncherWindow;
use global_hotkey::hotkey::{HotKey, Modifiers};
use slint::{ComponentHandle, platform::Key, winit_030::winit::keyboard};

pub fn bind(ui: &LauncherWindow) {
    let weak = ui.as_weak();
    ui.on_capture_shortcut(move |text, control, alt, shift, meta| {
        let Some(ui) = weak.upgrade() else { return };
        // Slint swaps Command and Control on macOS; global-hotkey uses the
        // physical modifier names on every platform.
        let (control, super_key) = if cfg!(target_os = "macos") {
            (meta, control)
        } else {
            (control, meta)
        };
        let mut modifiers = Modifiers::empty();
        modifiers.set(Modifiers::CONTROL, control);
        modifiers.set(Modifiers::ALT, alt);
        modifiers.set(Modifiers::SHIFT, shift);
        modifiers.set(Modifiers::SUPER, super_key);
        let special = [
            (Key::Control, "ControlLeft"),
            (Key::ControlR, "ControlRight"),
            (Key::Meta, "SuperLeft"),
            (Key::MetaR, "SuperRight"),
            (Key::Alt, "AltLeft"),
            (Key::AltGr, "AltRight"),
            (Key::Shift, "ShiftLeft"),
            (Key::ShiftR, "ShiftRight"),
            (Key::Space, "Space"),
            (Key::Return, "Enter"),
            (Key::Escape, "Escape"),
            (Key::Tab, "Tab"),
            (Key::Backspace, "Backspace"),
            (Key::Delete, "Delete"),
            (Key::UpArrow, "ArrowUp"),
            (Key::DownArrow, "ArrowDown"),
            (Key::LeftArrow, "ArrowLeft"),
            (Key::RightArrow, "ArrowRight"),
            (Key::Home, "Home"),
            (Key::End, "End"),
            (Key::PageUp, "PageUp"),
            (Key::PageDown, "PageDown"),
        ];
        let character = text.chars().next();
        let name = special
            .iter()
            .find_map(|(key, name)| {
                (character == Some(char::from(*key))).then_some((*name).to_owned())
            })
            .unwrap_or_else(|| {
                if let Some(code) = character.map(u32::from)
                    && (0xf704..=0xf71b).contains(&code)
                {
                    return format!("F{}", code - 0xf704 + 1);
                }
                text.to_ascii_uppercase()
            });
        record(&ui, &name, modifiers);
    });
}

/// Use physical key codes before text conversion, so Option+A records Alt+A
/// instead of the character produced by the current keyboard layout.
pub fn capture_native(
    ui: &LauncherWindow,
    key: keyboard::KeyCode,
    state: keyboard::ModifiersState,
) -> bool {
    if matches!(
        key,
        keyboard::KeyCode::ControlLeft
            | keyboard::KeyCode::ControlRight
            | keyboard::KeyCode::ShiftLeft
            | keyboard::KeyCode::ShiftRight
            | keyboard::KeyCode::AltLeft
            | keyboard::KeyCode::AltRight
            | keyboard::KeyCode::SuperLeft
            | keyboard::KeyCode::SuperRight
    ) {
        return false;
    }
    if key == keyboard::KeyCode::Tab
        && !state.intersects(
            keyboard::ModifiersState::CONTROL
                | keyboard::ModifiersState::ALT
                | keyboard::ModifiersState::SUPER,
        )
    {
        return false;
    }
    let mut modifiers = Modifiers::empty();
    modifiers.set(Modifiers::CONTROL, state.control_key());
    modifiers.set(Modifiers::ALT, state.alt_key());
    modifiers.set(Modifiers::SHIFT, state.shift_key());
    modifiers.set(Modifiers::SUPER, state.super_key());
    record(ui, &format!("{key:?}"), modifiers);
    true
}

fn record(ui: &LauncherWindow, key: &str, modifiers: Modifiers) {
    if matches!(
        key,
        "ControlLeft"
            | "ControlRight"
            | "ShiftLeft"
            | "ShiftRight"
            | "AltLeft"
            | "AltRight"
            | "SuperLeft"
            | "SuperRight"
    ) {
        return;
    }
    if modifiers.is_empty() {
        match key {
            "Enter" => {
                ui.invoke_save_shortcut(ui.get_shortcut_draft());
                return;
            }
            "Escape" => {
                ui.invoke_close_settings();
                return;
            }
            _ => {}
        }
    }
    let mut parts = Vec::new();
    for (modifier, name) in [
        (Modifiers::CONTROL, "Ctrl"),
        (Modifiers::ALT, "Alt"),
        (Modifiers::SHIFT, "Shift"),
        (Modifiers::SUPER, "Super"),
    ] {
        if modifiers.contains(modifier) {
            parts.push(name);
        }
    }
    let key = key
        .strip_prefix("Key")
        .or_else(|| key.strip_prefix("Digit"))
        .unwrap_or(key);
    parts.push(key);
    let value = parts.join("+");
    if value.parse::<HotKey>().is_err() {
        ui.set_settings_failed(true);
        ui.set_settings_message("暂不支持这个按键，请尝试其他组合键。".into());
        return;
    }
    ui.set_shortcut_draft(value.into());
    ui.set_settings_failed(false);
    ui.set_settings_message("已录入，点击“保存快捷键”生效。".into());
}
