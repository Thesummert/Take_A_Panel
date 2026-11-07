use super::Status;
use anyhow::{Context, Result, bail};
use std::{os::windows::ffi::OsStrExt, ptr};
use windows_sys::Win32::{
    Foundation::{ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, ERROR_SUCCESS},
    System::Registry::{
        HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_BINARY, RRF_RT_REG_SZ, RegDeleteKeyValueW,
        RegGetValueW, RegSetKeyValueW,
    },
};

const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const APPROVED: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";
const NAME: &str = "Take a Panel";

fn wide(value: impl AsRef<std::ffi::OsStr>) -> Vec<u16> {
    value.as_ref().encode_wide().chain(Some(0)).collect()
}

fn read_value(key: &str, flags: u32) -> Result<Option<Vec<u8>>> {
    let key = wide(key);
    let name = wide(NAME);
    let mut size = 0;
    // SAFETY: NUL-terminated key/value names, valid size output, and predefined
    // HKCU handle. Null data queries the required size before allocating.
    let result = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            name.as_ptr(),
            flags,
            ptr::null_mut(),
            ptr::null_mut(),
            &mut size,
        )
    };
    if result == ERROR_FILE_NOT_FOUND || result == ERROR_PATH_NOT_FOUND {
        return Ok(None);
    }
    if result != ERROR_SUCCESS {
        return Err(std::io::Error::from_raw_os_error(result as i32)).context("无法读取系统启动项");
    }
    let mut data = vec![0u8; size as usize];
    // SAFETY: data has the byte capacity returned by RegGetValueW.
    let result = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            name.as_ptr(),
            flags,
            ptr::null_mut(),
            data.as_mut_ptr().cast(),
            &mut size,
        )
    };
    if result != ERROR_SUCCESS {
        return Err(std::io::Error::from_raw_os_error(result as i32)).context("无法读取系统启动项");
    }
    data.truncate(size as usize);
    Ok(Some(data))
}

pub(super) fn status() -> Result<Status> {
    let Some(command) = read_value(RUN, RRF_RT_REG_SZ)? else {
        return Ok(Status::Disabled);
    };
    if command.len() <= 2 {
        return Ok(Status::Disabled);
    }
    // Read Windows' startup approval without changing a decision made in Settings.
    if let Some(approval) = read_value(APPROVED, RRF_RT_REG_BINARY)?
        && approval.first().is_some_and(|value| matches!(value, 3 | 7))
    {
        return Ok(Status::RequiresApproval);
    }
    Ok(Status::Enabled)
}

pub(super) fn set_enabled(enabled: bool) -> Result<()> {
    let key = wide(RUN);
    let name = wide(NAME);
    let result = if enabled {
        let executable = std::env::current_exe().context("无法确定应用路径")?;
        let mut command = wide("\"");
        command.pop();
        command.extend(executable.as_os_str().encode_wide());
        command.extend("\" --hidden".encode_utf16());
        if command.len() > 260 {
            bail!("应用路径过长，无法注册 Windows 启动项，请将应用移动到较短的路径");
        }
        command.push(0);
        // SAFETY: REG_SZ data is a NUL-terminated UTF-16 buffer; byte size
        // includes its terminator. Only this app's HKCU value is changed.
        unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                name.as_ptr(),
                REG_SZ,
                command.as_ptr().cast(),
                (command.len() * 2) as u32,
            )
        }
    } else {
        // SAFETY: Removes only this app's named Run value, leaving the key intact.
        unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr()) }
    };
    if result == ERROR_SUCCESS
        || (!enabled && matches!(result, ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND))
    {
        return Ok(());
    }
    Err(std::io::Error::from_raw_os_error(result as i32)).context("系统拒绝修改开机自启")
}

pub(super) fn open_settings() -> Result<()> {
    let operation = wide("open");
    let uri = wide("ms-settings:startupapps");
    // SAFETY: ShellExecuteW receives valid UTF-16 strings and no parent window;
    // the fixed URI opens the user's startup settings without a command shell.
    let result = unsafe {
        windows_sys::Win32::UI::Shell::ShellExecuteW(
            ptr::null_mut(),
            operation.as_ptr(),
            uri.as_ptr(),
            ptr::null(),
            ptr::null(),
            1,
        )
    };
    anyhow::ensure!(
        result as isize > 32,
        "无法打开系统启动项设置：{}",
        result as isize
    );
    Ok(())
}
