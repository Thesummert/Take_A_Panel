use anyhow::{Context, Result, bail};
use std::{
    fs::{File, OpenOptions, TryLockError},
    io::{BufRead, BufReader, Read, Write},
    net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const PROTOCOL: &str = "TAKE_A_PANEL/1";
const IO_TIMEOUT: Duration = Duration::from_millis(750);
const MAX_MESSAGE: u64 = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    Ping,
    Show,
    Hide,
    Toggle,
    Refresh,
    Quit,
}

impl Command {
    fn wire(self) -> &'static str {
        match self {
            Self::Ping => "PING",
            Self::Show => "SHOW",
            Self::Hide => "HIDE",
            Self::Toggle => "TOGGLE",
            Self::Refresh => "REFRESH",
            Self::Quit => "QUIT",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        [
            Self::Ping,
            Self::Show,
            Self::Hide,
            Self::Toggle,
            Self::Refresh,
            Self::Quit,
        ]
        .into_iter()
        .find(|command| command.wire() == value)
    }
}

struct Endpoint {
    pid: u32,
    port: u16,
    token: String,
}

impl Endpoint {
    fn read(directory: &Path) -> Result<Self> {
        let mut value = String::new();
        File::open(directory.join("server.endpoint"))?
            .take(MAX_MESSAGE)
            .read_to_string(&mut value)?;
        let fields: Vec<_> = value.split_whitespace().collect();
        if fields.len() != 4
            || fields[0] != PROTOCOL
            || fields[3].len() != 64
            || !fields[3].bytes().all(|b| b.is_ascii_hexdigit())
        {
            bail!("server 端点尚未就绪");
        }
        Ok(Self {
            pid: fields[1].parse()?,
            port: fields[2].parse()?,
            token: fields[3].into(),
        })
    }

    fn address(&self) -> SocketAddr {
        (Ipv4Addr::LOCALHOST, self.port).into()
    }
}

pub fn runtime_directory() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("TAKE_A_PANEL_RUNTIME_DIR").filter(|s| !s.is_empty()) {
        return Ok(PathBuf::from(path));
    }
    #[cfg(target_os = "macos")]
    if let Some(home) = std::env::var_os("HOME") {
        return Ok(PathBuf::from(home).join("Library/Caches/TakeAPanel/runtime"));
    }
    #[cfg(target_os = "windows")]
    if let Some(base) = std::env::var_os("LOCALAPPDATA") {
        return Ok(PathBuf::from(base).join("TakeAPanel/runtime"));
    }
    #[cfg(target_os = "linux")]
    {
        for name in ["XDG_RUNTIME_DIR", "XDG_CACHE_HOME"] {
            if let Some(base) = std::env::var_os(name).filter(|s| !s.is_empty()) {
                return Ok(PathBuf::from(base).join("take-a-panel"));
            }
        }
        if let Some(home) = std::env::var_os("HOME") {
            return Ok(PathBuf::from(home).join(".cache/take-a-panel/runtime"));
        }
    }
    bail!("无法确定用户运行目录，请设置 TAKE_A_PANEL_RUNTIME_DIR")
}

pub fn prepare_directory(directory: &Path) -> Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(directory)
        .with_context(|| format!("无法创建运行目录 {}", directory.display()))?;
    Ok(())
}

pub fn private_file(path: &Path, append: bool) -> Result<File> {
    let mut options = OpenOptions::new();
    options
        .create(true)
        .write(true)
        .append(append)
        .truncate(!append);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    Ok(options.open(path)?)
}

fn lock_file(directory: &Path, create: bool) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .create(create)
        .truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(directory.join("server.lock"))
}

pub fn is_running(directory: &Path) -> Result<bool> {
    let file = match lock_file(directory, false) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    match file.try_lock() {
        Ok(()) => Ok(false), // Dropping the handle releases our temporary lock.
        Err(TryLockError::WouldBlock) => Ok(true),
        Err(TryLockError::Error(error)) => Err(error.into()),
    }
}

/// The OS lock elects one owner and is released even if the process crashes.
/// Endpoints are separate from the locked file so Windows clients can read them.
pub struct Server {
    _lock: File,
    listener: Option<TcpListener>,
    endpoint: Endpoint,
    directory: PathBuf,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Server {
    pub fn claim(directory: &Path) -> Result<Option<Self>> {
        prepare_directory(directory)?;
        let lock = lock_file(directory, true)?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => return Ok(None),
            Err(TryLockError::Error(error)) => return Err(error.into()),
        }
        let listener =
            TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).context("无法创建本地 server")?;
        let mut random = [0u8; 32];
        getrandom::fill(&mut random)
            .map_err(|error| anyhow::anyhow!("无法生成 server 凭据: {error}"))?;
        let token = random
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let endpoint = Endpoint {
            pid: std::process::id(),
            port: listener.local_addr()?.port(),
            token,
        };
        Ok(Some(Self {
            _lock: lock,
            listener: Some(listener),
            endpoint,
            directory: directory.to_owned(),
            stop: Arc::new(AtomicBool::new(false)),
            thread: None,
        }))
    }

    pub fn start(
        &mut self,
        handler: impl Fn(Command) -> Result<()> + Send + 'static,
    ) -> Result<()> {
        let listener = self.listener.take().context("server 已启动")?;
        let stop = self.stop.clone();
        let token = self.endpoint.token.clone();
        self.thread = Some(thread::spawn(move || {
            for stream in listener.incoming() {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                let mut stream = match stream {
                    Ok(stream) => stream,
                    Err(_) => break,
                };
                let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
                let _ = stream.set_write_timeout(Some(IO_TIMEOUT));
                let result = (|| -> Result<()> {
                    let request = read_line(&mut stream)?;
                    let command = parse_request(&request, &token).context("无效的 server 请求")?;
                    if command != Command::Ping {
                        handler(command)?;
                    }
                    Ok(())
                })();
                let response = if result.is_ok() {
                    "TAKE_A_PANEL/1 OK\n"
                } else {
                    "TAKE_A_PANEL/1 ERROR\n"
                };
                let _ = stream.write_all(response.as_bytes());
            }
        }));
        let mut file = private_file(&self.directory.join("server.endpoint"), false)?;
        writeln!(
            file,
            "{PROTOCOL} {} {} {}",
            self.endpoint.pid, self.endpoint.port, self.endpoint.token
        )?;
        Ok(())
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            // Wake blocking accept; no timer or polling is used while idle.
            let _ = TcpStream::connect_timeout(&self.endpoint.address(), IO_TIMEOUT);
            let _ = thread.join();
        }
    }
}

fn read_line(stream: &mut TcpStream) -> Result<String> {
    let mut line = String::new();
    BufReader::new(stream.take(MAX_MESSAGE)).read_line(&mut line)?;
    if !line.ends_with('\n') {
        bail!("server 消息不完整或过长");
    }
    Ok(line)
}

fn parse_request(request: &str, token: &str) -> Option<Command> {
    let fields: Vec<_> = request.split_whitespace().collect();
    if fields.len() != 3 || fields[0] != PROTOCOL || fields[1] != token {
        return None;
    }
    Command::parse(fields[2])
}

/// Returns false for a stopped server, including stale endpoint files.
pub fn request(directory: &Path, command: Command) -> Result<bool> {
    if !is_running(directory)? {
        return Ok(false);
    }
    let endpoint = Endpoint::read(directory)?;
    let mut stream = TcpStream::connect_timeout(&endpoint.address(), IO_TIMEOUT)?;
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    writeln!(stream, "{PROTOCOL} {} {}", endpoint.token, command.wire())?;
    if read_line(&mut stream)? != "TAKE_A_PANEL/1 OK\n" {
        bail!("server 拒绝了请求");
    }
    Ok(true)
}

pub fn wait_request(directory: &Path, command: Command, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        let result = request(directory, command);
        if matches!(result, Ok(true)) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return match result {
                Err(error) => Err(error.context("调用后台 server 失败")),
                _ => Err(anyhow::anyhow!("后台 server 未启动")),
            };
        }
        thread::sleep(Duration::from_millis(40));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_owner_routes_commands_rejects_unauthenticated_requests_and_recovers() {
        let directory =
            std::env::temp_dir().join(format!("take-a-panel-server-test-{}", std::process::id()));
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut server = Server::claim(&directory).unwrap().unwrap();
        assert!(Server::claim(&directory).unwrap().is_none());
        server
            .start(move |command| {
                sender.send(command)?;
                Ok(())
            })
            .unwrap();
        assert!(request(&directory, Command::Ping).unwrap());
        assert!(request(&directory, Command::Show).unwrap());
        assert_eq!(receiver.recv_timeout(IO_TIMEOUT).unwrap(), Command::Show);
        let mut invalid = TcpStream::connect(server.endpoint.address()).unwrap();
        invalid.set_read_timeout(Some(IO_TIMEOUT)).unwrap();
        writeln!(invalid, "{PROTOCOL} wrong-token QUIT").unwrap();
        assert_eq!(read_line(&mut invalid).unwrap(), "TAKE_A_PANEL/1 ERROR\n");
        assert!(receiver.try_recv().is_err());
        assert!(request(&directory, Command::Refresh).unwrap());
        assert_eq!(receiver.recv_timeout(IO_TIMEOUT).unwrap(), Command::Refresh);
        drop(server);
        assert!(!request(&directory, Command::Show).unwrap());
        let mut restarted = Server::claim(&directory).unwrap().unwrap();
        restarted.start(|_| Ok(())).unwrap();
        assert!(request(&directory, Command::Ping).unwrap());
        drop(restarted);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn protocol_rejects_unknown_and_extra_commands() {
        assert_eq!(
            parse_request("TAKE_A_PANEL/1 secret TOGGLE\n", "secret"),
            Some(Command::Toggle)
        );
        for request in [
            "TAKE_A_PANEL/2 secret SHOW\n",
            "TAKE_A_PANEL/1 secret LAUNCH\n",
            "TAKE_A_PANEL/1 secret QUIT extra\n",
        ] {
            assert_eq!(parse_request(request, "secret"), None);
        }
    }
}
