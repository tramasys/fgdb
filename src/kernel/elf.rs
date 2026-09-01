use std::{
    collections::{HashMap, HashSet},
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant, UNIX_EPOCH},
};

use goblin::elf::{
    Elf,
    program_header::PT_TLS,
    sym::{self, STT_TLS},
};

use super::{KernelSnapshot, KernelTlsModule, KernelTlsSymbol};

const MAX_ELF_BYTES: usize = 64 * 1024 * 1024;
const MAX_TOTAL_ELF_BYTES: usize = 128 * 1024 * 1024;
const MAX_SCAN_TIME: Duration = Duration::from_millis(500);
const MAX_MODULES: usize = 128;
const MAX_TLS_SYMBOLS_PER_MODULE: usize = 256;
// One fully populated process can contribute at most MAX_MODULES entries.
// Retaining more mostly preserves metadata for old inferiors and can keep tens
// of thousands of symbol strings alive after the user changes targets.
const MAX_CACHE_ENTRIES: usize = MAX_MODULES;

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct CacheKey {
    path: String,
    bytes: u64,
    modified_nanos: u128,
    device: u64,
    inode: u64,
}

#[derive(Clone, Debug)]
struct ParsedTls {
    template_address: u64,
    initialized_bytes: u64,
    total_bytes: u64,
    alignment: u64,
    symbol_count: usize,
    symbols: Vec<KernelTlsSymbol>,
}

#[derive(Clone, Debug)]
struct ModuleCandidate {
    display_path: String,
    open_path: PathBuf,
    role: String,
}

#[derive(Clone, Debug)]
struct CacheEntry {
    parsed: Option<ParsedTls>,
    last_used: u64,
}

struct ScanBudget {
    remaining_bytes: usize,
    deadline: Instant,
}

static TLS_CACHE: OnceLock<Mutex<HashMap<CacheKey, CacheEntry>>> = OnceLock::new();
static TLS_CACHE_CLOCK: AtomicU64 = AtomicU64::new(1);

pub(super) fn populate_tls_metadata(snapshot: &mut KernelSnapshot, root: &Path) {
    let candidates = module_candidates(snapshot, root);
    let mut budget = ScanBudget {
        remaining_bytes: MAX_TOTAL_ELF_BYTES,
        deadline: Instant::now() + MAX_SCAN_TIME,
    };
    let mut skipped = candidates.len().saturating_sub(MAX_MODULES);
    let mut failures = Vec::new();
    for candidate in candidates.into_iter().take(MAX_MODULES) {
        let tls =
            match cached_tls_analysis(&candidate.open_path, &candidate.display_path, &mut budget) {
                Ok(Some(tls)) => tls,
                Ok(None) => continue,
                Err(error) => {
                    skipped += 1;
                    if failures.len() < 3 {
                        failures.push(format!("{}: {error}", candidate.display_path));
                    }
                    continue;
                }
            };
        snapshot.tls_modules.push(KernelTlsModule {
            module: module_name(&candidate.display_path),
            path: candidate.display_path,
            role: candidate.role,
            template_address: tls.template_address,
            initialized_bytes: tls.initialized_bytes,
            total_bytes: tls.total_bytes,
            alignment: tls.alignment,
            symbol_count: tls.symbol_count,
            symbols: tls.symbols,
        });
    }
    if skipped > 0 {
        let detail = if failures.is_empty() {
            String::new()
        } else {
            format!(" ({})", failures.join(" · "))
        };
        snapshot.warnings.push(format!(
            "TLS metadata scan skipped {skipped} module(s) because of scan limits or read errors{detail}"
        ));
    }
    snapshot.tls_modules.sort_by_key(|module| {
        (
            module.role != "Main executable",
            module.module.to_ascii_lowercase(),
        )
    });
}

fn module_candidates(snapshot: &KernelSnapshot, root: &Path) -> Vec<ModuleCandidate> {
    let mut candidates = Vec::new();
    let executable = fs::read_link(root.join("exe")).ok();
    if let Some(executable) = executable.as_deref() {
        candidates.push(ModuleCandidate {
            display_path: display_path(executable),
            open_path: root.join("exe"),
            role: String::from("Main executable"),
        });
    }
    let mut seen = executable
        .as_deref()
        .map(display_path)
        .map(|path| HashSet::from([normalized_deleted_path(&path).to_owned()]))
        .unwrap_or_default();
    for mapping in snapshot
        .mappings
        .iter()
        .filter(|mapping| mapping.permissions.contains('x'))
    {
        let Some(path) = mapping.path.as_deref().filter(|path| path.starts_with('/')) else {
            continue;
        };
        let normalized = normalized_deleted_path(path);
        if !seen.insert(normalized.to_owned()) {
            continue;
        }
        let rooted_path = path_in_process_root(root, normalized);
        let mapped_file = root
            .join("map_files")
            .join(format!("{:x}-{:x}", mapping.start, mapping.end));
        candidates.push(ModuleCandidate {
            display_path: path.to_owned(),
            open_path: if rooted_path.exists() {
                rooted_path
            } else {
                mapped_file
            },
            role: module_role(normalized),
        });
    }
    candidates
}

fn path_in_process_root(root: &Path, path: &str) -> PathBuf {
    root.join("root").join(path.trim_start_matches('/'))
}

fn normalized_deleted_path(path: &str) -> &str {
    path.strip_suffix(" (deleted)").unwrap_or(path)
}

fn display_path(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn module_name(path: &str) -> String {
    Path::new(normalized_deleted_path(path))
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(path)
        .to_owned()
}

fn module_role(path: &str) -> String {
    let name = module_name(path);
    if name.starts_with("ld-") || name.starts_with("ld-linux") {
        String::from("Dynamic loader")
    } else if name.contains(".so") {
        String::from("Shared library")
    } else {
        String::from("Mapped ELF")
    }
}

fn cached_tls_analysis(
    path: &Path,
    display_path: &str,
    budget: &mut ScanBudget,
) -> Result<Option<ParsedTls>, String> {
    let metadata = fs::metadata(path).map_err(|error| format!("Cannot inspect ELF: {error}"))?;
    if metadata.len() > MAX_ELF_BYTES as u64 {
        return Err(format!(
            "ELF exceeds the {} TLS analysis limit",
            super::format_bytes(MAX_ELF_BYTES as u64)
        ));
    }
    let key = CacheKey {
        path: normalized_deleted_path(display_path).to_owned(),
        bytes: metadata.len(),
        modified_nanos: metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |duration| duration.as_nanos()),
        device: metadata.dev(),
        inode: metadata.ino(),
    };
    let cache = TLS_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    {
        let mut cache = cache.lock().unwrap_or_else(|poison| poison.into_inner());
        if let Some(cached) = cache.get_mut(&key) {
            cached.last_used = TLS_CACHE_CLOCK.fetch_add(1, Ordering::Relaxed);
            return Ok(cached.parsed.clone());
        }
    }
    if Instant::now() >= budget.deadline {
        return Err(String::from("TLS scan time budget exhausted"));
    }
    let file_bytes = usize::try_from(metadata.len())
        .map_err(|_| String::from("ELF size does not fit this platform"))?;
    if file_bytes > budget.remaining_bytes {
        return Err(String::from("TLS scan byte budget exhausted"));
    }
    budget.remaining_bytes -= file_bytes;
    let bytes = crate::bounded::read_bytes(path, MAX_ELF_BYTES)
        .map_err(|error| format!("Cannot read ELF: {error}"))?;
    let parsed = parse_elf_tls(&bytes)?;
    let mut cache = cache.lock().unwrap_or_else(|poison| poison.into_inner());
    if cache.len() >= MAX_CACHE_ENTRIES {
        let oldest = cache
            .iter()
            .min_by_key(|(_, entry)| entry.last_used)
            .map(|(key, _)| key.clone());
        if let Some(oldest) = oldest {
            cache.remove(&oldest);
        }
    }
    cache.insert(
        key,
        CacheEntry {
            parsed: parsed.clone(),
            last_used: TLS_CACHE_CLOCK.fetch_add(1, Ordering::Relaxed),
        },
    );
    drop(cache);
    Ok(parsed)
}

fn parse_elf_tls(bytes: &[u8]) -> Result<Option<ParsedTls>, String> {
    let elf = Elf::parse(bytes).map_err(|error| format!("Cannot parse ELF: {error}"))?;
    let Some(header) = elf
        .program_headers
        .iter()
        .find(|header| header.p_type == PT_TLS)
    else {
        return Ok(None);
    };
    let (symbol_count, symbols) = tls_symbols(&elf);
    Ok(Some(ParsedTls {
        template_address: header.p_vaddr,
        initialized_bytes: header.p_filesz,
        total_bytes: header.p_memsz,
        alignment: header.p_align,
        symbol_count,
        symbols,
    }))
}

fn tls_symbols(elf: &Elf<'_>) -> (usize, Vec<KernelTlsSymbol>) {
    let mut tls = HashMap::<(String, u64), KernelTlsSymbol>::new();
    for (table, strings) in [(&elf.dynsyms, &elf.dynstrtab), (&elf.syms, &elf.strtab)] {
        for symbol in table.iter().filter(|symbol| symbol.st_type() == STT_TLS) {
            let Some(name) = strings
                .get_at(symbol.st_name)
                .filter(|name| !name.is_empty())
            else {
                continue;
            };
            tls.entry((name.to_owned(), symbol.st_value))
                .or_insert_with(|| KernelTlsSymbol {
                    name: name.to_owned(),
                    offset: symbol.st_value,
                    size: symbol.st_size,
                    binding: symbol_binding(symbol.st_bind()).to_owned(),
                });
        }
    }
    let count = tls.len();
    let mut symbols = tls.into_values().collect::<Vec<_>>();
    symbols.sort_by_key(|symbol| (symbol.offset, symbol.name.clone()));
    symbols.truncate(MAX_TLS_SYMBOLS_PER_MODULE);
    (count, symbols)
}

fn symbol_binding(binding: u8) -> &'static str {
    match binding {
        sym::STB_LOCAL => "local",
        sym::STB_GLOBAL => "global",
        sym::STB_WEAK => "weak",
        sym::STB_GNU_UNIQUE => "unique",
        _ => "other",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_current_test_elf_for_tls_metadata() {
        let bytes = fs::read(std::env::current_exe().expect("test executable path"))
            .expect("read test executable");
        parse_elf_tls(&bytes).expect("parse test executable");
    }

    #[test]
    fn classifies_modules_and_deleted_paths() {
        assert_eq!(module_role("/usr/lib/libc.so.6"), "Shared library");
        assert_eq!(
            module_role("/usr/lib/ld-linux-x86-64.so.2"),
            "Dynamic loader"
        );
        assert_eq!(normalized_deleted_path("/tmp/demo (deleted)"), "/tmp/demo");
    }
}
