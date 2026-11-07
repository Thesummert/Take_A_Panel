use super::{ICON_SIZE, IconData, decode};
use crate::apps::Application;
use slint::SharedPixelBuffer;
use std::{
    path::{Path, PathBuf},
    sync::LazyLock,
};

static THEME: LazyLock<String> =
    LazyLock::new(|| freedesktop_icons::default_theme_gtk().unwrap_or_else(|| "hicolor".into()));

pub fn load(app: &Application) -> Option<IconData> {
    let name = app.icon_name.as_deref()?;
    let path = resolve(name)?;
    let metadata = std::fs::metadata(&path).ok()?;
    if metadata.len() > 16 * 1024 * 1024 {
        return None;
    }
    let bytes = std::fs::read(&path).ok()?;
    if path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("svg"))
    {
        return decode_svg(&bytes);
    }
    decode(&bytes)
}

fn resolve(name: &str) -> Option<PathBuf> {
    if name.is_empty() {
        return None;
    }
    let path = Path::new(name);
    if path.is_absolute() {
        return path.is_file().then(|| path.to_owned());
    }
    freedesktop_icons::lookup(name)
        .with_size(ICON_SIZE as u16)
        .with_theme(&THEME)
        .with_cache()
        .find()
}

fn decode_svg(bytes: &[u8]) -> Option<IconData> {
    let tree = resvg::usvg::Tree::from_data(bytes, &resvg::usvg::Options::default()).ok()?;
    let size = tree.size();
    let scale = ICON_SIZE as f32 / size.width().max(size.height());
    let width = (size.width() * scale).round().max(1.) as u32;
    let height = (size.height() * scale).round().max(1.) as u32;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(width, height)?;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    Some(IconData {
        pixels: SharedPixelBuffer::clone_from_slice(pixmap.data(), width, height),
        premultiplied: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn svg_icon_is_rasterized_with_transparency_and_aspect_ratio() {
        let icon = decode_svg(br##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="50"><rect width="100" height="50" fill="#f00" fill-opacity=".5"/></svg>"##).unwrap();
        assert_eq!((icon.pixels.width(), icon.pixels.height()), (76, 38));
        assert!(icon.premultiplied);
        assert!(icon.pixels.as_slice()[0].a > 100);
        assert!(decode_svg(b"broken svg").is_none());
    }

    #[test]
    fn absolute_icon_paths_and_invalid_icon_fallback() {
        let path = std::env::temp_dir().join(format!(
            "take a panel invalid icon {}.png",
            std::process::id()
        ));
        std::fs::write(&path, b"not an image").unwrap();
        assert_eq!(resolve(path.to_str().unwrap()), Some(path.clone()));
        let app = Application {
            name: "Demo".into(),
            detail: String::new(),
            keywords: String::new(),
            target: crate::apps::LaunchTarget::File("/unused/demo.desktop".into()),
            icon_name: Some(path.to_string_lossy().into_owned()),
        };
        assert!(load(&app).is_none());
        std::fs::remove_file(&path).unwrap();
        assert!(resolve(path.to_str().unwrap()).is_none());
        assert!(resolve("").is_none());
    }
}
