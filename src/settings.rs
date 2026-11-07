use anyhow::{Context, Result, bail};
use global_hotkey::hotkey::HotKey;
use std::{io::Write, path::PathBuf};

pub fn directory() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("TAKE_A_PANEL_CONFIG_DIR").filter(|s| !s.is_empty()) {
        return Ok(path.into());
    }
    #[cfg(target_os = "macos")]
    if let Some(home) = std::env::var_os("HOME") {
        return Ok(PathBuf::from(home).join("Library/Application Support/TakeAPanel"));
    }
    #[cfg(target_os = "windows")]
    if let Some(base) = std::env::var_os("APPDATA") {
        return Ok(PathBuf::from(base).join("TakeAPanel"));
    }
    #[cfg(target_os = "linux")]
    {
        if let Some(base) = std::env::var_os("XDG_CONFIG_HOME").filter(|s| !s.is_empty()) {
            return Ok(PathBuf::from(base).join("take-a-panel"));
        }
        if let Some(home) = std::env::var_os("HOME") {
            return Ok(PathBuf::from(home).join(".config/take-a-panel"));
        }
    }
    bail!("无法确定设置目录，请设置 TAKE_A_PANEL_CONFIG_DIR")
}

pub struct Settings {
    directory: PathBuf,
}

impl Settings {
    pub fn new(directory: PathBuf) -> Self {
        Self { directory }
    }

    pub fn load_hotkey(&self) -> Result<Option<String>> {
        match std::fs::read_to_string(self.directory.join("hotkey")) {
            Ok(value) => {
                let value = value.trim();
                value.parse::<HotKey>().context("已保存的快捷键格式错误")?;
                Ok(Some(value.into()))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error).context("无法读取快捷键设置"),
        }
    }

    pub fn save_hotkey(&self, value: &str) -> Result<()> {
        value.parse::<HotKey>().context("快捷键格式错误")?;
        std::fs::create_dir_all(&self.directory).context("无法创建设置目录")?;
        let mut random = [0; 8];
        getrandom::fill(&mut random)
            .map_err(|error| anyhow::anyhow!("无法创建临时设置文件名：{error}"))?;
        let temporary = self
            .directory
            .join(format!("hotkey.{:016x}.tmp", u64::from_ne_bytes(random)));
        let result = (|| -> Result<()> {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)?;
            writeln!(file, "{value}")?;
            file.sync_all()?;
            drop(file);
            std::fs::rename(&temporary, self.directory.join("hotkey"))?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        result.context("无法保存快捷键设置")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_hotkey_survives_reload_and_invalid_input_preserves_previous_value() {
        let mut random = [0; 8];
        getrandom::fill(&mut random).unwrap();
        let path = std::env::temp_dir().join(format!(
            "take-a-panel-settings-{:016x}",
            u64::from_ne_bytes(random)
        ));
        let settings = Settings::new(path.clone());
        assert_eq!(settings.load_hotkey().unwrap(), None);
        settings.save_hotkey("Alt+Space").unwrap();
        assert_eq!(
            Settings::new(path.clone())
                .load_hotkey()
                .unwrap()
                .as_deref(),
            Some("Alt+Space")
        );
        settings.save_hotkey("Ctrl+Alt+Space").unwrap();
        assert!(settings.save_hotkey("Ctrl+").is_err());
        assert_eq!(
            settings.load_hotkey().unwrap().as_deref(),
            Some("Ctrl+Alt+Space")
        );
        assert_eq!(std::fs::read_dir(&path).unwrap().count(), 1);
        std::fs::write(path.join("hotkey"), "invalid").unwrap();
        assert!(settings.load_hotkey().is_err());
        std::fs::remove_dir_all(path).unwrap();
    }
}
