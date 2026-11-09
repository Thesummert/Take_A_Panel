use anyhow::{Context, Result, bail};
use global_hotkey::hotkey::HotKey;
use std::collections::BTreeMap;
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

pub fn cache_directory() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("TAKE_A_PANEL_CONFIG_DIR").filter(|s| !s.is_empty()) {
        return Ok(PathBuf::from(path).join("cache"));
    }
    #[cfg(target_os = "macos")]
    if let Some(home) = std::env::var_os("HOME") {
        return Ok(PathBuf::from(home).join("Library/Caches/TakeAPanel"));
    }
    #[cfg(target_os = "windows")]
    if let Some(base) = std::env::var_os("LOCALAPPDATA") {
        return Ok(PathBuf::from(base).join("TakeAPanel/cache"));
    }
    #[cfg(target_os = "linux")]
    {
        if let Some(base) = std::env::var_os("XDG_CACHE_HOME").filter(|s| !s.is_empty()) {
            return Ok(PathBuf::from(base).join("take-a-panel"));
        }
        if let Some(home) = std::env::var_os("HOME") {
            return Ok(PathBuf::from(home).join(".cache/take-a-panel"));
        }
    }
    bail!("无法确定索引缓存目录")
}

#[derive(Clone)]
pub struct Settings {
    directory: PathBuf,
}

impl Settings {
    pub fn new(directory: PathBuf) -> Self {
        Self { directory }
    }

    pub fn load_window_size(&self) -> Result<Option<(u32, u32)>> {
        match std::fs::read_to_string(self.directory.join("window-size")) {
            Ok(value) => Ok(Some(parse_window_size(&value)?)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error).context("无法读取窗口大小"),
        }
    }

    pub fn save_window_size(&self, width: u32, height: u32) -> Result<()> {
        let value = format!("{width} {height}");
        parse_window_size(&value)?;
        self.save_text("window-size", &value)
            .context("无法保存窗口大小")
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
        self.save_text("hotkey", value)
            .context("无法保存快捷键设置")
    }

    pub fn load_file_search_roots(&self) -> Result<String> {
        match std::fs::read_to_string(self.directory.join("file-search-roots")) {
            Ok(value) => Ok(value),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
            Err(error) => Err(error).context("无法读取文件索引范围"),
        }
    }

    pub fn save_file_search_roots(&self, value: &str) -> Result<()> {
        self.save_text("file-search-roots", value)
            .context("无法保存文件索引范围")
    }

    pub fn load_launch_counts(&self) -> Result<BTreeMap<String, u64>> {
        match std::fs::read_to_string(self.directory.join("launch-counts.json")) {
            Ok(value) => serde_json::from_str(&value).context("启动次数记录格式错误"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(error) => Err(error).context("无法读取启动次数记录"),
        }
    }

    pub fn save_launch_counts(&self, counts: &BTreeMap<String, u64>) -> Result<()> {
        self.save_text("launch-counts.json", &serde_json::to_string(counts)?)
            .context("无法保存启动次数记录")
    }

    fn save_text(&self, name: &str, value: &str) -> Result<()> {
        std::fs::create_dir_all(&self.directory).context("无法创建设置目录")?;
        let mut random = [0; 8];
        getrandom::fill(&mut random)
            .map_err(|error| anyhow::anyhow!("无法创建临时设置文件名：{error}"))?;
        let temporary = self
            .directory
            .join(format!("{name}.{:016x}.tmp", u64::from_ne_bytes(random)));
        let result = (|| -> Result<()> {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)?;
            writeln!(file, "{value}")?;
            file.sync_all()?;
            drop(file);
            std::fs::rename(&temporary, self.directory.join(name))?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        result
    }
}

pub fn parse_window_size(value: &str) -> Result<(u32, u32)> {
    let mut parts = value.split_whitespace();
    let width = parts.next().and_then(|s| s.parse().ok());
    let height = parts.next().and_then(|s| s.parse().ok());
    match (width, height, parts.next()) {
        (Some(width @ 680..=1920), Some(height @ 540..=1440), None) => Ok((width, height)),
        _ => bail!("宽度须为 680–1920，高度须为 540–1440 的整数"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_size_round_trip_and_validation() {
        let mut random = [0; 8];
        getrandom::fill(&mut random).unwrap();
        let path =
            std::env::temp_dir().join(format!("panel-size-{:016x}", u64::from_ne_bytes(random)));
        let settings = Settings::new(path.clone());
        assert_eq!(settings.load_window_size().unwrap(), None);
        settings.save_window_size(900, 700).unwrap();
        assert_eq!(
            Settings::new(path.clone()).load_window_size().unwrap(),
            Some((900, 700))
        );
        assert!(settings.save_window_size(679, 700).is_err());
        assert!(parse_window_size("900 700 extra").is_err());
        assert_eq!(settings.load_window_size().unwrap(), Some((900, 700)));
        std::fs::write(path.join("window-size"), "invalid").unwrap();
        assert!(settings.load_window_size().is_err());
        std::fs::remove_dir_all(path).unwrap();
    }

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
