use super::*;
use std::{
    io::{BufRead, BufReader},
    process::{Command, Stdio},
    sync::mpsc,
};

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct Backend {
    pub kind: Kind,
    pub executable: String,
}

#[derive(Clone, Copy, Debug, Default, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    #[default]
    Builtin,
    Rg,
    Fd,
    Everything,
}
impl Kind {
    pub fn from_index(index: i32) -> Result<Self> {
        match index {
            0 => Ok(Self::Builtin),
            1 => Ok(Self::Rg),
            2 => Ok(Self::Fd),
            3 if cfg!(windows) => Ok(Self::Everything),
            _ => bail!("当前系统不支持该搜索后端"),
        }
    }
    pub fn index(self) -> i32 {
        match self {
            Self::Builtin => 0,
            Self::Rg => 1,
            Self::Fd => 2,
            Self::Everything => 3,
        }
    }
}
impl Backend {
    pub fn program(&self) -> &str {
        if !self.executable.trim().is_empty() {
            return self.executable.trim();
        }
        match self.kind {
            Kind::Builtin => "",
            Kind::Rg => "rg",
            Kind::Fd => "fd",
            Kind::Everything => "es.exe",
        }
    }
}

// Read stdout on a separate thread so a slow child can be killed as soon as a
// newer request arrives. Do not invoke a shell with user-supplied paths or text.
fn run(
    mut command: Command,
    separator: u8,
    allow_no_matches: bool,
    revision: &AtomicU64,
    generation: u64,
    mut accept: impl FnMut(Vec<u8>) -> bool,
) -> Result<bool> {
    command.stdout(Stdio::piped()).stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut child = command
        .spawn()
        .context("无法启动搜索程序，请检查是否已安装以及程序路径")?;
    let stdout = child.stdout.take().context("无法读取搜索程序输出")?;
    let (send, recv) = mpsc::sync_channel::<std::io::Result<Vec<u8>>>(64);
    let reader = std::thread::spawn(move || {
        let mut stdout = BufReader::new(stdout);
        loop {
            let mut bytes = Vec::new();
            match stdout.read_until(separator, &mut bytes) {
                Ok(0) => break,
                Ok(_) => {
                    if bytes.last() == Some(&separator) {
                        bytes.pop();
                    }
                    if send.send(Ok(bytes)).is_err() {
                        break;
                    }
                }
                Err(e) => {
                    let _ = send.send(Err(e));
                    break;
                }
            }
        }
    });
    let mut stopped = false;
    let mut error = None;
    loop {
        if revision.load(Ordering::Relaxed) != generation {
            stopped = true;
            break;
        }
        match recv.recv_timeout(Duration::from_millis(50)) {
            Ok(Ok(bytes)) => {
                if !accept(bytes) {
                    stopped = true;
                    break;
                }
            }
            Ok(Err(e)) => {
                error = Some(e);
                break;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    if stopped || error.is_some() {
        let _ = child.kill();
    }
    drop(recv);
    let status = child.wait().context("等待搜索程序失败")?;
    let _ = reader.join();
    if let Some(error) = error {
        return Err(error).context("读取搜索程序输出失败");
    }
    if revision.load(Ordering::Relaxed) != generation {
        return Ok(false);
    }
    if !stopped && !status.success() && !(allow_no_matches && status.code() == Some(1)) {
        bail!("搜索程序退出失败（{status}）；请检查程序和搜索范围，Everything 需保持运行");
    }
    Ok(true)
}

fn path_from_bytes(bytes: Vec<u8>) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        PathBuf::from(std::ffi::OsString::from_vec(bytes))
    }
    #[cfg(not(unix))]
    {
        PathBuf::from(
            String::from_utf8_lossy(&bytes)
                .trim_end_matches('\r')
                .to_string(),
        )
    }
}

pub fn build(
    backend: &Backend,
    roots: &[PathBuf],
    revision: &AtomicU64,
    generation: u64,
    mut progress: impl FnMut(usize),
) -> Result<Option<Index>> {
    let mut index = Index::default();
    let mut seen = HashSet::new();
    for root in roots {
        if seen.insert(root.clone()) {
            index.entries.push(Entry::new(root.clone(), true));
        }
        let mut command = Command::new(backend.program());
        match backend.kind {
            Kind::Rg => {
                command.args(["--no-config", "--files", "--null", "--"]);
                command.arg(root);
            }
            Kind::Fd => {
                command.args([
                    "--absolute-path",
                    "--print0",
                    "--type",
                    "file",
                    "--type",
                    "directory",
                    "--",
                    ".",
                ]);
                command.arg(root);
            }
            _ => bail!("无效的索引后端"),
        }
        let mut last = Instant::now();
        let complete = run(
            command,
            0,
            backend.kind == Kind::Rg,
            revision,
            generation,
            |bytes| {
                if bytes.is_empty() {
                    return true;
                }
                let path = path_from_bytes(bytes);
                if !seen.insert(path.clone()) {
                    return true;
                }
                if index.entries.len() >= MAX_ENTRIES {
                    index.limited = true;
                    return false;
                }
                let Ok(metadata) = std::fs::symlink_metadata(&path) else {
                    return true;
                };
                if !metadata.is_file() && !metadata.is_dir() {
                    return true;
                }
                index.entries.push(Entry::new(path, metadata.is_dir()));
                if last.elapsed() >= Duration::from_millis(250) {
                    progress(index.entries.len());
                    last = Instant::now();
                }
                true
            },
        )?;
        if !complete {
            return Ok(None);
        }
        if index.limited {
            break;
        }
    }
    index
        .entries
        .sort_unstable_by(|a, b| (&a.name_lower, &a.path).cmp(&(&b.name_lower, &b.path)));
    Ok(Some(index))
}

pub fn everything(
    backend: &Backend,
    query: &str,
    revision: &AtomicU64,
    generation: u64,
) -> Result<Option<Index>> {
    let mut command = Command::new(backend.program());
    command.args([
        "-n", "200", "-txt", "-cp", "65001", "-timeout", "1000", "--",
    ]);
    if !query.trim().is_empty() {
        command.arg(query);
    }
    let mut index = Index::default();
    let complete = run(command, b'\n', false, revision, generation, |bytes| {
        let path = path_from_bytes(bytes);
        if path.is_absolute() {
            let directory = path.is_dir();
            index.entries.push(Entry::new(path, directory));
        }
        true
    })?;
    Ok(complete.then_some(index))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file_search::test_support::TempDir;

    #[test]
    fn external_indexers_handle_files_and_empty_roots() {
        let root = TempDir::new();
        let sub = root.0.join("目录");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("报告.txt"), "data").unwrap();
        let revision = AtomicU64::new(1);
        for (kind, program) in [(Kind::Rg, "rg"), (Kind::Fd, "fd")] {
            if Command::new(program)
                .arg("--version")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_err()
            {
                continue;
            }
            let backend = Backend {
                kind,
                executable: program.into(),
            };
            let index = build(
                &backend,
                std::slice::from_ref(&root.0),
                &revision,
                1,
                |_| {},
            )
            .unwrap()
            .unwrap();
            assert!(index.entries.iter().any(|entry| entry.name == "报告.txt"));
            assert!(index.entries.iter().any(|entry| entry.path == root.0));
            if kind == Kind::Fd {
                assert!(
                    index
                        .entries
                        .iter()
                        .any(|entry| entry.path.canonicalize().ok() == sub.canonicalize().ok())
                );
            }
            let empty = TempDir::new();
            assert_eq!(
                build(&backend, &[empty.0.clone()], &revision, 1, |_| {})
                    .unwrap()
                    .unwrap()
                    .entries
                    .len(),
                1
            );
        }
        let missing = Backend {
            kind: Kind::Fd,
            executable: root.0.join("missing").to_string_lossy().into_owned(),
        };
        assert!(build(&missing, &[root.0.clone()], &revision, 1, |_| {}).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn everything_passes_query_as_one_argument_and_reads_unicode_paths() {
        use std::os::unix::fs::PermissionsExt;
        let root = TempDir::new();
        let file = root.0.join("报告.txt");
        std::fs::write(&file, "data").unwrap();
        let script = root.0.join("es");
        std::fs::write(&script, format!("#!/bin/sh\n[ \"$8\" = '--' ] || exit 3\n[ \"$9\" = '-foo bar' ] || exit 4\nprintf '%s\\n' '{}'\n", file.display())).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let backend = Backend {
            kind: Kind::Everything,
            executable: script.to_string_lossy().into_owned(),
        };
        let revision = AtomicU64::new(1);
        let index = everything(&backend, "-foo bar", &revision, 1)
            .unwrap()
            .unwrap();
        assert_eq!(index.entries.len(), 1);
        assert_eq!(index.entries[0].path, file);
    }

    #[cfg(unix)]
    #[test]
    fn cancelled_child_is_terminated_and_reported() {
        use std::os::unix::fs::PermissionsExt;
        let root = TempDir::new();
        let script = root.0.join("slow");
        std::fs::write(&script, "#!/bin/sh\nexec sleep 30\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let backend = Backend {
            kind: Kind::Fd,
            executable: script.to_string_lossy().into_owned(),
        };
        let revision = std::sync::Arc::new(AtomicU64::new(1));
        let other = revision.clone();
        let path = root.0.clone();
        let worker = std::thread::spawn(move || build(&backend, &[path], &other, 1, |_| {}));
        std::thread::sleep(Duration::from_millis(100));
        revision.store(2, Ordering::Relaxed);
        assert!(worker.join().unwrap().unwrap().is_none());
    }
}
