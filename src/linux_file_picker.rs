use anyhow::{Context, Result, bail};
use std::{collections::HashMap, path::PathBuf, process::Command};
use zbus::{
    blocking::{Connection, Proxy},
    zvariant::{OwnedObjectPath, OwnedValue, Value},
};

pub fn pick_background() -> Result<Option<PathBuf>> {
    match portal() {
        Ok(selection) => Ok(selection),
        Err(portal_error) => {
            let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
            fallback(&desktop).with_context(|| format!("系统文件选择器不可用：{portal_error:#}"))
        }
    }
}

fn portal() -> Result<Option<PathBuf>> {
    let connection = Connection::session()?;
    let mut random = [0; 8];
    getrandom::fill(&mut random).map_err(|e| anyhow::anyhow!("{e}"))?;
    let token = format!("takeapanel_{:016x}", u64::from_ne_bytes(random));
    let sender = connection
        .unique_name()
        .context("缺少 D-Bus 连接名")?
        .as_str()
        .trim_start_matches(':')
        .replace('.', "_");
    let path = format!("/org/freedesktop/portal/desktop/request/{sender}/{token}");
    let request = Proxy::new(
        &connection,
        "org.freedesktop.portal.Desktop",
        path.as_str(),
        "org.freedesktop.portal.Request",
    )?;
    // Subscribe before OpenFile so a fast response cannot be lost.
    let mut responses = request.receive_signal("Response")?;
    let chooser = Proxy::new(
        &connection,
        "org.freedesktop.portal.Desktop",
        "/org/freedesktop/portal/desktop",
        "org.freedesktop.portal.FileChooser",
    )?;
    let filters = vec![("图片", vec![(1u32, "image/png"), (1u32, "image/jpeg")])];
    let options: HashMap<&str, Value<'_>> = HashMap::from([
        ("handle_token", Value::from(token.as_str())),
        ("multiple", Value::from(false)),
        ("filters", Value::from(filters)),
    ]);
    let handle: OwnedObjectPath = chooser.call("OpenFile", &("", "选择背景图片", options))?;
    // Older portal implementations may return a different request path.
    let legacy;
    if handle.as_str() != path {
        legacy = Proxy::new(
            &connection,
            "org.freedesktop.portal.Desktop",
            handle.as_str(),
            "org.freedesktop.portal.Request",
        )?;
        responses = legacy.receive_signal("Response")?;
    }
    let message = responses.next().context("文件选择器连接已关闭")?;
    let (status, mut values): (u32, HashMap<String, OwnedValue>) = message.body().deserialize()?;
    match status {
        0 => {
            let uris = Vec::<String>::try_from(values.remove("uris").context("未返回文件路径")?)?;
            let uri = uris.first().context("未选择文件")?;
            Ok(Some(file_uri_path(uri)?))
        }
        1 => Ok(None),
        _ => bail!("系统文件选择器异常结束"),
    }
}

fn file_uri_path(uri: &str) -> Result<PathBuf> {
    url::Url::parse(uri)?
        .to_file_path()
        .map_err(|_| anyhow::anyhow!("请选择本地图片文件"))
}

fn candidates(desktop: &str) -> [&'static str; 3] {
    if desktop
        .split(':')
        .any(|part| part.eq_ignore_ascii_case("KDE"))
    {
        ["kdialog", "zenity", "yad"]
    } else {
        ["zenity", "yad", "kdialog"]
    }
}

fn fallback(desktop: &str) -> Result<Option<PathBuf>> {
    let mut errors = Vec::new();
    for program in candidates(desktop) {
        let args: &[&str] = if program == "kdialog" {
            &[
                "--getopenfilename",
                ".",
                "*.png *.jpg *.jpeg *.PNG *.JPG *.JPEG|图片",
                "--title",
                "选择背景图片",
            ]
        } else {
            &[
                "--file-selection",
                "--title=选择背景图片",
                "--file-filter=图片 | *.png *.jpg *.jpeg *.PNG *.JPG *.JPEG",
            ]
        };
        match Command::new(program).args(args).output() {
            Ok(output) if output.status.success() => {
                let path = String::from_utf8(output.stdout)?;
                let path = path.trim_end_matches(['\r', '\n']);
                return Ok((!path.is_empty()).then(|| PathBuf::from(path)));
            }
            // Cancellation must not open another dialog.
            Ok(output) if output.status.code() == Some(1) => return Ok(None),
            Ok(output) => errors.push(format!(
                "{program}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )),
            Err(error) => errors.push(format!("{program}: {error}")),
        }
    }
    bail!(
        "没有可用的文件选择器，可手动输入图片路径。{}",
        errors.join("；")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portal_paths_preserve_spaces_unicode_and_percent_signs() {
        assert_eq!(
            file_uri_path("file:///tmp/%E8%83%8C%E6%99%AF%20a%25.png").unwrap(),
            PathBuf::from("/tmp/背景 a%.png")
        );
        assert!(file_uri_path("https://example.com/image.png").is_err());
    }

    #[test]
    fn fallback_prefers_the_desktop_tool() {
        assert_eq!(candidates("KDE")[0], "kdialog");
        assert_eq!(candidates("ubuntu:GNOME")[0], "zenity");
        assert_eq!(candidates("plasma:KDE")[0], "kdialog");
    }
}
