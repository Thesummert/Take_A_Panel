use super::{Application, Catalog, LaunchTarget};
use anyhow::{Result, bail};
use std::{
    collections::HashSet,
    os::windows::{ffi::OsStrExt, process::CommandExt},
    path::PathBuf,
    process::Command,
};
use walkdir::WalkDir;
use windows_sys::Win32::{
    System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize},
    UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL},
};

pub fn discover() -> Catalog {
    let mut catalog = Catalog::default();
    let mut names = HashSet::new();
    for variable in ["APPDATA", "PROGRAMDATA"] {
        let Some(base) = std::env::var_os(variable) else {
            continue;
        };
        let root = PathBuf::from(base).join("Microsoft/Windows/Start Menu/Programs");
        if !root.exists() {
            continue;
        }
        for entry in WalkDir::new(root).max_depth(12) {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    catalog.warnings.push(error.to_string());
                    continue;
                }
            };
            let path = entry.path();
            let extension = path
                .extension()
                .unwrap_or_default()
                .to_string_lossy()
                .to_lowercase();
            if !entry.file_type().is_file() || !["lnk", "appref-ms"].contains(&extension.as_str()) {
                continue;
            }
            let name = path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            if !names.insert(name.to_lowercase()) {
                continue;
            }
            catalog.apps.push(Application {
                name,
                detail: path.display().to_string(),
                keywords: String::new(),
                target: LaunchTarget::File(path.to_owned()),
            });
        }
    }
    // Get-StartApps includes Store/UWP apps that have no .lnk entry. Force UTF-8
    // so localized names survive Windows PowerShell's legacy output encoding.
    let output = Command::new("powershell.exe")
        .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command",
            "[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new(); @(Get-StartApps | Select-Object Name,AppID) | ConvertTo-Json -Compress"])
        .creation_flags(0x08000000) // CREATE_NO_WINDOW
        .output();
    match output {
        Ok(output) if output.status.success() => {
            match serde_json::from_slice::<serde_json::Value>(&output.stdout) {
                Ok(value) => {
                    let items: Vec<_> = match &value {
                        serde_json::Value::Array(items) => items.iter().collect(),
                        serde_json::Value::Object(_) => vec![&value],
                        _ => vec![],
                    };
                    for item in items {
                        let (Some(name), Some(id)) =
                            (item["Name"].as_str(), item["AppID"].as_str())
                        else {
                            continue;
                        };
                        if name.is_empty() || id.is_empty() || !names.insert(name.to_lowercase()) {
                            continue;
                        }
                        catalog.apps.push(Application {
                            name: name.into(),
                            detail: id.into(),
                            keywords: id.into(),
                            target: LaunchTarget::AppId(id.into()),
                        });
                    }
                }
                Err(error) => catalog
                    .warnings
                    .push(format!("解析 Get-StartApps 失败: {error}")),
            }
        }
        Ok(output) => catalog.warnings.push(format!(
            "Get-StartApps 失败: {}",
            String::from_utf8_lossy(&output.stderr)
        )),
        Err(error) => catalog
            .warnings
            .push(format!("无法查询 Store 应用: {error}")),
    }
    catalog
}

pub fn launch(app: &Application) -> Result<()> {
    let target = match &app.target {
        LaunchTarget::File(path) => path.as_os_str().to_owned(),
        LaunchTarget::AppId(id) => format!("shell:AppsFolder\\{id}").into(),
    };
    let target: Vec<u16> = target.encode_wide().chain(Some(0)).collect();
    let operation: Vec<u16> = "open\0".encode_utf16().collect();
    // Shell extensions used to resolve shortcuts may require COM on this worker.
    let initialized = unsafe { CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32) };
    if initialized < 0 {
        bail!("无法初始化 Windows Shell COM（HRESULT {initialized:#x}）");
    }
    // Both strings are NUL terminated and live for this synchronous Shell call.
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            operation.as_ptr(),
            target.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    } as isize;
    unsafe { CoUninitialize() };
    if result <= 32 {
        bail!(
            "Windows 无法启动 {}（ShellExecute 错误 {result}）",
            app.name
        );
    }
    Ok(())
}
