use std::{
    collections::VecDeque,
    io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    time::SystemTime,
};

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

use super::MAX_SEARCHABLE_SOURCE_BYTES;

const MAX_CACHED_SOURCE_FILES: usize = 64;
const MAX_CACHED_SOURCE_BYTES: usize = 64 * 1024 * 1024;
const MAX_CACHED_LINE_RANGES: usize = 1_000_000;
const MAX_STABLE_READ_ATTEMPTS: usize = 2;

#[derive(Clone)]
pub(super) struct CachedSource {
    pub(super) contents: Arc<String>,
    line_ranges: Option<Arc<[(usize, usize)]>>,
}

impl CachedSource {
    fn from_bytes(bytes: Vec<u8>) -> Self {
        let contents = Arc::new(match String::from_utf8(bytes) {
            Ok(contents) => contents,
            Err(error) => String::from_utf8_lossy(error.as_bytes()).into_owned(),
        });

        let line_ranges = source_line_ranges(&contents).map(Arc::from);

        Self {
            contents,
            line_ranges,
        }
    }

    fn weight(&self) -> usize {
        self.contents
            .len()
            .saturating_add(self.line_ranges.as_ref().map_or(0, |ranges| {
                ranges
                    .len()
                    .saturating_mul(std::mem::size_of::<(usize, usize)>())
            }))
    }

    pub(super) fn lines(&self) -> CachedSourceLines<'_> {
        self.line_ranges.as_ref().map_or_else(
            || CachedSourceLines::Scanned(self.contents.lines()),
            |ranges| CachedSourceLines::Indexed {
                contents: &self.contents,
                ranges: ranges.iter(),
            },
        )
    }
}

pub(super) enum CachedSourceLines<'a> {
    Indexed {
        contents: &'a str,
        ranges: std::slice::Iter<'a, (usize, usize)>,
    },
    Scanned(std::str::Lines<'a>),
}

impl<'a> Iterator for CachedSourceLines<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Indexed { contents, ranges } => {
                ranges.next().map(|(start, end)| &contents[*start..*end])
            }
            Self::Scanned(lines) => lines.next(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SourceFileIdentity {
    path: PathBuf,
    size: u64,
    modified: SystemTime,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

impl SourceFileIdentity {
    fn read(path: &Path) -> io::Result<Self> {
        let metadata = std::fs::metadata(path)?;

        Ok(Self {
            path: path.to_owned(),
            size: metadata.len(),
            modified: metadata.modified()?,
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
        })
    }
}

struct SourceCacheEntry {
    identity: SourceFileIdentity,
    source: CachedSource,
    weight: usize,
}

struct SourceFileCache {
    entries: VecDeque<SourceCacheEntry>,
    bytes: usize,
    max_files: usize,
    max_bytes: usize,
}

impl SourceFileCache {
    fn new(max_files: usize, max_bytes: usize) -> Self {
        Self {
            entries: VecDeque::new(),
            bytes: 0,
            max_files,
            max_bytes,
        }
    }

    fn get(&mut self, identity: &SourceFileIdentity) -> Option<CachedSource> {
        let index = self
            .entries
            .iter()
            .position(|entry| entry.identity == *identity)?;

        let entry = self.entries.remove(index)?;
        let source = entry.source.clone();
        self.entries.push_back(entry);

        Some(source)
    }

    fn insert(&mut self, identity: SourceFileIdentity, source: CachedSource) {
        let weight = source.weight();

        if self.max_files == 0 || weight > self.max_bytes {
            return;
        }

        let mut index = 0;

        while index < self.entries.len() {
            if self.entries[index].identity.path == identity.path {
                if let Some(entry) = self.entries.remove(index) {
                    self.bytes = self.bytes.saturating_sub(entry.weight);
                }
            } else {
                index += 1;
            }
        }

        self.bytes = self.bytes.saturating_add(weight);

        self.entries.push_back(SourceCacheEntry {
            identity,
            source,
            weight,
        });

        while self.entries.len() > self.max_files || self.bytes > self.max_bytes {
            let Some(entry) = self.entries.pop_front() else {
                break;
            };

            self.bytes = self.bytes.saturating_sub(entry.weight);
        }
    }
}

fn source_cache() -> &'static Mutex<SourceFileCache> {
    static CACHE: OnceLock<Mutex<SourceFileCache>> = OnceLock::new();

    CACHE.get_or_init(|| {
        Mutex::new(SourceFileCache::new(
            MAX_CACHED_SOURCE_FILES,
            MAX_CACHED_SOURCE_BYTES,
        ))
    })
}

pub(super) fn searchable_source(path: &Path) -> Option<CachedSource> {
    let (identity, source) = stable_read(
        || SourceFileIdentity::read(path).ok(),
        |identity| {
            if identity.size > MAX_SEARCHABLE_SOURCE_BYTES as u64 {
                return None;
            }

            if let Some(source) = source_cache()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(identity)
            {
                return Some(source);
            }

            #[cfg(test)]
            record_source_file_read(path);
            let bytes = crate::bounded::read_bytes(path, MAX_SEARCHABLE_SOURCE_BYTES).ok()?;

            Some(CachedSource::from_bytes(bytes))
        },
    )?;

    source_cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(identity, source.clone());

    Some(source)
}

fn stable_read<I, T>(
    mut identity: impl FnMut() -> Option<I>,
    mut read: impl FnMut(&I) -> Option<T>,
) -> Option<(I, T)>
where
    I: Eq,
{
    for _ in 0..MAX_STABLE_READ_ATTEMPTS {
        let before = identity()?;
        let value = read(&before)?;
        let after = identity()?;

        if before == after {
            return Some((before, value));
        }
    }

    None
}

fn source_line_ranges(contents: &str) -> Option<Vec<(usize, usize)>> {
    let bytes = contents.as_bytes();
    let mut ranges = Vec::new();
    let mut start = 0;

    for (position, byte) in bytes.iter().enumerate() {
        if *byte != b'\n' {
            continue;
        }

        let end = if position > start && bytes[position - 1] == b'\r' {
            position - 1
        } else {
            position
        };

        ranges.push((start, end));

        if ranges.len() >= MAX_CACHED_LINE_RANGES {
            return None;
        }

        start = position + 1;
    }

    if start < bytes.len() {
        ranges.push((start, bytes.len()));
    }

    Some(ranges)
}

#[cfg(test)]
fn source_file_reads_by_path() -> &'static Mutex<std::collections::HashMap<PathBuf, usize>> {
    static READS: OnceLock<Mutex<std::collections::HashMap<PathBuf, usize>>> = OnceLock::new();

    READS.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

#[cfg(test)]
fn record_source_file_read(path: &Path) {
    let mut reads = source_file_reads_by_path()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    *reads.entry(path.to_owned()).or_default() += 1;
}

#[cfg(test)]
fn source_file_reads(path: &Path) -> usize {
    source_file_reads_by_path()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(path)
        .copied()
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);

            let path = std::env::temp_dir().join(format!(
                "fgdb-source-cache-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));

            std::fs::create_dir_all(&path).unwrap();

            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn unchanged_files_are_read_once_and_changed_files_are_reloaded() {
        let directory = TestDirectory::new();
        let path = directory.0.join("cached.c");
        std::fs::write(&path, "first\n").unwrap();
        let reads = source_file_reads(&path);
        assert_eq!(&*searchable_source(&path).unwrap().contents, "first\n");
        assert_eq!(&*searchable_source(&path).unwrap().contents, "first\n");
        assert_eq!(source_file_reads(&path), reads + 1);
        std::fs::write(&path, "second value\n").unwrap();

        assert_eq!(
            &*searchable_source(&path).unwrap().contents,
            "second value\n"
        );

        assert_eq!(source_file_reads(&path), reads + 2);
    }

    #[test]
    fn failed_reads_are_not_cached() {
        let directory = TestDirectory::new();
        let path = directory.0.join("later.c");
        assert!(searchable_source(&path).is_none());
        std::fs::write(&path, "available later\n").unwrap();

        assert_eq!(
            &*searchable_source(&path).unwrap().contents,
            "available later\n"
        );
    }

    #[test]
    fn cache_enforces_file_and_byte_bounds() {
        let directory = TestDirectory::new();

        let identity = |name: &str| SourceFileIdentity {
            path: directory.0.join(name),
            size: 1,
            modified: SystemTime::UNIX_EPOCH,
            #[cfg(unix)]
            device: 1,
            #[cfg(unix)]
            inode: name.len() as u64,
        };

        let source = |text: &str| CachedSource::from_bytes(text.as_bytes().to_vec());
        let mut cache = SourceFileCache::new(2, 128);
        cache.insert(identity("a"), source("a"));
        cache.insert(identity("bb"), source("b"));
        cache.insert(identity("ccc"), source("c"));
        assert_eq!(cache.entries.len(), 2);
        assert!(cache.get(&identity("a")).is_none());
        assert!(cache.get(&identity("bb")).is_some());
        let mut tiny = SourceFileCache::new(4, 1);
        tiny.insert(identity("large"), source("too large"));
        assert!(tiny.entries.is_empty());
    }

    #[test]
    fn cache_identity_includes_size_and_modification_time() {
        let directory = TestDirectory::new();

        let original = SourceFileIdentity {
            path: directory.0.join("identity.c"),
            size: 1,
            modified: SystemTime::UNIX_EPOCH,
            #[cfg(unix)]
            device: 1,
            #[cfg(unix)]
            inode: 2,
        };

        let mut changed_size = original.clone();
        changed_size.size = 2;
        let mut changed_time = original.clone();
        changed_time.modified = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1);
        let mut cache = SourceFileCache::new(4, 1024);
        cache.insert(original, CachedSource::from_bytes(b"source".to_vec()));
        assert!(cache.get(&changed_size).is_none());
        assert!(cache.get(&changed_time).is_none());
    }

    #[test]
    fn cached_line_ranges_match_rust_line_semantics() {
        let source = CachedSource::from_bytes(b"one\r\n\ntwo".to_vec());
        let lines = source.lines().collect::<Vec<_>>();
        assert_eq!(lines, ["one", "", "two"]);
    }

    #[test]
    fn pathological_line_counts_fall_back_to_bounded_scanning_storage() {
        let source = CachedSource::from_bytes(vec![b'\n'; MAX_CACHED_LINE_RANGES]);
        assert!(source.line_ranges.is_none());
        assert_eq!(source.lines().count(), MAX_CACHED_LINE_RANGES);
    }

    #[test]
    fn stable_reads_return_after_one_attempt() {
        let mut identities = VecDeque::from([1, 1]);
        let reads = std::cell::Cell::new(0);
        let result = stable_read(
            || identities.pop_front(),
            |_| {
                reads.set(reads.get() + 1);

                Some("stable")
            },
        );

        assert_eq!(result, Some((1, "stable")));
        assert_eq!(reads.get(), 1);
    }

    #[test]
    fn an_identity_change_retries_once_and_returns_the_stable_snapshot() {
        let mut identities = VecDeque::from([1, 2, 2, 2]);
        let reads = std::cell::Cell::new(0);
        let result = stable_read(
            || identities.pop_front(),
            |identity| {
                reads.set(reads.get() + 1);

                Some(*identity)
            },
        );

        assert_eq!(result, Some((2, 2)));
        assert_eq!(reads.get(), 2);
    }

    #[test]
    fn repeatedly_changing_identities_reject_the_source_snapshot() {
        let mut identities = VecDeque::from([1, 2, 3, 4]);
        let reads = std::cell::Cell::new(0);
        let result = stable_read(
            || identities.pop_front(),
            |identity| {
                reads.set(reads.get() + 1);

                Some(*identity)
            },
        );

        assert_eq!(result, None);
        assert_eq!(reads.get(), MAX_STABLE_READ_ATTEMPTS);
    }
}
