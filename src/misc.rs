use std::{
    fs::File,
    io,
    os::unix::fs::FileExt,
    path::{Path, PathBuf},
};

use crate::{
    debugger::{Register, StackFrame, TargetArchitecture, TargetEndian},
    kernel::ProcessStartupSnapshot,
};

const MAX_AUXV_BYTES: usize = 1024 * 1024;
const MAX_MAPS_BYTES: usize = 16 * 1024 * 1024;
const MAX_TASKS: usize = 4096;
const MAX_MAPPINGS: usize = 8192;
const MAX_CORE_PROGRAM_HEADERS: usize = 65_536;
const MAX_CORE_PROGRAM_HEADER_BYTES: u64 = 64 * 1024 * 1024;
const MAX_CORE_NOTES: usize = 65_536;
const MAX_CORE_NOTE_BYTES: usize = 16 * 1024 * 1024;
const MAX_CORE_NOTE_NAME_BYTES: usize = 4096;
const MAX_CORE_FILES: usize = 65_536;

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
pub(crate) struct AllocatorSnapshot {
    pub implementation: String,
    pub heap_bytes: u64,
    pub anonymous_writable_bytes: u64,
    pub regions: Vec<AllocatorRegion>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AllocatorRegion {
    pub start: u64,
    pub end: u64,
    pub permissions: String,
    pub role: String,
    pub path: String,
}

impl AllocatorRegion {
    pub fn size(&self) -> u64 {
        self.end.saturating_sub(self.start)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct LockSnapshot {
    pub threads_scanned: usize,
    pub waits: Vec<LockWait>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LockWait {
    pub tid: u32,
    pub thread: String,
    pub state: String,
    pub address: Option<u64>,
    pub operation: String,
    pub expected: Option<u64>,
    pub details: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct CallAbiSnapshot {
    pub architecture: String,
    pub calling_convention: String,
    pub pointer_bits: u32,
    pub current_frame: Option<CallAbiFrame>,
    pub contract: Vec<CallAbiFact>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CallAbiFrame {
    pub level: u32,
    pub address: String,
    pub function: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CallAbiRegister {
    pub role: String,
    pub name: String,
    pub value: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CallAbiFact {
    pub aspect: String,
    pub value: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CallAbiPhase {
    OutgoingCall { target: Option<String> },
    IncomingEntry { function: String },
    Returning,
    Returned { target: Option<String> },
    Sequential,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct CallAbiTransfer {
    pub context: String,
    pub registers: Vec<CallAbiRegister>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct CoreDumpSnapshot {
    pub path: PathBuf,
    pub size: u64,
    pub architecture: String,
    pub class: String,
    pub endian: String,
    pub signal: Option<i32>,
    pub signal_code: Option<i32>,
    pub fault_address: Option<u64>,
    pub process_name: Option<String>,
    pub command: Option<String>,
    pub pid: Option<u32>,
    pub threads: Vec<u32>,
    pub auxv: Vec<AuxvEntry>,
    pub files: Vec<CoreMappedFile>,
    pub notes: Vec<CoreNote>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CoreMappedFile {
    pub start: u64,
    pub end: u64,
    pub file_offset: u64,
    pub path: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CoreNote {
    pub owner: String,
    pub kind: String,
    pub bytes: u64,
}

#[derive(Clone, Copy, Debug)]
struct Abi {
    architecture: TargetArchitecture,
    endian: TargetEndian,
    pointer_bits: u32,
}

#[derive(Clone, Debug)]
pub(crate) struct ProcessMapping {
    pub start: u64,
    pub end: u64,
    pub permissions: String,
    pub path: String,
}

#[derive(Clone, Debug)]
pub(crate) struct ProcessAddressSpace {
    pub executable: Option<String>,
    pub mappings: Vec<ProcessMapping>,
    pub capped: bool,
}

pub(crate) fn read_live_misc(
    pid: u32,
    debugger_pid: u32,
    include_locks: bool,
) -> Result<LiveMiscSnapshot, String> {
    let root = crate::kernel::verified_proc_root(pid, debugger_pid)?;
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
    let allocator = allocator_snapshot(&maps);
    let locks = include_locks.then(|| read_locks(&root, abi.architecture));
    crate::kernel::verified_proc_root(pid, debugger_pid)?;
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
    let root = crate::kernel::verified_proc_root(pid, debugger_pid)?;
    let executable = std::fs::read_link(root.join("exe"))
        .ok()
        .map(|path| path.to_string_lossy().into_owned());
    let (mappings, capped) = read_maps(&root.join("maps"))?;
    crate::kernel::verified_proc_root(pid, debugger_pid)?;
    Ok(ProcessAddressSpace {
        executable,
        mappings,
        capped,
    })
}

pub(crate) fn call_abi_snapshot(
    architecture: TargetArchitecture,
    pointer_bits: u32,
    selected_level: u32,
    frames: &[StackFrame],
) -> CallAbiSnapshot {
    let frames = frames
        .iter()
        .map(|frame| CallAbiFrame {
            level: frame.level,
            address: frame.address.clone(),
            function: frame.function.clone(),
        })
        .collect::<Vec<_>>();
    CallAbiSnapshot {
        architecture: architecture.display_name().to_owned(),
        calling_convention: linux_calling_convention(architecture, pointer_bits).to_owned(),
        pointer_bits,
        current_frame: frames
            .iter()
            .find(|frame| frame.level == selected_level)
            .cloned()
            .or_else(|| frames.first().cloned()),
        contract: call_abi_contract(architecture),
    }
}

pub(crate) fn call_abi_transfer(
    architecture: TargetArchitecture,
    phase: CallAbiPhase,
    registers: &[Register],
) -> CallAbiTransfer {
    let context = match &phase {
        CallAbiPhase::OutgoingCall { target } => {
            transfer_context("OUTGOING CALL", target.as_deref())
        }
        CallAbiPhase::IncomingEntry { function } => format!("FUNCTION ENTRY  ·  {function}"),
        CallAbiPhase::Returning => String::from("FUNCTION RETURN  ·  outgoing return value"),
        CallAbiPhase::Returned { target } => {
            transfer_context("RETURNED FROM CALL", target.as_deref())
        }
        CallAbiPhase::Sequential => String::from("No ABI call transfer at the current instruction"),
    };
    let mut selected = Vec::new();
    let mut add = |role: String, name: &str| {
        if let Some(register) = registers.iter().find(|register| register.name == name) {
            selected.push(CallAbiRegister {
                role,
                name: format!("${name}"),
                value: register.value.clone(),
            });
        }
    };
    match phase {
        CallAbiPhase::OutgoingCall { .. } => {
            for (index, name) in architecture.call_argument_registers().iter().enumerate() {
                add(
                    format!("Outgoing integer / pointer slot {}", index + 1),
                    name,
                );
            }
            add_stack_pointer(&mut add, architecture, registers, "Call-site stack pointer");
        }
        CallAbiPhase::IncomingEntry { .. } => {
            for (index, name) in architecture.call_argument_registers().iter().enumerate() {
                add(
                    format!("Incoming integer / pointer slot {}", index + 1),
                    name,
                );
            }
            add_stack_pointer(&mut add, architecture, registers, "Entry stack pointer");
        }
        CallAbiPhase::Returning | CallAbiPhase::Returned { .. } => {
            for (index, name) in architecture.call_return_registers().iter().enumerate() {
                let role = if index == 0 {
                    "Primary return register"
                } else {
                    "Secondary / wide return register"
                };
                add(role.to_owned(), name);
            }
            add_stack_pointer(
                &mut add,
                architecture,
                registers,
                "Return-site stack pointer",
            );
        }
        CallAbiPhase::Sequential => {}
    }
    CallAbiTransfer {
        context,
        registers: selected,
    }
}

fn add_stack_pointer(
    add: &mut impl FnMut(String, &str),
    architecture: TargetArchitecture,
    registers: &[Register],
    role: &str,
) {
    if let Some(name) =
        architecture.stack_pointer(registers.iter().map(|register| register.name.as_str()))
    {
        add(role.to_owned(), name);
    }
}

fn transfer_context(kind: &str, target: Option<&str>) -> String {
    target.map_or_else(|| kind.to_owned(), |target| format!("{kind}  ·  {target}"))
}

fn call_abi_contract(architecture: TargetArchitecture) -> Vec<CallAbiFact> {
    let argument_registers = architecture.call_argument_registers();
    let return_registers = architecture.call_return_registers();
    vec![
        CallAbiFact {
            aspect: String::from("Integer / pointer arguments"),
            value: if argument_registers.is_empty() {
                match architecture {
                    TargetArchitecture::X86 => String::from("stack"),
                    _ => String::from("target-defined"),
                }
            } else {
                format_register_list(argument_registers)
            },
        },
        CallAbiFact {
            aspect: String::from("Integer / pointer return"),
            value: if return_registers.is_empty() {
                String::from("target-defined")
            } else {
                format_register_list(return_registers)
            },
        },
        CallAbiFact {
            aspect: String::from("Call linkage"),
            value: call_linkage(architecture).to_owned(),
        },
        CallAbiFact {
            aspect: String::from("Stack contract"),
            value: call_stack_contract(architecture).to_owned(),
        },
    ]
}

fn format_register_list(registers: &[&str]) -> String {
    registers
        .iter()
        .map(|register| format!("${register}"))
        .collect::<Vec<_>>()
        .join("  ")
}

fn call_linkage(architecture: TargetArchitecture) -> &'static str {
    match architecture {
        TargetArchitecture::X86 | TargetArchitecture::X86_64 => "return address pushed on stack",
        TargetArchitecture::Arm | TargetArchitecture::AArch64 => "link register $lr",
        TargetArchitecture::RiscV32 | TargetArchitecture::RiscV64 => "return address register $ra",
        TargetArchitecture::Mips32 | TargetArchitecture::Mips64 => "return address register $ra",
        TargetArchitecture::PowerPc32 | TargetArchitecture::PowerPc64 => "link register $lr",
        TargetArchitecture::S390 | TargetArchitecture::S390x => "return address register $r14",
        TargetArchitecture::LoongArch64 => "return address register $ra",
        TargetArchitecture::Unknown => "target-defined",
    }
}

fn call_stack_contract(architecture: TargetArchitecture) -> &'static str {
    match architecture {
        TargetArchitecture::X86 => "downward-growing · arguments continue on stack",
        TargetArchitecture::X86_64 => {
            "downward-growing · 16-byte call alignment · 128-byte red zone"
        }
        TargetArchitecture::Arm => "downward-growing · 8-byte public-interface alignment",
        TargetArchitecture::AArch64 => "downward-growing · 16-byte alignment",
        TargetArchitecture::RiscV32 | TargetArchitecture::RiscV64 => {
            "downward-growing · 16-byte alignment"
        }
        TargetArchitecture::Mips32 | TargetArchitecture::Mips64 => {
            "downward-growing · ABI argument area"
        }
        TargetArchitecture::PowerPc32 | TargetArchitecture::PowerPc64 => {
            "downward-growing · stack-frame back chain"
        }
        TargetArchitecture::S390 | TargetArchitecture::S390x => {
            "downward-growing · register save area"
        }
        TargetArchitecture::LoongArch64 => "downward-growing · 16-byte alignment",
        TargetArchitecture::Unknown => "target-defined",
    }
}

fn linux_calling_convention(architecture: TargetArchitecture, pointer_bits: u32) -> &'static str {
    match (architecture, pointer_bits) {
        (TargetArchitecture::X86, _) => "System V i386 ABI",
        (TargetArchitecture::X86_64, 32) => "System V AMD64 x32 ABI",
        (TargetArchitecture::X86_64, _) => "System V AMD64 ABI",
        (TargetArchitecture::Arm, _) => "AAPCS32",
        (TargetArchitecture::AArch64, 32) => "AAPCS64 ILP32",
        (TargetArchitecture::AArch64, _) => "AAPCS64",
        (TargetArchitecture::RiscV32 | TargetArchitecture::RiscV64, _) => "RISC-V ELF psABI",
        (TargetArchitecture::Mips32, _) => "MIPS o32 ABI",
        (TargetArchitecture::Mips64, 32) => "MIPS n32 ABI",
        (TargetArchitecture::Mips64, _) => "MIPS n64 ABI",
        (TargetArchitecture::PowerPc32 | TargetArchitecture::PowerPc64, _) => "PowerPC ELF ABI",
        (TargetArchitecture::S390 | TargetArchitecture::S390x, _) => "zSeries ELF ABI",
        (TargetArchitecture::LoongArch64, _) => "LoongArch ELF psABI",
        (TargetArchitecture::Unknown, _) => "calling convention unknown",
    }
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
        3 | 7 | 9 | 15 | 24 | 25 | 31 | 32 | 33 => maps
            .iter()
            .find(|mapping| value >= mapping.start && value < mapping.end)
            .map_or_else(
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

fn allocator_snapshot(maps: &[ProcessMapping]) -> AllocatorSnapshot {
    let implementation = maps.iter().find_map(|mapping| {
        let path = mapping.path.to_ascii_lowercase();
        [
            ("jemalloc", "jemalloc"),
            ("mimalloc", "mimalloc"),
            ("tcmalloc", "tcmalloc"),
            ("libc.musl", "musl malloc"),
            ("libc.so", "glibc malloc / libc allocator"),
        ]
        .into_iter()
        .find_map(|(needle, name)| path.contains(needle).then_some(name))
    });
    let mut heap_bytes = 0_u64;
    let mut anonymous_writable_bytes = 0_u64;
    let mut regions = Vec::new();
    for mapping in maps {
        let writable_private = mapping.permissions.starts_with("rw")
            && mapping.permissions.as_bytes().get(3) == Some(&b'p');
        let role = if mapping.path == "[heap]" {
            heap_bytes = heap_bytes.saturating_add(mapping.end.saturating_sub(mapping.start));
            Some("brk heap")
        } else if writable_private && mapping.path.is_empty() {
            anonymous_writable_bytes =
                anonymous_writable_bytes.saturating_add(mapping.end.saturating_sub(mapping.start));
            Some("anonymous writable (possible arena)")
        } else if ["jemalloc", "mimalloc", "tcmalloc", "libc.so", "libc.musl"]
            .iter()
            .any(|needle| mapping.path.to_ascii_lowercase().contains(needle))
        {
            Some("allocator runtime")
        } else {
            None
        };
        if let Some(role) = role {
            regions.push(AllocatorRegion {
                start: mapping.start,
                end: mapping.end,
                permissions: mapping.permissions.clone(),
                role: role.to_owned(),
                path: if mapping.path.is_empty() {
                    String::from("anonymous")
                } else {
                    mapping.path.clone()
                },
            });
        }
    }
    AllocatorSnapshot {
        implementation: implementation.unwrap_or("not identified").to_owned(),
        heap_bytes,
        anonymous_writable_bytes,
        regions,
    }
}

fn read_locks(root: &Path, architecture: TargetArchitecture) -> LockSnapshot {
    let mut snapshot = LockSnapshot::default();
    let task_root = root.join("task");
    let entries = match std::fs::read_dir(&task_root) {
        Ok(entries) => entries,
        Err(error) => {
            snapshot
                .warnings
                .push(format!("Cannot enumerate {}: {error}", task_root.display()));
            return snapshot;
        }
    };
    for (index, entry) in entries.flatten().enumerate() {
        if index == MAX_TASKS {
            snapshot
                .warnings
                .push(format!("Lock inspection was capped at {MAX_TASKS} threads"));
            break;
        }
        let Some(tid) = entry
            .file_name()
            .to_str()
            .and_then(|value| value.parse::<u32>().ok())
        else {
            continue;
        };
        snapshot.threads_scanned += 1;
        let task = entry.path();
        let thread = crate::bounded::read_string(&task.join("comm"), 4096)
            .unwrap_or_default()
            .trim()
            .to_owned();
        let state = read_thread_state(&task.join("status"));
        let wchan = crate::bounded::read_string(&task.join("wchan"), 4096)
            .unwrap_or_default()
            .trim()
            .to_owned();
        let syscall = crate::bounded::read_string(&task.join("syscall"), 64 * 1024).ok();
        if let Some(wait) = syscall
            .as_deref()
            .and_then(|line| parse_lock_wait(tid, &thread, &state, &wchan, line, architecture))
        {
            snapshot.waits.push(wait);
        } else if wchan.contains("futex") {
            snapshot.waits.push(LockWait {
                tid,
                thread,
                state,
                address: None,
                operation: String::from("futex wait"),
                expected: None,
                details: format!("kernel wait channel {wchan}; syscall arguments unavailable"),
            });
        }
    }
    snapshot
        .waits
        .sort_by_key(|wait| (wait.address.unwrap_or(u64::MAX), wait.tid));
    snapshot
}

fn read_thread_state(path: &Path) -> String {
    crate::bounded::read_string(path, 64 * 1024)
        .ok()
        .and_then(|text| {
            text.lines()
                .find_map(|line| line.strip_prefix("State:").map(str::trim))
                .map(str::to_owned)
        })
        .unwrap_or_else(|| String::from("unknown"))
}

fn parse_lock_wait(
    tid: u32,
    thread: &str,
    state: &str,
    wchan: &str,
    syscall: &str,
    architecture: TargetArchitecture,
) -> Option<LockWait> {
    let values = syscall
        .split_whitespace()
        .take(7)
        .map(parse_kernel_number)
        .collect::<Option<Vec<_>>>()?;
    let (&number, arguments) = values.split_first()?;
    let name = architecture.syscall_name(number);
    if name != "futex" && name != "futex_waitv" {
        return None;
    }
    if name == "futex_waitv" {
        return Some(LockWait {
            tid,
            thread: thread.to_owned(),
            state: state.to_owned(),
            address: arguments.first().copied(),
            operation: String::from("FUTEX_WAITV"),
            expected: arguments.get(1).copied(),
            details: format!("wait vector · {wchan}"),
        });
    }
    let operation = arguments.get(1).copied().unwrap_or(0);
    let base = operation & 0x7f;
    let private = operation & 0x80 != 0;
    let realtime = operation & 0x100 != 0;
    let mut flags = Vec::new();
    if private {
        flags.push("private");
    }
    if realtime {
        flags.push("realtime clock");
    }
    if !wchan.is_empty() && wchan != "0" {
        flags.push(wchan);
    }
    Some(LockWait {
        tid,
        thread: thread.to_owned(),
        state: state.to_owned(),
        address: arguments.first().copied(),
        operation: futex_operation(base).to_owned(),
        expected: arguments.get(2).copied(),
        details: flags.join(" · "),
    })
}

fn parse_kernel_number(value: &str) -> Option<u64> {
    value.strip_prefix("0x").map_or_else(
        || value.parse().ok(),
        |value| u64::from_str_radix(value, 16).ok(),
    )
}

fn futex_operation(operation: u64) -> &'static str {
    match operation {
        0 => "FUTEX_WAIT",
        1 => "FUTEX_WAKE",
        3 => "FUTEX_REQUEUE",
        4 => "FUTEX_CMP_REQUEUE",
        5 => "FUTEX_WAKE_OP",
        6 => "FUTEX_LOCK_PI",
        7 => "FUTEX_UNLOCK_PI",
        8 => "FUTEX_TRYLOCK_PI",
        9 => "FUTEX_WAIT_BITSET",
        10 => "FUTEX_WAKE_BITSET",
        11 => "FUTEX_WAIT_REQUEUE_PI",
        12 => "FUTEX_CMP_REQUEUE_PI",
        13 => "FUTEX_LOCK_PI2",
        _ => "FUTEX_UNKNOWN",
    }
}

pub(crate) fn read_core_dump(path: &Path) -> Result<CoreDumpSnapshot, String> {
    let file =
        File::open(path).map_err(|error| format!("Cannot open {}: {error}", path.display()))?;
    let size = file
        .metadata()
        .map_err(|error| format!("Cannot stat {}: {error}", path.display()))?
        .len();
    let mut header = [0_u8; 64];
    read_exact_at(&file, &mut header, 0)
        .map_err(|error| format!("Cannot read ELF header from {}: {error}", path.display()))?;
    let (architecture, endian, pointer_bits) = TargetArchitecture::from_elf_ident(&header)
        .ok_or_else(|| format!("{} is not a supported ELF core file", path.display()))?;
    let abi = Abi {
        architecture,
        endian,
        pointer_bits,
    };
    let elf_type = read_u16(&header[16..18], endian).unwrap_or(0);
    if elf_type != 4 {
        return Err(format!("{} is ELF but not an ET_CORE file", path.display()));
    }
    let (phoff, phentsize, phnum) = if pointer_bits == 64 {
        (
            read_u64(&header[32..40], endian).unwrap_or(0),
            u64::from(read_u16(&header[54..56], endian).unwrap_or(0)),
            usize::from(read_u16(&header[56..58], endian).unwrap_or(0)),
        )
    } else {
        (
            u64::from(read_u32(&header[28..32], endian).unwrap_or(0)),
            u64::from(read_u16(&header[42..44], endian).unwrap_or(0)),
            usize::from(read_u16(&header[44..46], endian).unwrap_or(0)),
        )
    };
    let program_header_bytes = u64::try_from(phnum)
        .unwrap_or(u64::MAX)
        .saturating_mul(phentsize);
    if phnum > MAX_CORE_PROGRAM_HEADERS
        || phentsize < if pointer_bits == 64 { 56 } else { 32 }
        || phentsize > 4096
        || program_header_bytes > MAX_CORE_PROGRAM_HEADER_BYTES
    {
        return Err(String::from(
            "The core file has an invalid or excessive program-header table",
        ));
    }
    let mut snapshot = CoreDumpSnapshot {
        path: path.to_owned(),
        size,
        architecture: architecture.display_name().to_owned(),
        class: format!("ELF{pointer_bits}"),
        endian: match endian {
            TargetEndian::Little => String::from("little endian"),
            TargetEndian::Big => String::from("big endian"),
        },
        ..CoreDumpSnapshot::default()
    };
    for index in 0..phnum {
        let offset = phoff
            .checked_add(
                u64::try_from(index)
                    .unwrap_or(u64::MAX)
                    .saturating_mul(phentsize),
            )
            .ok_or_else(|| String::from("Program-header offset overflow"))?;
        let mut program = vec![0_u8; usize::try_from(phentsize).unwrap_or(0)];
        read_exact_at(&file, &mut program, offset)
            .map_err(|error| format!("Cannot read core program header {index}: {error}"))?;
        if read_u32(&program[..4], endian) != Some(4) {
            continue;
        }
        let (note_offset, note_size, alignment) = if pointer_bits == 64 {
            (
                read_u64(&program[8..16], endian).unwrap_or(0),
                read_u64(&program[32..40], endian).unwrap_or(0),
                read_u64(&program[48..56], endian).unwrap_or(4),
            )
        } else {
            (
                u64::from(read_u32(&program[4..8], endian).unwrap_or(0)),
                u64::from(read_u32(&program[16..20], endian).unwrap_or(0)),
                u64::from(read_u32(&program[28..32], endian).unwrap_or(4)),
            )
        };
        parse_note_segment(&file, note_offset, note_size, alignment, abi, &mut snapshot)?;
    }
    snapshot.threads.sort_unstable();
    snapshot.threads.dedup();
    Ok(snapshot)
}

fn parse_note_segment(
    file: &File,
    offset: u64,
    size: u64,
    alignment: u64,
    abi: Abi,
    snapshot: &mut CoreDumpSnapshot,
) -> Result<(), String> {
    let end = offset
        .checked_add(size)
        .ok_or_else(|| String::from("Core note range overflow"))?;
    let alignment = alignment.clamp(4, 4096);
    let mut cursor = offset;
    while cursor.checked_add(12).is_some_and(|value| value <= end)
        && snapshot.notes.len() < MAX_CORE_NOTES
    {
        let mut header = [0_u8; 12];
        read_exact_at(file, &mut header, cursor)
            .map_err(|error| format!("Cannot read core note: {error}"))?;
        let name_size = u64::from(read_u32(&header[0..4], abi.endian).unwrap_or(0));
        let desc_size = u64::from(read_u32(&header[4..8], abi.endian).unwrap_or(0));
        let kind = read_u32(&header[8..12], abi.endian).unwrap_or(0);
        let name_offset = cursor + 12;
        let desc_offset = align_up(name_offset + name_size, alignment)
            .ok_or_else(|| String::from("Core note offset overflow"))?;
        let next = align_up(desc_offset + desc_size, alignment)
            .ok_or_else(|| String::from("Core note offset overflow"))?;
        if next > end || next <= cursor {
            snapshot
                .warnings
                .push(String::from("Stopped at a malformed core note"));
            break;
        }
        let displayed_name_size = usize::try_from(name_size)
            .unwrap_or(MAX_CORE_NOTE_NAME_BYTES)
            .min(MAX_CORE_NOTE_NAME_BYTES);
        let mut name = vec![0_u8; displayed_name_size];
        read_exact_at(file, &mut name, name_offset)
            .map_err(|error| format!("Cannot read core note owner: {error}"))?;
        let owner = String::from_utf8_lossy(name.split(|byte| *byte == 0).next().unwrap_or(&[]))
            .into_owned();
        snapshot.notes.push(CoreNote {
            owner,
            kind: note_name(kind),
            bytes: desc_size,
        });
        if desc_size > MAX_CORE_NOTE_BYTES as u64 {
            push_core_warning_once(snapshot, "Skipped an oversized core note payload");
        } else if matches!(kind, 1 | 3 | 6 | 0x5349_4749 | 0x4649_4c45) {
            let mut descriptor = vec![0_u8; usize::try_from(desc_size).unwrap_or(0)];
            read_exact_at(file, &mut descriptor, desc_offset)
                .map_err(|error| format!("Cannot read core note payload: {error}"))?;
            parse_core_note(kind, &descriptor, abi, snapshot);
        }
        cursor = next;
    }
    if snapshot.notes.len() == MAX_CORE_NOTES {
        push_core_warning_once(snapshot, "Core note display was capped");
    }
    Ok(())
}

fn push_core_warning_once(snapshot: &mut CoreDumpSnapshot, warning: &str) {
    if !snapshot.warnings.iter().any(|existing| existing == warning) {
        snapshot.warnings.push(warning.to_owned());
    }
}

fn parse_core_note(kind: u32, bytes: &[u8], abi: Abi, snapshot: &mut CoreDumpSnapshot) {
    match kind {
        1 => {
            let pid_offset = if abi.pointer_bits == 64 { 32 } else { 24 };
            if let Some(pid) = bytes
                .get(pid_offset..pid_offset + 4)
                .and_then(|bytes| read_u32(bytes, abi.endian))
            {
                snapshot.threads.push(pid);
            }
        }
        3 => {
            let (pid_offset, name_offset, command_offset) = if abi.pointer_bits == 64 {
                (24, 40, 56)
            } else {
                (16, 32, 48)
            };
            snapshot.pid = bytes
                .get(pid_offset..pid_offset + 4)
                .and_then(|bytes| read_u32(bytes, abi.endian));
            snapshot.process_name = c_string_at(bytes, name_offset, 16);
            snapshot.command = c_string_at(bytes, command_offset, 80);
        }
        6 => snapshot.auxv = parse_auxv(bytes, abi, &[]),
        0x5349_4749 => {
            snapshot.signal = bytes
                .get(0..4)
                .and_then(|bytes| read_i32(bytes, abi.endian));
            snapshot.signal_code = bytes
                .get(8..12)
                .and_then(|bytes| read_i32(bytes, abi.endian));
            if snapshot
                .signal
                .is_some_and(|signal| matches!(signal, 4 | 5 | 7 | 8 | 11))
            {
                let address_offset = if abi.pointer_bits == 64 { 16 } else { 12 };
                let word = usize::try_from(abi.pointer_bits / 8).unwrap_or(8);
                snapshot.fault_address = bytes
                    .get(address_offset..address_offset + word)
                    .and_then(|bytes| read_word(bytes, abi.endian));
            }
        }
        0x4649_4c45 => parse_core_files(bytes, abi, snapshot),
        _ => {}
    }
}

fn parse_core_files(bytes: &[u8], abi: Abi, snapshot: &mut CoreDumpSnapshot) {
    let word = usize::try_from(abi.pointer_bits / 8)
        .unwrap_or(8)
        .clamp(4, 8);
    if bytes.len() < word * 2 {
        return;
    }
    let declared_count = read_word(&bytes[..word], abi.endian)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(0);
    let count = declared_count.min(MAX_CORE_FILES.saturating_sub(snapshot.files.len()));
    if count < declared_count {
        push_core_warning_once(snapshot, "Core mapped-file display was capped");
    }
    let page_size = read_word(&bytes[word..word * 2], abi.endian).unwrap_or(1);
    let table_end = word * 2 + declared_count.saturating_mul(word * 3);
    let Some(table) = bytes.get(word * 2..table_end) else {
        return;
    };
    let paths = bytes
        .get(table_end..)
        .unwrap_or(&[])
        .split(|byte| *byte == 0);
    for (index, path) in paths.take(count).enumerate() {
        let entry = &table[index * word * 3..(index + 1) * word * 3];
        let start = read_word(&entry[..word], abi.endian).unwrap_or(0);
        let end = read_word(&entry[word..word * 2], abi.endian).unwrap_or(0);
        let file_offset = read_word(&entry[word * 2..], abi.endian)
            .unwrap_or(0)
            .saturating_mul(page_size);
        snapshot.files.push(CoreMappedFile {
            start,
            end,
            file_offset,
            path: String::from_utf8_lossy(path).into_owned(),
        });
    }
}

fn note_name(kind: u32) -> String {
    match kind {
        1 => String::from("NT_PRSTATUS"),
        3 => String::from("NT_PRPSINFO"),
        6 => String::from("NT_AUXV"),
        0x5349_4749 => String::from("NT_SIGINFO"),
        0x4649_4c45 => String::from("NT_FILE"),
        0x202 => String::from("NT_X86_XSTATE"),
        _ => format!("NOTE 0x{kind:x}"),
    }
}

fn c_string_at(bytes: &[u8], offset: usize, maximum: usize) -> Option<String> {
    let bytes = bytes.get(offset..offset.saturating_add(maximum).min(bytes.len()))?;
    let bytes = bytes.split(|byte| *byte == 0).next().unwrap_or(bytes);
    (!bytes.is_empty()).then(|| String::from_utf8_lossy(bytes).trim().to_owned())
}

fn align_up(value: u64, alignment: u64) -> Option<u64> {
    let remainder = value % alignment;
    if remainder == 0 {
        Some(value)
    } else {
        value.checked_add(alignment - remainder)
    }
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

    #[test]
    fn parses_futex_wait_and_preserves_flags() {
        let wait = parse_lock_wait(
            17,
            "worker",
            "S (sleeping)",
            "futex_wait_queue",
            "202 0x1234 0x80 7 0 0 0 0 0",
            TargetArchitecture::X86_64,
        )
        .unwrap();
        assert_eq!(wait.address, Some(0x1234));
        assert_eq!(wait.operation, "FUTEX_WAIT");
        assert_eq!(wait.expected, Some(7));
        assert!(wait.details.contains("private"));
    }

    #[test]
    fn identifies_allocator_relevant_regions_without_claiming_chunks() {
        let snapshot = allocator_snapshot(&[
            ProcessMapping {
                start: 0x1000,
                end: 0x3000,
                permissions: String::from("rw-p"),
                path: String::from("[heap]"),
            },
            ProcessMapping {
                start: 0x4000,
                end: 0x8000,
                permissions: String::from("rw-p"),
                path: String::new(),
            },
            ProcessMapping {
                start: 0x9000,
                end: 0xa000,
                permissions: String::from("r-xp"),
                path: String::from("/usr/lib/libjemalloc.so"),
            },
        ]);
        assert_eq!(snapshot.implementation, "jemalloc");
        assert_eq!(snapshot.heap_bytes, 0x2000);
        assert_eq!(snapshot.anonymous_writable_bytes, 0x4000);
        assert_eq!(snapshot.regions.len(), 3);
    }

    #[test]
    fn call_abi_snapshot_and_transfer_use_only_exact_available_facts() {
        let frames = [StackFrame {
            level: 0,
            address: String::from("0x401000"),
            function: String::from("main"),
            architecture: None,
            file: Some(String::from("main.c")),
            fullname: None,
            line: Some(7),
        }];
        let registers = [
            Register {
                name: String::from("rip"),
                value: String::from("0x401000"),
                pointer_chain: Vec::new(),
            },
            Register {
                name: String::from("rsp"),
                value: String::from("0x7fff0000"),
                pointer_chain: Vec::new(),
            },
            Register {
                name: String::from("rdi"),
                value: String::from("0x2a"),
                pointer_chain: Vec::new(),
            },
        ];
        let snapshot = call_abi_snapshot(TargetArchitecture::X86_64, 64, 0, &frames);
        assert_eq!(snapshot.current_frame.unwrap().function, "main");
        assert_eq!(
            snapshot.contract[0].value,
            "$rdi  $rsi  $rdx  $rcx  $r8  $r9"
        );

        let transfer = call_abi_transfer(
            TargetArchitecture::X86_64,
            CallAbiPhase::OutgoingCall {
                target: Some(String::from("malloc")),
            },
            &registers,
        );
        assert_eq!(transfer.context, "OUTGOING CALL  ·  malloc");
        assert_eq!(transfer.registers.len(), 2);
        assert_eq!(transfer.registers[0].name, "$rdi");
        assert_eq!(transfer.registers[0].value, "0x2a");
        assert_eq!(transfer.registers[1].name, "$rsp");
    }

    #[test]
    fn reads_signal_metadata_from_a_minimal_streamed_core() {
        let mut bytes = vec![0_u8; 164];
        bytes[0..4].copy_from_slice(b"\x7fELF");
        bytes[4] = 2;
        bytes[5] = 1;
        bytes[6] = 1;
        bytes[16..18].copy_from_slice(&4_u16.to_le_bytes());
        bytes[18..20].copy_from_slice(&62_u16.to_le_bytes());
        bytes[20..24].copy_from_slice(&1_u32.to_le_bytes());
        bytes[32..40].copy_from_slice(&64_u64.to_le_bytes());
        bytes[52..54].copy_from_slice(&64_u16.to_le_bytes());
        bytes[54..56].copy_from_slice(&56_u16.to_le_bytes());
        bytes[56..58].copy_from_slice(&1_u16.to_le_bytes());

        let program = &mut bytes[64..120];
        program[0..4].copy_from_slice(&4_u32.to_le_bytes());
        program[8..16].copy_from_slice(&120_u64.to_le_bytes());
        program[32..40].copy_from_slice(&44_u64.to_le_bytes());
        program[48..56].copy_from_slice(&4_u64.to_le_bytes());

        let note = &mut bytes[120..];
        note[0..4].copy_from_slice(&5_u32.to_le_bytes());
        note[4..8].copy_from_slice(&24_u32.to_le_bytes());
        note[8..12].copy_from_slice(&0x5349_4749_u32.to_le_bytes());
        note[12..17].copy_from_slice(b"CORE\0");
        note[20..24].copy_from_slice(&11_i32.to_le_bytes());
        note[28..32].copy_from_slice(&1_i32.to_le_bytes());
        note[36..44].copy_from_slice(&0xdead_beef_u64.to_le_bytes());

        let path = std::env::temp_dir().join(format!(
            "fgdb-core-test-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("worker")
        ));
        std::fs::write(&path, bytes).unwrap();
        let snapshot = read_core_dump(&path).unwrap();
        std::fs::remove_file(path).unwrap();
        assert_eq!(snapshot.signal, Some(11));
        assert_eq!(snapshot.signal_code, Some(1));
        assert_eq!(snapshot.fault_address, Some(0xdead_beef));
        assert_eq!(snapshot.notes[0].kind, "NT_SIGINFO");
    }
}
