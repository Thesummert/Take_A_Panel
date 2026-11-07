use super::{ICON_SIZE, IconData};
use crate::apps::{Application, LaunchTarget};
use slint::SharedPixelBuffer;
use std::{ffi::OsString, os::windows::ffi::OsStrExt};
use windows::{
    Win32::{
        Foundation::SIZE,
        Graphics::Gdi::{
            BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, DIB_RGB_COLORS,
            DeleteDC, DeleteObject, GetDIBits, GetObjectW, HBITMAP, HDC, HGDIOBJ,
        },
        System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize},
        UI::Shell::{
            IShellItemImageFactory, SHCreateItemFromParsingName, SIIGBF_BIGGERSIZEOK,
            SIIGBF_ICONONLY,
        },
    },
    core::PCWSTR,
};

struct Apartment;
impl Apartment {
    fn new() -> Option<Self> {
        unsafe {
            CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok().ok()?;
        }
        Some(Self)
    }
}
impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

struct Bitmap(HBITMAP);
impl Drop for Bitmap {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteObject(HGDIOBJ(self.0.0));
        }
    }
}

struct DeviceContext(HDC);
impl Drop for DeviceContext {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteDC(self.0);
        }
    }
}

pub fn load(app: &Application) -> Option<IconData> {
    let _apartment = Apartment::new()?;
    let path: OsString = match &app.target {
        LaunchTarget::File(path) => path.as_os_str().to_owned(),
        LaunchTarget::AppId(id) => format!("shell:AppsFolder\\{id}").into(),
    };
    let path: Vec<u16> = path.encode_wide().chain(Some(0)).collect();
    // Shell item factories resolve both .lnk targets and UWP/Store app icons.
    let factory: IShellItemImageFactory =
        unsafe { SHCreateItemFromParsingName(PCWSTR(path.as_ptr()), None) }.ok()?;
    let bitmap = Bitmap(
        unsafe {
            factory.GetImage(
                SIZE {
                    cx: ICON_SIZE as i32,
                    cy: ICON_SIZE as i32,
                },
                SIIGBF_ICONONLY | SIIGBF_BIGGERSIZEOK,
            )
        }
        .ok()?,
    );
    let mut description = BITMAP::default();
    let read = unsafe {
        GetObjectW(
            HGDIOBJ(bitmap.0.0),
            std::mem::size_of::<BITMAP>() as i32,
            Some((&mut description as *mut BITMAP).cast()),
        )
    };
    if read == 0 {
        return None;
    }
    let width = description.bmWidth;
    let height = description.bmHeight;
    if !(1..=4096).contains(&width) || !(1..=4096).contains(&height) {
        return None;
    }
    let dc = DeviceContext(unsafe { CreateCompatibleDC(None) });
    if dc.0.0.is_null() {
        return None;
    }
    let mut bytes = vec![0u8; width as usize * height as usize * 4];
    let mut info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            biHeight: -height, // Top-down rows, matching Slint's pixel buffers.
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let read = unsafe {
        GetDIBits(
            dc.0,
            bitmap.0,
            0,
            height as u32,
            Some(bytes.as_mut_ptr().cast()),
            &mut info,
            DIB_RGB_COLORS,
        )
    };
    if read != height {
        return None;
    }
    for pixel in bytes.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    if !bytes.chunks_exact(4).any(|p| p[3] != 0) {
        return None;
    }
    let pixels = image::RgbaImage::from_raw(width as u32, height as u32, bytes)?;
    let pixels = image::DynamicImage::ImageRgba8(pixels)
        .thumbnail(ICON_SIZE, ICON_SIZE)
        .to_rgba8();
    Some(IconData {
        pixels: SharedPixelBuffer::clone_from_slice(
            pixels.as_raw(),
            pixels.width(),
            pixels.height(),
        ),
        premultiplied: true,
    })
}
