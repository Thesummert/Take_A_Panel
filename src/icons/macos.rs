use super::{ICON_SIZE, IconData, decode};
use crate::apps::{Application, LaunchTarget};
use objc2::{AllocAnyThread, rc::autoreleasepool};
use objc2_app_kit::{NSBitmapImageFileType, NSBitmapImageRep, NSImage, NSWorkspace};
use objc2_foundation::{NSDictionary, NSPoint, NSRect, NSSize, NSString};
use std::path::Path;

pub fn load(app: &Application) -> Option<IconData> {
    let LaunchTarget::File(path) = &app.target;
    load_path(path)
}

fn load_path(path: &Path) -> Option<IconData> {
    if !path.is_dir() {
        return None;
    }
    // NSWorkspace resolves bundle resources, asset catalogs and custom Finder
    // icons. Keep all AppKit objects on this worker and drain its autoreleases.
    autoreleasepool(|_| {
        let path = NSString::from_str(path.to_str()?);
        let workspace = NSWorkspace::sharedWorkspace();
        let icon: objc2::rc::Retained<NSImage> = workspace.iconForFile(&path);
        let mut rect = NSRect::new(
            NSPoint::ZERO,
            NSSize::new(ICON_SIZE as f64, ICON_SIZE as f64),
        );
        // The rectangle is valid for the duration of the call; no hints/context.
        let cg_image = unsafe { icon.CGImageForProposedRect_context_hints(&mut rect, None, None) }?;
        let bitmap = NSBitmapImageRep::initWithCGImage(NSBitmapImageRep::alloc(), &cg_image);
        // An empty dictionary contains no values with mismatched property types.
        let png = unsafe {
            bitmap.representationUsingType_properties(
                NSBitmapImageFileType::PNG,
                &NSDictionary::new(),
            )
        }?;
        decode(&png.to_vec())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_application_has_no_icon() {
        assert!(load_path(Path::new("/nonexistent/take-a-panel.app")).is_none());
    }

    // Opt-in native read-only smoke check for installed app icons.
    #[test]
    #[ignore = "requires installed macOS applications"]
    fn installed_zen_and_system_app_icons_can_be_loaded_on_a_worker() {
        std::thread::spawn(|| {
            for path in [
                "/Applications/Visual Studio Code.app",
                "/System/Applications/System Settings.app",
                "/Applications/Safari.app",
                "/Applications/Zen.app",
                "/System/Applications/Utilities/Terminal.app",
                "/System/Applications/Notes.app",
            ] {
                let icon = load_path(Path::new(path)).expect(path);
                assert!(icon.pixels.as_slice().iter().any(|p| p.a > 0));
                assert!(
                    icon.pixels
                        .as_slice()
                        .iter()
                        .any(|p| p.a > 0 && (p.r > 20 || p.g > 20 || p.b > 20)),
                    "{path}: system returned an empty placeholder icon"
                );
                assert!(icon.pixels.width() <= ICON_SIZE && icon.pixels.height() <= ICON_SIZE);
                if let Some(directory) = std::env::var_os("TAKE_A_PANEL_ICON_SNAPSHOT_DIR") {
                    let name = Path::new(path).file_stem().unwrap();
                    let destination = Path::new(&directory).join(name).with_extension("png");
                    image::save_buffer(
                        destination,
                        icon.pixels.as_bytes(),
                        icon.pixels.width(),
                        icon.pixels.height(),
                        image::ColorType::Rgba8,
                    )
                    .unwrap();
                }
            }
        })
        .join()
        .unwrap();
    }
}
