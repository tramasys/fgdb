use std::{
    path::{Path, PathBuf},
    process::Command,
};

use crate::config::LaunchConfig;

pub fn paths_match(open_path: &Path, reported_path: &str) -> bool {
    let reported_path = Path::new(reported_path);
    open_path == reported_path || open_path.ends_with(reported_path)
}

pub fn roots(config: &LaunchConfig) -> Vec<PathBuf> {
    let mut roots = vec![config.working_directory.clone()];
    if let Some(paths) = std::env::var_os("FGDB_SOURCE_PATH") {
        roots.extend(std::env::split_paths(&paths));
    }
    if let Ok(output) = Command::new("rustc").args(["--print", "sysroot"]).output()
        && output.status.success()
    {
        let sysroot = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        roots.push(
            PathBuf::from(sysroot)
                .join("lib")
                .join("rustlib")
                .join("src")
                .join("rust"),
        );
    }
    roots.push(PathBuf::from("/usr/src/debug"));
    if let Some(home) = std::env::var_os("HOME") {
        roots.push(PathBuf::from(home).join(".cache/debuginfod_client"));
    }
    roots.retain(|root| root.is_dir());
    roots
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
        let child_directories = std::fs::read_dir(root)
            .into_iter()
            .flatten()
            .flatten()
            .take(256)
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .collect::<Vec<_>>();
        for suffix in &suffixes {
            let candidate = root.join(suffix);
            if candidate.is_file() {
                return Some(candidate);
            }
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
