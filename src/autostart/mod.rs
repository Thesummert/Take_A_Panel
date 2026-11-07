use crate::LauncherWindow;
use anyhow::Result;
use slint::ComponentHandle;

#[cfg(any(target_os = "linux", test))]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "linux")]
use linux as platform;
#[cfg(target_os = "macos")]
use macos as platform;
#[cfg(target_os = "windows")]
use windows as platform;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Status {
    Disabled,
    Enabled,
    RequiresApproval,
}

impl Status {
    fn label(self) -> &'static str {
        match self {
            Self::Disabled => "未启用",
            Self::Enabled => "已启用：登录系统后在后台启动",
            Self::RequiresApproval => "已提交：等待在系统设置中批准",
        }
    }

    fn message(self) -> &'static str {
        match self {
            Self::Disabled => "点击“请求开机自启”向系统添加当前用户的启动项。",
            Self::Enabled => "可以随时在这里关闭，或在系统的启动项设置中管理。",
            Self::RequiresApproval => {
                "请打开系统启动项设置，允许 Take a Panel 运行，然后刷新状态。"
            }
        }
    }
}

#[derive(Clone, Copy)]
enum Action {
    Refresh,
    Set(bool),
    OpenSettings,
}

fn perform(ui: &LauncherWindow, action: Action) {
    if ui.get_autostart_busy() {
        return;
    }
    ui.set_autostart_busy(true);
    let weak = ui.as_weak();
    // Registration can contact a system service; keep it off the UI thread.
    std::thread::spawn(move || {
        let operation: Result<()> = match action {
            Action::Refresh => Ok(()),
            Action::Set(enabled) => platform::set_enabled(enabled),
            Action::OpenSettings => platform::open_settings(),
        };
        // Always read back the OS state, including after a failed request.
        let state = platform::status();
        let _ = weak.upgrade_in_event_loop(move |ui| {
            ui.set_autostart_busy(false);
            ui.set_autostart_available(state.is_ok());
            ui.set_autostart_failed(operation.is_err() || state.is_err());
            let mut errors = Vec::new();
            if let Err(error) = operation {
                errors.push(format!("操作失败：{error:#}"));
            }
            match state {
                Ok(state) => {
                    ui.set_autostart_registered(state != Status::Disabled);
                    ui.set_autostart_status(state.label().into());
                    if errors.is_empty() {
                        ui.set_autostart_message(state.message().into());
                    }
                }
                Err(error) => {
                    ui.set_autostart_status("无法读取系统启动项".into());
                    errors.push(format!("{error:#}"));
                }
            }
            if !errors.is_empty() {
                ui.set_autostart_message(errors.join("\n").into());
            }
        });
    });
}

pub fn bind(ui: &LauncherWindow) {
    ui.set_autostart_system_settings(cfg!(any(target_os = "macos", target_os = "windows")));
    let weak = ui.as_weak();
    ui.on_set_autostart(move |enabled| {
        if let Some(ui) = weak.upgrade() {
            perform(&ui, Action::Set(enabled));
        }
    });
    let weak = ui.as_weak();
    ui.on_refresh_autostart(move || {
        if let Some(ui) = weak.upgrade() {
            perform(&ui, Action::Refresh);
        }
    });
    let weak = ui.as_weak();
    ui.on_open_autostart_system_settings(move || {
        if let Some(ui) = weak.upgrade() {
            perform(&ui, Action::OpenSettings);
        }
    });
    perform(ui, Action::Refresh);
}
