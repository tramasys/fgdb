use std::{
    collections::{HashSet, VecDeque},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use crate::config::LaunchConfig;

const MAX_SOURCE_TREE_DIRECTORIES: usize = 25_000;
const MAX_SEARCHABLE_SOURCE_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceTreeMatch {
    pub path: PathBuf,
    pub line: u32,
    pub column: u32,
    pub preview: String,
}

pub fn paths_match(open_path: &Path, reported_path: &str) -> bool {
    let reported_path = Path::new(reported_path);
    open_path == reported_path || open_path.ends_with(reported_path)
}

pub fn roots(config: &LaunchConfig) -> Vec<PathBuf> {
    let mut roots = vec![config.working_directory.clone()];
    roots.extend(config.source_paths.iter().cloned());
    if let Some(paths) = std::env::var_os("RUST_SRC_PATH") {
        roots.extend(std::env::split_paths(&paths));
    }
    if let Some(sysroot) = rust_sysroot(Duration::from_millis(250)) {
        roots.push(sysroot.join("lib/rustlib/src/rust"));
    }
    roots.push(PathBuf::from("/usr/src/debug"));
    if let Some(home) = std::env::var_os("HOME") {
        roots.push(PathBuf::from(home).join(".cache/debuginfod_client"));
    }
    let mut seen = HashSet::new();
    roots.retain(|root| root.is_dir() && seen.insert(root.clone()));
    roots
}

pub fn search_roots(config: &LaunchConfig) -> Vec<PathBuf> {
    let mut roots = vec![config.working_directory.clone()];
    roots.extend(config.source_paths.iter().cloned());
    let mut seen = HashSet::new();
    roots.retain(|root| root.is_dir() && seen.insert(root.clone()));
    roots
}

pub fn discover_source_files(roots: &[PathBuf], limit: usize) -> Vec<PathBuf> {
    let mut pending = roots.iter().cloned().collect::<VecDeque<_>>();
    let mut files = Vec::new();
    let mut directories = 0_usize;
    while let Some(directory) = pending.pop_front() {
        if files.len() >= limit || directories >= MAX_SOURCE_TREE_DIRECTORIES {
            break;
        }
        directories += 1;
        let Ok(entries) = std::fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.flatten() {
            if files.len() >= limit {
                break;
            }
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                if !is_ignored_source_directory(&entry.file_name()) {
                    pending.push_back(path);
                }
            } else if file_type.is_file() && is_source_path(&path) {
                files.push(path);
            }
        }
    }
    files.sort_unstable();
    files.dedup();
    files
}

pub fn search_source_files(
    files: &[PathBuf],
    query: &str,
    match_limit: usize,
    mut should_continue: impl FnMut() -> bool,
) -> Vec<SourceTreeMatch> {
    let query = query.trim();
    if query.is_empty() {
        return Vec::new();
    }
    let query_lower = query.to_lowercase();
    let mut matches = Vec::new();
    for path in files {
        if !should_continue() {
            break;
        }
        let Ok(metadata) = std::fs::metadata(path) else {
            continue;
        };
        if metadata.len() > MAX_SEARCHABLE_SOURCE_BYTES {
            continue;
        }
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        let contents = String::from_utf8_lossy(&bytes);
        for (line_index, line) in contents.lines().enumerate() {
            let line_lower = line.to_lowercase();
            let Some(column) = line_lower.find(&query_lower) else {
                continue;
            };
            matches.push(SourceTreeMatch {
                path: path.clone(),
                line: u32::try_from(line_index + 1).unwrap_or(u32::MAX),
                column: u32::try_from(column + 1).unwrap_or(u32::MAX),
                preview: line.trim().chars().take(240).collect(),
            });
            if matches.len() >= match_limit {
                return matches;
            }
        }
    }
    matches
}

fn is_ignored_source_directory(name: &std::ffi::OsStr) -> bool {
    matches!(
        name.to_str(),
        Some(
            ".git"
                | ".hg"
                | ".svn"
                | ".cache"
                | ".venv"
                | "venv"
                | "__pycache__"
                | "node_modules"
                | "target"
                | "CMakeFiles"
                | "build"
                | "dist"
                | "out"
        )
    )
}

fn is_source_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "c" | "h"
                    | "cc"
                    | "cpp"
                    | "cxx"
                    | "hpp"
                    | "hh"
                    | "hxx"
                    | "tcc"
                    | "ipp"
                    | "ixx"
                    | "cppm"
                    | "rs"
                    | "s"
                    | "asm"
                    | "inc"
                    | "inl"
                    | "m"
                    | "mm"
                    | "go"
                    | "zig"
                    | "swift"
                    | "f"
                    | "for"
                    | "f90"
                    | "f95"
                    | "f03"
                    | "f08"
                    | "adb"
                    | "ads"
                    | "d"
                    | "di"
                    | "cu"
                    | "cuh"
                    | "cl"
                    | "pas"
                    | "pp"
                    | "java"
                    | "kt"
                    | "kts"
                    | "scala"
                    | "cs"
                    | "vala"
                    | "vapi"
                    | "py"
                    | "pyx"
                    | "pxd"
                    | "js"
                    | "jsx"
                    | "ts"
                    | "tsx"
                    | "sh"
                    | "bash"
                    | "zsh"
                    | "fish"
                    | "lua"
                    | "rb"
                    | "php"
            )
        })
}

fn rust_sysroot(timeout: Duration) -> Option<PathBuf> {
    let mut child = Command::new("rustc")
        .args(["--print", "sysroot"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let output = child.wait_with_output().ok()?;
                return status
                    .success()
                    .then(|| PathBuf::from(String::from_utf8_lossy(&output.stdout).trim()))
                    .filter(|path| !path.as_os_str().is_empty());
            }
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(5)),
            Ok(None) | Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

pub fn resolve(reported: &str, roots: &[PathBuf]) -> Option<PathBuf> {
    let reported = Path::new(reported);
    if reported.is_file() {
        return Some(reported.to_path_buf());
    }
    for root in roots {
        let direct = root.join(reported.strip_prefix("/").unwrap_or(reported));
        if direct.is_file() {
            return Some(direct);
        }
    }

    let components: Vec<_> = reported.components().collect();
    if let Some(rustc) = components
        .iter()
        .position(|component| component.as_os_str() == "rustc")
        && components.len() > rustc + 2
    {
        let suffix: PathBuf = components[rustc + 2..].iter().collect();
        for root in roots {
            let candidate = root.join(&suffix);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    let suffixes = (2..=components.len().min(7))
        .rev()
        .map(|length| components[components.len() - length..].iter().collect())
        .collect::<Vec<PathBuf>>();
    for root in roots {
        // Most compiler paths retain a useful suffix. Try those cheap direct
        // probes before enumerating child directories on every cache miss.
        if let Some(candidate) = suffixes
            .iter()
            .map(|suffix| root.join(suffix))
            .find(|candidate| candidate.is_file())
        {
            return Some(candidate);
        }
        let child_directories = std::fs::read_dir(root)
            .into_iter()
            .flatten()
            .flatten()
            .take(256)
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .collect::<Vec<_>>();
        for suffix in &suffixes {
            for child in &child_directories {
                for candidate in [child.join(suffix), child.join("source").join(suffix)] {
                    if candidate.is_file() {
                        return Some(candidate);
                    }
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{paths_match, search_source_files};
    use std::path::{Path, PathBuf};

    #[test]
    fn matches_absolute_and_debugger_relative_source_paths() {
        let open = Path::new("/home/user/project/src/main.rs");
        assert!(paths_match(open, "/home/user/project/src/main.rs"));
        assert!(paths_match(open, "src/main.rs"));
        assert!(!paths_match(open, "other/main.rs"));
    }

    #[test]
    fn searches_source_text_and_honors_cancellation() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/source.rs");
        let matches = search_source_files(
            std::slice::from_ref(&path),
            "MAX_SEARCHABLE_SOURCE_BYTES",
            10,
            || true,
        );
        assert!(!matches.is_empty());
        assert_eq!(matches[0].path, path);

        let cancelled = search_source_files(&[path], "SourceTreeMatch", 10, || false);
        assert!(cancelled.is_empty());
    }
}
