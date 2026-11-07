use anyhow::{Context, Result, bail};
use ignore::WalkBuilder;
use std::{
    collections::{BinaryHeap, HashSet},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

mod controller;
pub use controller::Controller;

const MAX_ENTRIES: usize = 250_000;
const MAX_RESULTS: usize = 200;

pub fn is_command(query: &str) -> bool {
    query.trim().eq_ignore_ascii_case("ss")
}

#[derive(Debug)]
struct Entry {
    path: PathBuf,
    name: String,
    name_lower: String,
    path_lower: String,
    directory: bool,
}

impl Entry {
    fn new(path: PathBuf, directory: bool) -> Self {
        let name = path
            .file_name()
            .unwrap_or(path.as_os_str())
            .to_string_lossy()
            .into_owned();
        Self {
            name_lower: name.to_lowercase(),
            path_lower: path.to_string_lossy().to_lowercase(),
            path,
            name,
            directory,
        }
    }
}

#[derive(Default, Debug)]
struct Index {
    entries: Vec<Entry>,
    warnings: usize,
    first_warning: Option<String>,
    limited: bool,
}

impl Index {
    fn status(&self, configured: bool) -> String {
        if !configured {
            return "尚未设置索引范围，请在设置 → 文件搜索中添加目录。".into();
        }
        let mut status = format!("已索引 {} 项", self.entries.len());
        if self.limited {
            status.push_str(&format!("；达到 {MAX_ENTRIES} 项上限，请缩小索引范围"));
        }
        if self.warnings > 0 {
            status.push_str(&format!("；{} 项无法读取", self.warnings));
            if let Some(warning) = &self.first_warning {
                status.push_str(&format!("：{warning}"));
            }
        }
        status
    }
}

/// Resolve and validate the complete draft before replacing saved settings.
fn validate_roots(value: &str) -> Result<Vec<PathBuf>> {
    let mut roots = Vec::new();
    for line in value.lines().map(str::trim).filter(|line| !line.is_empty()) {
        let path = if line == "~" || line.starts_with("~/") || line.starts_with("~\\") {
            let home = std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .context("无法展开 ~，请填写完整目录路径")?;
            PathBuf::from(home).join(line.get(2..).unwrap_or(""))
        } else {
            PathBuf::from(line)
        };
        anyhow::ensure!(path.is_absolute(), "索引范围必须是绝对目录路径：{line}");
        let path = path
            .canonicalize()
            .with_context(|| format!("无法访问目录：{line}"))?;
        anyhow::ensure!(path.is_dir(), "索引范围必须是目录：{line}");
        let text = path.to_str().context("索引目录路径必须是有效的 UTF-8")?;
        anyhow::ensure!(!text.contains(['\n', '\r']), "索引目录路径不能包含换行符");
        roots.push(path);
    }
    roots.sort_by_key(|path| path.components().count());
    let mut unique: Vec<PathBuf> = Vec::new();
    for root in roots {
        if !unique.iter().any(|parent| root.starts_with(parent)) {
            unique.push(root);
        }
    }
    Ok(unique)
}

fn roots_text(roots: &[PathBuf]) -> String {
    roots
        .iter()
        .map(|p| p.to_string_lossy())
        .collect::<Vec<_>>()
        .join("\n")
}

fn build_index(
    roots: &[PathBuf],
    revision: &AtomicU64,
    generation: u64,
    mut progress: impl FnMut(usize),
) -> Option<Index> {
    let mut index = Index::default();
    let Some(first) = roots.first() else {
        return Some(index);
    };
    let mut walker = WalkBuilder::new(first);
    for root in &roots[1..] {
        walker.add(root);
    }
    walker.hidden(true).follow_links(false).require_git(false);
    let mut seen = HashSet::new();
    let mut last_progress = Instant::now();
    for result in walker.build() {
        if revision.load(Ordering::Relaxed) != generation {
            return None;
        }
        let entry = match result {
            Ok(entry) => entry,
            Err(error) => {
                index.warnings += 1;
                if index.first_warning.is_none() {
                    index.first_warning = Some(error.to_string());
                }
                continue;
            }
        };
        if let Some(error) = entry.error() {
            index.warnings += 1;
            if index.first_warning.is_none() {
                index.first_warning = Some(error.to_string());
            }
        }
        // Never index symlink targets, sockets or other special files.
        let Some(kind) = entry
            .file_type()
            .filter(|kind| kind.is_file() || kind.is_dir())
        else {
            continue;
        };
        if entry.depth() == 0 || !seen.insert(entry.path().to_owned()) {
            continue;
        }
        if index.entries.len() == MAX_ENTRIES {
            index.limited = true;
            break;
        }
        index
            .entries
            .push(Entry::new(entry.into_path(), kind.is_dir()));
        if last_progress.elapsed() >= Duration::from_millis(250) {
            progress(index.entries.len());
            last_progress = Instant::now();
        }
    }
    if revision.load(Ordering::Relaxed) != generation {
        return None;
    }
    index
        .entries
        .sort_unstable_by(|a, b| (&a.name_lower, &a.path).cmp(&(&b.name_lower, &b.path)));
    Some(index)
}

#[derive(Debug, Default)]
struct Matches {
    indices: Vec<usize>,
    total: usize,
}

fn search_index(
    index: &Index,
    query: &str,
    revision: &AtomicU64,
    generation: u64,
) -> Option<Matches> {
    let query = query.trim().to_lowercase();
    let terms = query.split_whitespace().collect::<Vec<_>>();
    let mut best = BinaryHeap::with_capacity(MAX_RESULTS + 1);
    let mut total = 0;
    for (position, entry) in index.entries.iter().enumerate() {
        if position % 128 == 0 && revision.load(Ordering::Relaxed) != generation {
            return None;
        }
        if !terms.iter().all(|term| entry.path_lower.contains(term)) {
            continue;
        }
        total += 1;
        let rank = if query.is_empty() || entry.name_lower == query {
            0
        } else if entry.name_lower.starts_with(&query) {
            1
        } else if entry.name_lower.contains(&query) {
            2
        } else {
            3
        };
        if best.len() < MAX_RESULTS {
            best.push((rank, position));
        } else if best.peek().is_some_and(|worst| (rank, position) < *worst) {
            best.pop();
            best.push((rank, position));
        }
    }
    if revision.load(Ordering::Relaxed) != generation {
        return None;
    }
    Some(Matches {
        indices: best.into_sorted_vec().into_iter().map(|(_, i)| i).collect(),
        total,
    })
}

fn open(path: &Path) -> Result<()> {
    anyhow::ensure!(
        path.exists(),
        "文件已删除或不可访问，请按 Ctrl + R 重建索引：{}",
        path.display()
    );
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        let mut command = if cfg!(target_os = "macos") {
            std::process::Command::new("open")
        } else {
            std::process::Command::new("xdg-open")
        };
        // All indexed paths are absolute; arguments never pass through a shell.
        let output = command
            .arg(path)
            .output()
            .context("无法运行系统文件打开程序")?;
        if !output.status.success() {
            bail!(
                "系统无法打开文件：{}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(())
    }
    #[cfg(target_os = "windows")]
    {
        use std::{os::windows::ffi::OsStrExt, ptr};
        use windows_sys::Win32::System::Com::{
            COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize,
        };
        let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        // SAFETY: This dedicated worker has no other COM apartment; Shell file
        // associations may need COM. Balance successful initialization below.
        let initialized = unsafe { CoInitializeEx(ptr::null(), COINIT_APARTMENTTHREADED as u32) };
        anyhow::ensure!(
            initialized >= 0,
            "无法初始化 Windows Shell COM：{initialized:#x}"
        );
        // SAFETY: The absolute file path is NUL-terminated UTF-16. The shell
        // selects the file association; no command interpreter is involved.
        let result = unsafe {
            windows_sys::Win32::UI::Shell::ShellExecuteW(
                ptr::null_mut(),
                ptr::null(),
                path.as_ptr(),
                ptr::null(),
                ptr::null(),
                1,
            )
        };
        // SAFETY: CoInitializeEx succeeded on this thread above.
        unsafe {
            CoUninitialize();
        }
        anyhow::ensure!(
            result as isize > 32,
            "系统无法打开文件：{}",
            result as isize
        );
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    pub struct TempDir(pub PathBuf);
    impl TempDir {
        pub fn new() -> Self {
            let mut random = [0; 8];
            getrandom::fill(&mut random).unwrap();
            let path = std::env::temp_dir().join(format!(
                "panel-file-test-{:016x}",
                u64::from_ne_bytes(random)
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_support::TempDir;

    #[test]
    fn ignore_rules_hidden_items_overlapping_roots_and_symlinks_are_respected() {
        let dir = TempDir::new();
        std::fs::create_dir_all(dir.0.join("src")).unwrap();
        std::fs::create_dir_all(dir.0.join("ignored")).unwrap();
        std::fs::write(dir.0.join(".gitignore"), "ignored/\n*.tmp\n").unwrap();
        std::fs::write(dir.0.join(".ignore"), "local.txt\n").unwrap();
        for path in [
            "src/报告 final.txt",
            "ignored/secret.txt",
            "hidden.tmp",
            ".hidden",
            "local.txt",
        ] {
            std::fs::write(dir.0.join(path), "test").unwrap();
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(&dir.0, dir.0.join("src/loop")).unwrap();
        let revision = AtomicU64::new(1);
        let index = build_index(&[dir.0.clone(), dir.0.join("src")], &revision, 1, |_| {}).unwrap();
        let names: Vec<_> = index
            .entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect();
        assert_eq!(names, ["src", "报告 final.txt"]);
        assert!(index.entries[0].directory);
        assert!(!index.entries[1].directory);
        assert_eq!(index.warnings, 0);
        revision.store(2, Ordering::Relaxed);
        assert!(build_index(std::slice::from_ref(&dir.0), &revision, 1, |_| {}).is_none());
        let missing = build_index(&[dir.0.join("missing")], &revision, 2, |_| {}).unwrap();
        assert!(missing.warnings > 0);
    }

    #[test]
    fn ranks_names_before_paths_matches_unicode_and_bounds_results_without_losing_count() {
        let index = Index {
            entries: [
                "/tmp/notes.md/other.txt",
                "/tmp/x-notes.md",
                "/tmp/notes.md.backup",
                "/tmp/notes.md",
                "/tmp/中文/报告 final.txt",
            ]
            .into_iter()
            .map(|path| Entry::new(path.into(), false))
            .collect(),
            ..Default::default()
        };
        let revision = AtomicU64::new(1);
        assert_eq!(
            search_index(&index, " NOTES.MD ", &revision, 1)
                .unwrap()
                .indices,
            [3, 2, 1, 0]
        );
        assert_eq!(
            search_index(&index, "中文 FINAL", &revision, 1)
                .unwrap()
                .indices,
            [4]
        );
        assert!(
            search_index(&index, "other final", &revision, 1)
                .unwrap()
                .indices
                .is_empty()
        );
        let large = Index {
            entries: (0..1000)
                .map(|i| Entry::new(format!("/tmp/file{i:04}.txt").into(), false))
                .collect(),
            ..Default::default()
        };
        let matches = search_index(&large, "file", &revision, 1).unwrap();
        assert_eq!(matches.total, 1000);
        assert_eq!(matches.indices, (0..MAX_RESULTS).collect::<Vec<_>>());
        revision.store(2, Ordering::Relaxed);
        assert!(search_index(&large, "file", &revision, 1).is_none());
        assert!(search_index(&Index::default(), "", &revision, 1).is_none());
    }

    #[test]
    fn validates_scope_deduplicates_descendants_and_rejects_files_and_missing_paths() {
        let dir = TempDir::new();
        std::fs::create_dir_all(dir.0.join("child")).unwrap();
        std::fs::write(dir.0.join("file.txt"), "test").unwrap();
        let text = format!(
            "{}\n{}\n{}\n",
            dir.0.join("child").display(),
            dir.0.display(),
            dir.0.display()
        );
        assert_eq!(
            validate_roots(&text).unwrap(),
            [dir.0.canonicalize().unwrap()]
        );
        assert!(validate_roots("").unwrap().is_empty());
        assert!(validate_roots("relative/folder").is_err());
        assert!(validate_roots(dir.0.join("file.txt").to_str().unwrap()).is_err());
        assert!(validate_roots(dir.0.join("missing").to_str().unwrap()).is_err());
        assert!(open(&dir.0.join("deleted")).is_err());
        for command in ["ss", " SS ", "Ss"] {
            assert!(is_command(command));
        }
        for command in ["", "s", "ss notes", "settings"] {
            assert!(!is_command(command));
        }
    }
}
