use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use crate::config::LaunchConfig;

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
    use super::paths_match;
    use std::path::Path;

    #[test]
    fn matches_absolute_and_debugger_relative_source_paths() {
        let open = Path::new("/home/user/project/src/main.rs");
        assert!(paths_match(open, "/home/user/project/src/main.rs"));
        assert!(paths_match(open, "src/main.rs"));
        assert!(!paths_match(open, "other/main.rs"));
    }
}
