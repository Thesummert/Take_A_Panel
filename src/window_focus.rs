use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(target_os = "linux")]
pub fn prepare_for_show(window: &slint::Window) {
    if !window.is_visible() {
        // Slint destroys Wayland windows on hide, so Focused(false) can be
        // lost (including when we hide inside the native focus-event hook).
        // Clear the old activation before showing a new native window. Its
        // Focused(true) must reach the text input again to re-enable IME.
        window.dispatch_event(slint::platform::WindowEvent::WindowActiveChanged(false));
    }
}

/// Native windows can report Focused(false) before ever receiving focus.
/// Only a focused -> unfocused transition during this presentation dismisses
/// the panel. Reset this state every time the panel is shown.
#[derive(Default)]
pub struct WindowFocus {
    acquired: AtomicBool,
}

impl WindowFocus {
    pub fn begin_presentation(&self) {
        self.acquired.store(false, Ordering::Relaxed);
    }

    pub fn changed(&self, focused: bool) -> bool {
        let previously_focused = self.acquired.swap(focused, Ordering::Relaxed);
        previously_focused && !focused
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_unfocused_events_do_not_dismiss_the_panel() {
        let focus = WindowFocus::default();
        focus.begin_presentation();
        assert!(!focus.changed(false));
        assert!(!focus.changed(false));
        assert!(!focus.changed(true));
    }

    #[test]
    fn losing_acquired_focus_dismisses_once() {
        let focus = WindowFocus::default();
        focus.begin_presentation();
        assert!(!focus.changed(true));
        assert!(focus.changed(false));
        assert!(!focus.changed(false));
    }

    #[test]
    fn reopening_does_not_reuse_focus_from_previous_presentation() {
        let focus = WindowFocus::default();
        focus.begin_presentation();
        assert!(!focus.changed(true));
        // Escape or a launch can hide the window before its blur event arrives.
        focus.begin_presentation();
        assert!(!focus.changed(false));
        assert!(!focus.changed(true));
        assert!(focus.changed(false));
    }
}
