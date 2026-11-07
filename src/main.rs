#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod apps;
mod search;
mod window_focus;

use anyhow::{Context, Result};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState, hotkey::HotKey};
use slint::{
    ComponentHandle, ModelRc, VecModel,
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

const DEFAULT_HOTKEY: &str = "Ctrl+Alt+Space";

#[derive(Debug)]
struct Options {
    hotkey: String,
    hidden: bool,
    list: bool,
    no_hotkey: bool,
}

impl Options {
    fn parse() -> Result<Option<Self>> {
        let mut options = Self {
            hotkey: std::env::var("TAKE_A_PANEL_HOTKEY").unwrap_or_else(|_| DEFAULT_HOTKEY.into()),
            hidden: false,
            list: false,
            no_hotkey: false,
        };
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--hotkey" => {
                    options.hotkey = args
                        .next()
                        .context("--hotkey 需要一个快捷键，例如 Ctrl+Alt+Space")?
                }
                "--hidden" => options.hidden = true,
                "--list" => options.list = true,
                "--no-hotkey" => options.no_hotkey = true,
                "--help" | "-h" => {
                    println!(
                        "Take a Panel — Rust + Slint 应用启动器\n\n用法: take-a-panel [选项]\n\n  --hotkey <组合键>  设置全局快捷键（默认 Ctrl+Alt+Space）\n  --hidden          启动后隐藏，等待快捷键\n  --no-hotkey       不注册全局快捷键\n  --list            列出已发现应用，不启动界面\n  --help            显示帮助\n\n快捷键也可通过 TAKE_A_PANEL_HOTKEY 环境变量设置。\n窗口内: ↑/↓ 选择，Enter 启动，Esc 隐藏，Ctrl+R 刷新，Ctrl+Q 退出。"
                    );
                    return Ok(None);
                }
                _ => anyhow::bail!("未知参数: {arg}；使用 --help 查看帮助"),
            }
        }
        Ok(Some(options))
    }
}

enum WorkerEvent {
    Catalog(apps::Catalog),
    Launched(Result<(), String>),
}

struct Controller {
    ui: slint::Weak<LauncherWindow>,
    apps: RefCell<Vec<apps::Application>>,
    matches: RefCell<Vec<usize>>,
    sender: mpsc::Sender<WorkerEvent>,
    receiver: mpsc::Receiver<WorkerEvent>,
    timer: slint::Timer,
    hotkey_notice: String,
    has_hotkey: bool,
    focus: Arc<WindowFocus>,
}

impl Controller {
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
                // A deterministic monogram keeps the first version independent of
                // OS-specific icon codecs while retaining a consistent visual style.
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
                }
            })
            .collect::<Vec<_>>();
        *self.matches.borrow_mut() = matches;
        ui.set_results(ModelRc::new(VecModel::from(rows)));
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
            let _ = sender.send(WorkerEvent::Catalog(apps::discover()));
        });
        self.start_pump();
    }

    fn launch(self: &Rc<Self>, row: i32) {
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
        if self.has_hotkey {
            let _ = ui.hide();
        } else {
            ui.window().set_minimized(true);
        }
        let sender = self.sender.clone();
        std::thread::spawn(move || {
            let result =
                apps::launch(&app).map_err(|error| format!("无法打开 {}：{error:#}", app.name));
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
                            *controller.apps.borrow_mut() = catalog.apps;
                            controller.update_results(ui.get_query().as_str());
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
        return (None, "全局快捷键已禁用；Esc 最小化窗口".into());
    }
    #[cfg(target_os = "linux")]
    if std::env::var_os("WAYLAND_DISPLAY").is_some()
        || std::env::var("XDG_SESSION_TYPE").is_ok_and(|v| v == "wayland")
    {
        return (
            None,
            "Wayland 暂不支持全局快捷键；Esc 最小化窗口，可从任务栏恢复".into(),
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
            (None, format!("{message}；Esc 最小化窗口"))
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

    slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name("software".into())
        .with_winit_window_attributes_hook(|attributes| {
            attributes.with_transparent(true).with_resizable(false)
        })
        .select()?;
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
    let (sender, receiver) = mpsc::channel();
    let controller = Rc::new(Controller {
        ui: ui.as_weak(),
        apps: RefCell::new(Vec::new()),
        matches: RefCell::new(Vec::new()),
        sender,
        receiver,
        timer: slint::Timer::default(),
        hotkey_notice,
        has_hotkey,
        focus: focus.clone(),
    });

    let weak = Rc::downgrade(&controller);
    ui.on_query_edited(move |query| {
        if let Some(c) = weak.upgrade() {
            c.update_results(query.as_str());
        }
    });
    let weak = Rc::downgrade(&controller);
    ui.on_launch_selected(move |index| {
        if let Some(c) = weak.upgrade() {
            c.launch(index);
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
            if has_hotkey {
                let _ = ui.hide();
            } else {
                ui.window().set_minimized(true);
            }
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
            if has_hotkey && should_hide && window.is_visible() {
                let _ = window.hide();
            }
        }
        EventResult::Propagate
    });

    // First run is visible for discoverability. --hidden is honored only when
    // the shortcut registered successfully, so registration failures are visible.
    if !options.hidden || !has_hotkey {
        present(&ui, true, &focus);
    }
    ui.set_scanning(false);
    controller.refresh();
    slint::run_event_loop_until_quit()?;
    drop(hotkey_manager);
    Ok(())
}
