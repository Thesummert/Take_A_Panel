use crate::{LauncherWindow, settings};
use anyhow::{Context, Result};
use slint::ComponentHandle;
use std::{
    cell::RefCell,
    collections::VecDeque,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    rc::Rc,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime},
};

const MEMORY_CACHE_LIMIT: usize = 128 * 1024 * 1024;
const DISK_CACHE_LIMIT: u64 = 256 * 1024 * 1024;

#[derive(Clone)]
struct Appearance {
    path: String,
    opacity: f32,
    blur: f32,
    light: bool,
}

impl Appearance {
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
        let source = SourceKey::new(&self.path, width, height, self.light)?;
        let sigma = self.blur * scale;
        let key = cache_key(&source, sigma, self.opacity);
        if let Some(image) = cache.get(key) {
            return Ok(Some(image));
        }
        let file = disk.map(|dir| dir.join(format!("{key:016x}.rgb")));
        if let Some(image) = file
            .as_deref()
            .and_then(|path| read_cached(path, key, width, height))
        {
            cache.insert(key, image.clone());
            return Ok(Some(image));
        }
        let image = cache.render(source, sigma, self.opacity)?;
        if let Some(path) = file.as_deref() {
            let _ = write_cached(path, key, &image);
            if let Some(dir) = disk {
                trim_disk_cache(dir);
            }
        }
        cache.insert(key, image.clone());
        Ok(Some(image))
    }
}

fn cache_key(source: &SourceKey, sigma: f32, opacity: f32) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    source.hash(&mut hasher);
    sigma.to_bits().hash(&mut hasher);
    opacity.to_bits().hash(&mut hasher);
    hasher.finish()
}

#[derive(Hash, PartialEq, Eq)]
struct SourceKey {
    path: PathBuf,
    length: u64,
    modified: SystemTime,
    width: u32,
    height: u32,
    light: bool,
}

impl SourceKey {
    fn new(path: &str, width: u32, height: u32, light: bool) -> Result<Self> {
        let path = std::fs::canonicalize(path).context("无法读取背景图片")?;
        let metadata = std::fs::metadata(&path)?;
        Ok(Self {
            path,
            length: metadata.len(),
            modified: metadata.modified()?,
            width,
            height,
            light,
        })
    }
}

#[derive(Default)]
struct BackgroundCache {
    source: Option<(SourceKey, image::RgbImage)>,
    blurred: Option<(u32, image::RgbImage)>,
    images: VecDeque<(u64, image::RgbImage)>,
    bytes: usize,
}

impl BackgroundCache {
    fn get(&mut self, key: u64) -> Option<image::RgbImage> {
        let index = self.images.iter().position(|(id, _)| *id == key)?;
        let entry = self.images.remove(index)?;
        let image = entry.1.clone();
        self.images.push_back(entry);
        Some(image)
    }

    fn insert(&mut self, key: u64, image: image::RgbImage) {
        let size = image.as_raw().len();
        if size > MEMORY_CACHE_LIMIT {
            return;
        }
        if let Some(index) = self.images.iter().position(|(id, _)| *id == key) {
            if let Some((_, old)) = self.images.remove(index) {
                self.bytes -= old.as_raw().len();
            }
        }
        while self.bytes + size > MEMORY_CACHE_LIMIT {
            if let Some((_, old)) = self.images.pop_front() {
                self.bytes -= old.as_raw().len();
            }
        }
        self.bytes += size;
        self.images.push_back((key, image));
    }

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
            let matte = panel_color(source.light);
            self.source = Some((source, flatten(&resized, 1.0, matte)));
            self.blurred = None;
        }
        let matte = panel_color(self.source.as_ref().unwrap().0.light);
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
                    (value as f32 * opacity + matte[channel] as f32 * (1.0 - opacity)).round() as u8
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

fn trim_disk_cache(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<_> = entries
        .flatten()
        .filter_map(|entry| {
            if entry.path().extension()? != "rgb" {
                return None;
            }
            let metadata = entry.metadata().ok()?;
            if !metadata.is_file() {
                return None;
            }
            Some((
                entry.path(),
                metadata.len(),
                metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            ))
        })
        .collect();
    let mut size: u64 = files.iter().map(|(_, size, _)| size).sum();
    if size <= DISK_CACHE_LIMIT {
        return;
    }
    files.sort_by_key(|(_, _, modified)| *modified);
    for (path, bytes, _) in files {
        if size <= DISK_CACHE_LIMIT {
            break;
        }
        if std::fs::remove_file(path).is_ok() {
            size = size.saturating_sub(bytes);
        }
    }
}

fn clear_background_cache(cache: &mut BackgroundCache, dir: &Path) -> Result<()> {
    *cache = BackgroundCache::default();
    for path in [dir.join("background-cache"), dir.join("background.rgb")] {
        let result = if path.is_dir() {
            std::fs::remove_dir_all(path)
        } else {
            std::fs::remove_file(path)
        };
        if let Err(error) = result
            && error.kind() != std::io::ErrorKind::NotFound
        {
            return Err(error.into());
        }
    }
    Ok(())
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

fn panel_color(light: bool) -> [u8; 3] {
    if light { [248, 247, 251] } else { [25, 26, 32] }
}

fn flatten(image: &image::RgbaImage, opacity: f32, matte: [u8; 3]) -> image::RgbImage {
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
                + matte[channel] as f32 * (1.0 - alpha))
                .round() as u8;
        }
    }
    output
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct AppearanceConfig {
    paths: Vec<String>,
    opacity: f32,
    blur: f32,
    // 0: fixed, 1: on presentation, 2..: timed intervals.
    rotation: i32,
}

impl AppearanceConfig {
    fn parse(data: &str) -> Result<Self> {
        let config = if data.trim_start().starts_with('{') {
            serde_json::from_str::<Self>(data).context("外观设置格式错误")?
        } else {
            let mut lines = data.lines();
            let opacity = lines.next().context("缺少不透明度")?.parse()?;
            let blur = lines.next().context("缺少模糊强度")?.parse()?;
            let path = lines.next().unwrap_or_default();
            Self {
                paths: if path.is_empty() {
                    vec![]
                } else {
                    vec![path.into()]
                },
                opacity,
                blur,
                rotation: 0,
            }
        };
        anyhow::ensure!(
            config.opacity.is_finite()
                && config.blur.is_finite()
                && (0..=6).contains(&config.rotation),
            "外观设置无效"
        );
        Ok(Self {
            opacity: config.opacity.clamp(0.0, 1.0),
            blur: config.blur.clamp(0.0, 30.0),
            ..config
        })
    }

    fn load() -> Result<Self> {
        Self::parse(&std::fs::read_to_string(
            settings::directory()?.join("appearance"),
        )?)
    }

    fn save(&self) -> Result<()> {
        let dir = settings::directory()?;
        std::fs::create_dir_all(&dir)?;
        let mut random = [0; 8];
        getrandom::fill(&mut random).map_err(|e| anyhow::anyhow!("{e}"))?;
        let tmp = dir.join(format!(
            "appearance.{:016x}.tmp",
            u64::from_ne_bytes(random)
        ));
        let result = (|| -> Result<()> {
            std::fs::write(&tmp, serde_json::to_vec(self)?)?;
            std::fs::rename(&tmp, dir.join("appearance"))?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(tmp);
        }
        result
    }

    fn interval(&self) -> Option<Duration> {
        match self.rotation {
            2 => Some(Duration::from_secs(60)),
            3 => Some(Duration::from_secs(5 * 60)),
            4 => Some(Duration::from_secs(15 * 60)),
            5 => Some(Duration::from_secs(30 * 60)),
            6 => Some(Duration::from_secs(60 * 60)),
            _ => None,
        }
    }
}

struct Rotation {
    ui: slint::Weak<LauncherWindow>,
    config: RefCell<AppearanceConfig>,
    index: std::cell::Cell<usize>,
    pending: std::cell::Cell<bool>,
    theme_pending: std::cell::Cell<bool>,
    timer: slint::Timer,
    pump: slint::Timer,
    cache: Arc<Mutex<BackgroundCache>>,
    generation: Arc<AtomicU64>,
}

impl Rotation {
    fn refresh_theme(self: &Rc<Self>) {
        let Some(ui) = self.ui.upgrade() else {
            return;
        };
        if ui.get_background_busy() {
            self.theme_pending.set(true);
            return;
        }
        self.theme_pending.set(false);
        let config = self.config.borrow().clone();
        self.apply(config.clone(), self.index.get(), false);
        self.warm(config, ui.window().scale_factor().clamp(1.0, 3.0));
    }

    fn warm(&self, config: AppearanceConfig, scale: f32) {
        let Some(ui) = self.ui.upgrade() else {
            return;
        };
        let width = ((ui.get_window_width() - 24) as f32 * scale).round() as u32;
        let height = ((ui.get_window_height() - 24) as f32 * scale).round() as u32;
        let light = ui.get_light_mode();
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let revision = self.generation.clone();
        let cache = self.cache.clone();
        std::thread::spawn(move || {
            let disk = settings::directory()
                .ok()
                .map(|dir| dir.join("background-cache"));
            for path in &config.paths {
                if revision.load(Ordering::SeqCst) != generation {
                    break;
                }
                if config.opacity == 0.0 {
                    break;
                }
                let Ok(source) = SourceKey::new(path, width, height, light) else {
                    continue;
                };
                let key = cache_key(&source, config.blur * scale, config.opacity);
                let Ok(cache_guard) = cache.lock() else {
                    break;
                };
                if cache_guard.images.iter().any(|(id, _)| *id == key) {
                    continue;
                }
                drop(cache_guard);
                let appearance = Appearance {
                    path: path.clone(),
                    opacity: config.opacity,
                    blur: config.blur,
                    light,
                };
                let file = disk.as_ref().map(|dir| dir.join(format!("{key:016x}.rgb")));
                let image = file
                    .as_deref()
                    .and_then(|path| read_cached(path, key, width, height))
                    .or_else(|| {
                        appearance
                            .render(width, height, scale, &mut BackgroundCache::default(), None)
                            .ok()
                            .flatten()
                    });
                let Some(image) = image else {
                    continue;
                };
                let Ok(mut cache) = cache.lock() else {
                    break;
                };
                if revision.load(Ordering::SeqCst) != generation {
                    break;
                }
                if let Some(file) = file.as_deref() {
                    let _ = write_cached(file, key, &image);
                    if let Some(dir) = disk.as_deref() {
                        trim_disk_cache(dir);
                    }
                }
                cache.insert(key, image);
            }
        });
    }

    fn clear_cache(self: &Rc<Self>) {
        let Some(ui) = self.ui.upgrade() else {
            return;
        };
        if ui.get_background_busy() {
            return;
        }
        self.generation.fetch_add(1, Ordering::SeqCst);
        ui.set_background_busy(true);
        ui.set_settings_failed(false);
        ui.set_settings_message("正在清空背景缓存…".into());
        let (sender, receiver) = std::sync::mpsc::channel::<Result<()>>();
        let weak = Rc::downgrade(self);
        self.pump.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(30),
            move || {
                let result = match receiver.try_recv() {
                    Ok(result) => result,
                    Err(std::sync::mpsc::TryRecvError::Empty) => return,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        Err(anyhow::anyhow!("清理任务意外中断"))
                    }
                };
                let Some(rotation) = weak.upgrade() else {
                    return;
                };
                rotation.pump.stop();
                let Some(ui) = rotation.ui.upgrade() else {
                    return;
                };
                ui.set_background_busy(false);
                match result {
                    Ok(()) => {
                        ui.set_settings_failed(false);
                        ui.set_settings_message("背景缓存已清空。".into());
                    }
                    Err(error) => {
                        ui.set_settings_failed(true);
                        ui.set_settings_message(format!("清空背景缓存失败：{error:#}").into());
                    }
                }
                rotation.pending.set(false);
                if rotation.theme_pending.get() {
                    rotation.refresh_theme();
                }
            },
        );
        let cache = self.cache.clone();
        std::thread::spawn(move || {
            let result = (|| -> Result<()> {
                let mut cache = cache
                    .lock()
                    .map_err(|_| anyhow::anyhow!("图片缓存不可用"))?;
                let dir = settings::directory()?;
                clear_background_cache(&mut cache, &dir)
            })();
            let _ = sender.send(result);
        });
    }

    fn schedule(self: &Rc<Self>) {
        self.timer.stop();
        let config = self.config.borrow();
        if config.paths.len() < 2 {
            return;
        }
        if let Some(interval) = config.interval() {
            let weak = Rc::downgrade(self);
            self.timer
                .start(slint::TimerMode::Repeated, interval, move || {
                    if let Some(rotation) = weak.upgrade() {
                        rotation.advance();
                    }
                });
        }
    }

    fn advance(self: &Rc<Self>) {
        let Some(ui) = self.ui.upgrade() else {
            return;
        };
        let config = self.config.borrow().clone();
        if config.paths.len() < 2 {
            return;
        }
        if ui.get_background_busy() {
            self.pending.set(true);
            return;
        }
        let next = (self.index.get() + 1) % config.paths.len();
        self.apply(config, next, false);
    }

    fn apply(self: &Rc<Self>, config: AppearanceConfig, index: usize, save: bool) {
        let Some(ui) = self.ui.upgrade() else {
            return;
        };
        ui.set_background_busy(true);
        if save {
            ui.set_settings_failed(false);
            ui.set_settings_message("正在处理背景图片…".into());
        }
        let scale = ui.window().scale_factor().clamp(1.0, 3.0);
        let width = ((ui.get_window_width() - 24) as f32 * scale).round() as u32;
        let height = ((ui.get_window_height() - 24) as f32 * scale).round() as u32;
        let weak_ui = ui.as_weak();
        let weak_rotation = Rc::downgrade(self);
        // Rc cannot cross threads; return through the UI event loop using a
        // separately allocated callback registered on the event-loop thread.
        let (sender, receiver) = std::sync::mpsc::channel::<
            Result<(
                Option<slint::SharedPixelBuffer<slint::Rgb8Pixel>>,
                AppearanceConfig,
            )>,
        >();
        self.pump.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(30),
            move || {
                let result = match receiver.try_recv() {
                    Ok(result) => result,
                    Err(std::sync::mpsc::TryRecvError::Empty) => return,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        Err(anyhow::anyhow!("图片处理意外中断"))
                    }
                };
                let Some(rotation) = weak_rotation.upgrade() else {
                    return;
                };
                rotation.pump.stop();
                let Some(ui) = weak_ui.upgrade() else {
                    return;
                };
                ui.set_background_busy(false);
                match result {
                    Ok((image, config)) => {
                        ui.set_background_image(
                            image.map(slint::Image::from_rgb8).unwrap_or_default(),
                        );
                        rotation.index.set(index);
                        if save {
                            *rotation.config.borrow_mut() = config.clone();
                            rotation.schedule();
                            rotation.warm(config, scale);
                            ui.set_settings_failed(false);
                            ui.set_settings_message("外观已保存。".into());
                        }
                    }
                    Err(error) => {
                        if !save {
                            rotation.index.set(index);
                        }
                        ui.set_settings_failed(true);
                        ui.set_settings_message(format!("加载或保存外观失败：{error:#}").into());
                        if !save {
                            ui.set_error_message(format!("背景图片加载失败：{error:#}").into());
                        }
                    }
                }
                if rotation.theme_pending.get() {
                    rotation.refresh_theme();
                } else if rotation.pending.replace(false) && !save {
                    rotation.advance();
                }
            },
        );
        let cache = self.cache.clone();
        let light = ui.get_light_mode();
        std::thread::spawn(move || {
            let result = (|| -> Result<_> {
                if save {
                    for path in &config.paths {
                        image::open(path).with_context(|| format!("无效的背景图片：{path}"))?;
                    }
                }
                let appearance = Appearance {
                    path: config.paths.get(index).cloned().unwrap_or_default(),
                    opacity: config.opacity,
                    blur: config.blur,
                    light,
                };
                let disk = settings::directory()
                    .ok()
                    .map(|dir| dir.join("background-cache"));
                let mut cache = cache
                    .lock()
                    .map_err(|_| anyhow::anyhow!("图片缓存不可用"))?;
                let image = appearance.render(width, height, scale, &mut cache, disk.as_deref())?;
                if save {
                    config.save()?;
                }
                Ok((
                    image.map(|image| {
                        slint::SharedPixelBuffer::<slint::Rgb8Pixel>::clone_from_slice(
                            image.as_raw(),
                            image.width(),
                            image.height(),
                        )
                    }),
                    config,
                ))
            })();
            let _ = sender.send(result);
        });
    }
}

pub fn bind(ui: &LauncherWindow) {
    let theme_settings = settings::directory().map(settings::Settings::new);
    if let Ok(settings) = &theme_settings {
        match settings.load_theme() {
            Ok(Some(mode)) => ui.set_theme_mode(mode),
            Ok(None) => {}
            Err(error) => eprintln!("无法读取主题设置：{error:#}"),
        }
    }
    let loaded = AppearanceConfig::load().ok();
    let config = loaded.clone().unwrap_or(AppearanceConfig {
        paths: vec![],
        opacity: 1.0,
        blur: 0.0,
        rotation: 0,
    });
    ui.set_background_path(config.paths.join("\n").into());
    ui.set_background_opacity(config.opacity);
    ui.set_background_blur(config.blur);
    ui.set_background_rotation(config.rotation);
    let rotation = Rc::new(Rotation {
        ui: ui.as_weak(),
        config: RefCell::new(config.clone()),
        index: std::cell::Cell::new(0),
        pending: std::cell::Cell::new(false),
        theme_pending: std::cell::Cell::new(false),
        timer: slint::Timer::default(),
        pump: slint::Timer::default(),
        cache: Arc::new(Mutex::new(BackgroundCache::default())),
        generation: Arc::new(AtomicU64::new(0)),
    });
    rotation.schedule();
    if loaded.is_some() {
        rotation.apply(config.clone(), 0, false);
        rotation.warm(config, ui.window().scale_factor().clamp(1.0, 3.0));
    }
    let on_theme = rotation.clone();
    ui.on_set_theme(move |mode| {
        let Some(ui) = on_theme.ui.upgrade() else {
            return;
        };
        if ui.get_background_busy() {
            return;
        }
        let result = settings::directory()
            .map(settings::Settings::new)
            .and_then(|s| s.save_theme(mode));
        match result {
            Ok(()) => {
                ui.set_theme_mode(mode);
                ui.set_settings_failed(false);
                ui.set_settings_message("主题已保存。".into());
            }
            Err(error) => {
                ui.set_settings_failed(true);
                ui.set_settings_message(format!("保存主题失败：{error:#}").into());
            }
        }
    });
    let on_theme_changed = rotation.clone();
    ui.on_theme_changed(move || {
        on_theme_changed.refresh_theme();
    });
    let on_size = rotation.clone();
    ui.on_window_size_applied(move || {
        let config = on_size.config.borrow().clone();
        on_size.apply(config.clone(), on_size.index.get(), false);
        if let Some(ui) = on_size.ui.upgrade() {
            on_size.warm(config, ui.window().scale_factor().clamp(1.0, 3.0));
        }
    });
    let on_clear = rotation.clone();
    ui.on_clear_background_cache(move || on_clear.clear_cache());
    let on_present = rotation.clone();
    ui.on_background_presented(move || {
        if on_present.config.borrow().rotation == 1 {
            on_present.advance();
        }
    });
    ui.on_save_appearance(move |paths, opacity, blur, mode| {
        let Some(ui) = rotation.ui.upgrade() else {
            return;
        };
        if ui.get_background_busy() {
            return;
        }
        if !opacity.is_finite() || !blur.is_finite() || !(0..=6).contains(&mode) {
            ui.set_settings_failed(true);
            ui.set_settings_message("背景参数无效。".into());
            return;
        }
        let mut unique = Vec::new();
        for path in paths.lines().map(str::trim).filter(|p| !p.is_empty()) {
            if !unique.iter().any(|p| p == path) {
                unique.push(path.to_owned());
            }
        }
        rotation.apply(
            AppearanceConfig {
                paths: unique,
                opacity: opacity.clamp(0.0, 1.0),
                blur: blur.clamp(0.0, 30.0),
                rotation: mode,
            },
            0,
            true,
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multiple_backgrounds_are_cached_and_can_be_cleared() {
        let dir = temporary_directory();
        let disk = dir.join("background-cache");
        let mut cache = BackgroundCache::default();
        let mut originals = Vec::new();
        for (name, color) in [("one.png", [200, 20, 30]), ("two.png", [10, 200, 30])] {
            let path = dir.join(name);
            image::RgbImage::from_pixel(8, 8, image::Rgb(color))
                .save(&path)
                .unwrap();
            let appearance = Appearance {
                path: path.to_string_lossy().into_owned(),
                opacity: 1.0,
                blur: 0.0,
                light: false,
            };
            let image = appearance
                .render(16, 16, 1.0, &mut cache, Some(&disk))
                .unwrap()
                .unwrap();
            originals.push((appearance, image));
        }
        assert_eq!(cache.images.len(), 2);
        assert_eq!(std::fs::read_dir(&disk).unwrap().count(), 2);
        for (appearance, expected) in &originals {
            assert_eq!(
                &appearance
                    .render(16, 16, 1.0, &mut cache, Some(&disk))
                    .unwrap()
                    .unwrap(),
                expected
            );
        }
        let mut restarted = BackgroundCache::default();
        for (appearance, expected) in &originals {
            assert_eq!(
                &appearance
                    .render(16, 16, 1.0, &mut restarted, Some(&disk))
                    .unwrap()
                    .unwrap(),
                expected
            );
        }
        assert!(
            restarted.source.is_none(),
            "both images should be read from disk cache"
        );
        std::fs::write(dir.join("background.rgb"), b"legacy").unwrap();
        clear_background_cache(&mut cache, &dir).unwrap();
        assert!(cache.images.is_empty());
        assert!(!disk.exists());
        assert!(!dir.join("background.rgb").exists());
        assert_eq!(
            originals[0]
                .0
                .render(16, 16, 1.0, &mut cache, Some(&disk))
                .unwrap()
                .unwrap(),
            originals[0].1
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn legacy_and_multi_image_settings_round_trip() {
        let old = AppearanceConfig::parse("0.7\n3\n/tmp/背景.png\n").unwrap();
        assert_eq!(old.paths, ["/tmp/背景.png"]);
        assert_eq!(old.rotation, 0);
        let config = AppearanceConfig {
            paths: vec!["/tmp/一.png".into(), "/tmp/two.jpg".into()],
            opacity: 0.7,
            blur: 3.0,
            rotation: 3,
        };
        let reloaded = AppearanceConfig::parse(&serde_json::to_string(&config).unwrap()).unwrap();
        assert_eq!(reloaded.paths, config.paths);
        assert_eq!(reloaded.interval(), Some(Duration::from_secs(300)));
        assert!(
            AppearanceConfig::parse("{\"paths\":[],\"opacity\":1,\"blur\":0,\"rotation\":7}")
                .is_err()
        );
    }

    #[test]
    fn timed_rotation_advances_and_wraps() {
        i_slint_backend_testing::init_no_event_loop();
        let ui = LauncherWindow::new().unwrap();
        let config = AppearanceConfig {
            paths: vec!["first".into(), "second".into()],
            opacity: 0.0,
            blur: 0.0,
            rotation: 2,
        };
        let rotation = Rc::new(Rotation {
            ui: ui.as_weak(),
            config: RefCell::new(config),
            index: std::cell::Cell::new(0),
            pending: std::cell::Cell::new(false),
            theme_pending: std::cell::Cell::new(false),
            timer: slint::Timer::default(),
            pump: slint::Timer::default(),
            cache: Arc::new(Mutex::new(BackgroundCache::default())),
            generation: Arc::new(AtomicU64::new(0)),
        });
        rotation.schedule();
        for expected in [1, 0] {
            i_slint_backend_testing::mock_elapsed_time(Duration::from_secs(60));
            slint::platform::update_timers_and_animations();
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while rotation.index.get() != expected {
                assert!(std::time::Instant::now() < deadline, "rotation timed out");
                std::thread::sleep(Duration::from_millis(5));
                i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(50));
                slint::platform::update_timers_and_animations();
            }
        }
    }

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
            light: false,
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
        for file in std::fs::read_dir(&disk).unwrap() {
            std::fs::write(file.unwrap().path(), b"truncated").unwrap();
        }
        cache.images.clear();
        cache.bytes = 0;
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
            light: false,
        };
        let started = Instant::now();
        let old = image::open(&path)
            .unwrap()
            .resize_to_fill(656, 516, image::imageops::FilterType::Triangle)
            .to_rgba8();
        std::hint::black_box(flatten(
            &image::imageops::blur(&old, 15.0),
            0.8,
            panel_color(false),
        ));
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
        assert_eq!(
            flatten(&image, 0.5, panel_color(false)).get_pixel(0, 0).0,
            [75, 51, 57]
        );
        assert_eq!(
            flatten(&image, 0.0, panel_color(false)).get_pixel(0, 0).0,
            [25, 26, 32]
        );
        assert_eq!(
            flatten(&image, 0.0, panel_color(true)).get_pixel(0, 0).0,
            [248, 247, 251]
        );
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
            light: false,
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
