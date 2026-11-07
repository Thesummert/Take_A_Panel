use crate::{
    apps::{Application, LaunchTarget},
    settings::Settings,
};
use std::{cmp::Reverse, collections::BTreeMap, path::PathBuf};

#[derive(Clone, Default)]
pub struct History {
    counts: BTreeMap<String, u64>,
    settings: Option<Settings>,
}

fn key(app: &Application) -> String {
    match &app.target {
        LaunchTarget::File(path) => format!("file:{}", path.to_string_lossy()),
        #[cfg(target_os = "windows")]
        LaunchTarget::AppId(id) => format!("app-id:{id}"),
    }
}

impl History {
    pub fn load(directory: PathBuf) -> Self {
        let settings = Settings::new(directory);
        let counts = settings.load_launch_counts().unwrap_or_else(|error| {
            eprintln!("无法加载应用启动次数：{error:#}");
            BTreeMap::new()
        });
        Self {
            counts,
            settings: Some(settings),
        }
    }

    // Called on the launch worker only after the OS launch request succeeds.
    pub fn record(&mut self, app: &Application) {
        let count = self.counts.entry(key(app)).or_default();
        *count = count.saturating_add(1);
        if let Some(settings) = &self.settings
            && let Err(error) = settings.save_launch_counts(&self.counts)
        {
            eprintln!("无法保存应用启动次数：{error:#}");
        }
    }

    pub fn sort(&self, apps: &[Application], matches: &mut [usize]) {
        // Stable sorting retains the catalog's name order for equal counts.
        matches.sort_by_cached_key(|&index| {
            Reverse(self.counts.get(&key(&apps[index])).copied().unwrap_or(0))
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(name: &str, path: &str) -> Application {
        Application {
            name: name.into(),
            detail: String::new(),
            keywords: String::new(),
            icon_name: None,
            target: LaunchTarget::File(path.into()),
        }
    }

    #[test]
    fn counts_survive_reload_and_follow_targets_across_catalog_changes() {
        let dir = crate::file_search::test_support::TempDir::new();
        let alpha = app("Alpha", "/apps/alpha");
        let beta = app("Beta", "/apps/beta");
        let mut history = History::load(dir.0.clone());
        history.record(&alpha);
        history.record(&beta);
        history.record(&beta);

        let history = History::load(dir.0.clone());
        let apps = vec![
            app("Alpha", "/apps/another-alpha"),
            app("Beta renamed", "/apps/beta"),
            app("Delta", "/apps/delta"),
            app("Gamma", "/apps/alpha"),
        ];
        let mut indices = vec![0, 1, 2, 3];
        history.sort(&apps, &mut indices);
        assert_eq!(indices, vec![1, 3, 0, 2]);
    }

    #[test]
    fn missing_or_invalid_history_keeps_all_apps_and_can_save_new_counts() {
        let dir = crate::file_search::test_support::TempDir::new();
        let apps = vec![app("Alpha", "/a"), app("Beta", "/b")];
        let mut indices = vec![0, 1];
        History::load(dir.0.clone()).sort(&apps, &mut indices);
        assert_eq!(indices, vec![0, 1]);
        std::fs::write(dir.0.join("launch-counts.json"), "invalid").unwrap();
        let mut history = History::load(dir.0.clone());
        history.record(&apps[1]);
        History::load(dir.0.clone()).sort(&apps, &mut indices);
        assert_eq!(indices, vec![1, 0]);
    }

    #[test]
    fn maximum_count_does_not_overflow() {
        let mut history = History::default();
        let app = app("Alpha", "/a");
        history.counts.insert(key(&app), u64::MAX);
        history.record(&app);
        assert_eq!(history.counts[&key(&app)], u64::MAX);
    }
}
