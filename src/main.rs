#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod apps;
mod client;
mod icons;
mod options;
mod search;
mod server;
mod window_focus;

use anyhow::{Context, Result};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState, hotkey::HotKey};
use options::Options;
use slint::{
    ComponentHandle, Model, ModelRc, VecModel,
    winit_030::{EventResult, WinitWindowAccessor, winit},
};
use std::{
    cell::RefCell,
    rc::Rc,
    sync::{Arc, mpsc},
    time::Duration,
};
use window_focus::WindowFocus;

slint::include_modules!();

enum WorkerEvent {
    Catalog(apps::Catalog),
    Icons(Vec<(usize, icons::IconData)>),
    ScanFinished,
    Launched(Result<(), String>),
}

struct Controller {
    ui: slint::Weak<LauncherWindow>,
    apps: RefCell<Vec<apps::Application>>,
    matches: RefCell<Vec<usize>>,
    icons: RefCell<Vec<slint::Image>>,
    rows: Rc<VecModel<AppRow>>,
    sender: mpsc::Sender<WorkerEvent>,
    receiver: mpsc::Receiver<WorkerEvent>,
    timer: slint::Timer,
    hotkey_notice: String,
    focus: Arc<WindowFocus>,
}

impl Controller {
    fn new(ui: &LauncherWindow, hotkey_notice: String, focus: Arc<WindowFocus>) -> Rc<Self> {
        let (sender, receiver) = mpsc::channel();
        let controller = Rc::new(Self {
            ui: ui.as_weak(),
            apps: RefCell::new(Vec::new()),
            matches: RefCell::new(Vec::new()),
            icons: RefCell::new(Vec::new()),
            rows: Rc::new(VecModel::default()),
            sender,
            receiver,
            timer: slint::Timer::default(),
            hotkey_notice,
            focus,
        });
        ui.set_results(ModelRc::from(controller.rows.clone()));
        controller
    }

    fn apply_icons(&self, batch: Vec<(usize, icons::IconData)>) {
        let mut icons = self.icons.borrow_mut();
        let matches = self.matches.borrow();
        for (index, icon) in batch {
            let Some(cached) = icons.get_mut(index) else {
                continue;
            };
            *cached = icon.into_image();
            if let Some(row) = matches.iter().position(|&app_index| app_index == index)
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
        let apps = self.apps.borrow();
        let matches = search::search(&apps, query);
        let colors = [0xb7a4f4, 0x84c7b5, 0xe4b77b, 0x88b7e4, 0xdc9fb8];
        let rows = matches
            .iter()
            .map(|&index| {
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
                    icon,
                }
            })
            .collect::<Vec<_>>();
        *self.matches.borrow_mut() = matches;
        self.rows.set_vec(rows);
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

    fn launch(self: &Rc<Self>, row: i32, new_instance: bool) {
        let Some(ui) = self.ui.upgrade() else {
            return;
        };
        if ui.get_launching() {
            return;
        }
        let app = usize::try_from(row)
            .ok()
            .and_then(|row| self.matches.borrow().get(row).copied())
            .and_then(|index| self.apps.borrow().get(index).cloned());
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
        std::thread::spawn(move || {
            let result = apps::launch(&app, mode)
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
                            let notices = [&controller.hotkey_notice, &scan_notice]
                                .into_iter()
                                .filter(|s| !s.is_empty())
                                .cloned()
                                .collect::<Vec<_>>()
                                .join(" · ");
                            ui.set_notice(notices.into());
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
                            if let Err(error) = result {
                                eprintln!("{error}");
                                present(&ui, false, &controller.focus);
                                ui.set_error_message(error.into());
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

fn present(ui: &LauncherWindow, reset: bool, focus: &WindowFocus) {
    focus.begin_presentation();
    if reset {
        ui.invoke_reset_search();
    }
    ui.set_error_message("".into());
    ui.window().set_minimized(false);
    if let Err(error) = ui.show() {
        eprintln!("无法显示窗口: {error}");
        return;
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
) -> (Option<GlobalHotKeyManager>, String) {
    if options.no_hotkey {
        return (None, "全局快捷键已禁用；可从 P 菜单或命令行唤出".into());
    }
    #[cfg(target_os = "linux")]
    if std::env::var_os("WAYLAND_DISPLAY").is_some()
        || std::env::var("XDG_SESSION_TYPE").is_ok_and(|v| v == "wayland")
    {
        return (
            None,
            "Wayland 暂不支持全局快捷键；可从 P 菜单或命令行唤出".into(),
        );
    }
    let registration = (|| -> Result<_> {
        let hotkey: HotKey = options.hotkey.parse().context("快捷键格式错误")?;
        let manager = GlobalHotKeyManager::new().context("无法初始化全局快捷键")?;
        manager
            .register(hotkey)
            .context("无法注册全局快捷键，可能已被其他程序占用")?;
        let weak = ui.as_weak();
        GlobalHotKeyEvent::set_event_handler(Some(move |event: GlobalHotKeyEvent| {
            if event.id != hotkey.id() || event.state != HotKeyState::Pressed {
                return;
            }
            let focus = focus.clone();
            let _ = weak.upgrade_in_event_loop(move |ui| {
                if ui.window().is_visible() {
                    let _ = ui.hide();
                } else {
                    present(&ui, true, &focus);
                }
            });
        }));
        Ok(manager)
    })();
    match registration {
        Ok(manager) => (Some(manager), String::new()),
        Err(error) => {
            let message = format!("{error:#}");
            eprintln!("{message}");
            (None, format!("{message}；可从 P 菜单或命令行唤出"))
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
    run_server(options, server)
}

fn run_server(options: Options, mut server: server::Server) -> Result<()> {
    let backend = slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name("software".into())
        .with_winit_window_attributes_hook(|attributes| {
            attributes.with_transparent(true).with_resizable(false)
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
    let focus = Arc::new(WindowFocus::default());
    let (hotkey_manager, hotkey_notice) = register_hotkey(&ui, &options, focus.clone());
    let has_hotkey = hotkey_manager.is_some();
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
    ui.on_query_edited(move |query| {
        if let Some(c) = weak.upgrade() {
            c.update_results(query.as_str());
        }
    });
    let weak = Rc::downgrade(&controller);
    ui.on_launch_selected(move |index, new_instance| {
        if let Some(c) = weak.upgrade() {
            c.launch(index, new_instance);
        }
    });
    let weak = Rc::downgrade(&controller);
    ui.on_refresh(move || {
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
    ui.window().on_winit_window_event(move |window, event| {
        if let winit::event::WindowEvent::Focused(focused) = event {
            let should_hide = window_focus.changed(*focused);
            if should_hide && window.is_visible() {
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
            ui.invoke_refresh();
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
                server::Command::Refresh => ui.invoke_refresh(),
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
    drop(tray);
    drop(hotkey_manager);
    // Release ownership only after the tray and global shortcut are gone.
    drop(server);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn late_icon_batches_update_the_matching_filtered_row_and_are_cached() {
        i_slint_backend_testing::init_no_event_loop();
        let ui = LauncherWindow::new().unwrap();
        let controller = Controller::new(&ui, String::new(), Arc::default());
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
}
