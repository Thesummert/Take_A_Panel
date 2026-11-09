use slint::{
    ComponentHandle, ModelRc, VecModel,
    platform::{Key, PointerEventButton, WindowEvent},
};
use std::{cell::Cell, rc::Rc};

slint::include_modules!();

#[path = "../src/shortcut_recorder.rs"]
mod shortcut_recorder;

fn key(ui: &LauncherWindow, text: slint::SharedString) {
    ui.window()
        .dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
    ui.window()
        .dispatch_event(WindowEvent::KeyReleased { text });
}

fn click(ui: &LauncherWindow, x: f32, y: f32) {
    let position = slint::LogicalPosition::new(x, y);
    ui.window()
        .dispatch_event(WindowEvent::PointerMoved { position });
    ui.window().dispatch_event(WindowEvent::PointerPressed {
        position,
        button: PointerEventButton::Left,
    });
    ui.window().dispatch_event(WindowEvent::PointerReleased {
        position,
        button: PointerEventButton::Left,
    });
}

#[test]
fn right_click_selects_result_and_menu_runs_its_action() {
    slint::platform::set_platform(Box::new(i_slint_backend_testing::TestingBackend::new(
        i_slint_backend_testing::TestingBackendOptions {
            renderer_name: Some("software".into()),
            ..Default::default()
        },
    )))
    .unwrap();
    let ui = LauncherWindow::new().unwrap();
    ui.set_results(ModelRc::new(VecModel::from(vec![
        AppRow {
            name: "One".into(),
            app_result: true,
            has_path: true,
            ..Default::default()
        },
        AppRow {
            name: "Two".into(),
            app_result: true,
            has_path: true,
            ..Default::default()
        },
    ])));
    let action = Rc::new(Cell::new(-1));
    let result = action.clone();
    ui.on_copy_result_path(move |index| result.set(index));
    let launches = Rc::new(Cell::new(0));
    let count = launches.clone();
    ui.on_launch_selected(move |_, _| count.set(count.get() + 1));
    ui.show().unwrap();
    ui.invoke_focus_search();
    let position = slint::LogicalPosition::new(100., 245.);
    ui.window()
        .dispatch_event(WindowEvent::PointerMoved { position });
    ui.window().dispatch_event(WindowEvent::PointerPressed {
        position,
        button: PointerEventButton::Right,
    });
    ui.window().dispatch_event(WindowEvent::PointerReleased {
        position,
        button: PointerEventButton::Right,
    });
    assert_eq!(ui.get_selected_index(), 1);
    assert_eq!(launches.get(), 0);
    // The third menu item copies the path of the right-clicked row.
    click(&ui, 120., 330.);
    assert_eq!(action.get(), 1);
    ui.hide().unwrap();
}

#[test]
fn settings_navigation_moves_selected_style_between_pages() {
    slint::platform::set_platform(Box::new(i_slint_backend_testing::TestingBackend::new(
        i_slint_backend_testing::TestingBackendOptions {
            renderer_name: Some("software".into()),
            ..Default::default()
        },
    )))
    .unwrap();
    let ui = LauncherWindow::new().unwrap();
    ui.show().unwrap();
    ui.invoke_open_settings();
    let snapshot = ui.window().take_snapshot().unwrap();
    let pixel = |image: &slint::SharedPixelBuffer<slint::Rgba8Pixel>, x: usize, y: usize| {
        image.as_slice()[y * image.width() as usize + x]
    };
    let selected = pixel(&snapshot, 35, 110);
    let unselected = pixel(&snapshot, 35, 160);
    assert_ne!(selected, unselected);

    click(&ui, 80., 160.);
    ui.window().dispatch_event(WindowEvent::PointerMoved {
        position: slint::LogicalPosition::new(650., 450.),
    });
    let snapshot = ui.window().take_snapshot().unwrap();
    assert_eq!(pixel(&snapshot, 35, 110), unselected);
    assert_eq!(pixel(&snapshot, 35, 160), selected);
    assert!(!ui.get_shortcut_recording());
    let saved = Rc::new(std::cell::RefCell::new(None));
    let result = saved.clone();
    ui.on_save_appearance(move |paths, _, _, rotation| {
        *result.borrow_mut() = Some((paths.to_string(), rotation));
    });
    ui.set_background_path("/tmp/a.png\n/tmp/b.png".into());
    ui.set_background_rotation(3);
    click(&ui, 280., 441.);
    assert_eq!(*saved.borrow(), Some(("/tmp/a.png\n/tmp/b.png".into(), 3)));
    let cleared = Rc::new(Cell::new(false));
    let result = cleared.clone();
    ui.on_clear_background_cache(move || result.set(true));
    click(&ui, 540., 441.);
    assert!(cleared.get());
    ui.hide().unwrap();
}

#[test]
fn large_lists_only_fetch_visible_rows_and_can_scroll_to_distant_results() {
    use slint::{Model, ModelTracker};
    struct LargeModel(Rc<Cell<usize>>);
    impl Model for LargeModel {
        type Data = AppRow;
        fn row_count(&self) -> usize {
            10_000
        }
        fn row_data(&self, row: usize) -> Option<AppRow> {
            self.0.set(self.0.get() + 1);
            (row < self.row_count()).then(|| AppRow {
                name: format!("Application {row}").into(),
                ..Default::default()
            })
        }
        fn model_tracker(&self) -> &dyn ModelTracker {
            &()
        }
    }
    slint::platform::set_platform(Box::new(i_slint_backend_testing::TestingBackend::new(
        i_slint_backend_testing::TestingBackendOptions {
            renderer_name: Some("software".into()),
            ..Default::default()
        },
    )))
    .unwrap();
    let ui = LauncherWindow::new().unwrap();
    let fetches = Rc::new(Cell::new(0));
    ui.set_results(ModelRc::new(LargeModel(fetches.clone())));
    ui.show().unwrap();
    ui.invoke_focus_search();
    ui.window().take_snapshot().unwrap();
    assert!(
        fetches.get() < 1000,
        "list must not instantiate all 10,000 rows: {} fetches",
        fetches.get()
    );
    ui.set_selected_index(9998);
    key(&ui, Key::DownArrow.into());
    ui.window().take_snapshot().unwrap();
    let submitted = Rc::new(Cell::new(-1));
    let result = submitted.clone();
    ui.on_launch_selected(move |index, _| result.set(index));
    key(&ui, Key::Return.into());
    assert_eq!(submitted.get(), 9999);
    ui.hide().unwrap();
}

#[test]
fn settings_edits_save_without_launching_and_escape_returns_to_search() {
    slint::platform::set_platform(Box::new(i_slint_backend_testing::TestingBackend::new(
        i_slint_backend_testing::TestingBackendOptions {
            renderer_name: Some("software".into()),
            ..Default::default()
        },
    )))
    .unwrap();
    let ui = LauncherWindow::new().unwrap();
    shortcut_recorder::bind(&ui);
    ui.set_query("terminal".into());
    ui.set_configured_hotkey("Alt+Space".into());
    ui.set_hotkey_label("Alt+Space".into());
    ui.set_results(ModelRc::new(VecModel::from(vec![AppRow {
        name: "Terminal".into(),
        ..Default::default()
    }])));
    let launches = Rc::new(Cell::new(0));
    let calls = launches.clone();
    ui.on_launch_selected(move |_, _| calls.set(calls.get() + 1));
    let saves = Rc::new(Cell::new(0));
    let calls = saves.clone();
    ui.on_save_shortcut(move |value| {
        assert_eq!(value, "Ctrl+Alt+Space");
        calls.set(calls.get() + 1);
    });
    let dismissed = Rc::new(Cell::new(false));
    let result = dismissed.clone();
    ui.on_dismiss(move || result.set(true));
    ui.show().unwrap();
    ui.invoke_focus_search();
    // Open through the gear and record a real modifier combination.
    click(&ui, 574., 34.);
    assert!(ui.get_settings_open());
    assert_eq!(ui.get_shortcut_draft(), "Alt+Space");
    assert!(ui.get_shortcut_recording());
    let control = if cfg!(target_os = "macos") {
        Key::Meta
    } else {
        Key::Control
    };
    for modifier in [control, Key::Alt] {
        ui.window().dispatch_event(WindowEvent::KeyPressed {
            text: modifier.into(),
        });
        assert_eq!(
            ui.get_shortcut_draft(),
            "Alt+Space",
            "modifier-only input must not replace the combination"
        );
    }
    key(&ui, Key::Space.into());
    for modifier in [Key::Alt, control] {
        ui.window().dispatch_event(WindowEvent::KeyReleased {
            text: modifier.into(),
        });
    }
    assert_eq!(ui.get_shortcut_draft(), "Ctrl+Alt+Space");
    assert_eq!(ui.get_query(), "terminal");
    key(&ui, Key::Return.into());
    assert_eq!(saves.get(), 1);
    assert_eq!(launches.get(), 0);
    let snapshot = ui.window().take_snapshot().unwrap();
    if let Some(path) = std::env::var_os("TAKE_A_PANEL_SETTINGS_SNAPSHOT") {
        let mut encoder = png::Encoder::new(
            std::fs::File::create(path).unwrap(),
            snapshot.width(),
            snapshot.height(),
        );
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .write_header()
            .unwrap()
            .write_image_data(snapshot.as_bytes())
            .unwrap();
    }
    ui.set_shortcut_draft("Alt+F2".into());
    click(&ui, 223., 298.);
    assert_eq!(ui.get_shortcut_draft(), "Ctrl+Alt+Space");
    assert_eq!(saves.get(), 1, "restoring defaults must wait for save");
    click(&ui, 595., 298.);
    assert_eq!(saves.get(), 2);
    // Clicking the page's background must not reach an application underneath.
    click(&ui, 650., 200.);
    assert_eq!(launches.get(), 0);
    ui.invoke_focus_search();
    key(&ui, Key::Escape.into());
    assert!(!ui.get_settings_open());
    assert!(!dismissed.get());
    assert_eq!(ui.get_query(), "terminal");
    key(&ui, "x".into());
    assert_eq!(ui.get_query(), "xterminal");
    ui.invoke_open_settings();
    ui.set_shortcut_draft("unsaved".into());
    key(&ui, Key::Escape.into());
    ui.invoke_open_settings();
    assert_eq!(ui.get_shortcut_draft(), "Alt+Space");
    ui.invoke_reset_search();
    assert!(!ui.get_settings_open());
    ui.hide().unwrap();
}

#[test]
fn autostart_settings_require_an_explicit_click_and_do_not_record_shortcuts() {
    slint::platform::set_platform(Box::new(i_slint_backend_testing::TestingBackend::new(
        i_slint_backend_testing::TestingBackendOptions {
            renderer_name: Some("software".into()),
            ..Default::default()
        },
    )))
    .unwrap();
    let ui = LauncherWindow::new().unwrap();
    shortcut_recorder::bind(&ui);
    let requests = Rc::new(std::cell::RefCell::new(Vec::new()));
    let calls = requests.clone();
    ui.on_set_autostart(move |enabled| calls.borrow_mut().push(enabled));
    let refreshes = Rc::new(Cell::new(0));
    let calls = refreshes.clone();
    ui.on_refresh_autostart(move || calls.set(calls.get() + 1));
    ui.show().unwrap();
    ui.invoke_open_autostart_settings();
    assert!(ui.get_settings_open());
    assert!(!ui.get_shortcut_recording());
    assert_eq!(refreshes.get(), 1);
    assert!(
        requests.borrow().is_empty(),
        "opening settings must only read state"
    );
    ui.window().take_snapshot().unwrap();
    click(&ui, 300., 214.);
    assert!(
        requests.borrow().is_empty(),
        "unavailable system service must disable requests"
    );
    ui.set_autostart_available(true);
    click(&ui, 300., 214.);
    assert_eq!(*requests.borrow(), [true]);
    ui.set_autostart_busy(true);
    click(&ui, 300., 214.);
    assert_eq!(
        *requests.borrow(),
        [true],
        "a pending operation must prevent duplicate requests"
    );
    ui.set_autostart_busy(false);
    ui.set_autostart_registered(true);
    ui.set_autostart_status("已提交：等待在系统设置中批准".into());
    ui.set_autostart_system_settings(true);
    click(&ui, 300., 214.);
    assert_eq!(
        *requests.borrow(),
        [true, false],
        "pending registrations must be cancellable"
    );
    let before = ui.get_shortcut_draft();
    key(&ui, "a".into());
    assert_eq!(ui.get_shortcut_draft(), before);
    key(&ui, Key::Escape.into());
    assert!(!ui.get_settings_open());
    ui.hide().unwrap();
}

#[test]
fn file_search_escape_returns_to_apps_and_settings_do_not_capture_shortcuts() {
    slint::platform::set_platform(Box::new(i_slint_backend_testing::TestingBackend::new(
        i_slint_backend_testing::TestingBackendOptions {
            renderer_name: Some("software".into()),
            ..Default::default()
        },
    )))
    .unwrap();
    let ui = LauncherWindow::new().unwrap();
    shortcut_recorder::bind(&ui);
    let dismissed = Rc::new(Cell::new(false));
    let result = dismissed.clone();
    ui.on_dismiss(move || result.set(true));
    let folder_dialogs = Rc::new(Cell::new(0));
    let calls = folder_dialogs.clone();
    ui.on_browse_file_folder(move || calls.set(calls.get() + 1));
    let program_dialogs = Rc::new(Cell::new(0));
    let calls = program_dialogs.clone();
    ui.on_browse_file_program(move || calls.set(calls.get() + 1));
    ui.show().unwrap();
    ui.set_file_search_mode(true);
    ui.set_query("报告".into());
    ui.invoke_open_file_search_settings();
    assert!(ui.get_settings_open());
    assert!(!ui.get_shortcut_recording());
    ui.set_file_roots_draft("~/Documents\n~/Downloads".into());
    click(&ui, 535., 120.);
    assert_eq!(program_dialogs.get(), 0, "builtin needs no program");
    ui.set_file_backend_draft(1);
    click(&ui, 535., 120.);
    assert_eq!(program_dialogs.get(), 1);
    ui.set_file_program_dialog_open(true);
    click(&ui, 535., 120.);
    click(&ui, 400., 165.);
    assert_eq!(
        program_dialogs.get(),
        1,
        "prevent duplicate program dialogs"
    );
    assert_eq!(folder_dialogs.get(), 0, "wait for program selection");
    ui.set_file_program_dialog_open(false);
    ui.set_file_settings_busy(true);
    click(&ui, 535., 120.);
    assert_eq!(program_dialogs.get(), 1, "wait for settings to save");
    ui.set_file_settings_busy(false);
    click(&ui, 400., 165.);
    assert_eq!(folder_dialogs.get(), 1);
    ui.set_file_folder_dialog_open(true);
    click(&ui, 400., 165.);
    assert_eq!(
        folder_dialogs.get(),
        1,
        "only one folder dialog may be open"
    );
    ui.set_file_folder_dialog_open(false);
    ui.set_file_settings_busy(true);
    click(&ui, 400., 165.);
    assert_eq!(
        folder_dialogs.get(),
        1,
        "wait for settings to finish saving"
    );
    ui.set_file_settings_busy(false);
    let snapshot = ui.window().take_snapshot().unwrap();
    if let Some(path) = std::env::var_os("TAKE_A_PANEL_FILE_SEARCH_SNAPSHOT") {
        let mut encoder = png::Encoder::new(
            std::fs::File::create(path).unwrap(),
            snapshot.width(),
            snapshot.height(),
        );
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .write_header()
            .unwrap()
            .write_image_data(snapshot.as_bytes())
            .unwrap();
    }
    key(&ui, Key::Escape.into());
    assert!(!ui.get_settings_open());
    assert!(ui.get_file_search_mode());
    assert_eq!(ui.get_query(), "报告");
    key(&ui, Key::Escape.into());
    assert!(!ui.get_file_search_mode());
    assert_eq!(ui.get_query(), "");
    assert!(!dismissed.get());
    key(&ui, Key::Escape.into());
    assert!(dismissed.get());
    ui.hide().unwrap();
}

#[test]
fn native_shortcut_recording_uses_physical_keys_and_waits_for_save() {
    use slint::winit_030::winit::keyboard::{KeyCode, ModifiersState as Mods};
    i_slint_backend_testing::init_no_event_loop();
    let ui = LauncherWindow::new().unwrap();
    shortcut_recorder::bind(&ui);
    ui.invoke_open_settings();
    let saves = Rc::new(Cell::new(0));
    let calls = saves.clone();
    ui.on_save_shortcut(move |_| calls.set(calls.get() + 1));
    for (key, modifiers, expected) in [
        (KeyCode::Space, Mods::CONTROL | Mods::ALT, "Ctrl+Alt+Space"),
        (KeyCode::KeyA, Mods::ALT, "Alt+A"),
        (KeyCode::KeyK, Mods::SUPER | Mods::SHIFT, "Shift+Super+K"),
        (KeyCode::Digit1, Mods::CONTROL | Mods::SHIFT, "Ctrl+Shift+1"),
        (
            KeyCode::Equal,
            Mods::CONTROL | Mods::SHIFT,
            "Ctrl+Shift+Equal",
        ),
        (KeyCode::F12, Mods::ALT, "Alt+F12"),
        (KeyCode::Enter, Mods::CONTROL, "Ctrl+Enter"),
        (KeyCode::ArrowLeft, Mods::SUPER, "Super+ArrowLeft"),
        (KeyCode::NumpadAdd, Mods::CONTROL, "Ctrl+NumpadAdd"),
    ] {
        assert!(shortcut_recorder::capture_native(&ui, key, modifiers));
        assert_eq!(ui.get_shortcut_draft(), expected);
        assert!(!ui.get_settings_failed());
        assert_eq!(
            saves.get(),
            0,
            "recording must not save or register a shortcut"
        );
    }
    for key in [
        KeyCode::ControlLeft,
        KeyCode::AltRight,
        KeyCode::ShiftRight,
        KeyCode::SuperLeft,
        KeyCode::Tab,
    ] {
        assert!(!shortcut_recorder::capture_native(&ui, key, Mods::empty()));
        assert_eq!(ui.get_shortcut_draft(), "Ctrl+NumpadAdd");
    }
    shortcut_recorder::capture_native(&ui, KeyCode::IntlRo, Mods::CONTROL);
    assert!(ui.get_settings_failed());
    assert_eq!(ui.get_shortcut_draft(), "Ctrl+NumpadAdd");
    shortcut_recorder::capture_native(&ui, KeyCode::Enter, Mods::empty());
    assert_eq!(saves.get(), 1);
    shortcut_recorder::capture_native(&ui, KeyCode::Escape, Mods::empty());
    assert!(!ui.get_settings_open());
}

#[test]
fn keyboard_navigation_search_submit_and_empty_results() {
    slint::platform::set_platform(Box::new(i_slint_backend_testing::TestingBackend::new(
        i_slint_backend_testing::TestingBackendOptions {
            renderer_name: Some("software".into()),
            ..Default::default()
        },
    )))
    .unwrap();
    let ui = LauncherWindow::new().unwrap();
    let rows = [
        ("Visual Studio Code", "代码编辑器", "V", 0x88b7e4),
        ("Terminal", "终端", "T", 0x84c7b5),
        ("系统设置", "管理系统偏好设置", "系", 0xb7a4f4),
        ("Safari", "网页浏览器", "S", 0x88b7e4),
        ("Zen", "网页浏览器", "Z", 0xdc9fb8),
        ("Notes", "备忘录", "N", 0xe4b77b),
    ]
    .into_iter()
    .enumerate()
    .map(|(index, (name, detail, monogram, tint))| {
        let icon = if let Some(directory) = std::env::var_os("TAKE_A_PANEL_PREVIEW_ICONS") {
            let file = if name == "系统设置" {
                "System Settings"
            } else {
                name
            };
            let path = std::path::Path::new(&directory)
                .join(file)
                .with_extension("png");
            slint::Image::load_from_path(&path).expect("native preview icon")
        } else if index == 0 {
            slint::Image::from_rgba8(slint::SharedPixelBuffer::clone_from_slice(
                &[17u8, 103, 231, 255],
                1,
                1,
            ))
        } else {
            slint::Image::default()
        };
        AppRow {
            name: name.into(),
            detail: detail.into(),
            monogram: monogram.into(),
            tint: slint::Color::from_argb_encoded(0xff000000 | tint),
            has_icon: icon.size().width > 0,
            icon,
            ..Default::default()
        }
    })
    .collect::<Vec<_>>();
    ui.set_total_count(rows.len() as i32);
    ui.set_results(ModelRc::new(VecModel::from(rows)));
    ui.set_scanning(false);
    let submitted = Rc::new(Cell::new((-1, false)));
    let result = submitted.clone();
    let submit_count = Rc::new(Cell::new(0));
    let calls = submit_count.clone();
    ui.on_launch_selected(move |index, new_instance| {
        result.set((index, new_instance));
        calls.set(calls.get() + 1);
    });
    let edited = Rc::new(Cell::new(false));
    let result = edited.clone();
    ui.on_query_edited(move |_| result.set(true));
    let dismissed = Rc::new(Cell::new(false));
    let result = dismissed.clone();
    ui.on_dismiss(move || result.set(true));
    let refreshed = Rc::new(Cell::new(false));
    let result = refreshed.clone();
    ui.on_refresh(move || result.set(true));
    ui.show().unwrap();
    ui.invoke_focus_search();

    let snapshot = ui.window().take_snapshot().unwrap();
    if std::env::var_os("TAKE_A_PANEL_PREVIEW_ICONS").is_none() {
        assert!(
            snapshot
                .as_slice()
                .iter()
                .any(|pixel| (pixel.r, pixel.g, pixel.b, pixel.a) == (17, 103, 231, 255)),
            "the icon must be rendered in the result row"
        );
    }

    // Optional real renderer snapshot for reviewing the same UI the app runs.
    if let Some(path) = std::env::var_os("TAKE_A_PANEL_SNAPSHOT") {
        let mut encoder = png::Encoder::new(
            std::fs::File::create(path).unwrap(),
            snapshot.width(),
            snapshot.height(),
        );
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .write_header()
            .unwrap()
            .write_image_data(snapshot.as_bytes())
            .unwrap();
    }

    key(&ui, Key::UpArrow.into());
    assert_eq!(ui.get_selected_index(), 0);
    key(&ui, Key::DownArrow.into());
    assert_eq!(ui.get_selected_index(), 1);
    for _ in 0..12 {
        key(&ui, Key::DownArrow.into());
    }
    assert_eq!(ui.get_selected_index(), 5);
    key(&ui, Key::Return.into());
    assert_eq!(submitted.get(), (5, false));
    assert_eq!(submit_count.get(), 1);
    ui.window().dispatch_event(WindowEvent::KeyPressed {
        text: Key::Shift.into(),
    });
    key(&ui, Key::Return.into());
    assert_eq!(submitted.get(), (5, true));
    assert_eq!(
        submit_count.get(),
        2,
        "Shift+Enter must not also trigger normal launch"
    );
    ui.window().dispatch_event(WindowEvent::KeyReleased {
        text: Key::Shift.into(),
    });
    key(&ui, Key::Return.into());
    assert_eq!(submitted.get(), (5, false));
    assert_eq!(submit_count.get(), 3);
    key(&ui, "x".into());
    assert_eq!(ui.get_query(), "x");
    assert!(edited.get());
    assert_eq!(ui.get_selected_index(), 0);
    key(&ui, Key::Escape.into());
    assert!(dismissed.get());
    ui.window().dispatch_event(WindowEvent::KeyPressed {
        text: Key::Control.into(),
    });
    key(&ui, "r".into());
    ui.window().dispatch_event(WindowEvent::KeyReleased {
        text: Key::Control.into(),
    });
    assert!(refreshed.get());
    ui.set_results(ModelRc::new(VecModel::<AppRow>::default()));
    key(&ui, Key::DownArrow.into());
    assert_eq!(ui.get_selected_index(), 0);
    ui.invoke_reset_search();
    assert_eq!(ui.get_query(), "");
    ui.hide().unwrap();
}

#[test]
fn addon_settings_page_accepts_manifest_path() {
    slint::platform::set_platform(Box::new(i_slint_backend_testing::TestingBackend::new(
        i_slint_backend_testing::TestingBackendOptions {
            renderer_name: Some("software".into()),
            ..Default::default()
        },
    )))
    .unwrap();
    let ui = LauncherWindow::new().unwrap();
    ui.show().unwrap();
    ui.invoke_open_settings();
    click(&ui, 80., 360.);
    ui.set_addon_manifest_draft("/tmp/demo/manifest.json".into());
    let added = Rc::new(std::cell::RefCell::new(String::new()));
    let output = added.clone();
    ui.on_addon_add(move |value| *output.borrow_mut() = value.to_string());
    click(&ui, 600., 150.);
    assert_eq!(&*added.borrow(), "/tmp/demo/manifest.json");
    ui.hide().unwrap();
}

#[test]
fn theme_follows_system_only_when_selected() {
    slint::platform::set_platform(Box::new(i_slint_backend_testing::TestingBackend::new(
        i_slint_backend_testing::TestingBackendOptions {
            renderer_name: Some("software".into()),
            ..Default::default()
        },
    )))
    .unwrap();
    let ui = LauncherWindow::new().unwrap();
    ui.show().unwrap();
    assert_eq!(ui.get_theme_mode(), 0);
    assert!(!ui.get_light_mode());
    ui.set_system_light(true);
    assert!(ui.get_light_mode());
    ui.set_theme_mode(1);
    assert!(!ui.get_light_mode());
    ui.set_system_light(false);
    ui.set_theme_mode(2);
    assert!(ui.get_light_mode());
    ui.set_theme_mode(0);
    assert!(!ui.get_light_mode());
    ui.hide().unwrap();
}

#[test]
fn addon_form_edits_and_saves_through_native_controls() {
    slint::platform::set_platform(Box::new(i_slint_backend_testing::TestingBackend::new(
        i_slint_backend_testing::TestingBackendOptions {
            renderer_name: Some("software".into()),
            ..Default::default()
        },
    )))
    .unwrap();
    let ui = LauncherWindow::new().unwrap();
    ui.set_addon_list(ModelRc::new(VecModel::from(vec!["Demo".into()])));
    ui.set_addon_selected(0);
    ui.set_addon_fields(ModelRc::new(VecModel::from(vec![AddonSettingRow {
        label: "Name".into(),
        value: "Hello".into(),
        ..Default::default()
    }])));
    let edit = Rc::new(std::cell::RefCell::new(String::new()));
    let output = edit.clone();
    ui.on_addon_field_edited(move |_, value| *output.borrow_mut() = value.to_string());
    let saved = Rc::new(Cell::new(-1));
    let output = saved.clone();
    ui.on_addon_save_fields(move |index| output.set(index));
    ui.show().unwrap();
    ui.invoke_open_settings();
    click(&ui, 80., 360.);
    click(&ui, 270., 310.);
    key(&ui, "x".into());
    assert!(
        edit.borrow().contains('x'),
        "field was not edited: {:?}",
        edit.borrow()
    );
    click(&ui, 280., 400.);
    assert_eq!(saved.get(), 0);
    ui.hide().unwrap();
}

#[test]
fn addon_form_scrolls_to_later_fields() {
    slint::platform::set_platform(Box::new(i_slint_backend_testing::TestingBackend::new(
        i_slint_backend_testing::TestingBackendOptions {
            renderer_name: Some("software".into()),
            ..Default::default()
        },
    )))
    .unwrap();
    let ui = LauncherWindow::new().unwrap();
    ui.set_addon_list(ModelRc::new(VecModel::from(vec!["Demo".into()])));
    ui.set_addon_selected(0);
    let rows: Vec<_> = (0..8)
        .map(|i| AddonSettingRow {
            label: format!("Field {i}").into(),
            value: "value".into(),
            ..Default::default()
        })
        .collect();
    ui.set_addon_fields(ModelRc::new(VecModel::from(rows)));
    let edited = Rc::new(Cell::new(-1));
    let output = edited.clone();
    ui.on_addon_field_edited(move |index, _| output.set(index));
    ui.show().unwrap();
    ui.invoke_open_settings();
    click(&ui, 80., 360.);
    ui.window().dispatch_event(WindowEvent::PointerScrolled {
        position: slint::LogicalPosition::new(300., 330.),
        delta_x: 0.,
        delta_y: -500.,
    });
    for y in (280..390).step_by(10) {
        click(&ui, 280., y as f32);
        key(&ui, "x".into());
        if edited.get() >= 5 {
            break;
        }
    }
    assert!(
        edited.get() >= 5,
        "later field not accessible, edited {}",
        edited.get()
    );
    ui.hide().unwrap();
}
