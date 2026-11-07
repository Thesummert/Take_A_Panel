use crate::apps::Application;
use slint::{Rgba8Pixel, SharedPixelBuffer};

#[cfg(any(target_os = "linux", test))]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

// Twice the displayed size keeps icons sharp on HiDPI displays.
pub const ICON_SIZE: u32 = 76;

/// Pixel buffers can cross threads; Slint Images must be created on the UI thread.
pub struct IconData {
    pub pixels: SharedPixelBuffer<Rgba8Pixel>,
    pub premultiplied: bool,
}

impl IconData {
    pub fn into_image(self) -> slint::Image {
        if self.premultiplied {
            slint::Image::from_rgba8_premultiplied(self.pixels)
        } else {
            slint::Image::from_rgba8(self.pixels)
        }
    }
}

pub fn load(app: &Application) -> Option<IconData> {
    #[cfg(target_os = "macos")]
    return macos::load(app);
    #[cfg(target_os = "windows")]
    return windows::load(app);
    #[cfg(target_os = "linux")]
    return linux::load(app);
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    None
}

#[cfg(any(target_os = "macos", target_os = "linux", test))]
fn decode(bytes: &[u8]) -> Option<IconData> {
    use image::ImageReader;
    let mut reader = ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(4096);
    limits.max_image_height = Some(4096);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    let image = reader
        .decode()
        .ok()?
        .thumbnail(ICON_SIZE, ICON_SIZE)
        .to_rgba8();
    Some(IconData {
        pixels: SharedPixelBuffer::clone_from_slice(image.as_raw(), image.width(), image.height()),
        premultiplied: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoding_preserves_alpha_and_fits_icons_without_distorting_them() {
        let image = image::RgbaImage::from_pixel(152, 76, image::Rgba([200, 80, 20, 128]));
        let mut png = std::io::Cursor::new(Vec::new());
        image.write_to(&mut png, image::ImageFormat::Png).unwrap();
        let icon = decode(png.get_ref()).unwrap();
        assert_eq!((icon.pixels.width(), icon.pixels.height()), (76, 38));
        assert_eq!(icon.pixels.as_slice()[0].a, 128);
        assert!(!icon.premultiplied);
        assert!(decode(b"invalid icon").is_none());
    }
}
