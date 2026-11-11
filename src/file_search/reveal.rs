#[cfg(any(target_os = "macos", target_os = "linux"))]
use anyhow::Context;
use anyhow::Result;
use std::path::Path;

pub(super) fn reveal(path: &Path) -> Result<()> {
    anyhow::ensure!(
        path.exists(),
        "文件已删除或不可访问，请按 Ctrl + R 重建索引：{}",
        path.display()
    );
    reveal_existing(path)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn reveal_existing(path: &Path) -> Result<()> {
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = std::process::Command::new("open");
        command.arg("-R").arg(path);
        command
    };
    #[cfg(target_os = "linux")]
    let mut command = {
        let mut command = std::process::Command::new("xdg-open");
        command.arg(
            path.parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(path),
        );
        command
    };
    // Pass paths directly to the system tool, including spaces and Unicode.
    let output = command.output().context("无法运行系统文件管理器")?;
    anyhow::ensure!(
        output.status.success(),
        "无法在资源管理器中打开：{}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(())
}

#[cfg(target_os = "windows")]
fn reveal_existing(path: &Path) -> Result<()> {
    use std::{os::windows::ffi::OsStrExt, ptr};
    use windows_sys::Win32::{
        System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize},
        UI::Shell::{SHOpenFolderAndSelectItems, SHParseDisplayName},
    };
    let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: This operation runs on a dedicated worker. Balance successful
    // COM initialization and free the Shell-allocated item list on every path.
    unsafe {
        let initialized = CoInitializeEx(ptr::null(), COINIT_APARTMENTTHREADED as u32);
        anyhow::ensure!(
            initialized >= 0,
            "无法初始化 Windows Shell COM：{initialized:#x}"
        );
        let mut item = ptr::null_mut();
        let parsed = SHParseDisplayName(
            path.as_ptr(),
            ptr::null_mut(),
            &mut item,
            0,
            ptr::null_mut(),
        );
        let opened = if parsed >= 0 {
            // With no child array, Shell selects this item in its parent folder.
            SHOpenFolderAndSelectItems(item, 0, ptr::null(), 0)
        } else {
            parsed
        };
        CoTaskMemFree(item.cast());
        CoUninitialize();
        anyhow::ensure!(opened >= 0, "无法在资源管理器中打开：{opened:#x}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_result_reports_rebuild_hint_without_launching_file_manager() {
        let dir = crate::file_search::test_support::TempDir::new();
        let error = reveal(&dir.0.join("已删除的文件.txt")).unwrap_err();
        assert!(error.to_string().contains("Ctrl + R"));
    }
}
