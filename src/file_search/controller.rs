use super::*;
use crate::{AppRow, LauncherWindow, settings, window_focus::WindowFocus};
use slint::{ComponentHandle, ModelRc, VecModel};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::{Arc, mpsc},
};

enum Event {
    Saved(Result<Vec<PathBuf>>),
    Progress(u64, usize),
    Indexed(u64, Arc<Index>),
    Searched(u64, Matches),
    Opened(Result<()>),
}

struct Query {
    generation: u64,
    index: Arc<Index>,
    text: String,
}

pub struct Controller {
    ui: slint::Weak<LauncherWindow>,
    roots: RefCell<Vec<PathBuf>>,
    index: RefCell<Arc<Index>>,
    matches: RefCell<Vec<usize>>,
    rows: Rc<VecModel<AppRow>>,
    index_revision: Arc<AtomicU64>,
    query_revision: Arc<AtomicU64>,
    query_sender: mpsc::Sender<Query>,
    sender: mpsc::Sender<Event>,
    receiver: mpsc::Receiver<Event>,
    timer: slint::Timer,
    debounce: slint::Timer,
    querying: Cell<bool>,
    opening: Cell<bool>,
    focus: Arc<WindowFocus>,
    settings_directory: Option<PathBuf>,
}

impl Drop for Controller {
    fn drop(&mut self) {
        self.index_revision.fetch_add(1, Ordering::Relaxed);
        self.query_revision.fetch_add(1, Ordering::Relaxed);
    }
}

impl Controller {
    pub fn new(ui: &LauncherWindow, focus: Arc<WindowFocus>) -> Rc<Self> {
        Self::with_settings(ui, focus, settings::directory())
    }

    pub(crate) fn with_settings(
        ui: &LauncherWindow,
        focus: Arc<WindowFocus>,
        directory: Result<PathBuf>,
    ) -> Rc<Self> {
        let (sender, receiver) = mpsc::channel();
        let (query_sender, queries) = mpsc::channel::<Query>();
        let query_revision = Arc::new(AtomicU64::new(0));
        let revision = query_revision.clone();
        let events = sender.clone();
        // One sleeping worker coalesces queries instead of spawning a thread
        // per keystroke. Replaced queries also cancel an in-progress search.
        std::thread::spawn(move || {
            while let Ok(mut query) = queries.recv() {
                while let Ok(next) = queries.try_recv() {
                    query = next;
                }
                if let Some(matches) =
                    search_index(&query.index, &query.text, &revision, query.generation)
                    && events
                        .send(Event::Searched(query.generation, matches))
                        .is_err()
                {
                    break;
                }
            }
        });
        let settings_directory = directory.as_ref().ok().cloned();
        let roots =
            match directory.and_then(|dir| settings::Settings::new(dir).load_file_search_roots()) {
                Ok(text) => {
                    ui.set_file_roots_draft(text.clone().into());
                    let roots: Vec<PathBuf> = text
                        .lines()
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(PathBuf::from)
                        .collect();
                    if roots.iter().any(|path| !path.is_absolute()) {
                        ui.set_file_settings_failed(true);
                        ui.set_file_settings_message(
                            "已保存的索引范围包含相对路径，请修改后重新保存。".into(),
                        );
                        Vec::new()
                    } else {
                        roots
                    }
                }
                Err(error) => {
                    ui.set_file_settings_failed(true);
                    ui.set_file_settings_message(format!("{error:#}").into());
                    Vec::new()
                }
            };
        let controller = Rc::new(Self {
            ui: ui.as_weak(),
            roots: RefCell::new(roots),
            index: RefCell::default(),
            matches: RefCell::default(),
            rows: Rc::new(VecModel::default()),
            index_revision: Arc::new(AtomicU64::new(0)),
            query_revision,
            query_sender,
            sender,
            receiver,
            timer: slint::Timer::default(),
            debounce: slint::Timer::default(),
            querying: Cell::new(false),
            opening: Cell::new(false),
            focus,
            settings_directory,
        });
        let weak = Rc::downgrade(&controller);
        ui.on_save_file_roots(move |draft| {
            if let Some(controller) = weak.upgrade() {
                controller.save_roots(draft.to_string());
            }
        });
        let weak = Rc::downgrade(&controller);
        ui.on_rebuild_file_index(move || {
            if let Some(controller) = weak.upgrade() {
                controller.rebuild();
            }
        });
        controller.rebuild();
        controller
    }

    pub fn enter(self: &Rc<Self>) {
        let Some(ui) = self.ui.upgrade() else {
            return;
        };
        ui.set_file_search_mode(true);
        ui.set_query("".into());
        ui.set_error_message("".into());
        ui.set_results(ModelRc::from(self.rows.clone()));
        self.query("");
        ui.invoke_focus_search();
    }

    pub fn suspend(&self) {
        self.query_revision.fetch_add(1, Ordering::Relaxed);
        self.debounce.stop();
        self.querying.set(false);
        if let Some(ui) = self.ui.upgrade() {
            ui.set_file_querying(false);
        }
    }

    pub fn query(self: &Rc<Self>, text: &str) {
        let Some(ui) = self.ui.upgrade() else {
            return;
        };
        let generation = self.query_revision.fetch_add(1, Ordering::Relaxed) + 1;
        self.matches.borrow_mut().clear();
        self.rows.set_vec(Vec::new());
        ui.set_selected_index(0);
        ui.set_file_match_count(0);
        ui.set_file_querying(true);
        self.querying.set(true);
        let query = Query {
            generation,
            index: self.index.borrow().clone(),
            text: text.into(),
        };
        let sender = self.query_sender.clone();
        self.debounce.start(
            slint::TimerMode::SingleShot,
            Duration::from_millis(60),
            move || {
                let _ = sender.send(Query {
                    generation: query.generation,
                    index: query.index.clone(),
                    text: query.text.clone(),
                });
            },
        );
        self.start_pump();
    }

    fn save_roots(self: &Rc<Self>, draft: String) {
        let Some(ui) = self.ui.upgrade() else {
            return;
        };
        if ui.get_file_settings_busy() || ui.get_file_folder_dialog_open() {
            return;
        }
        ui.set_file_settings_busy(true);
        ui.set_file_settings_failed(false);
        ui.set_file_settings_message("正在检查并保存索引范围…".into());
        let sender = self.sender.clone();
        let directory = self.settings_directory.clone();
        std::thread::spawn(move || {
            let result = (|| {
                let roots = validate_roots(&draft)?;
                settings::Settings::new(directory.context("无法确定文件搜索设置目录")?)
                    .save_file_search_roots(&roots_text(&roots))?;
                Ok(roots)
            })();
            let _ = sender.send(Event::Saved(result));
        });
        self.start_pump();
    }

    pub fn rebuild(self: &Rc<Self>) {
        let Some(ui) = self.ui.upgrade() else {
            return;
        };
        let generation = self.index_revision.fetch_add(1, Ordering::Relaxed) + 1;
        self.suspend();
        *self.index.borrow_mut() = Arc::default();
        self.matches.borrow_mut().clear();
        self.rows.set_vec(Vec::new());
        ui.set_file_count(0);
        ui.set_file_match_count(0);
        let roots = self.roots.borrow().clone();
        ui.set_file_indexing(!roots.is_empty());
        if roots.is_empty() {
            ui.set_file_index_status(Index::default().status(false).into());
            return;
        }
        ui.set_file_index_status("正在建立索引，已发现 0 项…".into());
        let revision = self.index_revision.clone();
        let sender = self.sender.clone();
        std::thread::spawn(move || {
            let index = build_index(&roots, &revision, generation, |count| {
                let _ = sender.send(Event::Progress(generation, count));
            });
            if let Some(index) = index {
                let _ = sender.send(Event::Indexed(generation, Arc::new(index)));
            }
        });
        self.start_pump();
    }

    pub fn open(self: &Rc<Self>, row: i32) {
        let Some(ui) = self.ui.upgrade() else {
            return;
        };
        if ui.get_launching() {
            return;
        }
        let path = usize::try_from(row)
            .ok()
            .and_then(|row| self.matches.borrow().get(row).copied())
            .and_then(|index| {
                self.index
                    .borrow()
                    .entries
                    .get(index)
                    .map(|entry| entry.path.clone())
            });
        let Some(path) = path else {
            return;
        };
        self.opening.set(true);
        ui.set_launching(true);
        ui.set_error_message("".into());
        let _ = ui.hide();
        let sender = self.sender.clone();
        std::thread::spawn(move || {
            let _ = sender.send(Event::Opened(super::open(&path)));
        });
        self.start_pump();
    }

    fn start_pump(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.timer.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(40),
            move || {
                let Some(controller) = weak.upgrade() else {
                    return;
                };
                controller.pump();
            },
        );
    }

    fn pump(self: &Rc<Self>) {
        let Some(ui) = self.ui.upgrade() else {
            return;
        };
        while let Ok(event) = self.receiver.try_recv() {
            match event {
                Event::Saved(result) => {
                    ui.set_file_settings_busy(false);
                    ui.set_file_settings_failed(result.is_err());
                    match result {
                        Ok(roots) => {
                            ui.set_file_roots_draft(roots_text(&roots).into());
                            ui.set_file_settings_message(
                                "索引范围已保存，下次启动继续使用。".into(),
                            );
                            *self.roots.borrow_mut() = roots;
                            self.rebuild();
                        }
                        Err(error) => {
                            ui.set_file_settings_message(format!("保存失败：{error:#}").into())
                        }
                    }
                }
                Event::Progress(generation, count)
                    if generation == self.index_revision.load(Ordering::Relaxed) =>
                {
                    ui.set_file_index_status(format!("正在建立索引，已发现 {count} 项…").into());
                }
                Event::Indexed(generation, index)
                    if generation == self.index_revision.load(Ordering::Relaxed) =>
                {
                    ui.set_file_indexing(false);
                    ui.set_file_count(index.entries.len() as i32);
                    ui.set_file_index_status(index.status(true).into());
                    *self.index.borrow_mut() = index;
                    if ui.get_file_search_mode() {
                        self.query(ui.get_query().as_str());
                    }
                }
                Event::Searched(generation, matches)
                    if generation == self.query_revision.load(Ordering::Relaxed)
                        && ui.get_file_search_mode() =>
                {
                    self.querying.set(false);
                    ui.set_file_querying(false);
                    ui.set_file_match_count(matches.total as i32);
                    let index = self.index.borrow();
                    let rows: Vec<_> = matches
                        .indices
                        .iter()
                        .map(|&position| {
                            let entry = &index.entries[position];
                            AppRow {
                                name: entry.name.as_str().into(),
                                detail: format!(
                                    "{} · {}",
                                    if entry.directory {
                                        "文件夹"
                                    } else {
                                        "文件"
                                    },
                                    entry.path.display()
                                )
                                .into(),
                                monogram: if entry.directory { "夹" } else { "文" }.into(),
                                tint: slint::Color::from_rgb_u8(132, 199, 181),
                                ..Default::default()
                            }
                        })
                        .collect();
                    *self.matches.borrow_mut() = matches.indices;
                    self.rows.set_vec(rows);
                }
                Event::Opened(result) => {
                    self.opening.set(false);
                    ui.set_launching(false);
                    if let Err(error) = result {
                        crate::present(&ui, false, &self.focus);
                        ui.set_error_message(format!("{error:#}").into());
                    }
                }
                _ => {}
            }
        }
        if !ui.get_file_indexing()
            && !ui.get_file_settings_busy()
            && !self.querying.get()
            && !self.opening.get()
        {
            self.timer.stop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file_search::test_support::TempDir;
    use slint::Model;

    fn wait(controller: &Rc<Controller>, condition: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(100));
            controller.pump();
            if condition() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "background file operation timed out"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn saves_scope_indexes_on_reload_and_preserves_previous_scope_after_invalid_save() {
        i_slint_backend_testing::init_no_event_loop();
        let dir = TempDir::new();
        let root = dir.0.join("documents");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("报告.txt"), "example").unwrap();
        let settings_dir = dir.0.join("config");
        let settings = settings::Settings::new(settings_dir.clone());
        let ui = LauncherWindow::new().unwrap();
        let controller = Controller::with_settings(&ui, Arc::default(), Ok(settings_dir.clone()));
        assert_eq!(ui.get_file_count(), 0);
        controller.enter();
        ui.invoke_save_file_roots(root.to_str().unwrap().into());
        wait(&controller, || {
            !ui.get_file_settings_busy() && !ui.get_file_indexing() && !ui.get_file_querying()
        });
        assert_eq!(ui.get_file_count(), 2);
        let folder = ui.get_results().row_data(0).unwrap();
        assert_eq!(folder.name, "documents");
        assert!(folder.detail.starts_with("文件夹 · "));
        let file = ui.get_results().row_data(1).unwrap();
        assert_eq!(file.name, "报告.txt");
        assert!(file.detail.starts_with("文件 · "));
        let saved = settings.load_file_search_roots().unwrap();
        assert_eq!(saved.trim(), root.canonicalize().unwrap().to_str().unwrap());
        ui.invoke_save_file_roots(root.join("missing").to_str().unwrap().into());
        wait(&controller, || !ui.get_file_settings_busy());
        assert!(ui.get_file_settings_failed());
        assert_eq!(settings.load_file_search_roots().unwrap(), saved);
        assert_eq!(ui.get_file_count(), 2);
        assert_eq!(ui.get_results().row_data(1).unwrap().name, "报告.txt");

        let second = LauncherWindow::new().unwrap();
        let reloaded = Controller::with_settings(&second, Arc::default(), Ok(settings_dir));
        wait(&reloaded, || !second.get_file_indexing());
        assert_eq!(second.get_file_count(), 2);
        ui.invoke_save_file_roots("".into());
        wait(&controller, || {
            !ui.get_file_settings_busy() && !ui.get_file_indexing()
        });
        assert_eq!(ui.get_file_count(), 0);
        assert_eq!(ui.get_results().row_count(), 0);
        assert!(settings.load_file_search_roots().unwrap().trim().is_empty());
    }

    #[test]
    fn switching_queries_and_scopes_rejects_queued_stale_results() {
        i_slint_backend_testing::init_no_event_loop();
        let dir = TempDir::new();
        let root = dir.0.join("first");
        let next = dir.0.join("second");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&next).unwrap();
        std::fs::write(root.join("first.txt"), "example").unwrap();
        std::fs::write(next.join("second.txt"), "example").unwrap();
        let ui = LauncherWindow::new().unwrap();
        let controller = Controller::with_settings(&ui, Arc::default(), Ok(dir.0.join("config")));
        ui.invoke_save_file_roots(root.to_str().unwrap().into());
        wait(&controller, || {
            !ui.get_file_settings_busy() && !ui.get_file_indexing()
        });
        controller.enter();
        controller.query("first");
        let old_query = controller.query_revision.load(Ordering::Relaxed);
        controller.query("no match");
        controller
            .sender
            .send(Event::Searched(
                old_query,
                Matches {
                    indices: vec![0],
                    total: 1,
                },
            ))
            .unwrap();
        wait(&controller, || !ui.get_file_querying());
        assert_eq!(ui.get_results().row_count(), 0);
        let old_index = controller.index.borrow().clone();
        let old_generation = controller.index_revision.load(Ordering::Relaxed);
        ui.set_query("second".into());
        ui.invoke_save_file_roots(next.to_str().unwrap().into());
        wait(&controller, || {
            !ui.get_file_settings_busy() && !ui.get_file_indexing() && !ui.get_file_querying()
        });
        controller
            .sender
            .send(Event::Indexed(old_generation, old_index))
            .unwrap();
        controller.pump();
        assert_eq!(ui.get_results().row_count(), 2);
        assert_eq!(ui.get_results().row_data(0).unwrap().name, "second");
        assert_eq!(ui.get_results().row_data(1).unwrap().name, "second.txt");
        assert!(
            controller
                .index
                .borrow()
                .entries
                .iter()
                .all(|entry| entry.path.starts_with(next.canonicalize().unwrap()))
        );
    }
}
