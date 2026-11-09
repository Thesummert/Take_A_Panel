use crate::{LauncherWindow, window_focus::WindowFocus};
use anyhow::{Context, Result};
use slint::ComponentHandle;
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn decode_output(output: std::process::Output) -> Result<Option<PathBuf>> {
    anyhow::ensure!(
        output.status.success(),
        "系统文件选择器返回错误：{}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    let text = String::from_utf8(output.stdout).context("文件路径不是有效的 UTF-8")?;
    let text = text.trim_end_matches(['\r', '\n']);
    Ok((!text.is_empty()).then(|| PathBuf::from(text)))
}

fn pick_manifest() -> Result<Option<PathBuf>> {
    #[cfg(target_os = "macos")]
    let selection = decode_output(std::process::Command::new("osascript").args([
        "-e", "try\nreturn POSIX path of (choose file with prompt \"选择插件清单\" of type {\"public.json\"})\non error number -128\nreturn \"\"\nend try",
    ]).output().context("无法打开系统文件选择器")?)?;
    #[cfg(target_os = "windows")]
    let selection = {
        use std::os::windows::process::CommandExt;
        decode_output(std::process::Command::new("powershell.exe").args([
            "-NoLogo", "-NoProfile", "-STA", "-Command",
            "$ErrorActionPreference = 'Stop'; Add-Type -AssemblyName System.Windows.Forms; [Console]::OutputEncoding = [System.Text.UTF8Encoding]::new(); $d = New-Object System.Windows.Forms.OpenFileDialog; $d.Title = '选择插件清单'; $d.Filter = 'JSON 文件|*.json'; try { if ($d.ShowDialog() -eq [System.Windows.Forms.DialogResult]::OK) { [Console]::Write($d.FileName) } } finally { $d.Dispose() }",
        ]).creation_flags(0x08000000).output().context("无法打开系统文件选择器")?)?
    };
    #[cfg(target_os = "linux")]
    let selection = crate::linux_file_picker::pick_manifest()?;
    if let Some(path) = &selection {
        anyhow::ensure!(
            path.is_absolute() && path.is_file(),
            "请选择可访问的本地清单文件：{}",
            path.display()
        );
    }
    Ok(selection)
}

fn apply_selection(ui: &LauncherWindow, selection: Result<Option<PathBuf>>) {
    match selection {
        Ok(None) => {}
        Ok(Some(path)) => match path.to_str() {
            Some(path) => {
                ui.set_addon_manifest_draft(path.into());
                ui.set_addon_failed(false);
                ui.set_addon_message("已选择清单，点击“添加插件”生效。".into());
            }
            None => {
                ui.set_addon_failed(true);
                ui.set_addon_message("文件路径不是有效的 UTF-8。".into());
            }
        },
        Err(error) => {
            ui.set_addon_failed(true);
            ui.set_addon_message(format!("选择插件清单失败：{error:#}").into());
        }
    }
}

pub fn bind(ui: &LauncherWindow, focus: Arc<WindowFocus>, dialog_open: Arc<AtomicBool>) {
    let weak = ui.as_weak();
    ui.on_addon_browse(move || {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        if dialog_open.swap(true, Ordering::Relaxed) {
            return;
        }
        ui.set_addon_dialog_open(true);
        let weak = weak.clone();
        let focus = focus.clone();
        let flag = dialog_open.clone();
        std::thread::spawn(move || {
            let result = pick_manifest();
            let _ = weak.upgrade_in_event_loop(move |ui| {
                focus.begin_presentation();
                flag.store(false, Ordering::Relaxed);
                ui.set_addon_dialog_open(false);
                apply_selection(&ui, result);
                if ui.window().is_visible() {
                    crate::present(&ui, false, &focus);
                }
            });
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selection_preserves_draft_on_cancel_or_error() {
        i_slint_backend_testing::init_no_event_loop();
        let ui = LauncherWindow::new().unwrap();
        ui.set_addon_manifest_draft("previous".into());
        apply_selection(&ui, Ok(None));
        assert_eq!(ui.get_addon_manifest_draft(), "previous");
        apply_selection(&ui, Err(anyhow::anyhow!("chooser failed")));
        assert_eq!(ui.get_addon_manifest_draft(), "previous");
        assert!(ui.get_addon_failed());
        apply_selection(&ui, Ok(Some(PathBuf::from("/tmp/插件 manifest.json"))));
        assert_eq!(ui.get_addon_manifest_draft(), "/tmp/插件 manifest.json");
        assert!(!ui.get_addon_failed());
    }
}
