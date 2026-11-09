use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

const MAX_OUTPUT: u64 = 256 * 1024;
const MAX_RESULTS: usize = 30;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub api_version: u32,
    pub id: String,
    pub name: String,
    pub keyword: String,
    pub executable: PathBuf,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub commands: Vec<Item>,
    #[serde(default)]
    pub settings: Vec<Setting>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Setting {
    pub key: String,
    pub label: String,
    #[serde(default)]
    pub description: String,
    #[serde(rename = "type")]
    pub kind: SettingKind,
    pub default: serde_json::Value,
    #[serde(default)]
    pub options: Vec<String>,
    #[serde(default)]
    pub min: Option<f64>,
    #[serde(default)]
    pub max: Option<f64>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SettingKind {
    Text,
    Boolean,
    Integer,
    Number,
    Choice,
}

impl Setting {
    fn validate(&self, value: &serde_json::Value) -> Result<()> {
        let valid = match self.kind {
            SettingKind::Text => value.as_str().is_some_and(|v| v.len() <= 4096),
            SettingKind::Boolean => value.is_boolean(),
            SettingKind::Integer => value.as_i64().is_some(),
            SettingKind::Number => value.as_f64().is_some_and(f64::is_finite),
            SettingKind::Choice => value
                .as_str()
                .is_some_and(|v| self.options.iter().any(|o| o == v)),
        };
        ensure!(valid, "设置“{}”的值无效", self.label);
        if let Some(number) = value.as_f64() {
            ensure!(
                self.min.is_none_or(|min| number >= min)
                    && self.max.is_none_or(|max| number <= max),
                "设置“{}”超出允许范围",
                self.label
            );
        }
        Ok(())
    }

    pub fn parse_draft(&self, text: &str) -> Result<serde_json::Value> {
        let value = match self.kind {
            SettingKind::Text | SettingKind::Choice => serde_json::Value::String(text.into()),
            SettingKind::Boolean => serde_json::Value::Bool(text.parse().context("开关值无效")?),
            SettingKind::Integer => serde_json::Value::from(
                text.trim()
                    .parse::<i64>()
                    .with_context(|| format!("设置“{}”需要整数", self.label))?,
            ),
            SettingKind::Number => serde_json::Value::from(
                text.trim()
                    .parse::<f64>()
                    .with_context(|| format!("设置“{}”需要数字", self.label))?,
            ),
        };
        self.validate(&value)?;
        Ok(value)
    }
}

impl Manifest {
    pub fn effective_config(&self, config: &serde_json::Value) -> serde_json::Value {
        let mut config = config.as_object().cloned().unwrap_or_default();
        for setting in &self.settings {
            config
                .entry(setting.key.clone())
                .or_insert_with(|| setting.default.clone());
        }
        serde_json::Value::Object(config)
    }

    pub fn config_from_drafts(
        &self,
        config: &serde_json::Value,
        drafts: &[String],
    ) -> Result<serde_json::Value> {
        ensure!(
            drafts.len() == self.settings.len(),
            "设置项已变更，请重新选择插件"
        );
        let mut result = self.effective_config(config);
        let object = result.as_object_mut().unwrap();
        for (setting, draft) in self.settings.iter().zip(drafts) {
            object.insert(setting.key.clone(), setting.parse_draft(draft)?);
        }
        ensure!(
            serde_json::to_vec(&result)?.len() <= 32 * 1024,
            "配置不能超过 32 KiB"
        );
        Ok(result)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Item {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub detail: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Addon {
    pub manifest_path: PathBuf,
    pub enabled: bool,
    pub config: serde_json::Value,
}
#[derive(Default, Clone, Debug, Serialize, Deserialize)]
pub struct Registry {
    pub addons: Vec<Addon>,
}

impl Manifest {
    pub fn read(path: &Path) -> Result<Self> {
        let metadata = std::fs::metadata(path).context("无法读取插件清单")?;
        ensure!(metadata.len() <= 64 * 1024, "插件清单过大");
        let mut manifest: Self =
            serde_json::from_slice(&std::fs::read(path)?).context("插件清单格式错误")?;
        ensure!(manifest.api_version == 1, "不支持的插件接口版本");
        ensure!(
            !manifest.id.is_empty()
                && manifest.id.len() <= 128
                && manifest
                    .id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c)),
            "插件 id 只能包含字母、数字、-、_、."
        );
        ensure!(
            !manifest.name.trim().is_empty() && manifest.name.len() <= 128,
            "插件名称无效"
        );
        ensure!(
            !manifest.keyword.is_empty()
                && manifest.keyword.len() <= 32
                && manifest
                    .keyword
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_".contains(c)),
            "触发词只能包含字母、数字、-、_"
        );
        ensure!(manifest.commands.len() <= MAX_RESULTS, "插件命令过多");
        ensure!(
            manifest.args.len() <= 32 && manifest.args.iter().all(|arg| arg.len() <= 4096),
            "插件参数过多或过长"
        );
        ensure!(
            !["ss", "clp"]
                .iter()
                .any(|word| manifest.keyword.eq_ignore_ascii_case(word)),
            "触发词与内置命令冲突"
        );
        ensure!(manifest.settings.len() <= 50, "插件设置项过多");
        let mut keys = std::collections::HashSet::new();
        for setting in &manifest.settings {
            ensure!(
                !setting.key.is_empty()
                    && setting.key.len() <= 128
                    && setting
                        .key
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || "-_".contains(c))
                    && keys.insert(&setting.key),
                "插件设置键无效或重复"
            );
            ensure!(
                !setting.label.trim().is_empty()
                    && setting.label.len() <= 128
                    && setting.description.len() <= 512,
                "插件设置说明无效"
            );
            ensure!(
                setting.options.len() <= 50
                    && setting
                        .options
                        .iter()
                        .all(|o| !o.is_empty() && o.len() <= 256),
                "插件设置选项无效"
            );
            ensure!(
                setting.kind == SettingKind::Choice || setting.options.is_empty(),
                "只有选项设置可以声明 options"
            );
            ensure!(
                matches!(setting.kind, SettingKind::Integer | SettingKind::Number)
                    || (setting.min.is_none() && setting.max.is_none()),
                "只有数字设置可以声明范围"
            );
            ensure!(
                setting.min.is_none_or(f64::is_finite)
                    && setting.max.is_none_or(f64::is_finite)
                    && setting
                        .min
                        .zip(setting.max)
                        .is_none_or(|(min, max)| min <= max),
                "插件设置范围无效"
            );
            setting
                .validate(&setting.default)
                .context("插件设置默认值无效")?;
        }
        for item in &manifest.commands {
            validate_item(item)?;
        }
        if manifest.executable.is_relative() {
            manifest.executable = path
                .parent()
                .context("清单路径无效")?
                .join(&manifest.executable);
        }
        manifest.executable = manifest
            .executable
            .canonicalize()
            .context("无法访问插件程序")?;
        ensure!(manifest.executable.is_file(), "插件程序不是文件");
        Ok(manifest)
    }
}
fn validate_item(item: &Item) -> Result<()> {
    ensure!(
        !item.id.is_empty()
            && item.id.len() <= 256
            && !item.title.trim().is_empty()
            && item.title.len() <= 512
            && item.detail.len() <= 2048,
        "插件结果格式无效"
    );
    Ok(())
}
impl Registry {
    pub fn load(settings: &crate::settings::Settings) -> Result<Self> {
        match settings.load_addons()? {
            Some(text) => serde_json::from_str(&text).context("插件设置格式错误"),
            None => Ok(Self::default()),
        }
    }
    pub fn save(&self, settings: &crate::settings::Settings) -> Result<()> {
        settings.save_addons(&serde_json::to_string(self)?)
    }
    pub fn add(&mut self, path: &str) -> Result<()> {
        let path = PathBuf::from(path.trim())
            .canonicalize()
            .context("无法访问插件清单")?;
        let manifest = Manifest::read(&path)?;
        for addon in &self.addons {
            if addon.manifest_path == path {
                bail!("插件已添加");
            }
            if let Ok(existing) = Manifest::read(&addon.manifest_path) {
                ensure!(existing.id != manifest.id, "插件 id 已存在");
                ensure!(
                    !existing.keyword.eq_ignore_ascii_case(&manifest.keyword),
                    "插件触发词已存在"
                );
            }
        }
        ensure!(self.addons.len() < 50, "插件数量已达上限");
        self.addons.push(Addon {
            manifest_path: path,
            enabled: true,
            config: serde_json::json!({}),
        });
        Ok(())
    }
    pub fn matching(&self, query: &str) -> Option<(Addon, Manifest, String)> {
        let query = query.trim_start();
        let (keyword, remainder) = query.split_once(char::is_whitespace).unwrap_or((query, ""));
        self.addons.iter().filter(|a| a.enabled).find_map(|addon| {
            let manifest = Manifest::read(&addon.manifest_path).ok()?;
            if manifest.keyword.eq_ignore_ascii_case(keyword) {
                Some((addon.clone(), manifest, remainder.trim_start().to_owned()))
            } else {
                None
            }
        })
    }
}

#[derive(Serialize)]
struct Request<'a> {
    api_version: u32,
    action: &'a str,
    query: &'a str,
    id: Option<&'a str>,
    config: &'a serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    values: Option<&'a serde_json::Value>,
}
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Panel {
    pub title: String,
    pub submit: String,
    #[serde(default)]
    pub focus: String,
    #[serde(default)]
    pub fields: Vec<PanelField>,
}
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct PanelField {
    pub key: String,
    pub label: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub value: String,
    #[serde(default)]
    pub options: Vec<String>,
}
impl Panel {
    fn validate(&self) -> Result<()> {
        ensure!(
            !self.title.trim().is_empty()
                && self.title.len() <= 128
                && !self.submit.trim().is_empty()
                && self.submit.len() <= 128,
            "插件面板标题或按钮无效"
        );
        ensure!(self.fields.len() <= 8, "插件面板字段过多");
        let mut keys = std::collections::HashSet::new();
        for field in &self.fields {
            ensure!(
                !field.key.is_empty()
                    && field.key.len() <= 128
                    && field
                        .key
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || "-_".contains(c))
                    && keys.insert(&field.key),
                "插件面板字段键无效"
            );
            ensure!(
                !field.label.trim().is_empty()
                    && field.label.len() <= 128
                    && field.value.len() <= 8192,
                "插件面板字段内容无效"
            );
            ensure!(
                matches!(field.kind.as_str(), "text" | "line" | "choice" | "output"),
                "不支持的插件面板字段类型"
            );
            ensure!(
                field.options.len() <= 30
                    && field
                        .options
                        .iter()
                        .all(|o| !o.is_empty() && o.len() <= 128),
                "插件面板选项无效"
            );
            ensure!(
                if field.kind == "choice" {
                    field.options.contains(&field.value)
                } else {
                    field.options.is_empty()
                },
                "插件面板选项或默认值无效"
            );
        }
        ensure!(
            self.focus.is_empty()
                || self.fields.iter().any(|field| field.key == self.focus
                    && matches!(field.kind.as_str(), "text" | "line")),
            "插件面板焦点字段无效"
        );
        Ok(())
    }
}
#[derive(Deserialize)]
struct Response {
    #[serde(default)]
    results: Vec<Item>,
    #[serde(default)]
    message: String,
    #[serde(default)]
    keep_open: bool,
    #[serde(default)]
    panel: Option<Panel>,
}

fn invoke(
    addon: &Addon,
    manifest: &Manifest,
    action: &str,
    query: &str,
    id: Option<&str>,
    revision: &AtomicU64,
    generation: u64,
    values: Option<&serde_json::Value>,
) -> Result<Option<Response>> {
    let config = manifest.effective_config(&addon.config);
    let request = serde_json::to_vec(&Request {
        api_version: 1,
        action,
        query,
        id,
        config: &config,
        values,
    })?;
    let mut command = Command::new(&manifest.executable);
    command
        .args(&manifest.args)
        .current_dir(manifest.manifest_path_parent(&addon.manifest_path))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut child = command.spawn().context("无法启动插件程序")?;
    let mut stdin = child.stdin.take().context("无法打开插件输入")?;
    let stdout = child.stdout.take().context("无法读取插件输出")?;
    std::thread::spawn(move || {
        let _ = stdin.write_all(&request);
    });
    let (output_tx, output_rx) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut data = Vec::new();
        let result = stdout.take(MAX_OUTPUT + 1).read_to_end(&mut data);
        let _ = output_tx.send((result, data));
    });
    let deadline =
        Instant::now() + Duration::from_secs(if action == "panel_submit" { 20 } else { 3 });
    let mut status = None;
    let mut output = None;
    loop {
        if revision.load(Ordering::Relaxed) != generation || Instant::now() >= deadline {
            let cancelled = revision.load(Ordering::Relaxed) != generation;
            if status.is_none() {
                let _ = child.kill();
                let _ = child.wait();
            }
            if cancelled {
                return Ok(None);
            } else {
                bail!("插件响应超时");
            }
        }
        if status.is_none() {
            status = child.try_wait()?;
        }
        if output.is_none() {
            output = output_rx.try_recv().ok();
        }
        if status.is_some() && output.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(15));
    }
    let (read, bytes) = output.unwrap();
    read.context("读取插件输出失败")?;
    ensure!(status.unwrap().success(), "插件进程退出失败");
    ensure!(bytes.len() <= MAX_OUTPUT as usize, "插件输出过大");
    let response: Response = serde_json::from_slice(&bytes).context("插件返回的 JSON 无效")?;
    ensure!(
        response.results.len() <= MAX_RESULTS && response.message.len() <= 2048,
        "插件返回的数据过多"
    );
    for item in &response.results {
        validate_item(item)?;
    }
    if let Some(panel) = &response.panel {
        panel.validate()?;
    }
    Ok(Some(response))
}
impl Manifest {
    fn manifest_path_parent<'a>(&self, path: &'a Path) -> &'a Path {
        path.parent().unwrap_or(Path::new("."))
    }
}

pub fn search(
    addon: &Addon,
    manifest: &Manifest,
    query: &str,
    revision: &AtomicU64,
    generation: u64,
) -> Result<Option<Vec<Item>>> {
    let mut items: Vec<Item> = manifest
        .commands
        .iter()
        .filter(|item| {
            query.is_empty()
                || format!("{} {}", item.title, item.detail)
                    .to_lowercase()
                    .contains(&query.to_lowercase())
        })
        .cloned()
        .collect();
    let Some(response) = invoke(
        addon, manifest, "search", query, None, revision, generation, None,
    )?
    else {
        return Ok(None);
    };
    items.extend(response.results);
    items.truncate(MAX_RESULTS);
    Ok(Some(items))
}
pub fn activate(
    addon: &Addon,
    manifest: &Manifest,
    query: &str,
    id: &str,
) -> Result<(String, bool, Option<Panel>)> {
    let response = invoke(
        addon,
        manifest,
        "activate",
        query,
        Some(id),
        &AtomicU64::new(0),
        0,
        None,
    )?
    .context("插件调用已取消")?;
    Ok((response.message, response.keep_open, response.panel))
}
pub fn submit_panel(
    addon: &Addon,
    manifest: &Manifest,
    query: &str,
    id: &str,
    values: &serde_json::Value,
    revision: &AtomicU64,
    generation: u64,
) -> Result<Option<Panel>> {
    let response = invoke(
        addon,
        manifest,
        "panel_submit",
        query,
        Some(id),
        revision,
        generation,
        Some(values),
    )?;
    match response {
        Some(response) => Ok(Some(response.panel.context("插件未返回面板")?)),
        None => Ok(None),
    }
}
pub fn revision() -> Arc<AtomicU64> {
    Arc::new(AtomicU64::new(0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file_search::test_support::TempDir;

    #[test]
    fn manifest_registry_and_settings_round_trip() {
        let dir = TempDir::new();
        let manifest = dir.0.join("manifest.json");
        let executable = std::env::current_exe().unwrap();
        std::fs::write(&manifest, serde_json::to_vec(&serde_json::json!({"api_version":1,"id":"test.demo","name":"Demo","keyword":"demo","executable":executable})).unwrap()).unwrap();
        let settings = crate::settings::Settings::new(dir.0.join("settings"));
        let mut registry = Registry::default();
        registry.add(manifest.to_str().unwrap()).unwrap();
        assert!(registry.add(manifest.to_str().unwrap()).is_err());
        registry.addons[0].config = serde_json::json!({"greeting":"你好"});
        registry.save(&settings).unwrap();
        let reloaded = Registry::load(&settings).unwrap();
        let (_, _, query) = reloaded.matching("DeMo  张三").unwrap();
        assert_eq!(query, "张三");
        assert_eq!(reloaded.addons[0].config["greeting"], "你好");
        assert!(reloaded.matching("demonstration").is_none());
        let invalid = dir.0.join("invalid.json");
        std::fs::write(&invalid, serde_json::to_vec(&serde_json::json!({"api_version":2,"id":"x","name":"X","keyword":"x","executable":executable})).unwrap()).unwrap();
        assert!(registry.add(invalid.to_str().unwrap()).is_err());
    }

    #[test]
    fn setting_schema_defaults_validation_and_preserves_other_values() {
        let dir = TempDir::new();
        let path = dir.0.join("manifest.json");
        let mut json = serde_json::json!({
            "api_version": 1, "id": "form", "name": "Form", "keyword": "form",
            "executable": std::env::current_exe().unwrap(),
            "settings": [
                {"key":"name", "type":"text", "label":"Name", "default":"Default"},
                {"key":"enabled", "type":"boolean", "label":"Enabled", "default":true},
                {"key":"count", "type":"integer", "label":"Count", "default":2, "min":1, "max":3},
                {"key":"scale", "type":"number", "label":"Scale", "default":1.5, "min":0.5},
                {"key":"style", "type":"choice", "label":"Style", "default":"a", "options":["a","b"]}
            ]
        });
        std::fs::write(&path, serde_json::to_vec(&json).unwrap()).unwrap();
        let manifest = Manifest::read(&path).unwrap();
        let existing = serde_json::json!({"name":"Saved", "extra":42});
        let effective = manifest.effective_config(&existing);
        assert_eq!(effective["name"], "Saved");
        assert_eq!(effective["enabled"], true);
        let drafts = ["New", "false", "3", "0.75", "b"].map(str::to_owned);
        let saved = manifest.config_from_drafts(&existing, &drafts).unwrap();
        assert_eq!(
            saved,
            serde_json::json!({"name":"New", "enabled":false, "count":3, "scale":0.75, "style":"b", "extra":42})
        );
        for (index, invalid) in [(1, "yes"), (2, "4"), (2, "1.5"), (3, "NaN"), (4, "missing")] {
            let mut invalid_drafts = drafts.clone();
            invalid_drafts[index] = invalid.into();
            assert!(
                manifest
                    .config_from_drafts(&existing, &invalid_drafts)
                    .is_err()
            );
        }
        json["settings"][1]["key"] = "name".into();
        std::fs::write(&path, serde_json::to_vec(&json).unwrap()).unwrap();
        assert!(Manifest::read(&path).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn hung_plugin_times_out_and_cancelled_search_is_discarded() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new();
        let script = dir.0.join("slow");
        std::fs::write(&script, "#!/bin/sh\nexec sleep 10\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = dir.0.join("manifest.json");
        std::fs::write(&path, serde_json::to_vec(&serde_json::json!({"api_version":1,"id":"slow","name":"Slow","keyword":"slow","executable":script})).unwrap()).unwrap();
        let manifest = Manifest::read(&path).unwrap();
        let addon = Addon {
            manifest_path: path,
            enabled: true,
            config: serde_json::json!({}),
        };
        let revision = Arc::new(AtomicU64::new(1));
        let worker_revision = revision.clone();
        let worker_addon = addon.clone();
        let worker_manifest = manifest.clone();
        let worker = std::thread::spawn(move || {
            search(&worker_addon, &worker_manifest, "", &worker_revision, 1)
        });
        std::thread::sleep(Duration::from_millis(100));
        revision.store(2, Ordering::Relaxed);
        assert!(worker.join().unwrap().unwrap().is_none());
        let started = Instant::now();
        assert!(
            search(&addon, &manifest, "", &revision, 2)
                .unwrap_err()
                .to_string()
                .contains("超时")
        );
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[cfg(unix)]
    #[test]
    fn demo_search_commands_activation_and_cancellation() {
        if Command::new("python3").arg("--version").output().is_err() {
            return;
        }
        let path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/addon-demo/manifest.json");
        let manifest = Manifest::read(&path).unwrap();
        let addon = Addon {
            manifest_path: path,
            enabled: true,
            config: serde_json::json!({"greeting":"早上好"}),
        };
        let revision = AtomicU64::new(1);
        let items = search(&addon, &manifest, "世界", &revision, 1)
            .unwrap()
            .unwrap();
        assert_eq!(items[0].title, "早上好，世界");
        assert_eq!(
            activate(&addon, &manifest, "世界", "greet").unwrap(),
            ("早上好，世界".into(), true, None)
        );
        let items = search(&addon, &manifest, "", &revision, 1)
            .unwrap()
            .unwrap();
        assert!(items.iter().any(|item| item.id == "about"));
        revision.store(2, Ordering::Relaxed);
        assert!(
            search(&addon, &manifest, "世界", &revision, 1)
                .unwrap()
                .is_none()
        );
    }
}
