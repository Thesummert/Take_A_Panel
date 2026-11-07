use crate::{LauncherWindow, settings};
use anyhow::{Context, Result};
use slint::ComponentHandle;
use std::{
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::SystemTime,
};

#[derive(Clone)]
struct Appearance {
    path: String,
    opacity: f32,
    blur: f32,
}

impl Appearance {
    fn load() -> Result<Self> {
        let data = std::fs::read_to_string(settings::directory()?.join("appearance"))?;
        let mut lines = data.lines();
        let opacity: f32 = lines.next().context("缺少不透明度")?.parse()?;
        let blur: f32 = lines.next().context("缺少模糊强度")?.parse()?;
        anyhow::ensure!(opacity.is_finite() && blur.is_finite(), "外观设置无效");
        Ok(Self {
            path: lines.next().unwrap_or_default().into(),
            opacity: opacity.clamp(0.0, 1.0),
            blur: blur.clamp(0.0, 30.0),
        })
    }

    fn save(&self) -> Result<()> {
        let dir = settings::directory()?;
        std::fs::create_dir_all(&dir)?;
        std::fs::write(
            dir.join("appearance"),
            format!("{}\n{}\n{}\n", self.opacity, self.blur, self.path),
        )?;
        Ok(())
    }

    fn render(
        &self,
        width: u32,
        height: u32,
        scale: f32,
        cache: &mut BackgroundCache,
        disk: Option<&Path>,
    ) -> Result<Option<image::RgbImage>> {
        if self.path.is_empty() || self.opacity == 0.0 {
            return Ok(None);
        }
        let source = SourceKey::new(&self.path, width, height)?;
        let sigma = self.blur * scale;
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        source.hash(&mut hasher);
        sigma.to_bits().hash(&mut hasher);
        self.opacity.to_bits().hash(&mut hasher);
        let key = hasher.finish();
        if let Some(image) = disk.and_then(|path| read_cached(path, key, width, height)) {
            return Ok(Some(image));
        }
        let image = cache.render(source, sigma, self.opacity)?;
        if let Some(path) = disk {
            let _ = write_cached(path, key, &image);
        }
        Ok(Some(image))
    }
}

#[derive(Hash, PartialEq, Eq)]
struct SourceKey {
    path: PathBuf,
    length: u64,
    modified: SystemTime,
    width: u32,
    height: u32,
}

impl SourceKey {
    fn new(path: &str, width: u32, height: u32) -> Result<Self> {
        let path = std::fs::canonicalize(path).context("无法读取背景图片")?;
        let metadata = std::fs::metadata(&path)?;
        Ok(Self {
            path,
            length: metadata.len(),
            modified: metadata.modified()?,
            width,
            height,
        })
    }
}

#[derive(Default)]
struct BackgroundCache {
    // Keep only one source and one blur result; memory does not grow with edits.
    source: Option<(SourceKey, image::RgbImage)>,
    blurred: Option<(u32, image::RgbImage)>,
}

impl BackgroundCache {
    fn render(&mut self, source: SourceKey, sigma: f32, opacity: f32) -> Result<image::RgbImage> {
        if self.source.as_ref().map(|(key, _)| key) != Some(&source) {
            let decoded = image::open(&source.path).context("无法读取背景图片")?;
            // Crop before resampling, especially for very wide/tall images.
            let (w, h) = (decoded.width() as u64, decoded.height() as u64);
            let (tw, th) = (source.width as u64, source.height as u64);
            let (cw, ch) = if w * th > h * tw {
                (h * tw / th, h)
            } else {
                (w, w * th / tw)
            };
            let cropped = decoded.crop_imm(
                ((w - cw) / 2) as u32,
                ((h - ch) / 2) as u32,
                cw.max(1) as u32,
                ch.max(1) as u32,
            );
            let resized = cropped
                .resize_exact(
                    source.width,
                    source.height,
                    image::imageops::FilterType::Triangle,
                )
                .to_rgba8();
            // Composite before blur: RGB is cheaper and transparent edges have
            // the correct matte instead of bleeding unassociated RGB values.
            self.source = Some((source, flatten(&resized, 1.0)));
            self.blurred = None;
        }
        let base = &self.source.as_ref().unwrap().1;
        let prepared = if sigma > 0.0 {
            if self.blurred.as_ref().map(|(key, _)| *key) != Some(sigma.to_bits()) {
                // Use the image crate's compiled implementation so development
                // builds benefit from its package optimization too.
                self.blurred = Some((
                    sigma.to_bits(),
                    image::DynamicImage::ImageRgb8(base.clone())
                        .fast_blur(sigma)
                        .into_rgb8(),
                ));
            }
            &self.blurred.as_ref().unwrap().1
        } else {
            base
        };
        let mut image = prepared.clone();
        if opacity < 1.0 {
            let tables: [[u8; 256]; 3] = std::array::from_fn(|channel| {
                std::array::from_fn(|value| {
                    (value as f32 * opacity + [25.0, 26.0, 32.0][channel] * (1.0 - opacity)).round()
                        as u8
                })
            });
            for pixel in image.as_flat_samples_mut().samples.as_chunks_mut::<3>().0 {
                pixel[0] = tables[0][pixel[0] as usize];
                pixel[1] = tables[1][pixel[1] as usize];
                pixel[2] = tables[2][pixel[2] as usize];
            }
        }
        Ok(image)
    }
}

fn read_cached(path: &Path, key: u64, width: u32, height: u32) -> Option<image::RgbImage> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).ok()?;
    let mut header = [0; 24];
    file.read_exact(&mut header).ok()?;
    if &header[..8] != b"TAPBG002"
        || header[8..16] != key.to_le_bytes()
        || header[16..20] != width.to_le_bytes()
        || header[20..24] != height.to_le_bytes()
    {
        return None;
    }
    let size = width as usize * height as usize * 3;
    if file.metadata().ok()?.len() != 24 + size as u64 {
        return None;
    }
    let mut pixels = vec![0; size];
    file.read_exact(&mut pixels).ok()?;
    image::RgbImage::from_raw(width, height, pixels)
}

fn write_cached(path: &Path, key: u64, image: &image::RgbImage) -> Result<()> {
    use std::io::Write;
    let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
    let result = (|| -> Result<()> {
        std::fs::create_dir_all(path.parent().context("缓存路径无效")?)?;
        let mut file = std::fs::File::create(&temporary)?;
        file.write_all(b"TAPBG002")?;
        file.write_all(&key.to_le_bytes())?;
        file.write_all(&image.width().to_le_bytes())?;
        file.write_all(&image.height().to_le_bytes())?;
        file.write_all(image.as_raw())?;
        drop(file);
        std::fs::rename(&temporary, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temporary);
    }
    result
}

fn flatten(image: &image::RgbaImage, opacity: f32) -> image::RgbImage {
    let mut output = image::RgbImage::new(image.width(), image.height());
    for (pixel, target) in image
        .as_raw()
        .as_chunks::<4>()
        .0
        .iter()
        .zip(output.as_flat_samples_mut().samples.as_chunks_mut::<3>().0)
    {
        if pixel[3] == 255 && opacity == 1.0 {
            target.copy_from_slice(&pixel[..3]);
            continue;
        }
        let alpha = pixel[3] as f32 / 255.0 * opacity;
        for channel in 0..3 {
            target[channel] = (pixel[channel] as f32 * alpha
                + [25.0, 26.0, 32.0][channel] * (1.0 - alpha))
                .round() as u8;
        }
    }
    output
}

fn apply(
    ui: &LauncherWindow,
    appearance: Appearance,
    save: bool,
    cache: Arc<Mutex<BackgroundCache>>,
) {
    ui.set_background_busy(true);
    if save {
        ui.set_settings_failed(false);
        ui.set_settings_message("正在处理背景图片…".into());
    }
    let scale = ui.window().scale_factor().clamp(1.0, 3.0);
    let width = (656.0 * scale).round() as u32;
    let height = (516.0 * scale).round() as u32;
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let result = (|| {
            let disk = settings::directory()
                .ok()
                .map(|dir| dir.join("background.rgb"));
            let mut cache = cache
                .lock()
                .map_err(|_| anyhow::anyhow!("图片缓存不可用"))?;
            let image = appearance.render(width, height, scale, &mut cache, disk.as_deref())?;
            if save {
                appearance.save()?;
            }
            // Pixel buffers can cross threads. Leave only the cheap Slint Image
            // construction on the event loop, including for cached backgrounds.
            Ok::<_, anyhow::Error>(image.map(|image| {
                slint::SharedPixelBuffer::<slint::Rgb8Pixel>::clone_from_slice(
                    image.as_raw(),
                    image.width(),
                    image.height(),
                )
            }))
        })();
        let _ = weak.upgrade_in_event_loop(move |ui| {
            ui.set_background_busy(false);
            match result {
                Ok(image) => {
                    let image = image.map(slint::Image::from_rgb8).unwrap_or_default();
                    ui.set_background_image(image);
                    if save {
                        ui.set_settings_failed(false);
                        ui.set_settings_message("外观已保存。".into());
                    }
                }
                Err(error) => {
                    ui.set_settings_failed(true);
                    ui.set_settings_message(format!("加载或保存外观失败：{error:#}").into());
                }
            }
        });
    });
}

pub fn bind(ui: &LauncherWindow) {
    let cache = Arc::new(Mutex::new(BackgroundCache::default()));
    if let Ok(appearance) = Appearance::load() {
        ui.set_background_path(appearance.path.as_str().into());
        ui.set_background_opacity(appearance.opacity);
        ui.set_background_blur(appearance.blur);
        apply(ui, appearance, false, cache.clone());
    }
    let weak = ui.as_weak();
    ui.on_save_appearance(move |path, opacity, blur| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        if ui.get_background_busy() {
            return;
        }
        let path = path.trim();
        if path.contains(['\n', '\r']) || !opacity.is_finite() || !blur.is_finite() {
            ui.set_settings_failed(true);
            ui.set_settings_message("背景路径或参数无效。".into());
            return;
        }
        apply(
            &ui,
            Appearance {
                path: path.into(),
                opacity: opacity.clamp(0.0, 1.0),
                blur: blur.clamp(0.0, 30.0),
            },
            true,
            cache.clone(),
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary_directory() -> PathBuf {
        let mut bytes = [0; 8];
        getrandom::fill(&mut bytes).unwrap();
        let dir = std::env::temp_dir().join(format!(
            "panel-image-test-{:016x}",
            u64::from_ne_bytes(bytes)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn caches_reuse_images_and_invalidate_when_source_or_parameters_change() {
        use std::io::Write;
        let dir = temporary_directory();
        let path = dir.join("source.png");
        let disk = dir.join("background.rgb");
        image::RgbaImage::from_pixel(16, 16, image::Rgba([225, 126, 132, 255]))
            .save(&path)
            .unwrap();
        let mut appearance = Appearance {
            path: path.to_string_lossy().into_owned(),
            opacity: 1.0,
            blur: 2.0,
        };
        let mut cache = BackgroundCache::default();
        let first = appearance
            .render(32, 24, 1.0, &mut cache, Some(&disk))
            .unwrap()
            .unwrap();
        // A fresh in-memory cache stays empty when startup can use the disk result.
        let mut restarted = BackgroundCache::default();
        assert_eq!(
            appearance
                .render(32, 24, 1.0, &mut restarted, Some(&disk))
                .unwrap()
                .unwrap(),
            first
        );
        assert!(restarted.source.is_none());
        let blurred = cache.blurred.as_ref().unwrap().1.as_ptr();
        appearance.opacity = 0.5;
        let faded = appearance
            .render(32, 24, 1.0, &mut cache, Some(&disk))
            .unwrap()
            .unwrap();
        assert_eq!(
            cache.blurred.as_ref().unwrap().1.as_ptr(),
            blurred,
            "opacity edits must reuse the blur result"
        );
        assert_ne!(first, faded);
        image::RgbaImage::from_pixel(16, 16, image::Rgba([20, 220, 40, 255]))
            .save(&path)
            .unwrap();
        // Force a different source length even on filesystems with coarse mtimes.
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"changed")
            .unwrap();
        let changed = appearance
            .render(32, 24, 1.0, &mut cache, Some(&disk))
            .unwrap()
            .unwrap();
        assert_ne!(changed, faded);
        // Corrupt cache files are disposable: fall back to source processing.
        std::fs::write(&disk, b"truncated").unwrap();
        assert_eq!(
            appearance
                .render(32, 24, 1.0, &mut cache, Some(&disk))
                .unwrap()
                .unwrap(),
            changed
        );
        let resized = appearance
            .render(20, 20, 1.0, &mut cache, Some(&disk))
            .unwrap()
            .unwrap();
        assert_eq!(resized.dimensions(), (20, 20));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    #[ignore = "manual background processing performance comparison"]
    fn background_loading_benchmark() {
        use std::time::Instant;
        let dir = temporary_directory();
        let path = dir.join("large.png");
        let disk = dir.join("background.rgb");
        image::RgbImage::from_fn(2560, 1440, |x, y| {
            image::Rgb([(x % 256) as u8, (y % 256) as u8, ((x + y) % 256) as u8])
        })
        .save(&path)
        .unwrap();
        let mut appearance = Appearance {
            path: path.to_string_lossy().into_owned(),
            opacity: 0.8,
            blur: 15.0,
        };
        let started = Instant::now();
        let old = image::open(&path)
            .unwrap()
            .resize_to_fill(656, 516, image::imageops::FilterType::Triangle)
            .to_rgba8();
        std::hint::black_box(flatten(&image::imageops::blur(&old, 15.0), 0.8));
        println!("previous: {:?}", started.elapsed());
        let mut cache = BackgroundCache::default();
        let started = Instant::now();
        std::hint::black_box(
            appearance
                .render(656, 516, 1.0, &mut cache, Some(&disk))
                .unwrap(),
        );
        println!("first load: {:?}", started.elapsed());
        let started = Instant::now();
        std::hint::black_box(
            appearance
                .render(656, 516, 1.0, &mut BackgroundCache::default(), Some(&disk))
                .unwrap(),
        );
        println!("disk cache: {:?}", started.elapsed());
        appearance.opacity = 0.6;
        let started = Instant::now();
        std::hint::black_box(
            appearance
                .render(656, 516, 1.0, &mut cache, Some(&disk))
                .unwrap(),
        );
        println!("opacity edit: {:?}", started.elapsed());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn flatten_combines_image_alpha_and_opacity_over_panel_color() {
        let image = image::RgbaImage::from_pixel(1, 1, image::Rgba([225, 126, 132, 128]));
        assert_eq!(flatten(&image, 0.5).get_pixel(0, 0).0, [75, 51, 57]);
        assert_eq!(flatten(&image, 0.0).get_pixel(0, 0).0, [25, 26, 32]);
    }

    #[test]
    fn background_is_cropped_to_display_size_and_invalid_images_fail() {
        let path =
            std::env::temp_dir().join(format!("panel-background-{}.png", std::process::id()));
        image::RgbaImage::from_pixel(128, 32, image::Rgba([225, 126, 132, 255]))
            .save(&path)
            .unwrap();
        let mut appearance = Appearance {
            path: path.to_string_lossy().into_owned(),
            opacity: 1.0,
            blur: 2.0,
        };
        let image = appearance
            .render(64, 48, 1.0, &mut BackgroundCache::default(), None)
            .unwrap()
            .unwrap();
        assert_eq!(image.dimensions(), (64, 48));
        assert_eq!(image.get_pixel(32, 24).0, [225, 126, 132]);
        std::fs::remove_file(path).unwrap();
        assert!(
            appearance
                .render(64, 48, 1.0, &mut BackgroundCache::default(), None)
                .is_err()
        );
        appearance.path.clear();
        assert!(
            appearance
                .render(64, 48, 1.0, &mut BackgroundCache::default(), None)
                .unwrap()
                .is_none()
        );
    }
}
