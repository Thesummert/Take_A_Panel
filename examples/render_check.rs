//! Open a sample panel briefly to check the actual native renderer.
//! Run with `cargo run --example render_check`; override with
//! `SLINT_BACKEND=winit-software` to check the software path.
use slint::ComponentHandle;
use std::{cell::Cell, rc::Rc, time::Duration};
slint::include_modules!();

fn main() -> Result<(), Box<dyn std::error::Error>> {
    slint::BackendSelector::new()
        .backend_name("winit".into())
        .select()?;
    let ui = LauncherWindow::new()?;
    ui.set_scanning(false);
    ui.set_results(slint::ModelRc::new(slint::VecModel::from(
        (0..1000)
            .map(|index| AppRow {
                name: format!("Application {index}").into(),
                detail: "Renderer check".into(),
                ..Default::default()
            })
            .collect::<Vec<_>>(),
    )));
    ui.set_background_image(slint::Image::from_rgb8(
        slint::SharedPixelBuffer::clone_from_slice(&vec![70u8; 656 * 516 * 3], 656, 516),
    ));
    let frames = Rc::new(Cell::new(0));
    let count = frames.clone();
    let notifier = ui.window().set_rendering_notifier(move |state, api| {
        if matches!(state, slint::RenderingState::RenderingSetup)
            && matches!(api, slint::GraphicsAPI::NativeOpenGL { .. })
        {
            println!("renderer: OpenGL (FemtoVG)");
        }
        if matches!(state, slint::RenderingState::AfterRendering) {
            count.set(count.get() + 1);
        }
    });
    let weak = ui.as_weak();
    slint::Timer::single_shot(Duration::from_secs(1), move || {
        let ui = weak.upgrade().unwrap();
        let image = ui.window().take_snapshot().expect("render snapshot");
        assert!(image.width() > 0 && image.height() > 0);
        println!("snapshot: {}x{}", image.width(), image.height());
        slint::quit_event_loop().unwrap();
    });
    ui.run()?;
    if notifier.is_ok() {
        assert!(frames.get() > 0, "GPU renderer must produce a frame");
        println!("rendered frames: {}", frames.get());
    } else {
        println!("renderer: software (no rendering notifier)");
    }
    Ok(())
}
