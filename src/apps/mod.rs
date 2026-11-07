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
    use std::{
        io::{Read, Seek, SeekFrom},
        process::Stdio,
    };

    // Launched applications can inherit the helper's stdout/stderr. Capturing
    // pipes with output() would wait for those applications to close them, even
    // after gio has exited, leaving the panel stuck in its launching state.
    // A temporary file retains diagnostics without waiting for pipe EOF.
    let mut stderr = tempfile::tempfile().context("无法创建启动错误日志")?;
    let status = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(stderr.try_clone().context("无法打开启动错误日志")?)
        .status()
        .context("无法运行系统启动程序")?;
    if !status.success() {
        stderr.seek(SeekFrom::Start(0))?;
        let mut diagnostics = Vec::new();
        // The application may still be writing to the same file. Read only a
        // bounded snapshot, rather than collecting an unbounded application log.
        stderr.take(16 * 1024).read_to_end(&mut diagnostics)?;
        anyhow::bail!(
            "系统启动程序返回 {}: {}",
            status,
            String::from_utf8_lossy(&diagnostics).trim()
        );
    }
    Ok(())
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod tests {
    use super::run_helper;
    use std::{process::Command, sync::mpsc, time::Duration};

    #[test]
    fn helper_returns_while_launched_app_still_holds_stdout_and_stderr() {
        let directory = tempfile::tempdir().unwrap();
        let pid_file = directory.path().join("app.pid");
        let mut command = Command::new("/bin/sh");
        command
            .arg("-c")
            .arg("sleep 10 & echo $! > \"$1\"")
            .arg("launch-test")
            .arg(&pid_file);
        let (sender, receiver) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            sender.send(run_helper(&mut command)).unwrap();
        });
        let result = receiver.recv_timeout(Duration::from_secs(2));
        // Clean up the simulated application even if the helper regresses and
        // waits for its inherited pipes to close.
        let pid = std::fs::read_to_string(pid_file).unwrap();
        let alive = Command::new("kill")
            .args(["-0", pid.trim()])
            .status()
            .unwrap();
        Command::new("kill")
            .args(["-TERM", pid.trim()])
            .status()
            .unwrap();
        worker.join().unwrap();
        assert!(alive.success(), "the simulated application must still run");
        result
            .expect("must wait for the helper, not the application")
            .unwrap();
    }

    #[test]
    fn helper_preserves_failure_status_and_stderr() {
        let error = run_helper(
            Command::new("/bin/sh").args(["-c", "printf 'launch failed\\n' >&2; exit 7"]),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains('7'), "{error}");
        assert!(error.contains("launch failed"), "{error}");
    }

    #[test]
    fn helper_handles_large_diagnostics_without_blocking() {
        let error = run_helper(
            Command::new("/bin/sh").args(["-c", "head -c 131072 /dev/zero >&2; exit 1"]),
        )
        .unwrap_err()
        .to_string();
        assert!(error.len() < 17 * 1024);
    }

    #[test]
    fn helper_reports_spawn_failure() {
        let error =
            run_helper(&mut Command::new("/nonexistent/take-a-panel-launch-helper")).unwrap_err();
        assert!(error.to_string().contains("无法运行系统启动程序"));
    }
}
