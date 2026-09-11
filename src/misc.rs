use std::{
    collections::{HashMap, HashSet},
    fmt::Write as _,
    fs::File,
    io,
    os::unix::fs::FileExt,
    path::{Path, PathBuf},
};

use crate::{
    debugger::{TargetArchitecture, TargetEndian},
    kernel::ProcessStartupSnapshot,
};

mod allocator;
mod call_abi;
mod core_dump;
mod heap;
mod locks;

use allocator::allocator_snapshot;
pub(crate) use allocator::*;
pub(crate) use call_abi::*;
pub(crate) use core_dump::{CoreDumpSnapshot, CoreMappedFile, CoreNote, read_core_dump};
use locks::read_locks;
pub(crate) use locks::{LockDependency, LockOwnership, LockSnapshot, LockWait};

pub(crate) use heap::{
    HeapDiscovery, HeapReadBudget, NativeHeapQuery, NativeHeapReadRequest, inspect_native_heap,
};
const MAX_AUXV_BYTES: usize = 1024 * 1024;
const MAX_MAPS_BYTES: usize = 16 * 1024 * 1024;
const MAX_MAPPINGS: usize = 8192;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct LiveMiscSnapshot {
    pub startup: ProcessStartupSnapshot,
    pub auxv: Vec<AuxvEntry>,
    pub allocator: AllocatorSnapshot,
    pub locks: Option<LockSnapshot>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AuxvEntry {
    pub kind: u64,
    pub name: String,
    pub value: u64,
    pub interpretation: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct HeapInspectionSnapshot {
    pub command: String,
    pub summary: String,
    pub diagnostic: Option<String>,
    pub rows: Vec<HeapInspectionRow>,
    pub truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HeapInspectionRow {
    pub kind: String,
    pub location: String,
    pub metric: String,
    pub state: String,
    pub details: String,
    /// A validated chunk base that can be passed back to the native targeted
    /// inspector. Arena addresses and other pointers deliberately leave this
    /// unset so the UI never treats an arbitrary heap-related address as a
    /// malloc chunk.
    pub inspect_address: Option<u64>,
}

const MAX_HEAP_INSPECTION_ROWS: usize = 8_192;
const MAX_HEAP_INSPECTION_CELL_CHARS: usize = 2_048;

fn heap_inspection_row(
    kind: &str,
    location: &str,
    metric: &str,
    state: &str,
    details: &str,
) -> HeapInspectionRow {
    HeapInspectionRow {
        kind: bounded_heap_cell(kind),
        location: bounded_heap_cell(location),
        metric: bounded_heap_cell(metric),
        state: bounded_heap_cell(state),
        details: bounded_heap_cell(details),
        inspect_address: None,
    }
}

fn bounded_heap_cell(value: &str) -> String {
    value.chars().take(MAX_HEAP_INSPECTION_CELL_CHARS).collect()
}

fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}

#[derive(Clone, Copy, Debug)]
struct Abi {
    architecture: TargetArchitecture,
    endian: TargetEndian,
    pointer_bits: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProcessMapping {
    pub start: u64,
    pub end: u64,
    pub permissions: String,
    pub path: String,
}

#[derive(Clone, Debug)]
pub(crate) struct ProcessAddressSpace {
    pub executable: Option<String>,
    pub interpreter: Option<String>,
    pub mappings: Vec<ProcessMapping>,
    pub capped: bool,
}

pub(crate) fn read_live_misc(
    pid: u32,
    debugger_pid: u32,
    include_locks: bool,
    allocator_probe: AllocatorProbe,
) -> Result<LiveMiscSnapshot, String> {
    crate::kernel::read_verified_local_proc(pid, debugger_pid, |target| {
        read_live_misc_at(
            pid,
            debugger_pid,
            include_locks,
            allocator_probe,
            target.root(),
        )
    })
}

fn read_live_misc_at(
    pid: u32,
    debugger_pid: u32,
    include_locks: bool,
    allocator_probe: AllocatorProbe,
    root: &Path,
) -> Result<LiveMiscSnapshot, String> {
    let abi = read_abi(&root.join("exe")).unwrap_or(Abi {
        architecture: TargetArchitecture::Unknown,
        endian: TargetEndian::Little,
        pointer_bits: usize::BITS,
    });

    let (maps, maps_capped) = read_maps(&root.join("maps"))?;
    let startup = crate::kernel::read_process_startup(pid, debugger_pid)?;
    let mut warnings = Vec::new();

    if maps_capped {
        warnings.push(format!(
            "Mapping-backed Misc data was capped at {MAX_MAPPINGS} VMAs"
        ));
    }

    let mut auxv = match crate::bounded::read_bytes(&root.join("auxv"), MAX_AUXV_BYTES) {
        Ok(bytes) => parse_auxv(&bytes, abi, &maps),
        Err(error) => {
            warnings.push(format!("Cannot read /proc/{pid}/auxv: {error}"));

            Vec::new()
        }
    };

    let executable = std::fs::read_link(root.join("exe"))
        .ok()
        .map(|path| path.to_string_lossy().into_owned());

    for entry in &mut auxv {
        match entry.kind {
            15 => entry.interpretation = abi.architecture.display_name().to_owned(),
            31 => {
                if let Some(executable) = executable.as_ref() {
                    entry.interpretation.clone_from(executable);
                }
            }
            _ => {}
        }
    }

    let allocator = allocator_snapshot(&maps, &allocator_probe);
    let locks = include_locks.then(|| {
        let endian = (abi.architecture != TargetArchitecture::Unknown).then_some(abi.endian);

        read_locks(root, abi.architecture, endian, &maps)
    });

    Ok(LiveMiscSnapshot {
        startup,
        auxv,
        allocator,
        locks,
        warnings,
    })
}

pub(crate) fn read_process_address_space(
    pid: u32,
    debugger_pid: u32,
) -> Result<ProcessAddressSpace, String> {
    crate::kernel::read_verified_local_proc(pid, debugger_pid, |target| {
        read_process_address_space_at(target.root())
    })
}

fn read_process_address_space_at(root: &Path) -> Result<ProcessAddressSpace, String> {
    let abi = read_abi(&root.join("exe"));

    let executable = std::fs::read_link(root.join("exe"))
        .ok()
        .map(|path| path.to_string_lossy().into_owned());

    let (mappings, capped) = read_maps(&root.join("maps"))?;

    let interpreter = abi
        .and_then(|abi| {
            crate::bounded::read_bytes(&root.join("auxv"), MAX_AUXV_BYTES)
                .ok()
                .and_then(|bytes| auxv_value(&bytes, abi, 7))
        })
        .and_then(|base| {
            mappings
                .iter()
                .find(|mapping| mapping.start <= base && base < mapping.end)
                .map(|mapping| mapping.path.clone())
                .filter(|path| !path.is_empty())
        });

    Ok(ProcessAddressSpace {
        executable,
        interpreter,
        mappings,
        capped,
    })
}

fn read_abi(path: &Path) -> Option<Abi> {
    let bytes = crate::bounded::read_prefix(path, 40).ok()?;
    let (architecture, endian, pointer_bits) = TargetArchitecture::from_elf_ident(&bytes)?;

    Some(Abi {
        architecture,
        endian,
        pointer_bits,
    })
}

fn read_maps(path: &Path) -> Result<(Vec<ProcessMapping>, bool), String> {
    let text = crate::bounded::read_string(path, MAX_MAPS_BYTES)
        .map_err(|error| format!("Cannot read {}: {error}", path.display()))?;

    let mut mappings = Vec::new();
    let mut capped = false;

    for (index, line) in text.lines().enumerate() {
        if index == MAX_MAPPINGS {
            capped = true;
            break;
        }

        let mut fields = line
            .splitn(6, char::is_whitespace)
            .filter(|value| !value.is_empty());

        let Some(range) = fields.next() else { continue };

        let Some((start, end)) = range.split_once('-') else {
            continue;
        };

        let (Ok(start), Ok(end)) = (u64::from_str_radix(start, 16), u64::from_str_radix(end, 16))
        else {
            continue;
        };

        let permissions = fields.next().unwrap_or("").to_owned();
        let _offset = fields.next();
        let _device = fields.next();
        let _inode = fields.next();
        let path = fields.next().unwrap_or("").trim().to_owned();

        mappings.push(ProcessMapping {
            start,
            end,
            permissions,
            path,
        });
    }

    Ok((mappings, capped))
}

fn parse_auxv(bytes: &[u8], abi: Abi, maps: &[ProcessMapping]) -> Vec<AuxvEntry> {
    let word = usize::try_from(abi.pointer_bits / 8)
        .unwrap_or(8)
        .clamp(4, 8);

    bytes
        .chunks_exact(word * 2)
        .take(512)
        .filter_map(|pair| {
            let kind = read_word(&pair[..word], abi.endian)?;
            let value = read_word(&pair[word..], abi.endian)?;

            (kind != 0).then(|| AuxvEntry {
                kind,
                name: auxv_name(kind).to_owned(),
                value,
                interpretation: interpret_auxv(kind, value, abi, maps),
            })
        })
        .collect()
}

fn auxv_value(bytes: &[u8], abi: Abi, wanted_kind: u64) -> Option<u64> {
    let word = usize::try_from(abi.pointer_bits / 8)
        .unwrap_or(8)
        .clamp(4, 8);

    bytes.chunks_exact(word * 2).take(512).find_map(|pair| {
        let kind = read_word(&pair[..word], abi.endian)?;
        let value = read_word(&pair[word..], abi.endian)?;

        (kind == wanted_kind).then_some(value)
    })
}

fn read_word(bytes: &[u8], endian: TargetEndian) -> Option<u64> {
    match bytes.len() {
        4 => {
            let bytes: [u8; 4] = bytes.try_into().ok()?;

            Some(u64::from(match endian {
                TargetEndian::Little => u32::from_le_bytes(bytes),
                TargetEndian::Big => u32::from_be_bytes(bytes),
            }))
        }
        8 => {
            let bytes: [u8; 8] = bytes.try_into().ok()?;

            Some(match endian {
                TargetEndian::Little => u64::from_le_bytes(bytes),
                TargetEndian::Big => u64::from_be_bytes(bytes),
            })
        }
        _ => None,
    }
}

fn auxv_name(kind: u64) -> &'static str {
    match kind {
        1 => "AT_IGNORE",
        2 => "AT_EXECFD",
        3 => "AT_PHDR",
        4 => "AT_PHENT",
        5 => "AT_PHNUM",
        6 => "AT_PAGESZ",
        7 => "AT_BASE",
        8 => "AT_FLAGS",
        9 => "AT_ENTRY",
        11 => "AT_UID",
        12 => "AT_EUID",
        13 => "AT_GID",
        14 => "AT_EGID",
        15 => "AT_PLATFORM",
        16 => "AT_HWCAP",
        17 => "AT_CLKTCK",
        23 => "AT_SECURE",
        24 => "AT_BASE_PLATFORM",
        25 => "AT_RANDOM",
        26 => "AT_HWCAP2",
        27 => "AT_RSEQ_FEATURE_SIZE",
        28 => "AT_RSEQ_ALIGN",
        29 => "AT_HWCAP3",
        30 => "AT_HWCAP4",
        31 => "AT_EXECFN",
        32 => "AT_SYSINFO",
        33 => "AT_SYSINFO_EHDR",
        51 => "AT_MINSIGSTKSZ",
        _ => "AT_UNKNOWN",
    }
}

fn interpret_auxv(kind: u64, value: u64, abi: Abi, maps: &[ProcessMapping]) -> String {
    match kind {
        4 | 5 | 11..=14 | 17 => value.to_string(),
        6 | 27 | 28 | 51 => format_bytes(value),
        8 | 29 | 30 => format!("bit mask 0x{value:x}"),
        16 | 26 => format_hwcap(abi.architecture, kind == 26, value),
        23 => if value == 0 { "disabled" } else { "enabled" }.to_owned(),
        3 | 7 | 9 | 15 | 24 | 25 | 31 | 32 | 33 => mapping_containing(maps, value).map_or_else(
            || format!("{}-bit pointer", abi.pointer_bits),
            |mapping| {
                let path = if mapping.path.is_empty() {
                    "anonymous"
                } else {
                    &mapping.path
                };

                format!("{} + 0x{:x}", path, value.saturating_sub(mapping.start))
            },
        ),
        _ => String::new(),
    }
}

fn format_hwcap(architecture: TargetArchitecture, second: bool, value: u64) -> String {
    let names: &[(u8, &str)] = match (architecture, second) {
        (TargetArchitecture::X86 | TargetArchitecture::X86_64, false) => &[
            (0, "fpu"),
            (1, "vme"),
            (2, "de"),
            (3, "pse"),
            (4, "tsc"),
            (5, "msr"),
            (6, "pae"),
            (7, "mce"),
            (8, "cx8"),
            (9, "apic"),
            (11, "sep"),
            (12, "mtrr"),
            (13, "pge"),
            (14, "mca"),
            (15, "cmov"),
            (16, "pat"),
            (17, "pse36"),
            (19, "clflush"),
            (23, "mmx"),
            (24, "fxsr"),
            (25, "sse"),
            (26, "sse2"),
            (28, "htt"),
        ],
        (TargetArchitecture::X86 | TargetArchitecture::X86_64, true) => {
            &[(0, "ring3mwait"), (1, "fsgsbase")]
        }
        (TargetArchitecture::Arm, false) => &[
            (0, "swp"),
            (1, "half"),
            (2, "thumb"),
            (3, "26bit"),
            (4, "fast_mult"),
            (5, "fpa"),
            (6, "vfp"),
            (7, "edsp"),
            (8, "java"),
            (9, "iwmmxt"),
            (10, "crunch"),
            (11, "thumbee"),
            (12, "neon"),
            (13, "vfpv3"),
            (14, "vfpv3d16"),
            (15, "tls"),
            (16, "vfpv4"),
            (17, "idiva"),
            (18, "idivt"),
            (19, "vfpd32"),
            (20, "lpae"),
            (21, "evtstrm"),
        ],
        (TargetArchitecture::AArch64, false) => &[
            (0, "fp"),
            (1, "asimd"),
            (2, "evtstrm"),
            (3, "aes"),
            (4, "pmull"),
            (5, "sha1"),
            (6, "sha2"),
            (7, "crc32"),
            (8, "atomics"),
            (9, "fphp"),
            (10, "asimdhp"),
            (11, "cpuid"),
            (12, "asimdrdm"),
            (13, "jscvt"),
            (14, "fcma"),
            (15, "lrcpc"),
            (16, "dcpop"),
            (17, "sha3"),
            (18, "sm3"),
            (19, "sm4"),
            (20, "asimddp"),
            (21, "sha512"),
            (22, "sve"),
            (23, "asimdfhm"),
            (24, "dit"),
            (25, "uscat"),
            (26, "ilrcpc"),
            (27, "flagm"),
            (28, "ssbs"),
            (29, "sb"),
            (30, "paca"),
            (31, "pacg"),
        ],
        (TargetArchitecture::AArch64, true) => &[
            (0, "dcpodp"),
            (1, "sve2"),
            (2, "sveaes"),
            (3, "svepmull"),
            (4, "svebitperm"),
            (5, "svesha3"),
            (6, "svesm4"),
            (7, "flagm2"),
            (8, "frint"),
            (9, "svei8mm"),
            (10, "svef32mm"),
            (11, "svef64mm"),
            (12, "svebf16"),
            (13, "i8mm"),
            (14, "bf16"),
            (15, "dgh"),
            (16, "rng"),
            (17, "bti"),
            (18, "mte"),
            (19, "ecv"),
            (20, "afp"),
            (21, "rpres"),
        ],
        _ => &[],
    };

    let decoded = names
        .iter()
        .filter_map(|(bit, name)| (value & (1_u64 << bit) != 0).then_some(*name))
        .collect::<Vec<_>>();

    if decoded.is_empty() {
        format!("bit mask 0x{value:x}")
    } else {
        format!("0x{value:x}  [{}]", decoded.join(" "))
    }
}

fn mapping_containing(maps: &[ProcessMapping], address: u64) -> Option<&ProcessMapping> {
    let index = maps
        .partition_point(|mapping| mapping.start <= address)
        .checked_sub(1)?;

    maps.get(index).filter(|mapping| address < mapping.end)
}

fn read_exact_at(file: &File, mut bytes: &mut [u8], mut offset: u64) -> io::Result<()> {
    while !bytes.is_empty() {
        let read = file.read_at(bytes, offset)?;

        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "unexpected end of file",
            ));
        }

        offset = offset
            .checked_add(read as u64)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "file offset overflow"))?;

        bytes = &mut bytes[read..];
    }

    Ok(())
}

fn read_u16(bytes: &[u8], endian: TargetEndian) -> Option<u16> {
    let bytes: [u8; 2] = bytes.try_into().ok()?;

    Some(match endian {
        TargetEndian::Little => u16::from_le_bytes(bytes),
        TargetEndian::Big => u16::from_be_bytes(bytes),
    })
}

fn read_u32(bytes: &[u8], endian: TargetEndian) -> Option<u32> {
    let bytes: [u8; 4] = bytes.try_into().ok()?;

    Some(match endian {
        TargetEndian::Little => u32::from_le_bytes(bytes),
        TargetEndian::Big => u32::from_be_bytes(bytes),
    })
}

fn read_i32(bytes: &[u8], endian: TargetEndian) -> Option<i32> {
    let bytes: [u8; 4] = bytes.try_into().ok()?;

    Some(match endian {
        TargetEndian::Little => i32::from_le_bytes(bytes),
        TargetEndian::Big => i32::from_be_bytes(bytes),
    })
}

fn read_u64(bytes: &[u8], endian: TargetEndian) -> Option<u64> {
    let bytes: [u8; 8] = bytes.try_into().ok()?;

    Some(match endian {
        TargetEndian::Little => u64::from_le_bytes(bytes),
        TargetEndian::Big => u64::from_be_bytes(bytes),
    })
}

fn format_bytes(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.2} MiB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heap_cells_are_unicode_bounded_and_not_implicitly_inspectable() {
        let text = "λ".repeat(MAX_HEAP_INSPECTION_CELL_CHARS + 32);
        let row = heap_inspection_row(&text, &text, &text, &text, &text);

        for cell in [
            &row.kind,
            &row.location,
            &row.metric,
            &row.state,
            &row.details,
        ] {
            assert_eq!(cell.chars().count(), MAX_HEAP_INSPECTION_CELL_CHARS);
        }

        assert_eq!(row.inspect_address, None);
    }

    fn abi64() -> Abi {
        Abi {
            architecture: TargetArchitecture::X86_64,
            endian: TargetEndian::Little,
            pointer_bits: 64,
        }
    }

    #[test]
    fn parses_auxv_without_a_terminator_or_unbounded_entries() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&6_u64.to_le_bytes());
        bytes.extend_from_slice(&4096_u64.to_le_bytes());
        bytes.extend_from_slice(&23_u64.to_le_bytes());
        bytes.extend_from_slice(&0_u64.to_le_bytes());
        bytes.extend_from_slice(&27_u64.to_le_bytes());
        bytes.extend_from_slice(&33_u64.to_le_bytes());
        bytes.extend_from_slice(&28_u64.to_le_bytes());
        bytes.extend_from_slice(&64_u64.to_le_bytes());

        assert_eq!(
            parse_auxv(&bytes, abi64(), &[]),
            vec![
                AuxvEntry {
                    kind: 6,
                    name: String::from("AT_PAGESZ"),
                    value: 4096,
                    interpretation: String::from("4.0 KiB"),
                },
                AuxvEntry {
                    kind: 23,
                    name: String::from("AT_SECURE"),
                    value: 0,
                    interpretation: String::from("disabled"),
                },
                AuxvEntry {
                    kind: 27,
                    name: String::from("AT_RSEQ_FEATURE_SIZE"),
                    value: 33,
                    interpretation: String::from("33 B"),
                },
                AuxvEntry {
                    kind: 28,
                    name: String::from("AT_RSEQ_ALIGN"),
                    value: 64,
                    interpretation: String::from("64 B"),
                },
            ]
        );
    }
}
