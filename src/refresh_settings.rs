use crate::{
    Controller,
    settings::{self, RefreshIntervals},
};
use anyhow::Result;
use std::{rc::Rc, time::Duration};

impl Controller {
    pub(crate) fn bind_refresh_settings(self: &Rc<Self>, store: Result<settings::Settings>) {
        let Some(ui) = self.ui.upgrade() else { return };
        let loaded = store
            .as_ref()
            .map_err(|error| anyhow::anyhow!("{error:#}"))
            .and_then(|store| store.load_refresh_intervals());
        let intervals = match loaded {
            Ok(intervals) => intervals,
            Err(error) => {
                ui.set_refresh_settings_failed(true);
                ui.set_refresh_settings_message(
                    format!("读取自动刷新设置失败：{error:#}；已使用默认间隔。").into(),
                );
                RefreshIntervals::default()
            }
        };
        ui.set_app_refresh_draft(intervals.apps_minutes.to_string().into());
        ui.set_file_refresh_draft(intervals.files_minutes.to_string().into());
        self.apply_refresh_intervals(intervals);
        let weak = Rc::downgrade(self);
        ui.on_save_refresh_intervals(move |apps, files| {
            let Some(controller) = weak.upgrade() else {
                return;
            };
            let Some(ui) = controller.ui.upgrade() else {
                return;
            };
            let result = (|| -> Result<RefreshIntervals> {
                let intervals = RefreshIntervals::parse(apps.as_str(), files.as_str())?;
                store
                    .as_ref()
                    .map_err(|error| anyhow::anyhow!("{error:#}"))?
                    .save_refresh_intervals(intervals)?;
                Ok(intervals)
            })();
            ui.set_refresh_settings_failed(result.is_err());
            match result {
                Ok(intervals) => {
                    controller.apply_refresh_intervals(intervals);
                    ui.set_app_refresh_draft(intervals.apps_minutes.to_string().into());
                    ui.set_file_refresh_draft(intervals.files_minutes.to_string().into());
                    ui.set_refresh_settings_message("已保存，自动刷新间隔立即生效。".into());
                }
                Err(error) => {
                    ui.set_refresh_settings_message(format!("保存失败：{error:#}").into())
                }
            }
        });
    }

    fn apply_refresh_intervals(self: &Rc<Self>, intervals: RefreshIntervals) {
        self.app_refresh_timer.stop();
        if intervals.apps_minutes > 0 {
            let weak = Rc::downgrade(self);
            self.app_refresh_timer.start(
                slint::TimerMode::Repeated,
                Duration::from_secs(u64::from(intervals.apps_minutes) * 60),
                move || {
                    if let Some(controller) = weak.upgrade() {
                        // Preserve error messages from other operations during background scans.
                        controller.refresh_apps(false);
                    }
                },
            );
        }
        self.files.set_refresh_interval(intervals.files_minutes);
    }
}
