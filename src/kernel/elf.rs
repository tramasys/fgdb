use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

use crate::{
    bounded::{FileIdentity, open_regular_file},
    performance::BoundedLruCache,
};

use goblin::elf::{
    Elf,
    program_header::PT_TLS,
    sym::{self, STT_TLS},
};

use super::{KernelSnapshot, KernelTlsModule, KernelTlsSymbol, WorkDeadline};

const MAX_ELF_BYTES: usize = 64 * 1024 * 1024;
const MAX_TOTAL_ELF_BYTES: usize = 128 * 1024 * 1024;
const MAX_SCAN_TIME: Duration = Duration::from_millis(500);
const MAX_MODULES: usize = 128;
const MAX_TLS_SYMBOLS_PER_MODULE: usize = 256;
// One fully populated process can contribute at most MAX_MODULES entries.
// Retaining more mostly preserves metadata for old inferiors and can keep tens
// of thousands of symbol strings alive after the user changes targets.
const MAX_CACHE_ENTRIES: usize = MAX_MODULES;

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

struct ScanBudget {
    remaining_bytes: usize,
    deadline: Instant,
}

static TLS_CACHE: OnceLock<Mutex<BoundedLruCache<FileIdentity, Option<ParsedTls>>>> =
    OnceLock::new();

pub(super) fn populate_tls_metadata(
    snapshot: &mut KernelSnapshot,
    root: &Path,
    work: &WorkDeadline,
) {
    let candidates = module_candidates(snapshot, root, work);

    let mut budget = ScanBudget {
        remaining_bytes: MAX_TOTAL_ELF_BYTES,
        deadline: Instant::now() + MAX_SCAN_TIME,
    };

    let mut skipped = candidates.len().saturating_sub(MAX_MODULES);
    let mut failures = Vec::new();

    for candidate in candidates.into_iter().take(MAX_MODULES) {
        if work.should_stop() {
            return;
        }

        let tls = match cached_tls_analysis(
            &candidate.open_path,
            &candidate.display_path,
            &mut budget,
            work,
        ) {
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
            format!(" ({})", failures.join("  "))
        };

        snapshot.warnings.push(format!(
            "TLS metadata scan skipped {skipped} module(s) because of scan limits or read errors{detail}"
        ));
    }

    snapshot.tls_modules.sort_by_cached_key(|module| {
        (
            module.role != "Main executable",
            module.module.to_ascii_lowercase(),
        )
    });
}

fn module_candidates(
    snapshot: &KernelSnapshot,
    root: &Path,
    work: &WorkDeadline,
) -> Vec<ModuleCandidate> {
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

    for (index, mapping) in snapshot
        .mappings
        .iter()
        .filter(|mapping| mapping.permissions.contains('x'))
        .enumerate()
    {
        if index % 256 == 0 && work.should_stop() {
            break;
        }

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
            open_path: if path == normalized && rooted_path.exists() {
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
    work: &WorkDeadline,
) -> Result<Option<ParsedTls>, String> {
    work.check()?;
    let (file, metadata) =
        open_regular_file(path).map_err(|error| format!("Cannot inspect ELF: {error}"))?;

    if metadata.len() > MAX_ELF_BYTES as u64 {
        return Err(format!(
            "ELF exceeds the {} TLS analysis limit",
            super::format_bytes(MAX_ELF_BYTES as u64)
        ));
    }

    let identity = |metadata: &fs::Metadata| {
        FileIdentity::from_metadata(Path::new(normalized_deleted_path(display_path)), metadata)
            .map_err(|error| format!("Cannot identify ELF: {error}"))
    };

    let key = identity(&metadata)?;
    let cache = TLS_CACHE.get_or_init(|| Mutex::new(BoundedLruCache::new(MAX_CACHE_ENTRIES)));

    {
        let mut cache = cache.lock().unwrap_or_else(|poison| poison.into_inner());

        if let Some(cached) = cache.get_cloned(&key) {
            return Ok(cached);
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

    let bytes = crate::bounded::read_file(&file, path, file_bytes)
        .map_err(|error| format!("Cannot read ELF: {error}"))?;

    work.check()?;
    let parsed = parse_elf_tls(&bytes)?;
    work.check()?;

    let after = file
        .metadata()
        .map_err(|error| format!("Cannot recheck ELF: {error}"))?;

    if key != identity(&after)? {
        return Err(String::from(
            "ELF changed while its TLS metadata was being read",
        ));
    }

    cache
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .insert(key, parsed.clone());

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
    collect_tls_symbols(
        [(&elf.dynsyms, &elf.dynstrtab), (&elf.syms, &elf.strtab)]
            .into_iter()
            .flat_map(|(table, strings)| {
                table
                    .iter()
                    .filter(|symbol| symbol.st_type() == STT_TLS)
                    .filter_map(|symbol| {
                        let name = strings
                            .get_at(symbol.st_name)
                            .filter(|name| !name.is_empty())?;

                        Some((name, symbol))
                    })
            }),
    )
}

fn collect_tls_symbols<'a>(
    symbols: impl IntoIterator<Item = (&'a str, sym::Sym)>,
) -> (usize, Vec<KernelTlsSymbol>) {
    let mut tls = HashMap::new();

    for (name, symbol) in symbols {
        tls.entry((symbol.st_value, name)).or_insert(symbol);
    }

    let count = tls.len();
    let mut symbols = tls.into_iter().collect::<Vec<_>>();

    // Count every unique symbol, but only sort and allocate display strings
    // for the bounded prefix that the snapshot can actually retain.
    if symbols.len() > MAX_TLS_SYMBOLS_PER_MODULE {
        symbols.select_nth_unstable_by_key(MAX_TLS_SYMBOLS_PER_MODULE, |(key, _)| *key);
        symbols.truncate(MAX_TLS_SYMBOLS_PER_MODULE);
    }

    symbols.sort_unstable_by_key(|(key, _)| *key);

    let symbols = symbols
        .into_iter()
        .map(|((offset, name), symbol)| KernelTlsSymbol {
            name: name.to_owned(),
            offset,
            size: symbol.st_size,
            binding: symbol_binding(symbol.st_bind()).to_owned(),
        })
        .collect();

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
    #[ignore = "manual release-mode throughput benchmark"]
    fn benchmark_tls_symbol_collection() {
        let symbols = (0..20_000)
            .map(|index| {
                (
                    format!("tls_symbol_{index:05}"),
                    sym::Sym {
                        st_value: (index * 7919) % 20_000,
                        st_size: 8,
                        ..Default::default()
                    },
                )
            })
            .collect::<Vec<_>>();

        crate::benchmarks::measure("tls/symbols-20k", || {
            collect_tls_symbols(
                symbols
                    .iter()
                    .map(|(name, symbol)| (name.as_str(), *symbol)),
            )
        });
    }

    #[test]
    fn tls_symbols_are_deduplicated_ordered_and_bounded() {
        let names = (0..MAX_TLS_SYMBOLS_PER_MODULE + 100)
            .map(|index| format!("symbol_{index:04}"))
            .collect::<Vec<_>>();

        let symbols = names.iter().enumerate().rev().map(|(index, name)| {
            (
                name.as_str(),
                sym::Sym {
                    st_value: index as u64 % 7,
                    st_size: 4,
                    ..Default::default()
                },
            )
        });

        let duplicate = (
            names[0].as_str(),
            sym::Sym {
                st_value: 0,
                st_size: 99,
                ..Default::default()
            },
        );

        let (count, retained) = collect_tls_symbols(symbols.chain([duplicate]));
        assert_eq!(count, names.len());
        assert_eq!(retained.len(), MAX_TLS_SYMBOLS_PER_MODULE);

        let mut ordered = names
            .iter()
            .enumerate()
            .map(|(index, name)| (index as u64 % 7, name))
            .collect::<Vec<_>>();

        ordered.sort_unstable();

        for (symbol, (offset, name)) in retained.iter().zip(ordered) {
            assert_eq!(symbol.offset, offset);
            assert_eq!(&symbol.name, name);
            assert_eq!(symbol.size, 4);
        }

        for length in [0, 1, MAX_TLS_SYMBOLS_PER_MODULE] {
            let (count, retained) = collect_tls_symbols(
                names[..length]
                    .iter()
                    .map(|name| (name.as_str(), sym::Sym::default())),
            );

            assert_eq!(count, length);
            assert_eq!(retained.len(), length);
        }
    }

    fn tls_elf(total: u64) -> Vec<u8> {
        let mut bytes = vec![0; 120];
        bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
        bytes[16..18].copy_from_slice(&3_u16.to_le_bytes());
        bytes[18..20].copy_from_slice(&62_u16.to_le_bytes());
        bytes[20..24].copy_from_slice(&1_u32.to_le_bytes());
        bytes[32..40].copy_from_slice(&64_u64.to_le_bytes());
        bytes[52..54].copy_from_slice(&64_u16.to_le_bytes());
        bytes[54..56].copy_from_slice(&56_u16.to_le_bytes());
        bytes[56..58].copy_from_slice(&1_u16.to_le_bytes());
        bytes[64..68].copy_from_slice(&PT_TLS.to_le_bytes());
        bytes[104..112].copy_from_slice(&total.to_le_bytes());
        bytes[112..120].copy_from_slice(&8_u64.to_le_bytes());
        bytes
    }

    #[test]
    fn tls_cache_reloads_same_size_edits_with_restored_modification_time() {
        let root = gtk::glib::mkdtemp(std::env::temp_dir().join("fgdb-tls-cache-XXXXXX")).unwrap();
        let path = root.join("module.so");
        fs::write(&path, tls_elf(8)).unwrap();
        let modified = fs::metadata(&path).unwrap().modified().unwrap();

        let mut budget = ScanBudget {
            remaining_bytes: MAX_TOTAL_ELF_BYTES,
            deadline: Instant::now() + Duration::from_secs(5),
        };

        let work = WorkDeadline::new(Duration::from_secs(5));

        assert_eq!(
            cached_tls_analysis(&path, path.to_str().unwrap(), &mut budget, &work)
                .unwrap()
                .unwrap()
                .total_bytes,
            8
        );

        fs::write(&path, tls_elf(16)).unwrap();

        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(modified))
            .unwrap();

        let result = cached_tls_analysis(&path, path.to_str().unwrap(), &mut budget, &work);
        fs::remove_file(&path).unwrap();
        fs::remove_dir(root).unwrap();
        assert_eq!(result.unwrap().unwrap().total_bytes, 16);
    }

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

    #[test]
    fn deleted_modules_never_use_a_replacement_at_the_original_path() {
        let root =
            gtk::glib::mkdtemp(std::env::temp_dir().join("fgdb-tls-deleted-XXXXXX")).unwrap();
        fs::create_dir(root.join("root")).unwrap();
        fs::write(root.join("root/module.so"), tls_elf(8)).unwrap();

        let snapshot = KernelSnapshot {
            mappings: vec![super::super::KernelMapping {
                start: 0x1000,
                end: 0x2000,
                permissions: "r-xp".into(),
                path: Some("/module.so (deleted)".into()),
                ..Default::default()
            }],
            ..Default::default()
        };

        let candidates =
            module_candidates(&snapshot, &root, &WorkDeadline::new(Duration::from_secs(1)));
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].open_path, root.join("map_files/1000-2000"));
        fs::remove_file(root.join("root/module.so")).unwrap();
        fs::remove_dir(root.join("root")).unwrap();
        fs::remove_dir(root).unwrap();
    }
}
