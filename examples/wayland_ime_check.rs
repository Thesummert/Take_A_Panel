//! Run on a Wayland desktop with an input method running:
//! `cargo run --example wayland_ime_check`
//! Keep the test panel focused; it closes automatically after three shows.

#[cfg(target_os = "linux")]
#[allow(dead_code)]
#[path = "../src/window_focus.rs"]
mod window_focus;

#[cfg(target_os = "linux")]
slint::include_modules!();

#[cfg(target_os = "linux")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use slint::{
        ComponentHandle,
        winit_030::{EventResult, WinitWindowAccessor, winit},
    };
    use std::{cell::Cell, rc::Rc, time::Duration};
    use winit::platform::wayland::EventLoopBuilderExtWayland;

    fn show(ui: &LauncherWindow) {
        window_focus::prepare_for_show(ui.window());
        ui.invoke_reset_search();
        ui.show().unwrap();
        ui.invoke_focus_search();
    }

    let mut builder =
        winit::event_loop::EventLoop::<slint::winit_030::SlintEvent>::with_user_event();
    builder.with_wayland();
    slint::BackendSelector::new()
        .backend_name("winit".into())
        .with_winit_event_loop_builder(builder)
        .select()?;
    let ui = LauncherWindow::new()?;
    let ime_enabled = Rc::new(Cell::new(false));
    let presentations = Rc::new(Cell::new(0));
    let enabled = ime_enabled.clone();
    let shown = presentations.clone();
    let weak = ui.as_weak();
    ui.window().on_winit_window_event(move |_, event| {
        if matches!(
            event,
            winit::event::WindowEvent::Ime(winit::event::Ime::Enabled)
        ) && !enabled.replace(true)
        {
            shown.set(shown.get() + 1);
            println!("Presentation {}: IME enabled", shown.get());
            let weak = weak.clone();
            let enabled = enabled.clone();
            let finished = shown.get() == 3;
            // Allow the event to propagate before hiding. Wait for actual IME
            // activation rather than assuming a fixed window-creation time.
            slint::Timer::single_shot(Duration::from_millis(500), move || {
                if finished {
                    slint::quit_event_loop().unwrap();
                    return;
                }
                // Hide while Slint still considers the old window active,
                // as on Escape/--hide or inside a Focused(false) hook.
                weak.upgrade().unwrap().hide().unwrap();
                enabled.set(false);
                slint::Timer::single_shot(Duration::from_millis(150), move || {
                    show(&weak.upgrade().unwrap());
                });
            });
        }
        EventResult::Propagate
    });
    let shown = presentations.clone();
    slint::Timer::single_shot(Duration::from_secs(10), move || {
        eprintln!(
            "Timed out after {} IME activations; keep the test panel focused",
            shown.get()
        );
        slint::quit_event_loop().unwrap();
    });
    show(&ui);
    slint::run_event_loop_until_quit()?;
    assert_eq!(
        presentations.get(),
        3,
        "IME must be enabled in all three presentations"
    );
    println!("Wayland IME activation after showing and reopening twice: passed");
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn main() {
    println!("Wayland IME check requires Linux");
}
