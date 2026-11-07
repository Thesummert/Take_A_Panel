use super::Status;
use anyhow::{Context, Result, bail};
use std::{
    io::Write,
    path::{Path, PathBuf},
};

fn entry_path() -> Result<PathBuf> {
    let directory =
        if let Some(base) = std::env::var_os("XDG_CONFIG_HOME").filter(|s| !s.is_empty()) {
            PathBuf::from(base)
        } else {
            PathBuf::from(std::env::var_os("HOME").context("无法确定用户自启目录")?).join(".config")
        };
    anyhow::ensure!(directory.is_absolute(), "XDG 自启目录必须是绝对路径");
    Ok(directory.join("autostart/com.takeapanel.launcher.desktop"))
}

// Exec quoting and desktop-entry string escaping are separate layers.
fn desktop_entry(executable: &Path) -> Result<String> {
    let path = executable.to_str().context("程序路径不是有效的 UTF-8")?;
    anyhow::ensure!(executable.is_absolute(), "程序路径必须是绝对路径");
    if path.contains(['\n', '\r', '\0']) {
        bail!("程序路径包含无法用于启动项的字符");
    }
    let mut argument = String::from("\"");
    for ch in path.chars() {
        match ch {
            '"' | '`' | '$' | '\\' => {
                argument.push('\\');
                argument.push(ch);
            }
            '%' => argument.push_str("%%"),
            _ => argument.push(ch),
        }
    }
    argument.push('"');
    let argument = argument.replace('\\', "\\\\").replace('\t', "\\t");
    Ok(format!(
        "[Desktop Entry]\nType=Application\nName=Take a Panel\nComment=Start Take a Panel in the background\nExec={argument} --hidden\nTerminal=false\nHidden=false\nX-GNOME-Autostart-enabled=true\n"
    ))
}

fn read_status(path: &Path) -> Result<Status> {
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Status::Disabled),
        Err(error) => return Err(error).context("无法读取 XDG 启动项"),
    };
    let mut in_entry = false;
    let mut enabled = true;
    let mut executable = false;
    for line in contents.lines().map(str::trim) {
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
        } else if in_entry && let Some((key, value)) = line.split_once('=') {
            match (key.trim(), value.trim()) {
                ("Hidden", "true") | ("X-GNOME-Autostart-enabled", "false") => enabled = false,
                ("Exec", value) => executable = !value.is_empty(),
                _ => {}
            }
        }
    }
    Ok(if enabled && executable {
        Status::Enabled
    } else {
        Status::Disabled
    })
}

fn write_entry(path: &Path, contents: &str) -> Result<()> {
    let parent = path.parent().context("无效的启动项路径")?;
    std::fs::create_dir_all(parent).context("无法创建 XDG 自启目录")?;
    let mut random = [0; 8];
    getrandom::fill(&mut random).map_err(|e| anyhow::anyhow!("无法创建临时启动项：{e}"))?;
    let temporary = parent.join(format!(
        ".take-a-panel-{:016x}.tmp",
        u64::from_ne_bytes(random)
    ));
    let result = (|| -> Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(contents.as_bytes())?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temporary, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temporary);
    }
    result.context("无法保存 XDG 启动项")
}

pub(super) fn status() -> Result<Status> {
    read_status(&entry_path()?)
}

pub(super) fn set_enabled(enabled: bool) -> Result<()> {
    let path = entry_path()?;
    if enabled {
        write_entry(&path, &desktop_entry(&std::env::current_exe()?)?)
    } else {
        // A Hidden override also suppresses any system-wide entry of this name.
        write_entry(
            &path,
            "[Desktop Entry]\nType=Application\nName=Take a Panel\nHidden=true\n",
        )
    }
}

pub(super) fn open_settings() -> Result<()> {
    bail!("请在桌面环境的“启动应用程序”设置中管理，或使用本页的启用 / 关闭按钮")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_exec_escapes_paths_without_shell_expansion_or_field_codes() {
        assert!(desktop_entry(Path::new("relative/path")).is_err());
        assert!(desktop_entry(Path::new("/tmp/line\nbreak")).is_err());
        let entry = desktop_entry(Path::new("/tmp/a b/%test/\"$`\\panel")).unwrap();
        assert!(
            entry.contains("Exec=\"/tmp/a b/%%test/\\\\\"\\\\$\\\\`\\\\\\\\panel\" --hidden\n"),
            "{entry}"
        );
    }

    #[test]
    fn reads_desktop_disablement_and_atomically_replaces_entries() {
        let mut random = [0; 8];
        getrandom::fill(&mut random).unwrap();
        let dir = std::env::temp_dir().join(format!(
            "panel-autostart-{:016x}",
            u64::from_ne_bytes(random)
        ));
        let path = dir.join("autostart/test.desktop");
        assert_eq!(read_status(&path).unwrap(), Status::Disabled);
        let contents = desktop_entry(Path::new("/opt/Take a Panel/panel")).unwrap();
        write_entry(&path, &contents).unwrap();
        assert_eq!(read_status(&path).unwrap(), Status::Enabled);
        for disabled in [
            contents.replace("Hidden=false", "Hidden=true"),
            contents.replace("enabled=true", "enabled=false"),
        ] {
            write_entry(&path, &disabled).unwrap();
            assert_eq!(read_status(&path).unwrap(), Status::Disabled);
        }
        write_entry(
            &path,
            "[Desktop Entry]\nHidden=false\n[Other]\nExec=ignored\n",
        )
        .unwrap();
        assert_eq!(read_status(&path).unwrap(), Status::Disabled);
        write_entry(&path, &contents).unwrap();
        assert_eq!(read_status(&path).unwrap(), Status::Enabled);
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
