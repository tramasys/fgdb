use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::UNIX_EPOCH,
};

use goblin::elf::{
    Elf,
    program_header::PT_TLS,
    sym::{self, STT_TLS},
};

use super::{KernelSnapshot, KernelTlsModule, KernelTlsSymbol};

const MAX_ELF_BYTES: u64 = 256 * 1024 * 1024;
const MAX_MODULES: usize = 256;
const MAX_TLS_SYMBOLS_PER_MODULE: usize = 256;
const MAX_CACHE_ENTRIES: usize = 512;

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct CacheKey {
    path: String,
    bytes: u64,
    modified_nanos: u128,
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

static TLS_CACHE: OnceLock<Mutex<HashMap<CacheKey, Option<ParsedTls>>>> = OnceLock::new();

pub(super) fn populate_tls_metadata(snapshot: &mut KernelSnapshot, root: &Path) {
    for candidate in module_candidates(snapshot, root)
        .into_iter()
        .take(MAX_MODULES)
    {
        let Ok(Some(tls)) = cached_tls_analysis(&candidate.open_path, &candidate.display_path)
        else {
            continue;
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

fn cached_tls_analysis(path: &Path, display_path: &str) -> Result<Option<ParsedTls>, String> {
    let metadata = fs::metadata(path).map_err(|error| format!("Cannot inspect ELF: {error}"))?;
    if metadata.len() > MAX_ELF_BYTES {
        return Err(format!(
            "ELF exceeds the {} TLS analysis limit",
            super::format_bytes(MAX_ELF_BYTES)
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
    };
    let cache = TLS_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(cached) = cache
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .get(&key)
    {
        return Ok(cached.clone());
    }
    let bytes = fs::read(path).map_err(|error| format!("Cannot read ELF: {error}"))?;
    let parsed = parse_elf_tls(&bytes)?;
    let mut cache = cache.lock().unwrap_or_else(|poison| poison.into_inner());
    if cache.len() >= MAX_CACHE_ENTRIES {
        cache.clear();
    }
    cache.insert(key, parsed.clone());
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
