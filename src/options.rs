use crate::server::Command;
use anyhow::{Context, Result, bail};

pub const DEFAULT_HOTKEY: &str = "Ctrl+Alt+Space";

#[derive(Debug)]
pub struct Options {
    pub hotkey: String,
    pub list: bool,
    pub no_hotkey: bool,
    pub server: bool,
    pub status: bool,
    pub command: Command,
}

impl Options {
    pub fn parse() -> Result<Option<Self>> {
        let hotkey = std::env::var("TAKE_A_PANEL_HOTKEY")
            .ok()
            .unwrap_or_else(|| {
                match crate::settings::directory()
                    .and_then(|path| crate::settings::Settings::new(path).load_hotkey())
                {
                    Ok(Some(hotkey)) => hotkey,
                    Ok(None) => DEFAULT_HOTKEY.into(),
                    Err(error) => {
                        eprintln!("无法加载快捷键设置，使用默认值：{error:#}");
                        DEFAULT_HOTKEY.into()
                    }
                }
            });
        Self::parse_args(std::env::args().skip(1), hotkey)
    }

    fn parse_args(args: impl IntoIterator<Item = String>, hotkey: String) -> Result<Option<Self>> {
        let mut options = Self {
            hotkey,
            list: false,
            no_hotkey: false,
            server: false,
            status: false,
            command: Command::Show,
        };
        let mut command_supplied = false;
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--hotkey" => options.hotkey = args.next().context("--hotkey 需要一个组合键")?,
                "--no-hotkey" => options.no_hotkey = true,
                "--server" => options.server = true,
                "--list" => options.list = true,
                "--help" | "-h" => {
                    println!("{HELP}");
                    return Ok(None);
                }
                "--show" | "--hide" | "--toggle" | "--refresh" | "--quit" | "--status"
                | "--hidden" => {
                    if command_supplied {
                        bail!("一次只能指定一个 server 操作");
                    }
                    command_supplied = true;
                    options.command = match arg.as_str() {
                        "--hide" => Command::Hide,
                        "--toggle" => Command::Toggle,
                        "--refresh" => Command::Refresh,
                        "--quit" => Command::Quit,
                        "--status" | "--hidden" => Command::Ping,
                        _ => Command::Show,
                    };
                    options.status = arg == "--status";
                }
                _ => bail!("未知参数: {arg}；使用 --help 查看帮助"),
            }
        }
        if options.server && (command_supplied || options.list) || options.list && command_supplied
        {
            bail!("--server、--list 和客户端操作不能同时指定");
        }
        Ok(Some(options))
    }

    pub fn autostart(&self) -> bool {
        !self.status
            && matches!(
                self.command,
                Command::Show | Command::Toggle | Command::Ping
            )
    }
}

const HELP: &str = "Take a Panel — Rust + Slint 应用启动器

用法: take-a-panel [选项]
默认自动启动或复用后台 server，然后显示面板。

  --server          在当前进程运行 server，不打开面板
  --hidden          自动启动或复用后台 server，不打开面板
  --show            显示面板（默认）
  --hide            隐藏面板
  --toggle          显示 / 隐藏面板
  --refresh         请求 server 刷新应用列表
  --quit            退出后台 server
  --status          查询 server 状态
  --hotkey <组合键>  覆盖已保存的快捷键（初始默认 Ctrl+Alt+Space）
  --no-hotkey       server 启动时不注册快捷键
  --list            直接列出应用，不启动 server
  --help            显示帮助

server 常驻状态栏 P 菜单，可在“设置 → 快捷键设置”中修改并保存快捷键。
已有 server 时复用其快捷键设置。优先级：--hotkey > TAKE_A_PANEL_HOTKEY > 已保存设置 > 默认值。
窗口内: ↑/↓ 选择，Enter 启动，Shift+Enter 新开实例，Esc 隐藏，Ctrl+R 刷新，Ctrl+Q 退出。";

#[cfg(test)]
mod tests {
    use super::*;
    fn parse(args: &[&str]) -> Result<Options> {
        Ok(Options::parse_args(
            args.iter().map(|arg| (*arg).to_owned()),
            DEFAULT_HOTKEY.into(),
        )?
        .unwrap())
    }
    #[test]
    fn distinguishes_server_background_start_and_control_only_commands() {
        assert_eq!(parse(&[]).unwrap().command, Command::Show);
        assert!(parse(&["--server", "--no-hotkey"]).unwrap().server);
        assert!(parse(&["--hidden"]).unwrap().autostart());
        for arg in ["--status", "--quit", "--hide", "--refresh"] {
            assert!(!parse(&[arg]).unwrap().autostart());
        }
        assert!(parse(&["--show", "--quit"]).is_err());
        assert!(parse(&["--server", "--show"]).is_err());
        assert!(parse(&["--hotkey"]).is_err());
    }

    #[test]
    fn explicit_hotkey_overrides_the_loaded_default() {
        let options = Options::parse_args(
            ["--hotkey".into(), "Ctrl+Shift+Space".into()],
            "Alt+Space".into(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(options.hotkey, "Ctrl+Shift+Space");
        let options = Options::parse_args([], "Alt+Space".into())
            .unwrap()
            .unwrap();
        assert_eq!(options.hotkey, "Alt+Space");
    }
}
