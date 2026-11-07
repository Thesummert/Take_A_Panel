use base64::{Engine, engine::general_purpose::STANDARD};
use image::{DynamicImage, ImageFormat, ImageReader, RgbaImage};
use serde::{Deserialize, Serialize};
use std::{
    borrow::Cow,
    hash::{DefaultHasher, Hash, Hasher},
    io::Cursor,
    path::Path,
    sync::{Arc, Mutex, mpsc},
    time::Duration,
};

const LIMIT: usize = 100;
const MAX_TEXT_BYTES: usize = 1024 * 1024;
const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;
const MAX_PIXELS: usize = 10_000_000;
const MAX_HISTORY_BYTES: usize = 64 * 1024 * 1024;

// Untagged text preserves histories written by earlier versions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Entry {
    Text(String),
    Image {
        width: u32,
        height: u32,
        png: String,
    },
}

impl Entry {
    fn size(&self) -> usize {
        match self {
            Self::Text(text) => text.len(),
            Self::Image { png, .. } => png.len(),
        }
    }

    fn valid(&self) -> bool {
        match self {
            Self::Text(text) => !text.is_empty() && text.len() <= MAX_TEXT_BYTES,
            Self::Image { width, height, png } => {
                valid_dimensions(*width as usize, *height as usize)
                    && png.len() <= MAX_IMAGE_BYTES.div_ceil(3) * 4
                    && decode_image(png, *width, *height).is_some()
            }
        }
    }

    pub fn image(&self) -> Option<RgbaImage> {
        match self {
            Self::Image { width, height, png } => decode_image(png, *width, *height),
            Self::Text(_) => None,
        }
    }
}

fn valid_dimensions(width: usize, height: usize) -> bool {
    width > 0 && height > 0 && width.checked_mul(height).is_some_and(|n| n <= MAX_PIXELS)
}

fn decode_image(png: &str, width: u32, height: u32) -> Option<RgbaImage> {
    if !valid_dimensions(width as usize, height as usize) {
        return None;
    }
    let bytes = STANDARD.decode(png).ok()?;
    if bytes.len() > MAX_IMAGE_BYTES {
        return None;
    }
    let reader = ImageReader::with_format(Cursor::new(&bytes), ImageFormat::Png);
    let dimensions = reader.into_dimensions().ok()?;
    if dimensions != (width, height) {
        return None;
    }
    image::load_from_memory_with_format(&bytes, ImageFormat::Png)
        .ok()
        .map(|image| image.into_rgba8())
}

fn capture_image(image: arboard::ImageData<'_>) -> Option<Entry> {
    if !valid_dimensions(image.width, image.height) {
        return None;
    }
    let width = u32::try_from(image.width).ok()?;
    let height = u32::try_from(image.height).ok()?;
    let rgba = RgbaImage::from_raw(width, height, image.bytes.into_owned())?;
    let mut png = Cursor::new(Vec::new());
    DynamicImage::ImageRgba8(rgba)
        .write_to(&mut png, ImageFormat::Png)
        .ok()?;
    let bytes = png.into_inner();
    if bytes.len() > MAX_IMAGE_BYTES {
        return None;
    }
    Some(Entry::Image {
        width,
        height,
        png: STANDARD.encode(bytes),
    })
}

#[derive(Clone, Default)]
pub struct History {
    entries: Arc<Mutex<Vec<Entry>>>,
    sender: Option<mpsc::Sender<Entry>>,
}

impl History {
    pub fn start() -> Self {
        let path = crate::settings::directory()
            .ok()
            .map(|dir| dir.join("clipboard-history.json"));
        let entries = Arc::new(Mutex::new(
            path.as_deref().and_then(load).unwrap_or_default(),
        ));
        let (sender, receiver) = mpsc::channel::<Entry>();
        let shared = Arc::clone(&entries);
        std::thread::spawn(move || {
            let mut clipboard = None;
            let mut last: Option<u64> = None;
            loop {
                if clipboard.is_none() {
                    clipboard = arboard::Clipboard::new().ok();
                }
                if let Some(clipboard) = clipboard.as_mut() {
                    while let Ok(entry) = receiver.try_recv() {
                        let result = match &entry {
                            Entry::Text(text) => clipboard.set_text(text.clone()),
                            Entry::Image { width, height, .. } => match entry.image() {
                                Some(image) => clipboard.set_image(arboard::ImageData {
                                    width: *width as usize,
                                    height: *height as usize,
                                    bytes: Cow::Owned(image.into_raw()),
                                }),
                                None => {
                                    eprintln!("无法解码剪贴板图片");
                                    continue;
                                }
                            },
                        };
                        if let Err(error) = result {
                            eprintln!("无法复制剪贴板记录：{error}");
                        }
                    }
                    // Hash raw pixels before encoding to avoid recompressing an unchanged image.
                    let image = clipboard.get_image().ok().filter(|image| {
                        valid_dimensions(image.width, image.height)
                            && image
                                .width
                                .checked_mul(image.height)
                                .and_then(|n| n.checked_mul(4))
                                == Some(image.bytes.len())
                    });
                    let text = if image.is_none() {
                        clipboard
                            .get_text()
                            .ok()
                            .filter(|text| !text.is_empty() && text.len() <= MAX_TEXT_BYTES)
                    } else {
                        None
                    };
                    let mut hasher = DefaultHasher::new();
                    let current = if let Some(image) = &image {
                        1u8.hash(&mut hasher);
                        image.width.hash(&mut hasher);
                        image.height.hash(&mut hasher);
                        image.bytes.hash(&mut hasher);
                        Some(hasher.finish())
                    } else if let Some(text) = &text {
                        2u8.hash(&mut hasher);
                        text.hash(&mut hasher);
                        Some(hasher.finish())
                    } else {
                        None
                    };
                    if current != last {
                        last = current;
                        let entry = image
                            .and_then(capture_image)
                            .or_else(|| text.map(Entry::Text));
                        if let Some(entry) = entry {
                            let mut entries = shared.lock().unwrap_or_else(|e| e.into_inner());
                            record(&mut entries, entry);
                            if let Some(path) = &path {
                                if let Err(error) = save(path, &entries) {
                                    eprintln!("无法保存剪贴板记录：{error}");
                                }
                            }
                        }
                    }
                }
                std::thread::sleep(Duration::from_millis(500));
            }
        });
        Self {
            entries,
            sender: Some(sender),
        }
    }

    pub fn search(&self, query: &str) -> Vec<Entry> {
        let query = query.trim().to_lowercase();
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|entry| match entry {
                Entry::Text(text) => text.to_lowercase().contains(&query),
                Entry::Image { width, height, .. } => {
                    format!("图片 image {width}x{height}").contains(&query)
                }
            })
            .cloned()
            .collect()
    }

    pub fn copy(&self, entry: Entry) -> Result<(), String> {
        self.sender
            .as_ref()
            .ok_or("剪贴板监听未启动")?
            .send(entry)
            .map_err(|e| e.to_string())
    }
}

fn record(entries: &mut Vec<Entry>, entry: Entry) {
    entries.retain(|old| old != &entry);
    entries.insert(0, entry);
    let mut size = 0usize;
    entries.retain(|entry| {
        size += entry.size();
        size <= MAX_HISTORY_BYTES
    });
    entries.truncate(LIMIT);
}

fn load(path: &Path) -> Option<Vec<Entry>> {
    let metadata = std::fs::metadata(path).ok()?;
    if metadata.len() > (MAX_HISTORY_BYTES * 2) as u64 {
        return None;
    }
    let entries: Vec<Entry> = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    let mut result = Vec::new();
    for entry in entries.into_iter().rev() {
        if entry.valid() {
            record(&mut result, entry);
        }
    }
    Some(result)
}

fn save(path: &Path, entries: &[Entry]) -> anyhow::Result<()> {
    use std::io::Write;
    std::fs::create_dir_all(path.parent().unwrap())?;
    let temporary = path.with_extension("json.tmp");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    file.write_all(&serde_json::to_vec(entries)?)?;
    drop(file);
    std::fs::rename(temporary, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn history_deduplicates_limits_and_round_trips() {
        let mut entries = Vec::new();
        for i in 0..110 {
            record(&mut entries, Entry::Text(i.to_string()));
        }
        record(&mut entries, Entry::Text("20".into()));
        assert_eq!(entries.len(), LIMIT);
        assert_eq!(entries[0], Entry::Text("20".into()));
        assert_eq!(
            entries
                .iter()
                .filter(|s| **s == Entry::Text("20".into()))
                .count(),
            1
        );
        let dir = crate::file_search::test_support::TempDir::new();
        let path = dir.0.join("history.json");
        save(&path, &entries).unwrap();
        assert_eq!(load(&path).unwrap(), entries);
    }

    #[test]
    fn image_round_trip_and_invalid_data() {
        let bytes = vec![255, 0, 0, 255, 0, 0, 255, 127];
        let entry = capture_image(arboard::ImageData {
            width: 2,
            height: 1,
            bytes: Cow::Owned(bytes.clone()),
        })
        .unwrap();
        assert_eq!(entry.image().unwrap().into_raw(), bytes);
        let dir = crate::file_search::test_support::TempDir::new();
        let path = dir.0.join("history.json");
        save(&path, &[entry.clone(), Entry::Text("text".into())]).unwrap();
        assert_eq!(
            load(&path).unwrap(),
            vec![entry, Entry::Text("text".into())]
        );
        assert!(
            capture_image(arboard::ImageData {
                width: MAX_PIXELS + 1,
                height: 1,
                bytes: Cow::Borrowed(&[])
            })
            .is_none()
        );
        assert!(
            !Entry::Image {
                width: 1,
                height: 1,
                png: "bad".into()
            }
            .valid()
        );
    }
}
