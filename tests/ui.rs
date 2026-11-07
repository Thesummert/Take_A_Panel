use slint::{
    ComponentHandle, ModelRc, VecModel,
    platform::{Key, WindowEvent},
};
use std::{cell::Cell, rc::Rc};

slint::include_modules!();

fn key(ui: &LauncherWindow, text: slint::SharedString) {
    ui.window()
        .dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
    ui.window()
        .dispatch_event(WindowEvent::KeyReleased { text });
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
        ("Figma", "界面设计", "F", 0xdc9fb8),
        ("Notes", "备忘录", "N", 0xe4b77b),
    ]
    .into_iter()
    .map(|(name, detail, monogram, tint)| AppRow {
        name: name.into(),
        detail: detail.into(),
        monogram: monogram.into(),
        tint: slint::Color::from_argb_encoded(0xff000000 | tint),
    })
    .collect::<Vec<_>>();
    ui.set_total_count(rows.len() as i32);
    ui.set_results(ModelRc::new(VecModel::from(rows)));
    ui.set_scanning(false);
    let submitted = Rc::new(Cell::new(-1));
    let result = submitted.clone();
    ui.on_launch_selected(move |index| result.set(index));
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

    // Optional real renderer snapshot for reviewing the same UI the app runs.
    if let Some(path) = std::env::var_os("TAKE_A_PANEL_SNAPSHOT") {
        let snapshot = ui.window().take_snapshot().unwrap();
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
    assert_eq!(submitted.get(), 5);
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
