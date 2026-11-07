use anyhow::Result;
use std::path::PathBuf;

#[cfg(any(target_os = "linux", test))]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

#[derive(Clone, Debug)]
pub enum LaunchTarget {
    File(PathBuf),
    #[cfg(target_os = "windows")]
    AppId(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaunchMode {
    Normal,
    NewInstance,
}

#[derive(Clone, Debug)]
pub struct Application {
    pub name: String,
    pub detail: String,
    pub keywords: String,
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub icon_name: Option<String>,
    pub target: LaunchTarget,
}

#[derive(Default, Debug)]
pub struct Catalog {
    pub apps: Vec<Application>,
    pub warnings: Vec<String>,
}

pub fn discover() -> Catalog {
    #[cfg(target_os = "macos")]
    let mut catalog = macos::discover();
    #[cfg(target_os = "windows")]
    let mut catalog = windows::discover();
    #[cfg(target_os = "linux")]
    let mut catalog = linux::discover();
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    let mut catalog = Catalog {
        apps: vec![],
        warnings: vec!["当前系统尚未实现应用发现".into()],
    };

    catalog
        .apps
        .sort_by_cached_key(|app| app.name.to_lowercase());
    catalog
}

pub fn launch(app: &Application, mode: LaunchMode) -> Result<()> {
    #[cfg(target_os = "macos")]
    return macos::launch(app, mode);
    // Windows Shell and Linux desktop entries issue another launch request;
    // those applications decide whether to create or reuse an instance.
    #[cfg(not(target_os = "macos"))]
    let _ = mode;
    #[cfg(target_os = "windows")]
    return windows::launch(app);
    #[cfg(target_os = "linux")]
    return linux::launch(app);
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    anyhow::bail!("当前系统尚未实现应用启动: {}", app.name);
}

/// Wait for the short-lived OS launch helper, including its exit status. This is
/// only called on worker threads, and reaps children instead of leaving zombies.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn run_helper(command: &mut std::process::Command) -> Result<()> {
    use anyhow::Context;
    let output = command.output().context("无法运行系统启动程序")?;
    if !output.status.success() {
        anyhow::bail!(
            "系统启动程序返回 {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}
