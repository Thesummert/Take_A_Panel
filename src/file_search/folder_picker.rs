use crate::{LauncherWindow, window_focus::WindowFocus};
use anyhow::{Context, Result};
use slint::ComponentHandle;
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn decode_output(output: std::process::Output) -> Result<Option<PathBuf>> {
    anyhow::ensure!(
        output.status.success(),
        "系统文件夹选择器返回错误：{}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    let text = String::from_utf8(output.stdout).context("文件夹路径不是有效的 UTF-8")?;
    let text = text.trim_end_matches(['\r', '\n']);
    Ok((!text.is_empty()).then(|| PathBuf::from(text)))
}

fn pick_folder() -> Result<Option<PathBuf>> {
    #[cfg(target_os = "macos")]
    let selection = decode_output(std::process::Command::new("osascript").args([
        "-e",
        "try\nreturn POSIX path of (choose folder with prompt \"添加文件索引文件夹\")\non error number -128\nreturn \"\"\nend try",
    ]).output().context("无法打开系统文件夹选择器")?)?;
    #[cfg(target_os = "windows")]
    let selection = {
        use std::os::windows::process::CommandExt;
        decode_output(std::process::Command::new("powershell.exe").args([
            "-NoLogo", "-NoProfile", "-STA", "-Command",
            "$ErrorActionPreference = 'Stop'; Add-Type -AssemblyName System.Windows.Forms; [Console]::OutputEncoding = [System.Text.UTF8Encoding]::new(); $d = New-Object System.Windows.Forms.FolderBrowserDialog; $d.Description = '添加文件索引文件夹'; try { if ($d.ShowDialog() -eq [System.Windows.Forms.DialogResult]::OK) { [Console]::Write($d.SelectedPath) } } finally { $d.Dispose() }",
        ]).creation_flags(0x08000000).output().context("无法打开系统文件夹选择器")?)?
    };
    #[cfg(target_os = "linux")]
    let selection = crate::linux_file_picker::pick_folder()?;
    if let Some(path) = &selection {
        anyhow::ensure!(
            path.is_absolute() && path.is_dir(),
            "请选择可访问的本地文件夹：{}",
            path.display()
        );
    }
    Ok(selection)
}

fn append_folder(draft: &str, folder: &Path) -> Result<Option<String>> {
    let path = folder.to_str().context("文件夹路径不是有效的 UTF-8")?;
    anyhow::ensure!(folder.is_absolute(), "请选择完整的文件夹路径");
    anyhow::ensure!(
        !path.contains(['\n', '\r', '\0']) && path.trim() == path,
        "此文件夹路径无法表示为一行索引范围，请选择其他文件夹"
    );
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
    let duplicate = draft.lines().map(str::trim).any(|line| {
        let existing = if (line == "~" || line.starts_with("~/") || line.starts_with("~\\"))
            && let Some(home) = &home
        {
            PathBuf::from(home).join(line.get(2..).unwrap_or(""))
        } else {
            PathBuf::from(line)
        };
        existing == folder
    });
    if duplicate {
        return Ok(None);
    }
    let mut result = draft.to_owned();
    if !result.is_empty() && !result.ends_with('\n') {
        result.push('\n');
    }
    result.push_str(path);
    Ok(Some(result))
}

fn apply_selection(ui: &LauncherWindow, selection: Result<Option<PathBuf>>) {
    let result = match selection {
        Ok(Some(folder)) => append_folder(ui.get_file_roots_draft().as_str(), &folder),
        Ok(None) => return,
        Err(error) => Err(error),
    };
    ui.set_file_settings_failed(result.is_err());
    match result {
        Ok(Some(draft)) => {
            ui.set_file_roots_draft(draft.into());
            ui.set_file_settings_message("文件夹已添加，点击“保存并重建索引”生效。".into());
        }
        Ok(None) => ui.set_file_settings_message("此文件夹已在索引范围列表中。".into()),
        Err(error) => ui.set_file_settings_message(format!("添加文件夹失败：{error:#}").into()),
    }
}

pub fn bind(ui: &LauncherWindow, focus: Arc<WindowFocus>, dialog_open: Arc<AtomicBool>) {
    let weak = ui.as_weak();
    ui.on_browse_file_folder(move || {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        if ui.get_file_settings_busy() || dialog_open.swap(true, Ordering::Relaxed) {
            return;
        }
        ui.set_file_folder_dialog_open(true);
        let weak = weak.clone();
        let focus = focus.clone();
        let flag = dialog_open.clone();
        std::thread::spawn(move || {
            let result = pick_folder();
            let _ = weak.upgrade_in_event_loop(move |ui| {
                focus.begin_presentation();
                flag.store(false, Ordering::Relaxed);
                ui.set_file_folder_dialog_open(false);
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
    fn adds_paths_without_replacing_edits_and_rejects_unrepresentable_paths() {
        let dir = super::super::test_support::TempDir::new();
        let folder = dir.0.join("中文 a'b $folder");
        let draft = "~/Documents\nnot-yet-valid";
        let added = append_folder(draft, &folder).unwrap().unwrap();
        assert_eq!(added, format!("{draft}\n{}", folder.display()));
        assert!(append_folder(&added, &folder).unwrap().is_none());
        assert!(
            append_folder(&format!("{}\n", folder.display()), &folder.join(""))
                .unwrap()
                .is_none()
        );
        assert!(append_folder("", Path::new("relative")).is_err());
        assert!(append_folder("", &dir.0.join("line\nbreak")).is_err());
    }

    #[test]
    fn cancellation_and_errors_preserve_draft_and_selection_waits_for_save() {
        i_slint_backend_testing::init_no_event_loop();
        let ui = LauncherWindow::new().unwrap();
        let draft = "~/Documents";
        ui.set_file_roots_draft(draft.into());
        ui.set_file_settings_message("previous notice".into());
        apply_selection(&ui, Ok(None));
        assert_eq!(ui.get_file_roots_draft(), draft);
        assert_eq!(ui.get_file_settings_message(), "previous notice");
        apply_selection(&ui, Err(anyhow::anyhow!("chooser failed")));
        assert_eq!(ui.get_file_roots_draft(), draft);
        assert!(ui.get_file_settings_failed());
        let dir = super::super::test_support::TempDir::new();
        apply_selection(&ui, Ok(Some(dir.0.clone())));
        assert!(
            ui.get_file_roots_draft()
                .as_str()
                .ends_with(dir.0.to_str().unwrap())
        );
        assert!(!ui.get_file_settings_failed());
        assert!(!ui.get_file_indexing());
        assert_eq!(ui.get_file_count(), 0);
    }
}
