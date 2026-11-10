use anyhow::{Context, Result, ensure};
use std::process::{Command, Stdio};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Shutdown,
    Restart,
}

impl Action {
    pub fn from_query(query: &str) -> Option<Self> {
        let query = query.trim();
        if query.eq_ignore_ascii_case("shutdown") {
            Some(Self::Shutdown)
        } else if query.eq_ignore_ascii_case("restart") {
            Some(Self::Restart)
        } else {
            None
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::Shutdown => "关机",
            Self::Restart => "重启系统",
        }
    }

    fn command(self) -> Command {
        #[cfg(target_os = "windows")]
        {
            use std::os::windows::process::CommandExt;
            let mut command = Command::new("shutdown.exe");
            command.args([
                match self {
                    Self::Shutdown => "/s",
                    Self::Restart => "/r",
                },
                "/t",
                "0",
            ]);
            command.creation_flags(0x08000000); // CREATE_NO_WINDOW
            command
        }
        #[cfg(target_os = "macos")]
        {
            let mut command = Command::new("/usr/bin/osascript");
            command.args([
                "-e",
                match self {
                    Self::Shutdown => "tell application \"System Events\" to shut down",
                    Self::Restart => "tell application \"System Events\" to restart",
                },
            ]);
            command
        }
        #[cfg(target_os = "linux")]
        {
            let mut command = Command::new("systemctl");
            command.arg(match self {
                Self::Shutdown => "poweroff",
                Self::Restart => "reboot",
            });
            command
        }
        #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
        unreachable!("当前系统尚未实现电源操作")
    }

    /// Run only after the user activates the result, on a worker thread.
    pub fn execute(self) -> Result<()> {
        let output = self
            .command()
            .stdin(Stdio::null())
            .output()
            .context("无法运行系统电源命令")?;
        ensure!(
            output.status.success(),
            "系统电源命令返回 {}：{}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_complete_power_keywords_match() {
        assert_eq!(Action::from_query(" SHUTDOWN "), Some(Action::Shutdown));
        assert_eq!(Action::from_query("Restart"), Some(Action::Restart));
        for query in ["", "shut", "restart app", "shutdown /s"] {
            assert_eq!(Action::from_query(query), None);
        }
    }

    #[test]
    fn power_commands_use_fixed_arguments_without_executing() {
        for action in [Action::Shutdown, Action::Restart] {
            let command = action.command();
            let args: Vec<_> = command.get_args().collect();
            #[cfg(target_os = "linux")]
            {
                assert_eq!(command.get_program(), "systemctl");
                assert_eq!(
                    args,
                    [if action == Action::Shutdown {
                        "poweroff"
                    } else {
                        "reboot"
                    }]
                );
            }
            #[cfg(target_os = "windows")]
            {
                assert_eq!(command.get_program(), "shutdown.exe");
                assert_eq!(
                    args,
                    [
                        if action == Action::Shutdown {
                            "/s"
                        } else {
                            "/r"
                        },
                        "/t",
                        "0"
                    ]
                );
            }
            #[cfg(target_os = "macos")]
            {
                assert_eq!(command.get_program(), "/usr/bin/osascript");
                assert_eq!(
                    args,
                    [
                        "-e",
                        if action == Action::Shutdown {
                            "tell application \"System Events\" to shut down"
                        } else {
                            "tell application \"System Events\" to restart"
                        }
                    ]
                );
            }
        }
    }
}
