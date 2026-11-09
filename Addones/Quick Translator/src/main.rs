use serde::Deserialize;
use serde_json::json;
use std::io::{Read, Write};
use std::process::{Command, Stdio};

#[derive(Deserialize)]
struct Request {
    api_version: u32,
    action: String,
    #[serde(default)]
    query: String,
    id: Option<String>,
    #[serde(default)]
    values: serde_json::Value,
    #[serde(default)]
    config: serde_json::Value,
}

fn translate(text: &str, from: &str, to: &str) -> Result<String, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("请输入要翻译的文字。".into());
    }
    if text.len() > 500 {
        return Err("原文超过 500 字节，请缩短后重试。".into());
    }
    let valid_language = |value: &str| {
        !value.is_empty()
            && value.len() <= 35
            && value.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    };
    if !valid_language(from) || !valid_language(to) {
        return Err("请输入有效的语言代码，例如 en、zh-CN 或 ja。".into());
    }
    let pair = format!("{from}|{to}");
    let mut child = Command::new("curl")
        .args([
            "--silent",
            "--show-error",
            "--fail",
            "--max-time",
            "15",
            "--get",
            "--data-urlencode",
            "q@-",
            "--data-urlencode",
        ])
        .arg(format!("langpair={pair}"))
        .arg("https://api.mymemory.translated.net/get")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("无法启动 curl：{e}"))?;
    child
        .stdin
        .take()
        .ok_or("无法发送原文")?
        .write_all(text.as_bytes())
        .map_err(|e| format!("无法发送原文：{e}"))?;
    let output = child
        .wait_with_output()
        .map_err(|e| format!("翻译请求失败：{e}"))?;
    if !output.status.success() {
        return Err(format!(
            "翻译请求失败：{}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let data: serde_json::Value =
        serde_json::from_slice(&output.stdout).map_err(|_| "翻译服务返回了无效数据。")?;
    if data["responseStatus"].as_i64() != Some(200) {
        return Err(format!(
            "翻译服务错误：{}",
            data["responseDetails"].as_str().unwrap_or("请稍后重试")
        ));
    }
    data["responseData"]["translatedText"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| "翻译服务没有返回译文。".into())
}

const LANGUAGES: &[(&str, &str)] = &[
    ("简体中文", "zh-CN"),
    ("繁体中文", "zh-TW"),
    ("英语", "en"),
    ("日语", "ja"),
    ("韩语", "ko"),
    ("法语", "fr"),
    ("德语", "de"),
    ("西班牙语", "es"),
    ("葡萄牙语", "pt"),
    ("意大利语", "it"),
    ("俄语", "ru"),
    ("阿拉伯语", "ar"),
    ("印地语", "hi"),
    ("泰语", "th"),
    ("越南语", "vi"),
    ("印尼语", "id"),
    ("荷兰语", "nl"),
    ("波兰语", "pl"),
    ("土耳其语", "tr"),
    ("乌克兰语", "uk"),
];

fn language_label(value: &str) -> &str {
    LANGUAGES
        .iter()
        .find(|(name, code)| value == *code || value == *name)
        .map(|(name, _)| *name)
        .unwrap_or(value)
}

fn language_code(value: &str) -> &str {
    LANGUAGES
        .iter()
        .find(|(name, code)| value == *name || value == *code)
        .map(|(_, code)| *code)
        .unwrap_or(value)
}

fn language_options(value: &str) -> Vec<&str> {
    let mut options: Vec<&str> = LANGUAGES.iter().map(|(name, _)| *name).collect();
    // Preserve an existing custom language from an earlier plugin version.
    let value = language_label(value);
    if !options.contains(&value) && !value.is_empty() && value.len() <= 128 {
        options.push(value);
    }
    options
}

fn make_panel(source: &str, from: &str, to: &str, result: &str) -> serde_json::Value {
    json!({"title": "Quick Translator", "submit": "翻译", "focus": "source", "fields": [
        {"key": "source", "label": "原文（Enter 翻译，Shift+Enter 换行，最多 500 字节）", "type": "text", "value": source},
        {"key": "from", "label": "源语言", "type": "choice", "value": language_label(from), "options": language_options(from)},
        {"key": "to", "label": "目标语言", "type": "choice", "value": language_label(to), "options": language_options(to)},
        {"key": "result", "label": "译文", "type": "output", "value": result}
    ]})
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut input = String::new();
    std::io::stdin()
        .take(64 * 1024)
        .read_to_string(&mut input)?;
    let request: Request = serde_json::from_str(&input)?;
    if request.api_version != 1 {
        return Err("不支持的插件接口版本".into());
    }
    let response = match request.action.as_str() {
        "search" => {
            json!({"results": [{"id": "open", "title": "打开翻译面板", "detail": "按 Enter 打开 Quick Translator"}]})
        }
        "activate" if request.id.as_deref() == Some("open") => {
            let from = request.config["source_language"].as_str().unwrap_or("en");
            let to = request.config["target_language"]
                .as_str()
                .unwrap_or("zh-CN");
            json!({"panel": make_panel(&request.query, from, to, "")})
        }
        "panel_submit" if request.id.as_deref() == Some("open") => {
            let source = request.values["source"].as_str().unwrap_or("");
            let from = request.values["from"].as_str().unwrap_or("").trim();
            let to = request.values["to"].as_str().unwrap_or("").trim();
            let result = translate(source, language_code(from), language_code(to))
                .unwrap_or_else(|error| format!("翻译失败：{error}"));
            json!({"panel": make_panel(source, from, to, &result)})
        }
        _ => return Err("未知插件请求".into()),
    };
    println!("{response}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{language_code, language_label, language_options, make_panel, translate};
    #[test]
    fn dropdown_maps_labels_and_preserves_custom_codes() {
        assert_eq!(language_label("en"), "英语");
        assert_eq!(language_code("简体中文"), "zh-CN");
        assert!(language_options("eo").contains(&"eo"));
        let panel = make_panel("hello", "en", "zh-CN", "");
        assert_eq!(panel["fields"][1]["type"], "choice");
        assert_eq!(panel["fields"][1]["value"], "英语");
        assert_eq!(panel["fields"][2]["value"], "简体中文");
    }
    #[test]
    fn rejects_empty_and_overlong_input() {
        assert!(translate("  ", "en", "zh-CN").is_err());
        assert!(translate(&"中".repeat(167), "zh-CN", "en").is_err());
        assert!(translate("hello", "en|fr", "en").is_err());
    }
}
