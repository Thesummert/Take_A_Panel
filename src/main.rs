#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod appearance;
mod apps;
mod autostart;
mod calculator;
mod client;
mod clipboard;
mod file_search;
mod icons;
mod launch_history;
#[cfg(any(target_os = "linux", test))]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod linux_file_picker;
#[cfg(target_os = "macos")]
mod macos_app;
mod options;
mod search;
mod server;
mod settings;
mod shortcut_recorder;
mod shortcuts;
mod window_focus;

use anyhow::Result;
use global_hotkey::{GlobalHotKeyEvent, HotKeyState};
use options::Options;
use slint::{
    ComponentHandle, Model, ModelRc, VecModel,
    winit_030::{EventResult, WinitWindowAccessor, winit},
};
use std::{
    cell::RefCell,
    rc::Rc,
    sync::{Arc, atomic::Ordering, mpsc},
    time::Duration,
};
use window_focus::WindowFocus;

slint::include_modules!();

thread_local! {
    // Native context creation may fail inside a show callback, rather than the
    // event loop itself. Preserve that error for the process-level retry.
    static WINDOW_RENDER_ERROR: RefCell<Option<slint::PlatformError>> = const { RefCell::new(None) };
}

enum WorkerEvent {
    Catalog(apps::Catalog),
    Icons(Vec<(usize, icons::IconData)>),
    ScanFinished,
    Launched(Result<launch_history::History, String>),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ResultTarget {
    Calculation,
    App(usize),
    Files,
    Clipboard,
}

struct Controller {
    ui: slint::Weak<LauncherWindow>,
    apps: RefCell<Vec<apps::Application>>,
    launch_history: RefCell<launch_history::History>,
    matches: RefCell<Vec<ResultTarget>>,
    calculation: RefCell<Option<String>>,
    clipboard: RefCell<clipboard::History>,
    clipboard_matches: RefCell<Vec<clipboard::Entry>>,
    icons: RefCell<Vec<slint::Image>>,
    rows: Rc<VecModel<AppRow>>,
    sender: mpsc::Sender<WorkerEvent>,
    receiver: mpsc::Receiver<WorkerEvent>,
    timer: slint::Timer,
    hotkey_notice: RefCell<String>,
    scan_notice: RefCell<String>,
    focus: Arc<WindowFocus>,
    files: Rc<file_search::Controller>,
}

impl Controller {
    fn new(ui: &LauncherWindow, hotkey_notice: String, focus: Arc<WindowFocus>) -> Rc<Self> {
        let files = file_search::Controller::new(ui, focus.clone());
        let history = match settings::directory() {
            Ok(directory) => launch_history::History::load(directory),
            Err(error) => {
                eprintln!("无法保存应用启动次数：{error:#}");
                launch_history::History::default()
            }
        };
        {
            let controller = Self::with_files(ui, hotkey_notice, focus, files, history);
            *controller.clipboard.borrow_mut() = clipboard::History::start();
            controller
        }
    }

    fn with_files(
        ui: &LauncherWindow,
        hotkey_notice: String,
        focus: Arc<WindowFocus>,
        files: Rc<file_search::Controller>,
        history: launch_history::History,
    ) -> Rc<Self> {
        let (sender, receiver) = mpsc::channel();
        let controller = Rc::new(Self {
            ui: ui.as_weak(),
            apps: RefCell::new(Vec::new()),
            launch_history: RefCell::new(history),
            matches: RefCell::new(Vec::new()),
            calculation: RefCell::new(None),
            clipboard: RefCell::new(clipboard::History::default()),
            clipboard_matches: RefCell::new(Vec::new()),
            icons: RefCell::new(Vec::new()),
            rows: Rc::new(VecModel::default()),
            sender,
            receiver,
            timer: slint::Timer::default(),
            hotkey_notice: RefCell::new(hotkey_notice),
            scan_notice: RefCell::new(String::new()),
            focus,
            files,
        });
        ui.set_results(ModelRc::from(controller.rows.clone()));
        controller
    }

    fn update_notice(&self) {
        if let Some(ui) = self.ui.upgrade() {
            let notices = [&*self.hotkey_notice.borrow(), &*self.scan_notice.borrow()]
                .into_iter()
                .filter(|s| !s.is_empty())
                .cloned()
                .collect::<Vec<_>>()
                .join(" · ");
            ui.set_notice(notices.into());
        }
    }

    fn apply_icons(&self, batch: Vec<(usize, icons::IconData)>) {
        let mut icons = self.icons.borrow_mut();
        let matches = self.matches.borrow();
        for (index, icon) in batch {
            let Some(cached) = icons.get_mut(index) else {
                continue;
            };
            *cached = icon.into_image();
            if let Some(row) = matches
                .iter()
                .position(|&app_index| app_index == ResultTarget::App(index))
                && let Some(mut data) = self.rows.row_data(row)
            {
                data.icon = cached.clone();
                data.has_icon = true;
                self.rows.set_row_data(row, data);
            }
        }
    }

    fn update_results(&self, query: &str) {
        let Some(ui) = self.ui.upgrade() else {
            return;
        };
        if ui.get_file_search_mode() || ui.get_clipboard_mode() {
            return;
        }
        ui.set_results(ModelRc::from(self.rows.clone()));
        let apps = self.apps.borrow();
        let mut indices = search::search(&apps, query);
        if query.trim().is_empty() {
            self.launch_history.borrow().sort(&apps, &mut indices);
        }
        let mut matches = indices
            .into_iter()
            .map(ResultTarget::App)
            .collect::<Vec<_>>();
        if file_search::is_command(query) {
            matches.insert(0, ResultTarget::Files);
        }
        if query.trim().eq_ignore_ascii_case("clp") {
            matches.insert(0, ResultTarget::Clipboard);
        }
        let calculation = calculator::evaluate(query);
        if calculation.is_some() {
            matches.insert(0, ResultTarget::Calculation);
        }
        let colors = [0xb7a4f4, 0x84c7b5, 0xe4b77b, 0x88b7e4, 0xdc9fb8];
        let rows = matches
            .iter()
            .map(|&index| {
                let index = match index {
                    ResultTarget::Calculation => {
                        return AppRow {
                            name: calculation.as_deref().unwrap_or_default().into(),
                            detail: "计算结果 · 按 Enter 复制到剪贴板".into(),
                            monogram: "=".into(),
                            tint: slint::Color::from_rgb_u8(132, 199, 181),
                            ..Default::default()
                        };
                    }
                    ResultTarget::Clipboard => {
                        return AppRow {
                            name: "剪贴板记录".into(),
                            detail: "按 Enter 查看并重新复制历史文本和图片".into(),
                            monogram: "CLP".into(),
                            tint: slint::Color::from_rgb_u8(183, 164, 244),
                            ..Default::default()
                        };
                    }
                    ResultTarget::Files => {
                        return AppRow {
                            name: "文件搜索".into(),
                            detail: "按 Enter 进入，搜索索引范围中的文件和文件夹".into(),
                            monogram: "SS".into(),
                            tint: slint::Color::from_rgb_u8(132, 199, 181),
                            ..Default::default()
                        };
                    }
                    ResultTarget::App(index) => index,
                };
                let app = &apps[index];
                let icon = self.icons.borrow().get(index).cloned().unwrap_or_default();
                let color_index = app
                    .name
                    .bytes()
                    .fold(0usize, |hash, b| hash.wrapping_add(b as usize))
                    % colors.len();
                AppRow {
                    name: app.name.as_str().into(),
                    detail: app.detail.as_str().into(),
                    monogram: app
                        .name
                        .chars()
                        .next()
                        .unwrap_or('?')
                        .to_uppercase()
                        .to_string()
                        .into(),
                    tint: slint::Color::from_argb_encoded(0xff000000 | colors[color_index]),
                    has_icon: icon.size().width > 0,
                    app_result: true,
                    has_path: matches!(app.target, apps::LaunchTarget::File(_)),
                    icon,
                }
            })
            .collect::<Vec<_>>();
        *self.matches.borrow_mut() = matches;
        *self.calculation.borrow_mut() = calculation;
        ui.invoke_close_context_menu();
        self.rows.set_vec(rows);
        ui.set_selected_index(0);
    }

    fn update_clipboard_results(&self, query: &str) {
        let Some(ui) = self.ui.upgrade() else {
            return;
        };
        let entries = self.clipboard.borrow().search(query);
        let rows: Vec<_> = entries
            .iter()
            .map(|entry| match entry {
                clipboard::Entry::Text(text) => AppRow {
                    name: text.lines().next().unwrap_or("").into(),
                    detail: "按 Enter 重新复制文本".into(),
                    monogram: "C".into(),
                    tint: slint::Color::from_rgb_u8(183, 164, 244),
                    ..Default::default()
                },
                clipboard::Entry::Image { width, height, .. } => {
                    let icon = entry.image().map(|image| {
                        let thumbnail = image::imageops::thumbnail(&image, 64, 64);
                        let (width, height) = thumbnail.dimensions();
                        slint::Image::from_rgba8(slint::SharedPixelBuffer::clone_from_slice(
                            thumbnail.as_raw(),
                            width,
                            height,
                        ))
                    });
                    AppRow {
                        name: format!("图片  {width} × {height}").into(),
                        detail: "按 Enter 重新复制图片".into(),
                        monogram: "图".into(),
                        tint: slint::Color::from_rgb_u8(183, 164, 244),
                        has_icon: icon.is_some(),
                        icon: icon.unwrap_or_default(),
                        ..Default::default()
                    }
                }
            })
            .collect();
        *self.clipboard_matches.borrow_mut() = entries;
        ui.invoke_close_context_menu();
        self.rows.set_vec(rows);
        ui.set_results(ModelRc::from(self.rows.clone()));
        ui.set_selected_index(0);
    }

    fn refresh(self: &Rc<Self>) {
        let Some(ui) = self.ui.upgrade() else {
            return;
        };
        if ui.get_scanning() {
            return;
        }
        ui.set_scanning(true);
        ui.set_error_message("".into());
        let sender = self.sender.clone();
        std::thread::spawn(move || {
            let catalog = apps::discover();
            let apps = catalog.apps.clone();
            if sender.send(WorkerEvent::Catalog(catalog)).is_err() {
                return;
            }
            let mut batch = Vec::new();
            for (index, app) in apps.iter().enumerate() {
                if let Some(icon) = icons::load(app) {
                    batch.push((index, icon));
                }
                if batch.len() >= 8
                    && sender
                        .send(WorkerEvent::Icons(std::mem::take(&mut batch)))
                        .is_err()
                {
                    return;
                }
            }
            if !batch.is_empty() {
                let _ = sender.send(WorkerEvent::Icons(batch));
            }
            let _ = sender.send(WorkerEvent::ScanFinished);
        });
        self.start_pump();
    }

    fn copy_result_path(&self, row: i32) {
        let Some(ui) = self.ui.upgrade() else { return };
        let path = if ui.get_file_search_mode() {
            self.files.result_path(row)
        } else if !ui.get_clipboard_mode() {
            usize::try_from(row)
                .ok()
                .and_then(|row| self.matches.borrow().get(row).copied())
                .and_then(|target| match target {
                    ResultTarget::App(index) => {
                        self.apps
                            .borrow()
                            .get(index)
                            .and_then(|app| match &app.target {
                                apps::LaunchTarget::File(path) => Some(path.clone()),
                                #[cfg(target_os = "windows")]
                                apps::LaunchTarget::AppId(_) => None,
                            })
                    }
                    _ => None,
                })
        } else {
            None
        };
        if let Some(path) = path {
            match self
                .clipboard
                .borrow()
                .copy(clipboard::Entry::Text(path.to_string_lossy().into_owned()))
            {
                Ok(()) => {
                    ui.set_error_message("".into());
                    ui.set_notice("路径已复制".into());
                }
                Err(error) => ui.set_error_message(error.into()),
            }
        }
    }

    fn launch(self: &Rc<Self>, row: i32, new_instance: bool) {
        let Some(ui) = self.ui.upgrade() else {
            return;
        };
        if ui.get_launching() {
            return;
        }
        if ui.get_file_search_mode() {
            self.files.open(row);
            return;
        }
        if ui.get_clipboard_mode() {
            if let Some(text) = usize::try_from(row)
                .ok()
                .and_then(|i| self.clipboard_matches.borrow().get(i).cloned())
            {
                match self.clipboard.borrow().copy(text) {
                    Ok(()) => {
                        let _ = ui.hide();
                    }
                    Err(error) => ui.set_error_message(error.into()),
                }
            }
            return;
        }
        let selected = usize::try_from(row)
            .ok()
            .and_then(|row| self.matches.borrow().get(row).copied());
        let Some(selected) = selected else {
            return;
        };
        let index = match selected {
            ResultTarget::Calculation => {
                if let Some(result) = self.calculation.borrow().clone() {
                    match self.clipboard.borrow().copy(clipboard::Entry::Text(result)) {
                        Ok(()) => {
                            let _ = ui.hide();
                        }
                        Err(error) => ui.set_error_message(error.into()),
                    }
                }
                return;
            }
            ResultTarget::Files => {
                self.files.enter();
                return;
            }
            ResultTarget::Clipboard => {
                ui.set_clipboard_mode(true);
                ui.set_query("".into());
                ui.set_error_message("".into());
                self.update_clipboard_results("");
                ui.invoke_focus_search();
                return;
            }
            ResultTarget::App(index) => index,
        };
        let app = self.apps.borrow().get(index).cloned();
        let Some(app) = app else {
            return;
        };
        ui.set_launching(true);
        ui.set_error_message("".into());
        let _ = ui.hide();
        let sender = self.sender.clone();
        let mode = if new_instance {
            apps::LaunchMode::NewInstance
        } else {
            apps::LaunchMode::Normal
        };
        let mut history = self.launch_history.borrow().clone();
        std::thread::spawn(move || {
            let result = apps::launch(&app, mode)
                .map(|()| {
                    history.record(&app);
                    history
                })
                .map_err(|error| format!("无法打开 {}：{error:#}", app.name));
            let _ = sender.send(WorkerEvent::Launched(result));
        });
        self.start_pump();
    }

    fn start_pump(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        // Only poll while a worker is active. Hidden, idle launchers have no
        // recurring timer; global hotkeys wake the native event loop directly.
        self.timer.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(60),
            move || {
                let Some(controller) = weak.upgrade() else {
                    return;
                };
                let Some(ui) = controller.ui.upgrade() else {
                    return;
                };
                while let Ok(event) = controller.receiver.try_recv() {
                    match event {
                        WorkerEvent::Catalog(catalog) => {
                            for warning in &catalog.warnings {
                                eprintln!("应用扫描: {warning}");
                            }
                            let scan_notice = if catalog.warnings.is_empty() {
                                String::new()
                            } else {
                                format!(
                                    "部分应用扫描失败（{} 项），详情见终端",
                                    catalog.warnings.len()
                                )
                            };
                            *controller.scan_notice.borrow_mut() = scan_notice;
                            controller.update_notice();
                            ui.set_total_count(catalog.apps.len() as i32);
                            *controller.icons.borrow_mut() =
                                vec![slint::Image::default(); catalog.apps.len()];
                            *controller.apps.borrow_mut() = catalog.apps;
                            controller.update_results(ui.get_query().as_str());
                        }
                        WorkerEvent::Icons(batch) => {
                            controller.apply_icons(batch);
                        }
                        WorkerEvent::ScanFinished => {
                            ui.set_scanning(false);
                        }
                        WorkerEvent::Launched(result) => {
                            ui.set_launching(false);
                            match result {
                                Ok(history) => {
                                    *controller.launch_history.borrow_mut() = history;
                                    controller.update_results(ui.get_query().as_str());
                                }
                                Err(error) => {
                                    eprintln!("{error}");
                                    present(&ui, false, &controller.focus);
                                    ui.set_error_message(error.into());
                                }
                            }
                        }
                    }
                }
                if !ui.get_scanning() && !ui.get_launching() {
                    controller.timer.stop();
                }
            },
        );
    }
}

fn bind_window_size(ui: &LauncherWindow) {
    use slint::ComponentHandle;
    let loaded =
        settings::directory().and_then(|dir| settings::Settings::new(dir).load_window_size());
    let (width, height) = match loaded {
        Ok(size) => size.unwrap_or((680, 540)),
        Err(error) => {
            eprintln!("读取窗口大小失败：{error:#}");
            (680, 540)
        }
    };
    ui.set_window_width(width as i32);
    ui.set_window_height(height as i32);
    ui.set_window_width_draft(width.to_string().into());
    ui.set_window_height_draft(height.to_string().into());
    let weak = ui.as_weak();
    ui.on_save_window_size(move |width, height| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let result = settings::parse_window_size(&format!("{} {}", width.trim(), height.trim()))
            .and_then(|(width, height)| {
                settings::Settings::new(settings::directory()?).save_window_size(width, height)?;
                Ok((width, height))
            });
        match result {
            Ok((width, height)) => {
                ui.set_window_width(width as i32);
                ui.set_window_height(height as i32);
                ui.set_window_width_draft(width.to_string().into());
                ui.set_window_height_draft(height.to_string().into());
                ui.invoke_window_size_applied();
                ui.set_settings_failed(false);
                ui.set_settings_message("窗口大小已保存。".into());
            }
            Err(error) => {
                ui.set_settings_failed(true);
                ui.set_settings_message(format!("保存窗口大小失败：{error:#}").into());
            }
        }
    });
}

fn present(ui: &LauncherWindow, reset: bool, focus: &WindowFocus) {
    let was_visible = ui.window().is_visible();
    focus.begin_presentation();
    if reset {
        ui.invoke_reset_search();
    }
    ui.set_error_message("".into());
    ui.window().set_minimized(false);
    if let Err(error) = ui.show() {
        eprintln!("无法显示窗口: {error}");
        WINDOW_RENDER_ERROR.with_borrow_mut(|pending| *pending = Some(error));
        let _ = slint::quit_event_loop();
        return;
    }
    if !was_visible {
        ui.invoke_background_presented();
    }
    ui.invoke_focus_search();
    let weak = ui.as_weak();
    // Winit may create the native window only on the next event-loop iteration.
    slint::Timer::single_shot(Duration::from_millis(30), move || {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        if !ui.window().is_visible() {
            return;
        }
        ui.window().with_winit_window(|window| {
            if let Some(monitor) = window.current_monitor() {
                let origin = monitor.position();
                let screen = monitor.size();
                let size = window.outer_size();
                let x = origin.x + (screen.width.saturating_sub(size.width) / 2) as i32;
                let y = origin.y + (screen.height.saturating_sub(size.height) / 3) as i32;
                window.set_outer_position(winit::dpi::PhysicalPosition::new(x, y));
            }
            window.focus_window();
        });
        ui.invoke_focus_search();
    });
}

fn register_hotkey(
    ui: &LauncherWindow,
    options: &Options,
    focus: Arc<WindowFocus>,
) -> (Rc<RefCell<shortcuts::Shortcuts>>, String) {
    let shortcuts = Rc::new(RefCell::new(shortcuts::Shortcuts::default()));
    let active_id = shortcuts.borrow().active_id.clone();
    let weak = ui.as_weak();
    // The global handler can be installed only once. Read the current ID so
    // changes and enabling a shortcut after --no-hotkey take effect immediately.
    GlobalHotKeyEvent::set_event_handler(Some(move |event: GlobalHotKeyEvent| {
        if active_id.load(Ordering::Acquire) != u64::from(event.id) + 1
            || event.state != HotKeyState::Pressed
        {
            return;
        }
        let focus = focus.clone();
        let active_id = active_id.clone();
        let _ = weak.upgrade_in_event_loop(move |ui| {
            if active_id.load(Ordering::Acquire) != u64::from(event.id) + 1 {
                return;
            }
            if ui.window().is_visible() && ui.get_shortcut_recording() {
                ui.set_shortcut_draft(ui.get_configured_hotkey());
                ui.set_settings_failed(false);
                ui.set_settings_message("已录入，点击“保存快捷键”生效。".into());
                return;
            }
            if ui.window().is_visible() {
                let _ = ui.hide();
            } else {
                present(&ui, true, &focus);
            }
        });
    }));
    if options.no_hotkey {
        return (
            shortcuts,
            "全局快捷键已禁用；可从 P 菜单或命令行唤出".into(),
        );
    }
    let registration = shortcuts.borrow_mut().apply(&options.hotkey, || Ok(()));
    match registration {
        Ok(()) => (shortcuts, String::new()),
        Err(error) => {
            let message = format!("{error:#}");
            eprintln!("{message}");
            (shortcuts, format!("{message}；可从 P 菜单或命令行唤出"))
        }
    }
}

fn main() -> Result<()> {
    let Some(options) = Options::parse()? else {
        return Ok(());
    };
    if options.list {
        let catalog = apps::discover();
        for app in catalog.apps {
            println!("{}\t{}", app.name, app.detail);
        }
        for warning in catalog.warnings {
            eprintln!("{warning}");
        }
        return Ok(());
    }

    if !options.server {
        return client::run(&options);
    }
    let directory = server::runtime_directory()?;
    let Some(server) = server::Server::claim(&directory)? else {
        server::wait_request(&directory, server::Command::Ping, Duration::from_secs(15))?;
        return Ok(());
    };
    match run_server(options, server) {
        Err(error)
            if error.downcast_ref::<slint::PlatformError>().is_some()
                && std::env::var_os("TAKE_A_PANEL_SOFTWARE_RETRY").is_none() =>
        {
            // A GL context can fail when the window is first shown, after backend
            // selection. Slint's global backend cannot be replaced in-process.
            // run_server has dropped its server lock; retry in a fresh process.
            eprintln!("图形初始化失败，使用软件渲染重试：{error:#}");
            let status = std::process::Command::new(std::env::current_exe()?)
                .args(std::env::args_os().skip(1))
                .env("SLINT_BACKEND", "winit-software")
                .env("TAKE_A_PANEL_SOFTWARE_RETRY", "1")
                .status()?;
            anyhow::ensure!(status.success(), "软件渲染进程退出：{status}");
            Ok(())
        }
        result => result,
    }
}

fn run_server(options: Options, mut server: server::Server) -> Result<()> {
    let backend = slint::BackendSelector::new()
        .backend_name("winit".into())
        .with_winit_window_attributes_hook(|attributes| {
            let attributes = attributes.with_transparent(true).with_resizable(false);
            #[cfg(target_os = "linux")]
            let attributes = {
                use winit::platform::x11::{WindowAttributesExtX11, WindowType};
                attributes
                    .with_name("com.takeapanel.launcher", "take-a-panel")
                    .with_x11_window_type(vec![WindowType::Dialog])
            };
            attributes
        });
    #[cfg(target_os = "macos")]
    let backend = {
        use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};
        let mut builder =
            winit::event_loop::EventLoop::<slint::winit_030::SlintEvent>::with_user_event();
        builder.with_activation_policy(ActivationPolicy::Accessory);
        backend.with_winit_event_loop_builder(builder)
    };
    backend.select()?;
    let ui = LauncherWindow::new()?;
    shortcut_recorder::bind(&ui);
    bind_window_size(&ui);
    appearance::bind(&ui);
    autostart::bind(&ui);
    let focus = Arc::new(WindowFocus::default());
    #[cfg(target_os = "macos")]
    let _reopen_handler = {
        let weak = ui.as_weak();
        let reopen_focus = focus.clone();
        macos_app::install(move || {
            let focus = reopen_focus.clone();
            let _ = weak.upgrade_in_event_loop(move |ui| present(&ui, true, &focus));
        })?
    };
    let dialog_open = Arc::new(std::sync::atomic::AtomicBool::new(false));
    file_search::bind_folder_picker(&ui, focus.clone(), dialog_open.clone());
    let dialog_flag = dialog_open.clone();
    let dialog_ui = ui.as_weak();
    let dialog_focus = focus.clone();
    ui.on_browse_background(move || {
        if dialog_flag.swap(true, Ordering::Relaxed) {
            return;
        }
        let flag = dialog_flag.clone();
        let weak = dialog_ui.clone();
        let focus = dialog_focus.clone();
        std::thread::spawn(move || {
            #[cfg(target_os = "macos")]
            let result = std::process::Command::new("osascript")
                .args(["-e", "set selectedFiles to choose file with prompt \"选择背景图片\" of type {\"public.png\", \"public.jpeg\"} with multiple selections allowed", "-e", "set outputPaths to {}", "-e", "repeat with selectedFile in selectedFiles", "-e", "set end of outputPaths to POSIX path of selectedFile", "-e", "end repeat", "-e", "set AppleScript's text item delimiters to linefeed", "-e", "return outputPaths as text"]).output();
            #[cfg(target_os = "linux")]
            let result = linux_file_picker::pick_background().map(|paths| paths.map(|paths| paths.into_iter().map(|p| p.to_string_lossy().into_owned()).collect::<Vec<_>>()));
            #[cfg(target_os = "windows")]
            let result = std::process::Command::new("powershell")
                .args(["-NoProfile", "-STA", "-Command", "Add-Type -AssemblyName System.Windows.Forms; $d = New-Object System.Windows.Forms.OpenFileDialog; $d.Filter = '图片|*.png;*.jpg;*.jpeg'; $d.Multiselect = $true; if ($d.ShowDialog() -eq 'OK') { [Console]::OutputEncoding = [System.Text.Encoding]::UTF8; [Console]::Write(($d.FileNames -join [char]10)) }"]).output();
            #[cfg(not(target_os = "linux"))]
            let result = result.map_err(anyhow::Error::from).map(|output| {
                if output.status.success() {
                    let path = String::from_utf8_lossy(&output.stdout);
                    let path = path.trim_end_matches(['\r', '\n']);
                    if !path.is_empty() { return Some(path.lines().map(str::to_owned).collect::<Vec<_>>()); }
                }
                None
            });
            let _ = weak.upgrade_in_event_loop(move |ui| {
                focus.begin_presentation();
                flag.store(false, Ordering::Relaxed);
                match result {
                    Ok(Some(paths)) => {
                        let existing = ui.get_background_path().to_string();
                        let mut all = existing.lines().filter(|path| !path.trim().is_empty()).map(str::to_owned).collect::<Vec<_>>();
                        for path in paths {
                            if !all.contains(&path) { all.push(path); }
                        }
                        ui.set_background_path(all.join("\n").into());
                    }
                    Err(error) => {
                        ui.set_settings_failed(true);
                        ui.set_settings_message(format!("无法打开文件选择器：{error}").into());
                    }
                    _ => {}
                }
            });
        });
    });
    let (hotkey_manager, hotkey_notice) = register_hotkey(&ui, &options, focus.clone());
    let has_hotkey = hotkey_manager.borrow().enabled();
    ui.set_configured_hotkey(options.hotkey.as_str().into());
    ui.set_default_hotkey(options::DEFAULT_HOTKEY.into());
    ui.set_hotkey_label(
        if has_hotkey {
            options.hotkey.clone()
        } else {
            "快捷键未启用".into()
        }
        .into(),
    );
    ui.set_notice(hotkey_notice.as_str().into());
    let controller = Controller::new(&ui, hotkey_notice, focus.clone());

    let weak = Rc::downgrade(&controller);
    let shortcuts = Rc::downgrade(&hotkey_manager);
    ui.on_save_shortcut(move |value| {
        let (Some(controller), Some(shortcuts)) = (weak.upgrade(), shortcuts.upgrade()) else {
            return;
        };
        let Some(ui) = controller.ui.upgrade() else {
            return;
        };
        let value = value.trim();
        let result = shortcuts.borrow_mut().apply(value, || {
            settings::Settings::new(settings::directory()?).save_hotkey(value)
        });
        ui.set_settings_failed(result.is_err());
        match result {
            Ok(()) => {
                ui.set_configured_hotkey(value.into());
                ui.set_shortcut_draft(value.into());
                ui.set_hotkey_label(value.into());
                controller.hotkey_notice.borrow_mut().clear();
                controller.update_notice();
                ui.set_settings_message("已保存，快捷键立即生效，下次启动继续使用。".into());
            }
            Err(error) => {
                eprintln!("保存快捷键失败：{error:#}");
                ui.set_settings_message(format!("{error:#}").into());
            }
        }
    });

    let weak = Rc::downgrade(&controller);
    ui.on_query_edited(move |query| {
        if let Some(c) = weak.upgrade() {
            if c.ui.upgrade().is_some_and(|ui| ui.get_file_search_mode()) {
                c.files.query(query.as_str());
            } else if c.ui.upgrade().is_some_and(|ui| ui.get_clipboard_mode()) {
                c.update_clipboard_results(query.as_str());
            } else {
                c.files.suspend();
                c.update_results(query.as_str());
            }
        }
    });
    let weak = Rc::downgrade(&controller);
    ui.on_launch_selected(move |index, new_instance| {
        if let Some(c) = weak.upgrade() {
            c.launch(index, new_instance);
        }
    });
    let weak = Rc::downgrade(&controller);
    ui.on_copy_result_path(move |index| {
        if let Some(c) = weak.upgrade() {
            c.copy_result_path(index);
        }
    });
    let weak = Rc::downgrade(&controller);
    ui.on_refresh(move || {
        if let Some(c) = weak.upgrade() {
            if c.ui.upgrade().is_some_and(|ui| ui.get_file_search_mode()) {
                c.files.rebuild();
            } else {
                c.refresh();
            }
        }
    });
    let weak = Rc::downgrade(&controller);
    ui.on_refresh_apps(move || {
        if let Some(c) = weak.upgrade() {
            c.refresh();
        }
    });
    let weak = ui.as_weak();
    ui.on_dismiss(move || {
        if let Some(ui) = weak.upgrade() {
            let _ = ui.hide();
        }
    });
    ui.on_quit(|| {
        let _ = slint::quit_event_loop();
    });
    let weak = ui.as_weak();
    ui.window().on_close_requested(move || {
        if let Some(ui) = weak.upgrade() {
            ui.invoke_dismiss();
        }
        slint::CloseRequestResponse::KeepWindowShown
    });
    let window_focus = focus.clone();
    let weak = ui.as_weak();
    let mut modifiers = winit::keyboard::ModifiersState::empty();
    ui.window().on_winit_window_event(move |window, event| {
        if let winit::event::WindowEvent::ModifiersChanged(state) = event {
            modifiers = state.state();
        }
        if let winit::event::WindowEvent::KeyboardInput {
            event,
            is_synthetic: false,
            ..
        } = event
            && let Some(ui) = weak.upgrade()
            && ui.get_shortcut_recording()
            && event.state == winit::event::ElementState::Pressed
            && let winit::keyboard::PhysicalKey::Code(key) = event.physical_key
            && (event.repeat || shortcut_recorder::capture_native(&ui, key, modifiers))
        {
            return EventResult::PreventDefault;
        }
        if let winit::event::WindowEvent::Focused(focused) = event {
            if *focused
                && let Some(ui) = weak.upgrade()
                && ui.get_settings_open()
            {
                ui.invoke_refresh_autostart();
            }
            if !focused {
                modifiers = winit::keyboard::ModifiersState::empty();
            }
            let should_hide = window_focus.changed(*focused);
            if should_hide && !dialog_open.load(Ordering::Relaxed) && window.is_visible() {
                let _ = window.hide();
            }
        }
        EventResult::Propagate
    });

    let tray = LauncherTray::new()?;
    let weak = ui.as_weak();
    let tray_focus = focus.clone();
    tray.on_show_panel(move || {
        if let Some(ui) = weak.upgrade() {
            present(&ui, true, &tray_focus);
        }
    });
    let weak = ui.as_weak();
    tray.on_refresh_apps(move || {
        if let Some(ui) = weak.upgrade() {
            ui.invoke_refresh_apps();
        }
    });
    let weak = ui.as_weak();
    let settings_focus = focus.clone();
    tray.on_shortcut_settings(move || {
        if let Some(ui) = weak.upgrade() {
            ui.invoke_open_settings();
            present(&ui, false, &settings_focus);
        }
    });
    let weak = ui.as_weak();
    let startup_focus = focus.clone();
    tray.on_autostart_settings(move || {
        if let Some(ui) = weak.upgrade() {
            ui.invoke_open_autostart_settings();
            present(&ui, false, &startup_focus);
        }
    });
    let weak = ui.as_weak();
    tray.on_quit_server(move || {
        if let Some(ui) = weak.upgrade() {
            ui.invoke_quit();
        }
    });
    tray.show()?;

    let weak = ui.as_weak();
    server.start(move |command| {
        let focus = focus.clone();
        weak.upgrade_in_event_loop(move |ui| {
            match command {
                server::Command::Show => present(&ui, true, &focus),
                server::Command::Hide => {
                    let _ = ui.hide();
                }
                server::Command::Toggle => {
                    if ui.window().is_visible() {
                        let _ = ui.hide();
                    } else {
                        present(&ui, true, &focus);
                    }
                }
                server::Command::Refresh => ui.invoke_refresh_apps(),
                server::Command::Quit => {
                    // Let the listener acknowledge the request before exiting.
                    slint::Timer::single_shot(Duration::from_millis(100), || {
                        let _ = slint::quit_event_loop();
                    });
                }
                server::Command::Ping => {}
            }
        })?;
        Ok(())
    })?;
    ui.set_scanning(false);
    controller.refresh();
    slint::run_event_loop_until_quit()?;
    if let Some(error) = WINDOW_RENDER_ERROR.with_borrow_mut(Option::take) {
        return Err(error.into());
    }
    drop(tray);
    drop(hotkey_manager);
    // Release ownership only after the tray and global shortcut are gone.
    drop(server);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn controller(ui: &LauncherWindow, settings: &std::path::Path) -> Rc<Controller> {
        let focus = Arc::default();
        let files =
            file_search::Controller::with_settings(ui, Arc::clone(&focus), Ok(settings.to_owned()));
        Controller::with_files(
            ui,
            String::new(),
            focus,
            files,
            launch_history::History::load(settings.to_owned()),
        )
    }

    #[test]
    fn ss_is_first_and_enter_switches_modes_while_app_updates_preserve_file_results() {
        i_slint_backend_testing::init_no_event_loop();
        let dir = file_search::test_support::TempDir::new();
        let ui = LauncherWindow::new().unwrap();
        let controller = controller(&ui, &dir.0);
        *controller.apps.borrow_mut() = vec![apps::Application {
            name: "ss".into(),
            detail: "An application named ss".into(),
            keywords: String::new(),
            icon_name: None,
            target: apps::LaunchTarget::File("/unused".into()),
        }];
        *controller.icons.borrow_mut() = vec![slint::Image::default()];
        controller.update_results(" SS ");
        assert_eq!(controller.rows.row_data(0).unwrap().name, "文件搜索");
        assert_eq!(controller.rows.row_data(1).unwrap().name, "ss");
        let pixel = icons::IconData {
            pixels: slint::SharedPixelBuffer::clone_from_slice(&[255, 0, 0, 255], 1, 1),
            premultiplied: false,
        };
        controller.apply_icons(vec![(0, pixel)]);
        assert!(!controller.rows.row_data(0).unwrap().has_icon);
        assert!(controller.rows.row_data(1).unwrap().has_icon);
        controller.launch(0, false);
        assert!(ui.get_file_search_mode());
        assert_eq!(ui.get_query(), "");
        assert!(!ui.get_launching());
        controller.update_results("ss");
        assert_eq!(
            ui.get_results().row_count(),
            0,
            "application scans must not replace file rows"
        );
        ui.set_file_search_mode(false);
        controller.files.suspend();
        controller.update_results("");
        assert_eq!(ui.get_results().row_data(0).unwrap().name, "ss");
    }

    #[test]
    fn clp_command_has_priority_and_opens_clipboard_mode() {
        i_slint_backend_testing::init_no_event_loop();
        let dir = file_search::test_support::TempDir::new();
        let ui = LauncherWindow::new().unwrap();
        let controller = controller(&ui, &dir.0);
        *controller.apps.borrow_mut() = vec![apps::Application {
            name: "clp".into(),
            detail: String::new(),
            keywords: String::new(),
            icon_name: None,
            target: apps::LaunchTarget::File("/unused".into()),
        }];
        controller.update_results(" CLP ");
        assert_eq!(controller.rows.row_data(0).unwrap().name, "剪贴板记录");
        assert_eq!(controller.rows.row_data(1).unwrap().name, "clp");
        controller.launch(0, false);
        assert!(ui.get_clipboard_mode());
        assert_eq!(ui.get_query(), "");
        controller.update_results("clp");
        assert_eq!(ui.get_results().row_count(), 0);
    }

    #[test]
    fn late_icon_batches_update_the_matching_filtered_row_and_are_cached() {
        i_slint_backend_testing::init_no_event_loop();
        let ui = LauncherWindow::new().unwrap();
        let dir = file_search::test_support::TempDir::new();
        let controller = controller(&ui, &dir.0);
        *controller.apps.borrow_mut() = ["Alpha", "Beta"]
            .into_iter()
            .map(|name| apps::Application {
                name: name.into(),
                detail: String::new(),
                keywords: String::new(),
                icon_name: None,
                target: apps::LaunchTarget::File("/unused".into()),
            })
            .collect();
        *controller.icons.borrow_mut() = vec![slint::Image::default(); 2];
        controller.update_results("beta");
        assert!(!controller.rows.row_data(0).unwrap().has_icon);
        let pixel = |rgba: &[u8]| icons::IconData {
            pixels: slint::SharedPixelBuffer::clone_from_slice(rgba, 1, 1),
            premultiplied: false,
        };
        controller.apply_icons(vec![
            (0, pixel(&[255, 0, 0, 255])),
            (1, pixel(&[0, 0, 255, 255])),
        ]);
        let row = controller.rows.row_data(0).unwrap();
        assert_eq!(row.name, "Beta");
        assert!(row.has_icon);
        assert_eq!(row.icon.to_rgba8().unwrap().as_bytes(), &[0, 0, 255, 255]);
        controller.update_results("alpha");
        let row = controller.rows.row_data(0).unwrap();
        assert_eq!(row.name, "Alpha");
        assert!(row.has_icon);
        assert_eq!(row.icon.to_rgba8().unwrap().as_bytes(), &[255, 0, 0, 255]);
    }

    #[test]
    fn default_rows_use_saved_counts_while_search_keeps_relevance_and_icon_mapping() {
        i_slint_backend_testing::init_no_event_loop();
        let dir = file_search::test_support::TempDir::new();
        let apps: Vec<_> = ["Code", "Code Studio", "Other"]
            .into_iter()
            .map(|name| apps::Application {
                name: name.into(),
                detail: String::new(),
                keywords: String::new(),
                icon_name: None,
                target: apps::LaunchTarget::File(format!("/apps/{name}").into()),
            })
            .collect();
        let mut history = launch_history::History::load(dir.0.clone());
        history.record(&apps[1]);
        let ui = LauncherWindow::new().unwrap();
        let controller = controller(&ui, &dir.0);
        *controller.apps.borrow_mut() = apps;
        *controller.icons.borrow_mut() = vec![slint::Image::default(); 3];
        controller.update_results("  ");
        assert_eq!(controller.rows.row_data(0).unwrap().name, "Code Studio");
        assert!(controller.matches.borrow()[0] == ResultTarget::App(1));
        controller.apply_icons(vec![(
            1,
            icons::IconData {
                pixels: slint::SharedPixelBuffer::clone_from_slice(&[0, 0, 255, 255], 1, 1),
                premultiplied: false,
            },
        )]);
        assert!(controller.rows.row_data(0).unwrap().has_icon);
        assert!(!controller.rows.row_data(1).unwrap().has_icon);
        controller.update_results("code");
        assert_eq!(controller.rows.row_data(0).unwrap().name, "Code");
        assert_eq!(controller.rows.row_data(1).unwrap().name, "Code Studio");
    }

    #[test]
    fn calculation_is_first_and_activation_copies_only_the_current_result() {
        i_slint_backend_testing::init_no_event_loop();
        let dir = file_search::test_support::TempDir::new();
        let ui = LauncherWindow::new().unwrap();
        let controller = controller(&ui, &dir.0);
        let (clipboard, copies) = clipboard::History::copy_channel();
        *controller.clipboard.borrow_mut() = clipboard;
        *controller.apps.borrow_mut() = vec![apps::Application {
            name: "1+2*3".into(),
            detail: String::new(),
            keywords: String::new(),
            icon_name: None,
            target: apps::LaunchTarget::File("/unused".into()),
        }];
        *controller.icons.borrow_mut() = vec![slint::Image::default()];
        controller.update_results("1+2*3");
        assert_eq!(controller.rows.row_data(0).unwrap().name, "7");
        assert_eq!(controller.rows.row_data(1).unwrap().name, "1+2*3");
        assert_eq!(ui.get_selected_index(), 0);
        controller.apply_icons(vec![(
            0,
            icons::IconData {
                pixels: slint::SharedPixelBuffer::clone_from_slice(&[255, 0, 0, 255], 1, 1),
                premultiplied: false,
            },
        )]);
        assert!(!controller.rows.row_data(0).unwrap().has_icon);
        assert!(controller.rows.row_data(1).unwrap().has_icon);
        controller.launch(0, false);
        assert_eq!(
            copies.try_recv().unwrap(),
            clipboard::Entry::Text("7".into())
        );
        assert!(!ui.get_launching());
        assert!(!dir.0.join("launch-counts.json").exists());
        controller.update_results("0.1+0.2");
        controller.launch(0, false);
        assert_eq!(
            copies.try_recv().unwrap(),
            clipboard::Entry::Text("0.3".into())
        );
        controller.update_results("1/0");
        assert_eq!(controller.rows.row_count(), 0);
        controller.launch(0, false);
        assert!(copies.try_recv().is_err());
        assert!(controller.calculation.borrow().is_none());
    }

    #[test]
    fn calculation_copy_failure_is_visible_to_the_user() {
        i_slint_backend_testing::init_no_event_loop();
        let dir = file_search::test_support::TempDir::new();
        let ui = LauncherWindow::new().unwrap();
        let controller = controller(&ui, &dir.0);
        controller.update_results("2+2");
        controller.launch(0, false);
        assert!(!ui.get_error_message().is_empty());
        assert!(!ui.get_launching());
    }
}
