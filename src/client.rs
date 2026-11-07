use crate::{
    options::Options,
    server::{self, Command},
};
use anyhow::{Context, Result, bail};
use std::{
    path::Path,
    process::{Child, Stdio},
    time::{Duration, Instant},
};

pub fn run(options: &Options) -> Result<()> {
    let directory = server::runtime_directory()?;
    if options.status {
        if !server::is_running(&directory)? {
            println!("server 未运行");
        } else {
            server::wait_request(&directory, Command::Ping, Duration::from_secs(3))?;
            println!("server 正在运行");
        }
        return Ok(());
    }
    let mut child: Option<Child> = None;
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let result = server::request(&directory, options.command);
        match &result {
            Ok(true) => {
                if options.command == Command::Quit {
                    while server::is_running(&directory)? {
                        if Instant::now() >= deadline {
                            bail!("server 未能及时退出");
                        }
                        std::thread::sleep(Duration::from_millis(30));
                    }
                }
                return Ok(());
            }
            Ok(false) if options.autostart() && child.is_none() => {
                child = Some(spawn(options, &directory)?)
            }
            Ok(false) if !options.autostart() => {
                println!("server 未运行");
                return Ok(());
            }
            _ => {}
        }
        if let Some(child) = &mut child
            && let Some(status) = child.try_wait()?
            && !status.success()
        {
            bail!(
                "后台 server 启动失败（{status}），详情见 {}",
                directory.join("server.log").display()
            );
        }
        if Instant::now() >= deadline {
            let error = result
                .err()
                .map(|e| format!("{e:#}"))
                .unwrap_or_else(|| "server 未就绪".into());
            bail!("{error}；详情见 {}", directory.join("server.log").display());
        }
        std::thread::sleep(Duration::from_millis(40));
    }
}

fn spawn(options: &Options, directory: &Path) -> Result<Child> {
    server::prepare_directory(directory)?;
    let log = server::private_file(&directory.join("server.log"), true)?;
    let mut command = std::process::Command::new(std::env::current_exe()?);
    command.args(["--server", "--hotkey", &options.hotkey]);
    if options.no_hotkey {
        command.arg("--no-hotkey");
    }
    command
        .env("TAKE_A_PANEL_RUNTIME_DIR", directory)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // setsid is async-signal-safe and detaches the child's terminal session.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x00000008 | 0x00000200); // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP
    }
    command.spawn().context("无法启动后台 server")
}
