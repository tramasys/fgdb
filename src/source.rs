use std::{
    cmp::Ordering,
    collections::{BTreeMap, BinaryHeap, HashMap, HashSet, VecDeque},
    ffi::OsString,
    path::{Path, PathBuf},
};

use crate::config::LaunchConfig;

mod cache;

const MAX_SOURCE_TREE_DIRECTORIES: usize = 25_000;
const MAX_SEARCHABLE_SOURCE_BYTES: usize = 16 * 1024 * 1024;
const MAX_INDEXED_SUFFIX_COMPONENTS: usize = 8;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceResolution {
    Missing,
    Unique(PathBuf),
    Ambiguous,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SourceId(PathBuf);

impl SourceId {
    pub fn from_path(path: &Path) -> Self {
        Self(canonical_source_path(path))
    }

    fn from_indexed_path(path: &Path) -> Self {
        Self(path.to_path_buf())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceMatch {
    Exact,
    Different,
    Ambiguous,
    Unresolved,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IndexedCandidate {
    Unique(usize),
    Ambiguous,
}

/// Canonical, immutable lookup data shared by source navigation operations.
///
/// Building the suffix table is filesystem work, so callers construct it on a
/// worker alongside source discovery and retain it until the source roots are
/// invalidated. Resolution is then deterministic and does not silently pick
/// one of several files with the same debugger-reported suffix.
#[derive(Clone, Debug, Default)]
pub struct SourceIndex {
    roots: Vec<PathBuf>,
    files: Vec<PathBuf>,
    suffixes: HashMap<PathBuf, IndexedCandidate>,
}

impl SourceIndex {
    pub fn new(files: &[PathBuf], roots: &[PathBuf]) -> Self {
        let mut files = files
            .iter()
            .map(|path| canonical_source_path(path))
            .collect::<Vec<_>>();

        files.sort_unstable();
        files.dedup();

        let mut roots = roots
            .iter()
            .map(|path| canonical_source_path(path))
            .collect::<Vec<_>>();

        roots.sort_unstable();
        roots.dedup();
        let mut suffixes = HashMap::new();

        for (index, file) in files.iter().enumerate() {
            let components = normal_components(file);

            for length in 1..=components.len().min(MAX_INDEXED_SUFFIX_COMPONENTS) {
                let suffix = components[components.len() - length..]
                    .iter()
                    .collect::<PathBuf>();

                suffixes
                    .entry(suffix)
                    .and_modify(|candidate| {
                        if *candidate != IndexedCandidate::Unique(index) {
                            *candidate = IndexedCandidate::Ambiguous;
                        }
                    })
                    .or_insert(IndexedCandidate::Unique(index));
            }
        }

        Self {
            roots,
            files,
            suffixes,
        }
    }

    pub fn resolve(&self, reported: &str) -> SourceResolution {
        let reported = Path::new(reported);

        if reported.is_absolute() && reported.is_file() {
            return SourceResolution::Unique(canonical_source_path(reported));
        }

        let direct = self
            .roots
            .iter()
            .map(|root| root.join(reported.strip_prefix("/").unwrap_or(reported)))
            .filter(|candidate| candidate.is_file());

        match unique_existing_path(direct) {
            SourceResolution::Missing => {}
            resolution => return resolution,
        }

        let components = normal_components(reported);

        for length in (1..=components.len().min(MAX_INDEXED_SUFFIX_COMPONENTS)).rev() {
            let suffix = components[components.len() - length..]
                .iter()
                .collect::<PathBuf>();

            match self.suffixes.get(&suffix) {
                Some(IndexedCandidate::Unique(index)) => {
                    return SourceResolution::Unique(self.files[*index].clone());
                }
                Some(IndexedCandidate::Ambiguous) => return SourceResolution::Ambiguous,
                None => {}
            }
        }

        SourceResolution::Missing
    }

    pub fn files(&self) -> &[PathBuf] {
        &self.files
    }

    pub fn match_reported_id(&self, open: &SourceId, reported_path: &str) -> SourceMatch {
        let reported = Path::new(reported_path);

        if reported.is_absolute() {
            return if open.0 == reported || *open == SourceId::from_path(reported) {
                SourceMatch::Exact
            } else {
                SourceMatch::Different
            };
        }

        let components = normal_components(reported);
        let length = components.len().min(MAX_INDEXED_SUFFIX_COMPONENTS);

        if length == 0 {
            return SourceMatch::Unresolved;
        }

        let suffix = components[components.len() - length..]
            .iter()
            .collect::<PathBuf>();

        match self.suffixes.get(&suffix) {
            Some(IndexedCandidate::Unique(index)) => {
                if open.0 == self.files[*index] {
                    SourceMatch::Exact
                } else {
                    SourceMatch::Different
                }
            }
            Some(IndexedCandidate::Ambiguous) => SourceMatch::Ambiguous,
            None => SourceMatch::Unresolved,
        }
    }
}

fn normal_components(path: &Path) -> Vec<OsString> {
    path.components()
        .filter_map(|component| match component {
            std::path::Component::Normal(component) => Some(component.to_os_string()),
            _ => None,
        })
        .collect()
}

fn canonical_source_path(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn unique_existing_path(candidates: impl Iterator<Item = PathBuf>) -> SourceResolution {
    let mut unique = None;

    for candidate in candidates {
        let candidate = canonical_source_path(&candidate);

        if unique.as_ref() == Some(&candidate) {
            continue;
        }

        if unique.is_some() {
            return SourceResolution::Ambiguous;
        }

        unique = Some(candidate);
    }

    unique.map_or(SourceResolution::Missing, SourceResolution::Unique)
}

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
    pub file_routes: HashMap<SourceId, Box<[u32]>>,
}

#[derive(Clone, Debug, Default)]
pub struct SourceSearchIndex {
    entries: Vec<SourceSearchEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SourceSearchEntry {
    path: PathBuf,
    normalized_path: Box<str>,
    normalized_filename: Box<str>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceFileMatch {
    pub path: PathBuf,
    pub loaded: bool,
}

impl SourceSearchIndex {
    pub fn new(files: &[PathBuf]) -> Self {
        let entries = files
            .iter()
            .map(|path| SourceSearchEntry {
                path: path.clone(),
                normalized_path: path.to_string_lossy().to_lowercase().into_boxed_str(),
                normalized_filename: path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or_default()
                    .to_lowercase()
                    .into_boxed_str(),
            })
            .collect();

        Self { entries }
    }
}

struct NormalizedSourceQuery {
    text: Box<str>,
    terms: Vec<Box<str>>,
}

impl NormalizedSourceQuery {
    fn new(query: &str) -> Self {
        let text = query.trim().to_lowercase().into_boxed_str();
        let terms = text
            .split_whitespace()
            .map(|term| term.to_owned().into_boxed_str())
            .collect();

        Self { text, terms }
    }

    fn score(&self, entry: &SourceSearchEntry) -> Option<u16> {
        if self.text.is_empty() {
            return Some(1);
        }

        if !self
            .terms
            .iter()
            .all(|term| entry.normalized_path.contains(term.as_ref()))
        {
            return None;
        }

        Some(
            if entry.normalized_filename.as_ref() == self.text.as_ref() {
                500
            } else if entry.normalized_filename.starts_with(self.text.as_ref()) {
                400
            } else if entry.normalized_filename.contains(self.text.as_ref()) {
                300
            } else if entry.normalized_path.ends_with(self.text.as_ref()) {
                200
            } else {
                100
            },
        )
    }
}

#[derive(Eq)]
struct RankedSource<'a> {
    score: u16,
    entry: &'a SourceSearchEntry,
    loaded: bool,
}

impl PartialEq for RankedSource<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.score == other.score && self.entry.path == other.entry.path
    }
}

impl Ord for RankedSource<'_> {
    fn cmp(&self, other: &Self) -> Ordering {
        // BinaryHeap keeps the greatest value at the root. Invert the score so
        // the worst retained candidate can be replaced in constant time.
        other
            .score
            .cmp(&self.score)
            .then_with(|| self.entry.path.cmp(&other.entry.path))
    }
}

impl PartialOrd for RankedSource<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

pub fn search_source_paths(
    loaded: &SourceSearchIndex,
    tree: &SourceSearchIndex,
    query: &str,
    limit: usize,
) -> Vec<SourceFileMatch> {
    if limit == 0 {
        return Vec::new();
    }

    let query = NormalizedSourceQuery::new(query);
    let mut seen = HashSet::new();
    let mut matches = BinaryHeap::with_capacity(limit.saturating_add(1));

    for (index, loaded) in [(loaded, true), (tree, false)] {
        for entry in &index.entries {
            if !seen.insert(entry.path.as_path()) {
                continue;
            }

            let Some(score) = query.score(entry) else {
                continue;
            };

            let candidate = RankedSource {
                score,
                entry,
                loaded,
            };

            if matches.len() < limit {
                matches.push(candidate);
            } else if matches.peek().is_some_and(|worst| candidate < *worst) {
                matches.pop();
                matches.push(candidate);
            }
        }
    }

    let mut matches = matches.into_vec();
    matches.sort_unstable_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| left.entry.path.cmp(&right.entry.path))
    });

    matches
        .into_iter()
        .map(|candidate| SourceFileMatch {
            path: candidate.entry.path.clone(),
            loaded: candidate.loaded,
        })
        .collect()
}

#[derive(Default)]
struct SourceTreeDirectory {
    path: PathBuf,
    directories: BTreeMap<OsString, SourceTreeDirectory>,
    files: BTreeMap<OsString, PathBuf>,
}

pub fn paths_match(index: Option<&SourceIndex>, open_path: &Path, reported_path: &str) -> bool {
    paths_match_id(index, &SourceId::from_path(open_path), reported_path)
}

pub fn paths_match_id(index: Option<&SourceIndex>, open: &SourceId, reported_path: &str) -> bool {
    if let Some(index) = index {
        return index.match_reported_id(open, reported_path) == SourceMatch::Exact;
    }

    let reported_path = Path::new(reported_path);

    if reported_path.is_absolute() {
        return open.0 == reported_path || *open == SourceId::from_path(reported_path);
    }

    // A suffix cannot identify a source file without the index proving it is
    // unique. Rejecting an unresolved relative path is safer than associating
    // a breakpoint with the wrong `main.rs` in another crate or build root.
    false
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

#[cfg(test)]
pub fn discover_source_files(roots: &[PathBuf], limit: usize) -> Vec<PathBuf> {
    discover_source_files_while(roots, limit, || true)
}

pub fn discover_source_files_while(
    roots: &[PathBuf],
    limit: usize,
    mut should_continue: impl FnMut() -> bool,
) -> Vec<PathBuf> {
    let mut pending = roots.iter().cloned().collect::<VecDeque<_>>();
    let mut visited_directories = HashSet::new();
    let mut seen_files = HashSet::new();
    let mut files = Vec::new();
    let mut directories = 0_usize;

    while let Some(directory) = pending.pop_front() {
        if !should_continue() {
            break;
        }

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

        let mut entries = entries.flatten().collect::<Vec<_>>();
        entries.sort_unstable_by_key(std::fs::DirEntry::file_name);

        for (index, entry) in entries.into_iter().enumerate() {
            if index % 256 == 0 && !should_continue() {
                files.sort_unstable();
                files.dedup();
                return files;
            }

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
        .collect::<Vec<_>>();

    let file_routes = source_tree_file_routes(&roots);

    SourceTreeBuild {
        roots,
        file_count,
        file_routes,
    }
}

fn source_tree_file_routes(roots: &[SourceTreeNodeData]) -> HashMap<SourceId, Box<[u32]>> {
    fn visit(
        node: &SourceTreeNodeData,
        route: &mut Vec<u32>,
        routes: &mut HashMap<SourceId, Box<[u32]>>,
    ) {
        if !node.directory {
            // Source-tree paths came from SourceIndex and are already
            // canonical. Avoid another filesystem lookup for every file each
            // time a filtered tree is rebuilt.
            routes.insert(
                SourceId::from_indexed_path(&node.path),
                route.clone().into_boxed_slice(),
            );
            return;
        }

        for (index, child) in node.children.iter().enumerate() {
            let Ok(index) = u32::try_from(index) else {
                break;
            };

            route.push(index);
            visit(child, route, routes);
            route.pop();
        }
    }

    let mut routes = HashMap::new();

    for (index, root) in roots.iter().enumerate() {
        let Ok(index) = u32::try_from(index) else {
            break;
        };

        let mut route = vec![index];
        visit(root, &mut route, &mut routes);
    }

    routes
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
        return Some(canonical_source_path(reported));
    }

    let direct = roots
        .iter()
        .map(|root| root.join(reported.strip_prefix("/").unwrap_or(reported)))
        .filter(|candidate| candidate.is_file());

    match unique_existing_path(direct) {
        SourceResolution::Unique(path) => return Some(path),
        SourceResolution::Ambiguous => return None,
        SourceResolution::Missing => {}
    }

    let components: Vec<_> = reported.components().collect();

    if let Some(rustc) = components
        .iter()
        .position(|component| component.as_os_str() == "rustc")
        && components.len() > rustc + 2
    {
        let suffix: PathBuf = components[rustc + 2..].iter().collect();

        let candidates = roots
            .iter()
            .map(|root| root.join(&suffix))
            .filter(|candidate| candidate.is_file());

        match unique_existing_path(candidates) {
            SourceResolution::Unique(path) => return Some(path),
            SourceResolution::Ambiguous => return None,
            SourceResolution::Missing => {}
        }
    }

    let suffixes = (2..=components.len().min(7))
        .rev()
        .map(|length| components[components.len() - length..].iter().collect())
        .collect::<Vec<PathBuf>>();

    let mut child_directories = Vec::new();

    for root in roots {
        // Most compiler paths retain a useful suffix. Try those cheap direct
        // probes before enumerating child directories on every cache miss.
        let mut children = std::fs::read_dir(root)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .collect::<Vec<_>>();

        children.sort_unstable();
        children.truncate(256);
        child_directories.extend(children);
    }

    for suffix in &suffixes {
        let direct = roots
            .iter()
            .map(|root| root.join(suffix))
            .filter(|candidate| candidate.is_file());

        match unique_existing_path(direct) {
            SourceResolution::Unique(path) => return Some(path),
            SourceResolution::Ambiguous => return None,
            SourceResolution::Missing => {}
        }

        let nested = child_directories.iter().flat_map(|child| {
            [child.join(suffix), child.join("source").join(suffix)]
                .into_iter()
                .filter(|candidate| candidate.is_file())
        });

        match unique_existing_path(nested) {
            SourceResolution::Unique(path) => return Some(path),
            SourceResolution::Ambiguous => return None,
            SourceResolution::Missing => {}
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_SEARCHABLE_SOURCE_BYTES, SourceId, SourceIndex, SourceResolution, SourceSearchIndex,
        build_source_tree, build_source_tree_while, case_insensitive_match_column,
        discover_source_files, paths_match, resolve, search_source_files, search_source_paths,
    };
    use std::path::{Path, PathBuf};

    #[test]
    fn matches_absolute_and_debugger_relative_source_paths() {
        let open = Path::new("/home/user/project/src/main.rs");
        assert!(paths_match(None, open, "/home/user/project/src/main.rs"));
        assert!(!paths_match(None, open, "src/main.rs"));

        let index = SourceIndex::new(
            &[open.to_path_buf()],
            &[PathBuf::from("/home/user/project")],
        );

        assert!(paths_match(Some(&index), open, "src/main.rs"));
        assert!(!paths_match(Some(&index), open, "other/main.rs"));
    }

    #[test]
    fn source_index_rejects_ambiguous_suffixes_and_keeps_longer_unique_paths() {
        let directory = temporary_test_directory("source-index-ambiguity");
        let first = directory.join("one/src/shared/main.rs");
        let second = directory.join("two/src/shared/main.rs");
        std::fs::create_dir_all(first.parent().unwrap()).unwrap();
        std::fs::create_dir_all(second.parent().unwrap()).unwrap();
        std::fs::write(&first, "fn one() {}\n").unwrap();
        std::fs::write(&second, "fn two() {}\n").unwrap();

        let index = SourceIndex::new(
            &[first.clone(), second.clone()],
            std::slice::from_ref(&directory),
        );

        assert_eq!(
            index.resolve("src/shared/main.rs"),
            SourceResolution::Ambiguous
        );

        assert_eq!(
            index.resolve("one/src/shared/main.rs"),
            SourceResolution::Unique(std::fs::canonicalize(&first).unwrap())
        );

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn filesystem_resolution_never_selects_between_duplicate_direct_matches() {
        let directory = temporary_test_directory("source-resolution-ambiguity");
        let first_root = directory.join("one");
        let second_root = directory.join("two");
        let relative = Path::new("src/main.c");
        std::fs::create_dir_all(first_root.join("src")).unwrap();
        std::fs::create_dir_all(second_root.join("src")).unwrap();
        std::fs::write(first_root.join(relative), "int one;\n").unwrap();
        std::fs::write(second_root.join(relative), "int two;\n").unwrap();
        assert_eq!(resolve("src/main.c", &[first_root, second_root]), None);
        std::fs::remove_dir_all(directory).unwrap();
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
    fn source_tree_build_indexes_direct_routes_to_nested_files() {
        let root = PathBuf::from("/project");
        let target = root.join("src/parser/deep/mod.rs");
        let build = build_source_tree(
            &[root.join("src/main.rs"), target.clone()],
            std::slice::from_ref(&root),
            &[],
            "",
        );

        let route = build
            .file_routes
            .get(&SourceId::from_path(&target))
            .expect("nested file must have a direct route");

        let mut node = &build.roots[route[0] as usize];

        for &index in &route[1..] {
            node = &node.children[index as usize];
        }

        assert_eq!(node.path, target);
        assert!(!node.directory);
    }

    #[test]
    fn quick_open_preserves_ranking_deduplication_and_loaded_status() {
        let exact = PathBuf::from("/project/src/main.rs");
        let prefix = PathBuf::from("/project/src/main_helper.rs");
        let contains = PathBuf::from("/project/src/domain.rs");
        let path_only = PathBuf::from("/project/main/generated.rs");
        let loaded = SourceSearchIndex::new(std::slice::from_ref(&exact));
        let tree =
            SourceSearchIndex::new(&[path_only, contains.clone(), prefix.clone(), exact.clone()]);

        let matches = search_source_paths(&loaded, &tree, "MAIN", 3);

        assert_eq!(
            matches
                .iter()
                .map(|result| result.path.as_path())
                .collect::<Vec<_>>(),
            [exact.as_path(), prefix.as_path(), contains.as_path()]
        );

        assert!(matches[0].loaded);
        assert!(matches[1..].iter().all(|result| !result.loaded));
    }

    #[test]
    fn quick_open_applies_the_limit_during_top_n_selection() {
        let files = (0..1_000)
            .rev()
            .map(|index| PathBuf::from(format!("/project/src/file-{index:04}.rs")))
            .collect::<Vec<_>>();

        let matches = search_source_paths(
            &SourceSearchIndex::default(),
            &SourceSearchIndex::new(&files),
            "file",
            4,
        );

        assert_eq!(
            matches
                .iter()
                .map(|result| result.path.as_path())
                .collect::<Vec<_>>(),
            [
                Path::new("/project/src/file-0000.rs"),
                Path::new("/project/src/file-0001.rs"),
                Path::new("/project/src/file-0002.rs"),
                Path::new("/project/src/file-0003.rs"),
            ]
        );
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
