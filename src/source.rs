use std::{
    collections::{BTreeMap, HashSet, VecDeque},
    ffi::OsString,
    path::{Path, PathBuf},
};

use crate::config::LaunchConfig;

mod cache;

const MAX_SOURCE_TREE_DIRECTORIES: usize = 25_000;
const MAX_SEARCHABLE_SOURCE_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceTreeMatch {
    pub path: PathBuf,
    pub line: u32,
    pub column: u32,
    pub preview: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceTreeNodeData {
    pub name: String,
    pub path: PathBuf,
    pub directory: bool,
    pub loaded: bool,
    pub children: Vec<SourceTreeNodeData>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SourceTreeBuild {
    pub roots: Vec<SourceTreeNodeData>,
    pub file_count: usize,
}

#[derive(Default)]
struct SourceTreeDirectory {
    path: PathBuf,
    directories: BTreeMap<OsString, SourceTreeDirectory>,
    files: BTreeMap<OsString, PathBuf>,
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
    if let Some(sysroot) = config.rust_sysroot() {
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
    let mut visited_directories = HashSet::new();
    let mut seen_files = HashSet::new();
    let mut files = Vec::new();
    let mut directories = 0_usize;
    while let Some(directory) = pending.pop_front() {
        if files.len() >= limit || directories >= MAX_SOURCE_TREE_DIRECTORIES {
            break;
        }
        if !visited_directories.insert(directory.clone()) {
            continue;
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
            } else if file_type.is_file()
                && is_source_path(&path)
                && seen_files.insert(path.clone())
            {
                files.push(path);
            }
        }
    }
    files.sort_unstable();
    files.dedup();
    files
}

#[cfg(test)]
pub fn build_source_tree(
    files: &[PathBuf],
    roots: &[PathBuf],
    loaded_files: &[PathBuf],
    query: &str,
) -> SourceTreeBuild {
    build_source_tree_while(files, roots, loaded_files, query, || true)
}

pub fn build_source_tree_while(
    files: &[PathBuf],
    roots: &[PathBuf],
    loaded_files: &[PathBuf],
    query: &str,
    mut should_continue: impl FnMut() -> bool,
) -> SourceTreeBuild {
    let terms = query
        .split_whitespace()
        .map(str::to_lowercase)
        .collect::<Vec<_>>();
    let loaded = loaded_files
        .iter()
        .map(PathBuf::as_path)
        .collect::<HashSet<_>>();
    let mut directories = roots
        .iter()
        .cloned()
        .map(|path| SourceTreeDirectory {
            path,
            ..SourceTreeDirectory::default()
        })
        .collect::<Vec<_>>();
    let root_depths = roots
        .iter()
        .map(|root| root.components().count())
        .collect::<Vec<_>>();
    let mut file_count = 0;
    for (index, file) in files.iter().enumerate() {
        if index % 256 == 0 && !should_continue() {
            return SourceTreeBuild::default();
        }
        if !terms.is_empty() {
            let path_text = file.to_string_lossy().to_lowercase();
            if !terms.iter().all(|term| path_text.contains(term)) {
                continue;
            }
        }
        let Some((root_index, root)) = roots
            .iter()
            .enumerate()
            .filter(|(_, root)| file.starts_with(root))
            .max_by_key(|(index, _)| root_depths[*index])
        else {
            continue;
        };
        let Ok(relative) = file.strip_prefix(root) else {
            continue;
        };
        let components = relative
            .components()
            .filter_map(|component| match component {
                std::path::Component::Normal(component) => Some(component.to_os_string()),
                _ => None,
            })
            .collect::<Vec<_>>();
        let Some((file_name, parents)) = components.split_last() else {
            continue;
        };
        let mut directory = &mut directories[root_index];
        for parent in parents {
            let path = directory.path.join(parent);
            directory = directory
                .directories
                .entry(parent.clone())
                .or_insert_with(|| SourceTreeDirectory {
                    path,
                    ..SourceTreeDirectory::default()
                });
        }
        if directory
            .files
            .insert(file_name.clone(), file.clone())
            .is_none()
        {
            file_count += 1;
        }
    }
    let roots = directories
        .into_iter()
        .filter(|directory| !directory.directories.is_empty() || !directory.files.is_empty())
        .map(|directory| source_tree_directory_node(directory, &loaded, true))
        .collect();
    SourceTreeBuild { roots, file_count }
}

fn source_tree_directory_node(
    directory: SourceTreeDirectory,
    loaded_files: &HashSet<&Path>,
    root: bool,
) -> SourceTreeNodeData {
    let mut children = directory
        .directories
        .into_values()
        .map(|directory| source_tree_directory_node(directory, loaded_files, false))
        .collect::<Vec<_>>();
    children.extend(
        directory
            .files
            .into_iter()
            .map(|(name, path)| SourceTreeNodeData {
                name: name.to_string_lossy().into_owned(),
                loaded: loaded_files.contains(path.as_path()),
                path,
                directory: false,
                children: Vec::new(),
            }),
    );
    let loaded = children.iter().any(|child| child.loaded);
    let name = if root {
        directory
            .path
            .file_name()
            .filter(|name| !name.is_empty())
            .map_or_else(
                || directory.path.display().to_string(),
                |name| name.to_string_lossy().into_owned(),
            )
    } else {
        directory.path.file_name().map_or_else(
            || String::from("source"),
            |name| name.to_string_lossy().into_owned(),
        )
    };
    SourceTreeNodeData {
        name,
        path: directory.path,
        directory: true,
        loaded,
        children,
    }
}

pub fn search_source_files(
    files: &[PathBuf],
    query: &str,
    match_limit: usize,
    scope: Option<&Path>,
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
        if scope.is_some_and(|scope| !path.starts_with(scope)) {
            continue;
        }
        let Some(source) = cache::searchable_source(path) else {
            continue;
        };
        for (line_index, line) in source.lines().enumerate() {
            if line_index % 256 == 0 && !should_continue() {
                return matches;
            }
            let Some(column) = case_insensitive_match_column(line, &query_lower) else {
                continue;
            };
            matches.push(SourceTreeMatch {
                path: path.clone(),
                line: u32::try_from(line_index + 1).unwrap_or(u32::MAX),
                column: u32::try_from(column).unwrap_or(u32::MAX),
                preview: line.trim().chars().take(240).collect(),
            });
            if matches.len() >= match_limit {
                return matches;
            }
        }
    }
    matches
}

fn case_insensitive_match_column(line: &str, query_lower: &str) -> Option<usize> {
    if query_lower.is_empty() {
        return Some(1);
    }
    if line.is_ascii() && query_lower.is_ascii() {
        let query = query_lower.as_bytes();
        let byte_offset = line
            .as_bytes()
            .windows(query.len())
            .position(|window| window.eq_ignore_ascii_case(query))?;
        return Some(byte_offset + 1);
    }
    let line_lower = line.to_lowercase();
    let byte_offset = line_lower.find(query_lower)?;
    let lowered_character_offset = line_lower.get(..byte_offset)?.chars().count();
    let mut produced_lowercase_characters = 0;
    for (original_character_offset, character) in line.chars().enumerate() {
        if produced_lowercase_characters >= lowered_character_offset {
            return Some(original_character_offset + 1);
        }
        produced_lowercase_characters += character.to_lowercase().count();
    }
    Some(line.chars().count().saturating_add(1))
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
    use super::{
        MAX_SEARCHABLE_SOURCE_BYTES, build_source_tree, build_source_tree_while,
        case_insensitive_match_column, discover_source_files, paths_match, search_source_files,
    };
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
            None,
            || true,
        );
        assert!(!matches.is_empty());
        assert_eq!(matches[0].path, path);

        let outside_scope = search_source_files(
            std::slice::from_ref(&path),
            "MAX_SEARCHABLE_SOURCE_BYTES",
            10,
            Some(Path::new("/outside/source/root")),
            || true,
        );
        assert!(outside_scope.is_empty());

        let cancelled = search_source_files(&[path], "SourceTreeMatch", 10, None, || false);
        assert!(cancelled.is_empty());
    }

    #[test]
    fn source_search_accepts_files_below_and_at_the_byte_limit() {
        let directory = temporary_test_directory("bounded-search-accepted");
        for (name, length) in [
            ("below.c", MAX_SEARCHABLE_SOURCE_BYTES - 1),
            ("exact.c", MAX_SEARCHABLE_SOURCE_BYTES),
        ] {
            let path = directory.join(name);
            let mut contents = vec![b' '; length];
            contents[..6].copy_from_slice(b"needle");
            std::fs::write(&path, contents).unwrap();
            let matches = search_source_files(&[path], "needle", 1, None, || true);
            assert_eq!(matches.len(), 1, "{name} should be searchable");
        }
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn source_search_rejects_files_over_the_byte_limit() {
        let directory = temporary_test_directory("bounded-search-rejected");
        let path = directory.join("over.c");
        let mut contents = vec![b' '; MAX_SEARCHABLE_SOURCE_BYTES + 1];
        contents[..6].copy_from_slice(b"needle");
        std::fs::write(&path, contents).unwrap();

        assert!(search_source_files(&[path], "needle", 1, None, || true).is_empty());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn reports_source_columns_in_original_unicode_characters() {
        assert_eq!(
            case_insensitive_match_column("Alpha TARGET", "alpha"),
            Some(1)
        );
        assert_eq!(
            case_insensitive_match_column("Alpha TARGET", "target"),
            Some(7)
        );
        assert_eq!(
            case_insensitive_match_column("alpha target", "target"),
            Some(7)
        );
        assert_eq!(case_insensitive_match_column("İtarget", "target"), Some(2));
        assert_eq!(case_insensitive_match_column("Ärger", "är"), Some(1));
    }

    #[test]
    fn nested_source_roots_do_not_consume_the_file_limit_twice() {
        let root = std::env::temp_dir().join(format!(
            "fgdb-source-roots-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let nested = root.join("nested");
        let sibling = root.join("sibling");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir_all(&sibling).unwrap();
        std::fs::write(root.join("a.c"), "int a;\n").unwrap();
        std::fs::write(nested.join("b.c"), "int b;\n").unwrap();
        std::fs::write(sibling.join("c.c"), "int c;\n").unwrap();
        let files = discover_source_files(&[root.clone(), nested], 3);
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(files.len(), 3);
    }

    #[test]
    fn builds_filtered_hierarchies_and_marks_loaded_sources() {
        let root = PathBuf::from("/project");
        let main = root.join("src/main.rs");
        let parser = root.join("src/parser/mod.rs");
        let build = build_source_tree(
            &[main.clone(), parser.clone(), root.join("tests/parser.rs")],
            std::slice::from_ref(&root),
            std::slice::from_ref(&parser),
            "src parser",
        );
        assert_eq!(build.file_count, 1);
        assert_eq!(build.roots.len(), 1);
        let src = &build.roots[0].children[0];
        assert_eq!(src.name, "src");
        assert!(src.loaded);
        let parser_directory = &src.children[0];
        assert_eq!(parser_directory.name, "parser");
        assert!(parser_directory.loaded);
        assert_eq!(parser_directory.children[0].path, parser);
        assert!(!build.roots[0].children.iter().any(|node| node.path == main));
    }

    #[test]
    fn cancels_stale_source_tree_builds_before_allocating_the_tree() {
        let files = (0..300)
            .map(|index| PathBuf::from(format!("/project/src/file-{index}.rs")))
            .collect::<Vec<_>>();
        let build =
            build_source_tree_while(&files, &[PathBuf::from("/project")], &[], "", || false);
        assert_eq!(build, Default::default());
    }

    fn temporary_test_directory(name: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "fgdb-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        directory
    }
}
