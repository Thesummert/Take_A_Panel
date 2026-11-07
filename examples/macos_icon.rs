use anyhow::{Context, Result};
use resvg::{tiny_skia, usvg};
use std::path::PathBuf;

fn main() -> Result<()> {
    let directory = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .context("用法: cargo run --example macos_icon -- <AppIcon.iconset 目录>")?,
    );
    std::fs::create_dir_all(&directory)?;
    let tree = usvg::Tree::from_data(
        include_bytes!("../ui/assets/tray-p.svg"),
        &usvg::Options::default(),
    )?;
    for points in [16, 32, 128, 256, 512] {
        for scale in [1, 2] {
            let pixels = points * scale;
            let mut pixmap = tiny_skia::Pixmap::new(pixels, pixels).context("无法创建图标画布")?;
            resvg::render(
                &tree,
                tiny_skia::Transform::from_scale(
                    pixels as f32 / tree.size().width(),
                    pixels as f32 / tree.size().height(),
                ),
                &mut pixmap.as_mut(),
            );
            let suffix = if scale == 2 { "@2x" } else { "" };
            pixmap.save_png(directory.join(format!("icon_{points}x{points}{suffix}.png")))?;
        }
    }
    println!("图标已生成: {}", directory.display());
    Ok(())
}
