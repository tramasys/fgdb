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

mod heap;

pub(crate) use heap::{HeapDiscovery, NativeHeapQuery, NativeHeapReadRequest, inspect_native_heap};

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
    pub detection_basis: String,
    pub probe_complete: bool,
    pub probe_dispatch_failures: usize,
    pub default_bindings: Vec<AllocatorBinding>,
    pub detected_runtimes: Vec<String>,
    pub allocation_frontends: Vec<String>,
    pub evidence: Vec<String>,
    pub heap_bytes: u64,
    pub anonymous_writable_bytes: u64,
    pub regions: Vec<AllocatorRegion>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AllocatorBinding {
    pub symbol: String,
    pub address: u64,
    pub owner: String,
    pub indirect: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct AllocatorProbe {
    pub complete: bool,
    pub dispatch_failures: usize,
    pub symbols: Vec<AllocatorProbeSymbol>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AllocatorProbeSymbol {
    pub name: String,
    pub address: u64,
    pub indirect: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AllocatorProbeSpec {
    pub name: &'static str,
    pub expression: &'static str,
}

pub(crate) const ALLOCATOR_PROBE_SPECS: &[AllocatorProbeSpec] = &[
    probe_function("malloc"),
    probe_function("free"),
    probe_function("calloc"),
    probe_function("realloc"),
    probe_function("__libc_malloc"),
    AllocatorProbeSpec {
        name: "__malloc_context",
        expression: "&__malloc_context",
    },
    probe_function("__uClibc_main"),
    probe_function("android_mallopt"),
    probe_function("mallctl"),
    probe_function("malloc_stats_print"),
    probe_function("je_malloc"),
    probe_function("je_mallctl"),
    probe_function("tc_malloc"),
    probe_function("tc_free"),
    probe_function("MallocExtension_GetNumericProperty"),
    probe_function("TCMallocInternalMalloc"),
    probe_function("mi_malloc"),
    probe_function("mi_free"),
    probe_function("mi_version"),
    probe_function("rpmalloc"),
    probe_function("rpfree"),
    probe_function("__scudo_print_stats"),
    probe_function("__asan_init"),
    probe_function("__hwasan_init"),
    probe_function("__tsan_init"),
    probe_function("malloc_object_size"),
    probe_function("dlmalloc"),
    probe_function("dlfree"),
    probe_function("nedmalloc"),
    probe_function("nedfree"),
    probe_function("tlsf_malloc"),
    probe_function("tlsf_free"),
    probe_function("scalable_malloc"),
    probe_function("scalable_free"),
    probe_function("dmalloc_malloc"),
    probe_function("__rust_alloc"),
    probe_function("__rg_alloc"),
    probe_function("__rdl_alloc"),
    probe_function("_Znwm"),
    probe_function("_Znwj"),
    probe_function("PyObject_Malloc"),
    probe_function("PyMem_RawMalloc"),
    probe_function("ruby_xmalloc"),
    probe_function("GC_malloc"),
    AllocatorProbeSpec {
        name: "runtime.mallocgc",
        expression: "&'runtime.mallocgc'",
    },
];

const fn probe_function(name: &'static str) -> AllocatorProbeSpec {
    AllocatorProbeSpec {
        name,
        expression: name,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AllocatorRegion {
    pub start: u64,
    pub end: u64,
    pub permissions: String,
    pub role: String,
    pub path: String,
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

impl AllocatorRegion {
    pub fn size(&self) -> u64 {
        self.end.saturating_sub(self.start)
    }
}

const MAX_HEAP_INSPECTION_ROWS: usize = 8_192;
const MAX_HEAP_INSPECTION_CELL_CHARS: usize = 2_048;

#[cfg(test)]
fn parse_heap_inspection(command: &str, output: &str) -> HeapInspectionSnapshot {
    let output = strip_terminal_sequences(output);
    let truncated_by_capture = output.contains("[fgdb: console output truncated");
    let exception = output
        .lines()
        .skip_while(|line| !line.contains("Exception raised"))
        .skip(1)
        .map(str::trim)
        .find(|line| !line.is_empty() && !is_heap_output_divider(line))
        .map(str::to_owned);
    let mut rows = Vec::new();
    let mut section = String::new();
    let mut diagnostic = exception;
    let mut arena_count = 0_usize;
    let mut chunk_count = 0_usize;
    let mut bin_count = 0_usize;

    for raw_line in output.lines() {
        if rows.len() == MAX_HEAP_INSPECTION_ROWS {
            break;
        }
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with("[fgdb: console output truncated") {
            continue;
        }
        if line.contains("Exception raised") {
            break;
        }
        if let Some(title) = heap_output_title(line) {
            section = title.to_owned();
            rows.push(heap_inspection_row("Section", "", "", "", title));
            continue;
        }
        if let Some(row) = parse_heap_arena_row(line, &section) {
            arena_count += 1;
            rows.push(row);
            continue;
        }
        if let Some(row) = parse_heap_bin_row(line) {
            bin_count += 1;
            rows.push(row);
            continue;
        }
        if let Some(row) = parse_heap_chunk_row(line) {
            chunk_count += 1;
            rows.push(row);
            continue;
        }
        if let Some(row) = parse_heap_table_row(line) {
            chunk_count += usize::from(row.kind == "Chunk");
            rows.push(row);
            continue;
        }
        if let Some(row) = parse_heap_field_row(line, &section) {
            rows.push(row);
            continue;
        }
        if let Some(row) = parse_heap_label_row(line, &section) {
            rows.push(row);
            continue;
        }

        let is_error = heap_output_error(line);
        if is_error && diagnostic.is_none() {
            diagnostic = Some(trim_heap_status_prefix(line).to_owned());
        }
        rows.push(heap_inspection_row(
            if is_error { "Error" } else { "Info" },
            "",
            "",
            if is_error { "failed" } else { "" },
            trim_heap_status_prefix(line),
        ));
    }

    let row_limit_reached = rows.len() == MAX_HEAP_INSPECTION_ROWS;
    let truncated = truncated_by_capture || row_limit_reached;
    let mut counts = Vec::new();
    if arena_count > 0 {
        counts.push(format!("{arena_count} arena{}", plural(arena_count)));
    }
    if chunk_count > 0 {
        counts.push(format!("{chunk_count} chunk{}", plural(chunk_count)));
    }
    if bin_count > 0 {
        counts.push(format!("{bin_count} bin{}", plural(bin_count)));
    }
    if counts.is_empty() {
        counts.push(format!("{} output row{}", rows.len(), plural(rows.len())));
    }
    if truncated {
        counts.push(String::from("display capped"));
    }
    HeapInspectionSnapshot {
        command: command.to_owned(),
        summary: counts.join("  ·  "),
        diagnostic,
        rows,
        truncated,
    }
}

#[cfg(test)]
fn parse_heap_arena_row(line: &str, section: &str) -> Option<HeapInspectionRow> {
    let fields = parse_heap_object(line, "Arena")?;
    let location = heap_field(&fields, &["addr", "base"]).unwrap_or_default();
    let metric = heap_field(&fields, &["system_mem", "size"]).unwrap_or_default();
    let state = if section.to_ascii_lowercase().contains("main_arena") {
        "main"
    } else if section.to_ascii_lowercase().contains("thread_arena") {
        "thread"
    } else {
        ""
    };
    Some(heap_inspection_row(
        "Arena",
        &location,
        &metric,
        state,
        &format_heap_fields(&fields, &["addr", "base", "system_mem", "size"]),
    ))
}

#[cfg(test)]
fn parse_heap_chunk_row(line: &str) -> Option<HeapInspectionRow> {
    let fields = parse_heap_object(line, "Chunk")?;
    let location = heap_field(&fields, &["addr", "base"]).unwrap_or_default();
    let metric = heap_field(&fields, &["size", "usable_size"]).unwrap_or_default();
    let flags = heap_field(&fields, &["flags"]).unwrap_or_default();
    let suffix = line
        .rsplit_once(')')
        .map_or("", |(_, suffix)| suffix.trim());
    let details = [
        format_heap_fields(&fields, &["addr", "base", "size", "usable_size", "flags"]),
        suffix.to_owned(),
    ]
    .into_iter()
    .filter(|value| !value.is_empty())
    .collect::<Vec<_>>()
    .join("  ·  ");
    Some(heap_inspection_row(
        "Chunk", &location, &metric, &flags, &details,
    ))
}

#[cfg(test)]
fn parse_heap_bin_row(line: &str) -> Option<HeapInspectionRow> {
    let open = line.find("[idx=")?;
    let close = line[open..].find(']').map(|offset| open + offset)?;
    let name = line[..open].trim().trim_end_matches(':');
    if !name.to_ascii_lowercase().contains("bin") {
        return None;
    }
    let fields = parse_comma_fields(&line[open + 1..close]);
    let index = heap_field(&fields, &["idx"]).unwrap_or_default();
    let size = heap_field(&fields, &["size"]).unwrap_or_default();
    let count = heap_field(&fields, &["count"]);
    let metric = count.map_or(size.clone(), |count| {
        if size.is_empty() {
            format!("count {count}")
        } else {
            format!("{size}  ·  count {count}")
        }
    });
    let details = line[close + 1..].trim().trim_start_matches(':').trim();
    let lower = details.to_ascii_lowercase();
    let state = if lower.contains("corrupt") || lower.contains("loop detected") {
        "warning"
    } else if details.is_empty() || details == "0x00" {
        "empty"
    } else {
        "occupied"
    };
    Some(heap_inspection_row(
        &normalize_heap_bin_name(name),
        &format!("index {index}"),
        &metric,
        state,
        details,
    ))
}

#[cfg(test)]
fn parse_heap_table_row(line: &str) -> Option<HeapInspectionRow> {
    let tokens = line.split_whitespace().collect::<Vec<_>>();
    let address = tokens.first()?.trim_end_matches(':');
    if !is_hex_address(address) || tokens.len() < 2 {
        return None;
    }
    let mut size_index = tokens
        .iter()
        .enumerate()
        .skip(1)
        .find_map(|(index, token)| {
            is_hex_address(token.trim_matches(['(', ')', ','])).then_some(index)
        });
    if size_index.is_some_and(|index| tokens[index].starts_with('(')) {
        size_index = tokens
            .iter()
            .enumerate()
            .skip(size_index.unwrap_or(0) + 1)
            .find_map(|(index, token)| {
                is_hex_address(token.trim_matches(['(', ')', ','])).then_some(index)
            });
    }
    let state_index = size_index.and_then(|index| (index + 1 < tokens.len()).then_some(index + 1));
    let metric = size_index.map_or(String::new(), |index| {
        tokens[index].trim_matches(['(', ')', ',']).to_owned()
    });
    let state = state_index.map_or(String::new(), |index| tokens[index].to_owned());
    let detail_start = state_index.map_or(1, |index| index + 1);
    Some(heap_inspection_row(
        "Chunk",
        address,
        &metric,
        &state,
        &tokens[detail_start..].join(" "),
    ))
}

#[cfg(test)]
fn parse_heap_field_row(line: &str, section: &str) -> Option<HeapInspectionRow> {
    let line = line.trim().trim_end_matches(',');
    let (name, value) = line.split_once(" = ")?;
    let name = name.trim();
    if name.is_empty()
        || name.len() > 96
        || !name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "_[]".contains(character))
    {
        return None;
    }
    Some(heap_inspection_row(
        if section.is_empty() { "Field" } else { section },
        name,
        value.trim(),
        "",
        "",
    ))
}

#[cfg(test)]
fn parse_heap_label_row(line: &str, section: &str) -> Option<HeapInspectionRow> {
    let (name, value) = line.split_once(':')?;
    let name = name
        .trim()
        .trim_start_matches(['[', '+', '!', '-', '*', ']']);
    let value = value.trim();
    if name.is_empty()
        || value.is_empty()
        || name.len() > 96
        || name.contains("0x")
        || !name.chars().all(|character| {
            character.is_ascii_alphanumeric()
                || character.is_whitespace()
                || matches!(character, '_' | '-' | '/' | '(' | ')')
        })
    {
        return None;
    }
    Some(heap_inspection_row(
        if section.is_empty() {
            "Property"
        } else {
            section
        },
        name,
        value,
        "",
        "",
    ))
}

#[cfg(test)]
fn parse_heap_object(line: &str, kind: &str) -> Option<Vec<(String, String)>> {
    let start = line.find(&format!("{kind}("))? + kind.len() + 1;
    let end = line[start..].find(')').map(|offset| start + offset)?;
    Some(parse_comma_fields(&line[start..end]))
}

#[cfg(test)]
fn parse_comma_fields(fields: &str) -> Vec<(String, String)> {
    fields
        .split(',')
        .filter_map(|field| {
            let (name, value) = field.trim().split_once('=')?;
            Some((name.trim().to_owned(), value.trim().to_owned()))
        })
        .collect()
}

#[cfg(test)]
fn heap_field(fields: &[(String, String)], names: &[&str]) -> Option<String> {
    names.iter().find_map(|name| {
        fields
            .iter()
            .find(|(field, _)| field == name)
            .map(|(_, value)| value.clone())
    })
}

#[cfg(test)]
fn format_heap_fields(fields: &[(String, String)], excluded: &[&str]) -> String {
    fields
        .iter()
        .filter(|(name, _)| !excluded.contains(&name.as_str()))
        .map(|(name, value)| format!("{name} {value}"))
        .collect::<Vec<_>>()
        .join("  ·  ")
}

#[cfg(test)]
fn normalize_heap_bin_name(name: &str) -> String {
    let lower = name.to_ascii_lowercase().replace('_', " ");
    if lower.contains("tcache") {
        String::from("Tcache bin")
    } else if lower.contains("fast") {
        String::from("Fast bin")
    } else if lower.contains("unsorted") {
        String::from("Unsorted bin")
    } else if lower.contains("small") {
        String::from("Small bin")
    } else if lower.contains("large") {
        String::from("Large bin")
    } else {
        String::from("Bin")
    }
}

#[cfg(test)]
fn heap_output_title(line: &str) -> Option<&str> {
    let divider_count = line
        .chars()
        .filter(|character| matches!(character, '-' | '─' | '='))
        .count();
    if divider_count < 8 {
        return None;
    }
    let title = line.trim_matches(|character: char| {
        character.is_whitespace() || matches!(character, '-' | '─' | '=')
    });
    (!title.is_empty()).then_some(title)
}

#[cfg(test)]
fn is_heap_output_divider(line: &str) -> bool {
    !line.is_empty()
        && line
            .chars()
            .all(|character| character.is_whitespace() || matches!(character, '-' | '─' | '='))
}

#[cfg(test)]
fn heap_output_error(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    lower.starts_with("[-]")
        || lower.starts_with("error")
        || lower.contains("undefined command")
        || lower.contains("invalid arena")
        || lower.contains("no valid arena")
        || lower.contains("cannot access memory")
        || lower.contains("failed to execute properly")
        || lower.contains("heap is not initialized")
        || lower.contains("heap not initialized")
        || lower.contains("could not find glibc main arena")
        || lower.contains("gdb request timed out")
}

#[cfg(test)]
fn trim_heap_status_prefix(line: &str) -> &str {
    line.strip_prefix("[+] ")
        .or_else(|| line.strip_prefix("[*] "))
        .or_else(|| line.strip_prefix("[!] "))
        .or_else(|| line.strip_prefix("[-] "))
        .unwrap_or(line)
}

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

#[cfg(test)]
fn is_hex_address(value: &str) -> bool {
    value.strip_prefix("0x").is_some_and(|digits| {
        !digits.is_empty() && digits.chars().all(|digit| digit.is_ascii_hexdigit())
    })
}

fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}

#[cfg(test)]
fn strip_terminal_sequences(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut characters = input.chars().peekable();
    while let Some(character) = characters.next() {
        if character == '\u{1b}' {
            match characters.next() {
                Some('[') => {
                    for control in characters.by_ref() {
                        if ('@'..='~').contains(&control) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    while let Some(control) = characters.next() {
                        if control == '\u{7}' {
                            break;
                        }
                        if control == '\u{1b}' && characters.next_if_eq(&'\\').is_some() {
                            break;
                        }
                    }
                }
                Some(_) | None => {}
            }
        } else if character == '\r' {
            continue;
        } else if !character.is_control() || matches!(character, '\n' | '\t') {
            output.push(character);
        }
    }
    output
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
    allocator_probe: AllocatorProbe,
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
    let allocator = allocator_snapshot(&maps, &allocator_probe);
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

fn allocator_snapshot(
    maps: &[ProcessMapping],
    allocator_probe: &AllocatorProbe,
) -> AllocatorSnapshot {
    let mut runtime_families = Vec::new();
    let mut runtime_modules = Vec::new();
    let mut allocation_frontends = Vec::new();
    let mut frontend_modules = Vec::new();
    for mapping in maps {
        if let Some(family) = allocator_family_for_path(&mapping.path) {
            push_unique(&mut runtime_families, family);
            let module = mapping_display_name(&mapping.path);
            if !runtime_modules.iter().any(|(known, _)| *known == family) {
                runtime_modules.push((family, module));
            }
        }
        if let Some(frontend) = allocation_frontend_for_path(&mapping.path) {
            push_unique(&mut allocation_frontends, frontend.to_owned());
            let module = mapping_display_name(&mapping.path);
            if !frontend_modules.iter().any(|(known, _)| *known == frontend) {
                frontend_modules.push((frontend, module));
            }
        }
    }

    let mut default_bindings = Vec::new();
    let mut marker_families = Vec::new();
    let mut colocated_marker_families = Vec::new();
    let mut evidence = Vec::new();
    let primary_probe_symbol = allocator_probe
        .symbols
        .iter()
        .find(|symbol| symbol.name == "malloc" && !symbol.indirect)
        .or_else(|| {
            allocator_probe
                .symbols
                .iter()
                .find(|symbol| symbol.name == "free" && !symbol.indirect)
        });
    let primary_mapping =
        primary_probe_symbol.and_then(|symbol| mapping_containing(maps, symbol.address));
    for symbol in &allocator_probe.symbols {
        let mapping = mapping_containing(maps, symbol.address);
        let owner = mapping.map_or_else(
            || String::from("unmapped address"),
            |mapping| mapping_display_name(&mapping.path),
        );
        if is_default_allocator_binding(&symbol.name) {
            default_bindings.push(AllocatorBinding {
                symbol: symbol.name.clone(),
                address: symbol.address,
                owner,
                indirect: symbol.indirect,
            });
            if symbol.indirect {
                evidence.push(format!(
                    "{} resolves only to a PLT/GOT trampoline at 0x{:x}",
                    symbol.name, symbol.address
                ));
            }
            continue;
        }
        if let Some(family) = allocator_family_for_symbol(&symbol.name) {
            push_unique(&mut marker_families, family);
            push_unique(&mut runtime_families, family);
            if mappings_have_same_owner(primary_mapping, mapping) {
                push_unique(&mut colocated_marker_families, family);
            }
            evidence.push(format!(
                "{} at 0x{:x} in {owner}",
                symbol.name, symbol.address
            ));
        } else if let Some(frontend) = allocation_frontend_for_symbol(&symbol.name) {
            push_unique(&mut allocation_frontends, frontend.to_owned());
            evidence.push(format!(
                "allocation frontend {} at 0x{:x} in {owner}",
                frontend, symbol.address
            ));
        }
    }

    let primary_binding = default_bindings
        .iter()
        .find(|binding| binding.symbol == "malloc" && !binding.indirect)
        .or_else(|| {
            default_bindings
                .iter()
                .find(|binding| binding.symbol == "free" && !binding.indirect)
        });
    let primary_owner_family =
        primary_mapping.and_then(|mapping| allocator_family_for_path(&mapping.path));
    let colocated_marker_family = strongest_marker_family(&colocated_marker_families);
    let marker_family = strongest_marker_family(&marker_families);
    let malloc_mapping = direct_binding_mapping(allocator_probe, maps, "malloc");
    let free_mapping = direct_binding_mapping(allocator_probe, maps, "free");
    let split_core_bindings = malloc_mapping
        .zip(free_mapping)
        .is_some_and(|(malloc, free)| !mappings_have_same_owner(Some(malloc), Some(free)));
    if split_core_bindings {
        evidence.push(format!(
            "malloc resolves to {} but free resolves to {}",
            malloc_mapping.map_or("an unknown mapping".to_owned(), |mapping| {
                mapping_display_name(&mapping.path)
            }),
            free_mapping.map_or("an unknown mapping".to_owned(), |mapping| {
                mapping_display_name(&mapping.path)
            })
        ));
    } else if allocator_probe.complete && allocator_probe.dispatch_failures == 0 {
        match (malloc_mapping, free_mapping) {
            (Some(_), None) => evidence.push(String::from(
                "free could not be resolved directly. Paired ownership was not verified",
            )),
            (None, Some(_)) => evidence.push(String::from(
                "malloc could not be resolved directly. Paired ownership was not verified",
            )),
            _ => {}
        }
    }
    if let Some(primary_mapping) = primary_mapping {
        for binding in &default_bindings {
            if binding.indirect || matches!(binding.symbol.as_str(), "malloc" | "free") {
                continue;
            }
            let mapping = mapping_containing(maps, binding.address);
            if mapping.is_some() && !mappings_have_same_owner(Some(primary_mapping), mapping) {
                evidence.push(format!(
                    "{} resolves separately in {}",
                    binding.symbol, binding.owner
                ));
            }
        }
    }
    let binding_basis = primary_binding.map_or("resolved allocator binding", |binding| {
        if binding.symbol == "malloc" {
            "resolved malloc binding"
        } else {
            "resolved free binding (malloc unavailable)"
        }
    });
    let indirect_binding_count = default_bindings
        .iter()
        .filter(|binding| binding.indirect)
        .count();
    let conflicting_colocated_markers = colocated_marker_families.len() > 1
        && strongest_marker_family(&colocated_marker_families).is_none();
    let owner_marker_conflict = primary_owner_family.is_some_and(|owner| {
        !owner.is_libc_dispatch()
            && colocated_marker_families
                .iter()
                .any(|marker| *marker != owner && !marker.is_libc_dispatch())
    });
    let (implementation, detection_basis, selected_family) = if split_core_bindings {
        (
            String::from("split allocator bindings"),
            String::from("malloc and free resolve to different modules"),
            None,
        )
    } else if conflicting_colocated_markers || owner_marker_conflict {
        (
            String::from("conflicting allocator evidence"),
            String::from("the resolved binding owner exposes incompatible allocator markers"),
            None,
        )
    } else if let Some(family) = primary_owner_family {
        if family.is_libc_dispatch()
            && let Some(marker) = colocated_marker_family
            && marker.specificity() > family.specificity()
        {
            (
                marker.display_name().to_owned(),
                String::from("resolved libc binding with colocated allocator symbols"),
                Some(marker),
            )
        } else {
            (
                family.display_name().to_owned(),
                binding_basis.to_owned(),
                Some(family),
            )
        }
    } else if primary_binding.is_some()
        && let Some(family) = colocated_marker_family
    {
        (
            family.display_name().to_owned(),
            String::from("resolved binding with allocator-specific symbols"),
            Some(family),
        )
    } else if primary_binding.is_some() {
        (
            String::from("custom or interposed allocator"),
            String::from("malloc resolves outside a recognized allocator runtime"),
            None,
        )
    } else if let Some(family) = marker_family {
        (
            family.display_name().to_owned(),
            if indirect_binding_count > 0 {
                String::from("allocator symbols found. C bindings remain indirect")
            } else {
                String::from("allocator-specific symbols")
            },
            Some(family),
        )
    } else if marker_families.len() > 1 || runtime_families.len() > 1 {
        (
            String::from("multiple allocator runtimes detected"),
            if indirect_binding_count > 0 {
                String::from("C bindings remain indirect. No single owner is proven")
            } else {
                String::from("no single resolved malloc owner")
            },
            None,
        )
    } else if let Some(family) = runtime_families.first() {
        (
            family.display_name().to_owned(),
            if indirect_binding_count > 0 {
                String::from("loaded module evidence. C bindings remain indirect")
            } else {
                String::from("loaded module evidence")
            },
            Some(*family),
        )
    } else if indirect_binding_count > 0 {
        (
            String::from("allocator binding unresolved"),
            String::from("GDB returned only PLT/GOT trampoline addresses"),
            None,
        )
    } else {
        (
            String::from("not identified"),
            if allocator_probe.complete {
                String::from("no recognized bindings, symbols, or modules")
            } else {
                String::from("mapping evidence only")
            },
            None,
        )
    };

    if let Some(selected_family) = selected_family
        && let Some(index) = runtime_families
            .iter()
            .position(|family| *family == selected_family)
    {
        runtime_families.swap(0, index);
    }

    for (family, module) in runtime_modules {
        evidence.push(format!("{} runtime module {module}", family.display_name()));
    }
    for (frontend, module) in frontend_modules {
        evidence.push(format!("{frontend} runtime module {module}"));
    }
    if allocator_probe.dispatch_failures > 0 {
        evidence.push(format!(
            "{} optional GDB symbol probes could not be queued",
            allocator_probe.dispatch_failures
        ));
    }
    let mut heap_bytes = 0_u64;
    let mut anonymous_writable_bytes = 0_u64;
    let mut regions = Vec::new();
    for mapping in maps {
        let writable_private = mapping.permissions.starts_with("rw")
            && mapping.permissions.as_bytes().get(3) == Some(&b'p');
        let role = if mapping.path == "[heap]" {
            heap_bytes = heap_bytes.saturating_add(mapping.end.saturating_sub(mapping.start));
            Some(String::from("brk heap"))
        } else if writable_private && mapping.path.is_empty() {
            anonymous_writable_bytes =
                anonymous_writable_bytes.saturating_add(mapping.end.saturating_sub(mapping.start));
            Some(String::from("anonymous writable (possible arena)"))
        } else {
            allocator_family_for_path(&mapping.path)
                .map(|family| format!("{} runtime", family.display_name()))
        };
        if let Some(role) = role {
            regions.push(AllocatorRegion {
                start: mapping.start,
                end: mapping.end,
                permissions: mapping.permissions.clone(),
                role,
                path: if mapping.path.is_empty() {
                    String::from("anonymous")
                } else {
                    mapping.path.clone()
                },
            });
        }
    }
    AllocatorSnapshot {
        implementation,
        detection_basis,
        probe_complete: allocator_probe.complete,
        probe_dispatch_failures: allocator_probe.dispatch_failures,
        default_bindings,
        detected_runtimes: runtime_families
            .into_iter()
            .map(|family| family.display_name().to_owned())
            .collect(),
        allocation_frontends,
        evidence,
        heap_bytes,
        anonymous_writable_bytes,
        regions,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AllocatorFamily {
    Glibc,
    Musl,
    Uclibc,
    Bionic,
    Jemalloc,
    Tcmalloc,
    Mimalloc,
    Rpmalloc,
    Snmalloc,
    Scalloc,
    Ssmalloc,
    Hoard,
    Scudo,
    HardenedMalloc,
    Asan,
    Hwasan,
    Tsan,
    Dlmalloc,
    Nedmalloc,
    Tlsf,
    TbbMalloc,
    Dmalloc,
    ElectricFence,
}

impl AllocatorFamily {
    fn display_name(self) -> &'static str {
        match self {
            Self::Glibc => "glibc / ptmalloc",
            Self::Musl => "musl allocator",
            Self::Uclibc => "uClibc allocator",
            Self::Bionic => "Android Bionic malloc dispatch",
            Self::Jemalloc => "jemalloc",
            Self::Tcmalloc => "tcmalloc",
            Self::Mimalloc => "mimalloc",
            Self::Rpmalloc => "rpmalloc",
            Self::Snmalloc => "snmalloc",
            Self::Scalloc => "scalloc",
            Self::Ssmalloc => "SSMalloc",
            Self::Hoard => "Hoard",
            Self::Scudo => "Scudo",
            Self::HardenedMalloc => "hardened_malloc",
            Self::Asan => "AddressSanitizer allocator",
            Self::Hwasan => "HWAddressSanitizer allocator",
            Self::Tsan => "ThreadSanitizer allocator",
            Self::Dlmalloc => "dlmalloc",
            Self::Nedmalloc => "nedmalloc",
            Self::Tlsf => "TLSF",
            Self::TbbMalloc => "oneTBB scalable allocator",
            Self::Dmalloc => "dmalloc",
            Self::ElectricFence => "Electric Fence",
        }
    }

    fn specificity(self) -> u8 {
        match self {
            Self::Glibc | Self::Musl | Self::Uclibc | Self::Bionic => 10,
            Self::Dlmalloc
            | Self::Nedmalloc
            | Self::Tlsf
            | Self::TbbMalloc
            | Self::Dmalloc
            | Self::ElectricFence => 80,
            Self::Jemalloc
            | Self::Tcmalloc
            | Self::Mimalloc
            | Self::Rpmalloc
            | Self::Snmalloc
            | Self::Scalloc
            | Self::Ssmalloc
            | Self::Hoard
            | Self::Scudo
            | Self::HardenedMalloc
            | Self::Asan
            | Self::Hwasan
            | Self::Tsan => 100,
        }
    }

    fn is_libc_dispatch(self) -> bool {
        matches!(self, Self::Glibc | Self::Musl | Self::Uclibc | Self::Bionic)
    }
}

fn allocator_family_for_path(path: &str) -> Option<AllocatorFamily> {
    let path = normalized_mapping_path(path);
    let name = Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(path);
    let lower_name = name.to_ascii_lowercase();
    let lower_path = path.to_ascii_lowercase();
    if lower_name.starts_with("libjemalloc.so") {
        Some(AllocatorFamily::Jemalloc)
    } else if lower_name.starts_with("libtcmalloc") || lower_name.starts_with("libgoogle-perftools")
    {
        Some(AllocatorFamily::Tcmalloc)
    } else if lower_name.starts_with("libmimalloc") {
        Some(AllocatorFamily::Mimalloc)
    } else if lower_name.starts_with("librpmalloc") {
        Some(AllocatorFamily::Rpmalloc)
    } else if lower_name.starts_with("libsnmalloc") {
        Some(AllocatorFamily::Snmalloc)
    } else if lower_name.starts_with("libscalloc") {
        Some(AllocatorFamily::Scalloc)
    } else if lower_name.starts_with("libssmalloc") {
        Some(AllocatorFamily::Ssmalloc)
    } else if lower_name.starts_with("libhoard") {
        Some(AllocatorFamily::Hoard)
    } else if lower_name.starts_with("libscudo") || lower_name.starts_with("libclang_rt.scudo") {
        Some(AllocatorFamily::Scudo)
    } else if lower_name.starts_with("libhardened_malloc") {
        Some(AllocatorFamily::HardenedMalloc)
    } else if lower_name.starts_with("libasan") || lower_name.starts_with("libclang_rt.asan") {
        Some(AllocatorFamily::Asan)
    } else if lower_name.starts_with("libhwasan") || lower_name.starts_with("libclang_rt.hwasan") {
        Some(AllocatorFamily::Hwasan)
    } else if lower_name.starts_with("libtsan") || lower_name.starts_with("libclang_rt.tsan") {
        Some(AllocatorFamily::Tsan)
    } else if lower_name.starts_with("libdlmalloc") {
        Some(AllocatorFamily::Dlmalloc)
    } else if lower_name.starts_with("libnedmalloc") {
        Some(AllocatorFamily::Nedmalloc)
    } else if lower_name.starts_with("libtlsf") {
        Some(AllocatorFamily::Tlsf)
    } else if lower_name.starts_with("libtbbmalloc") {
        Some(AllocatorFamily::TbbMalloc)
    } else if lower_name.starts_with("libdmalloc") {
        Some(AllocatorFamily::Dmalloc)
    } else if lower_name.starts_with("libefence") {
        Some(AllocatorFamily::ElectricFence)
    } else if lower_name.starts_with("libc.musl") || lower_name.starts_with("ld-musl-") {
        Some(AllocatorFamily::Musl)
    } else if lower_name.starts_with("libuclibc-")
        || lower_name.starts_with("ld-uclibc")
        || lower_name == "libc.so.0"
    {
        Some(AllocatorFamily::Uclibc)
    } else if lower_name == "libc.so" && lower_path.contains("/bionic/") {
        Some(AllocatorFamily::Bionic)
    } else if lower_name == "libc.so.6"
        || (lower_name.starts_with("libc-") && lower_name.contains(".so"))
    {
        Some(AllocatorFamily::Glibc)
    } else {
        None
    }
}

fn allocator_family_for_symbol(symbol: &str) -> Option<AllocatorFamily> {
    match symbol {
        "__libc_malloc" => Some(AllocatorFamily::Glibc),
        "__malloc_context" => Some(AllocatorFamily::Musl),
        "__uClibc_main" => Some(AllocatorFamily::Uclibc),
        "android_mallopt" => Some(AllocatorFamily::Bionic),
        "mallctl" | "malloc_stats_print" | "je_malloc" | "je_mallctl" => {
            Some(AllocatorFamily::Jemalloc)
        }
        "tc_malloc"
        | "tc_free"
        | "MallocExtension_GetNumericProperty"
        | "TCMallocInternalMalloc" => Some(AllocatorFamily::Tcmalloc),
        "mi_malloc" | "mi_free" | "mi_version" => Some(AllocatorFamily::Mimalloc),
        "rpmalloc" | "rpfree" => Some(AllocatorFamily::Rpmalloc),
        "__scudo_print_stats" => Some(AllocatorFamily::Scudo),
        "malloc_object_size" => Some(AllocatorFamily::HardenedMalloc),
        "__asan_init" => Some(AllocatorFamily::Asan),
        "__hwasan_init" => Some(AllocatorFamily::Hwasan),
        "__tsan_init" => Some(AllocatorFamily::Tsan),
        "dlmalloc" | "dlfree" => Some(AllocatorFamily::Dlmalloc),
        "nedmalloc" | "nedfree" => Some(AllocatorFamily::Nedmalloc),
        "tlsf_malloc" | "tlsf_free" => Some(AllocatorFamily::Tlsf),
        "scalable_malloc" | "scalable_free" => Some(AllocatorFamily::TbbMalloc),
        "dmalloc_malloc" => Some(AllocatorFamily::Dmalloc),
        _ => None,
    }
}

fn allocation_frontend_for_symbol(symbol: &str) -> Option<&'static str> {
    match symbol {
        "__rust_alloc" => Some("Rust allocation shim"),
        "__rg_alloc" => Some("Rust custom global allocator"),
        "__rdl_alloc" => Some("Rust standard-library allocator"),
        "_Znwm" | "_Znwj" => Some("C++ operator new"),
        "PyObject_Malloc" | "PyMem_RawMalloc" => Some("CPython pymalloc"),
        "ruby_xmalloc" => Some("Ruby object allocator"),
        "runtime.mallocgc" => Some("Go managed heap"),
        "GC_malloc" => Some("Boehm conservative GC heap"),
        _ => None,
    }
}

fn allocation_frontend_for_path(path: &str) -> Option<&'static str> {
    let name = Path::new(normalized_mapping_path(path))
        .file_name()
        .and_then(|name| name.to_str())?
        .to_ascii_lowercase();
    if name.starts_with("libpython") {
        Some("CPython pymalloc")
    } else if name.starts_with("libruby") {
        Some("Ruby object allocator")
    } else if name.starts_with("libjvm") {
        Some("JVM managed heap")
    } else if name.starts_with("libnode") || name.starts_with("libv8") {
        Some("V8 managed heap")
    } else if name.starts_with("libcoreclr") {
        Some(".NET managed heap")
    } else if name.starts_with("libmono") {
        Some("Mono managed heap")
    } else if name == "libgc.so" || name.starts_with("libgc.so.") {
        Some("Boehm conservative GC heap")
    } else {
        None
    }
}

fn strongest_marker_family(families: &[AllocatorFamily]) -> Option<AllocatorFamily> {
    let strongest = families.iter().map(|family| family.specificity()).max()?;
    let mut candidates = families
        .iter()
        .copied()
        .filter(|family| family.specificity() == strongest);
    let family = candidates.next()?;
    candidates.next().is_none().then_some(family)
}

fn is_default_allocator_binding(symbol: &str) -> bool {
    matches!(symbol, "malloc" | "free" | "calloc" | "realloc")
}

fn direct_binding_mapping<'a>(
    probe: &AllocatorProbe,
    maps: &'a [ProcessMapping],
    name: &str,
) -> Option<&'a ProcessMapping> {
    probe
        .symbols
        .iter()
        .find(|symbol| symbol.name == name && !symbol.indirect)
        .and_then(|symbol| mapping_containing(maps, symbol.address))
}

fn mapping_containing(maps: &[ProcessMapping], address: u64) -> Option<&ProcessMapping> {
    maps.iter()
        .find(|mapping| mapping.start <= address && address < mapping.end)
}

fn mappings_have_same_owner(left: Option<&ProcessMapping>, right: Option<&ProcessMapping>) -> bool {
    left.zip(right).is_some_and(|(left, right)| {
        if left.start == right.start && left.end == right.end {
            return true;
        }
        let left_path = normalized_mapping_path(&left.path);
        let right_path = normalized_mapping_path(&right.path);
        !left_path.is_empty() && left_path == right_path
    })
}

fn normalized_mapping_path(path: &str) -> &str {
    path.strip_suffix(" (deleted)").unwrap_or(path)
}

pub(crate) fn allocator_probe_value_is_indirect(value: &str) -> bool {
    let value = value.to_ascii_lowercase();
    value.contains("@plt")
        || value.contains(".plt>")
        || value.contains("<plt")
        || value.contains("@got")
        || value.contains(".got>")
        || value.contains("<got")
}

fn mapping_display_name(path: &str) -> String {
    if path.is_empty() {
        return String::from("anonymous mapping");
    }
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(path)
        .to_owned()
}

fn push_unique<T: PartialEq>(values: &mut Vec<T>, value: T) {
    if !values.contains(&value) {
        values.push(value);
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
                details: format!("kernel wait channel {wchan}. Syscall arguments unavailable"),
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

    #[test]
    fn parses_gef_heap_objects_bins_and_parsed_tables() {
        let output = concat!(
            "\u{1b}[36m---------------- main_arena ----------------\u{1b}[0m\n",
            "Arena(addr=0x7ffff7e19ac0, heap_base=0x555555559000, top=0x555555561080, system_mem=0x21000)\n",
            "Tcachebins[idx=3, size=0x50, count=2] ← Chunk(addr=0x555555559090, size=0x50, flags=PREV_INUSE)\n",
            "0x555555559000  (0x0)  0x70  Used  -  -\n",
            "Chunk(base=0x555555561080, addr=0x555555561090, size=0x18f80, flags=PREV_INUSE) <- top\n",
        );
        let snapshot = parse_heap_inspection("heap bins tcache", output);

        assert_eq!(snapshot.diagnostic, None);
        assert_eq!(snapshot.rows[1].kind, "Arena");
        assert_eq!(snapshot.rows[1].location, "0x7ffff7e19ac0");
        assert_eq!(snapshot.rows[2].kind, "Tcache bin");
        assert_eq!(snapshot.rows[2].location, "index 3");
        assert_eq!(snapshot.rows[2].metric, "0x50  ·  count 2");
        assert_eq!(snapshot.rows[3].metric, "0x70");
        assert_eq!(snapshot.rows[3].state, "Used");
        assert_eq!(snapshot.rows[4].kind, "Chunk");
        assert!(snapshot.summary.contains("1 arena"));
        assert!(snapshot.summary.contains("2 chunks"));
        assert!(snapshot.summary.contains("1 bin"));
    }

    #[test]
    fn preserves_partial_heap_results_but_reports_gef_exceptions() {
        let output = concat!(
            "---------------- arena ----------------\n",
            "top: 0x555555561080 (sz:0x18f80)\n",
            "---------------- Exception raised ----------------\n",
            "TypeError: unsupported format string passed to NoneType.__format__\n",
            "---------------- Detailed stacktrace ----------------\n",
            "File /tmp/gef.py, line 1\n",
        );
        let snapshot = parse_heap_inspection("heap bins", output);

        assert_eq!(
            snapshot.diagnostic.as_deref(),
            Some("TypeError: unsupported format string passed to NoneType.__format__")
        );
        assert!(snapshot.rows.iter().any(|row| row.location == "top"));
        assert!(
            !snapshot
                .rows
                .iter()
                .any(|row| row.details.contains("stacktrace") || row.details.contains("gef.py"))
        );
    }

    #[test]
    fn strips_gef_terminal_coloring_from_heap_diagnostics() {
        let output = "\u{1b}[1m\u{1b}[31m[!]\u{1b}[0m Heap not initialized\n";
        let snapshot = parse_heap_inspection("heap bins tcache", output);

        assert_eq!(snapshot.diagnostic.as_deref(), Some("Heap not initialized"));
        assert_eq!(snapshot.rows.len(), 1);
        assert_eq!(snapshot.rows[0].details, "Heap not initialized");
        assert!(!snapshot.rows[0].details.contains("[31m"));
    }

    #[test]
    fn caps_heap_output_rows_and_cells() {
        let oversized = "x".repeat(MAX_HEAP_INSPECTION_CELL_CHARS + 32);
        let output = std::iter::repeat_n(oversized.as_str(), MAX_HEAP_INSPECTION_ROWS + 8)
            .collect::<Vec<_>>()
            .join("\n");
        let snapshot = parse_heap_inspection("backend-dump", &output);

        assert!(snapshot.truncated);
        assert_eq!(snapshot.rows.len(), MAX_HEAP_INSPECTION_ROWS);
        assert_eq!(
            snapshot.rows[0].details.chars().count(),
            MAX_HEAP_INSPECTION_CELL_CHARS
        );
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
        let snapshot = allocator_snapshot(
            &[
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
            ],
            &AllocatorProbe::default(),
        );
        assert_eq!(snapshot.implementation, "jemalloc");
        assert_eq!(snapshot.detection_basis, "loaded module evidence");
        assert_eq!(snapshot.heap_bytes, 0x2000);
        assert_eq!(snapshot.anonymous_writable_bytes, 0x4000);
        assert_eq!(snapshot.regions.len(), 3);
    }

    #[test]
    fn resolved_malloc_owner_wins_over_also_loaded_libc() {
        let maps = [
            ProcessMapping {
                start: 0x7000,
                end: 0x8000,
                permissions: String::from("r-xp"),
                path: String::from("/usr/lib/libc.so.6"),
            },
            ProcessMapping {
                start: 0x9000,
                end: 0xa000,
                permissions: String::from("r-xp"),
                path: String::from("/usr/lib/libjemalloc.so.2"),
            },
        ];
        let probe = AllocatorProbe {
            complete: true,
            dispatch_failures: 0,
            symbols: vec![
                AllocatorProbeSymbol {
                    name: String::from("malloc"),
                    address: 0x9100,
                    indirect: false,
                },
                AllocatorProbeSymbol {
                    name: String::from("free"),
                    address: 0x9200,
                    indirect: false,
                },
                AllocatorProbeSymbol {
                    name: String::from("mallctl"),
                    address: 0x9300,
                    indirect: false,
                },
            ],
        };
        let snapshot = allocator_snapshot(&maps, &probe);
        assert_eq!(snapshot.implementation, "jemalloc");
        assert_eq!(snapshot.detection_basis, "resolved malloc binding");
        assert_eq!(snapshot.default_bindings[0].owner, "libjemalloc.so.2");
        assert_eq!(
            snapshot.detected_runtimes,
            [String::from("jemalloc"), String::from("glibc / ptmalloc")]
        );
    }

    #[test]
    fn allocator_marker_identifies_static_or_wrapped_backend() {
        let maps = [ProcessMapping {
            start: 0x1000,
            end: 0x3000,
            permissions: String::from("r-xp"),
            path: String::from("/opt/bin/service"),
        }];
        let probe = AllocatorProbe {
            complete: true,
            dispatch_failures: 0,
            symbols: vec![
                AllocatorProbeSymbol {
                    name: String::from("malloc"),
                    address: 0x1100,
                    indirect: false,
                },
                AllocatorProbeSymbol {
                    name: String::from("tc_malloc"),
                    address: 0x1200,
                    indirect: false,
                },
            ],
        };
        let snapshot = allocator_snapshot(&maps, &probe);
        assert_eq!(snapshot.implementation, "tcmalloc");
        assert_eq!(
            snapshot.detection_basis,
            "resolved binding with allocator-specific symbols"
        );
    }

    #[test]
    fn reports_ambiguous_modules_and_unknown_interposers_honestly() {
        let maps = [
            ProcessMapping {
                start: 0x1000,
                end: 0x2000,
                permissions: String::from("r-xp"),
                path: String::from("/opt/lib/libmimalloc.so"),
            },
            ProcessMapping {
                start: 0x3000,
                end: 0x4000,
                permissions: String::from("r-xp"),
                path: String::from("/usr/lib/libc.so.6"),
            },
            ProcessMapping {
                start: 0x5000,
                end: 0x6000,
                permissions: String::from("r-xp"),
                path: String::from("/opt/lib/libmalloc-wrapper.so"),
            },
        ];
        let ambiguous = allocator_snapshot(&maps, &AllocatorProbe::default());
        assert_eq!(
            ambiguous.implementation,
            "multiple allocator runtimes detected"
        );

        let custom = allocator_snapshot(
            &maps,
            &AllocatorProbe {
                complete: true,
                dispatch_failures: 0,
                symbols: vec![
                    AllocatorProbeSymbol {
                        name: String::from("malloc"),
                        address: 0x5100,
                        indirect: false,
                    },
                    AllocatorProbeSymbol {
                        name: String::from("__libc_malloc"),
                        address: 0x3100,
                        indirect: false,
                    },
                ],
            },
        );
        assert_eq!(custom.implementation, "custom or interposed allocator");
    }

    #[test]
    fn reports_split_malloc_and_free_ownership() {
        let maps = [
            allocator_test_mapping(0x1000, 0x2000, "/usr/lib/libjemalloc.so.2"),
            allocator_test_mapping(0x3000, 0x4000, "/usr/lib/libc.so.6"),
        ];
        let probe = allocator_test_probe(&[
            allocator_test_symbol("malloc", 0x1100),
            allocator_test_symbol("free", 0x3100),
        ]);
        let snapshot = allocator_snapshot(&maps, &probe);
        assert_eq!(snapshot.implementation, "split allocator bindings");
        assert!(snapshot.detection_basis.contains("different modules"));
        assert!(
            snapshot
                .evidence
                .iter()
                .any(|item| item.contains("malloc resolves to libjemalloc.so.2"))
        );
    }

    #[test]
    fn treats_separate_segments_and_deleted_suffixes_as_one_module() {
        let maps = [
            allocator_test_mapping(0x1000, 0x2000, "/tmp/libtcmalloc.so.4 (deleted)"),
            allocator_test_mapping(0x3000, 0x4000, "/tmp/libtcmalloc.so.4"),
        ];
        let probe = allocator_test_probe(&[
            allocator_test_symbol("malloc", 0x1100),
            allocator_test_symbol("free", 0x3100),
        ]);
        let snapshot = allocator_snapshot(&maps, &probe);
        assert_eq!(snapshot.implementation, "tcmalloc");
        assert_ne!(snapshot.implementation, "split allocator bindings");
    }

    #[test]
    fn does_not_claim_a_plt_trampoline_as_allocator_ownership() {
        let maps = [allocator_test_mapping(0x1000, 0x2000, "/opt/bin/service")];
        let mut malloc = allocator_test_symbol("malloc", 0x1100);
        malloc.indirect = true;
        let snapshot = allocator_snapshot(&maps, &allocator_test_probe(&[malloc]));
        assert_eq!(snapshot.implementation, "allocator binding unresolved");
        assert!(snapshot.default_bindings[0].indirect);
        assert!(snapshot.evidence[0].contains("PLT/GOT trampoline"));
    }

    #[test]
    fn recognizes_common_gdb_plt_and_got_spellings() {
        for value in [
            "(void *) 0x401030 <malloc@plt>",
            "0x401030 <malloc.plt>",
            "0x401030 <malloc@got.plt>",
            "0x401030 <plt for malloc>",
        ] {
            assert!(allocator_probe_value_is_indirect(value), "{value}");
        }
        assert!(!allocator_probe_value_is_indirect(
            "(void *) 0x7ffff7e1c920 <__GI___libc_malloc>"
        ));
    }

    #[test]
    fn recognizes_versioned_platform_and_instrumentation_allocator_modules() {
        for (path, expected) in [
            ("/usr/lib/libc.so.6", "glibc / ptmalloc"),
            ("/lib/ld-musl-x86_64.so.1", "musl allocator"),
            ("/lib/libc.so.0", "uClibc allocator"),
            (
                "/apex/com.android.runtime/lib64/bionic/libc.so",
                "Android Bionic malloc dispatch",
            ),
            ("/usr/lib/libjemalloc.so.2", "jemalloc"),
            ("/usr/lib/libtcmalloc_and_profiler.so.4", "tcmalloc"),
            ("/usr/lib/libmimalloc.so.2", "mimalloc"),
            ("/usr/lib/libscalloc.so.1", "scalloc"),
            ("/usr/lib/libssmalloc.so", "SSMalloc"),
            ("/usr/lib/libasan.so.8.0.0", "AddressSanitizer allocator"),
            ("/usr/lib/libclang_rt.scudo_standalone-x86_64.so", "Scudo"),
            (
                "/usr/lib/libtbbmalloc_proxy.so.2",
                "oneTBB scalable allocator",
            ),
            ("/usr/lib/libefence.so.0", "Electric Fence"),
        ] {
            assert_eq!(
                allocator_family_for_path(path).map(AllocatorFamily::display_name),
                Some(expected),
                "{path}"
            );
        }
    }

    #[test]
    fn records_a_degraded_probe_without_overstating_completeness() {
        let maps = [allocator_test_mapping(0x1000, 0x2000, "/usr/lib/libc.so.6")];
        let snapshot = allocator_snapshot(
            &maps,
            &AllocatorProbe {
                complete: true,
                dispatch_failures: 3,
                symbols: Vec::new(),
            },
        );
        assert_eq!(snapshot.probe_dispatch_failures, 3);
        assert!(
            snapshot
                .evidence
                .iter()
                .any(|item| item.contains("3 optional GDB symbol probes"))
        );
    }

    #[test]
    fn does_not_infer_an_allocator_from_an_executable_filename() {
        let maps = [allocator_test_mapping(
            0x1000,
            0x2000,
            "/opt/bin/jemalloc-benchmark",
        )];
        let probe = allocator_test_probe(&[allocator_test_symbol("malloc", 0x1100)]);
        let snapshot = allocator_snapshot(&maps, &probe);
        assert_eq!(snapshot.implementation, "custom or interposed allocator");
        assert!(snapshot.detected_runtimes.is_empty());
    }

    #[test]
    fn a_specific_static_allocator_marker_beats_generic_libc_evidence() {
        let maps = [allocator_test_mapping(
            0x1000,
            0x3000,
            "/opt/bin/static-service",
        )];
        let probe = allocator_test_probe(&[
            allocator_test_symbol("malloc", 0x1100),
            allocator_test_symbol("__libc_malloc", 0x1200),
            allocator_test_symbol("tc_malloc", 0x1300),
        ]);
        let snapshot = allocator_snapshot(&maps, &probe);
        assert_eq!(snapshot.implementation, "tcmalloc");
        assert!(snapshot.detection_basis.contains("allocator-specific"));
    }

    #[test]
    fn conflicting_static_allocator_markers_are_not_guessed() {
        let maps = [allocator_test_mapping(
            0x1000,
            0x3000,
            "/opt/bin/static-service",
        )];
        let probe = allocator_test_probe(&[
            allocator_test_symbol("malloc", 0x1100),
            allocator_test_symbol("tc_malloc", 0x1200),
            allocator_test_symbol("mi_malloc", 0x1300),
        ]);
        let snapshot = allocator_snapshot(&maps, &probe);
        assert_eq!(snapshot.implementation, "conflicting allocator evidence");
    }

    #[test]
    fn reports_language_frontends_without_replacing_the_c_allocator() {
        let maps = [
            allocator_test_mapping(0x1000, 0x2000, "/usr/lib/libc.so.6"),
            allocator_test_mapping(0x3000, 0x4000, "/opt/bin/go-service"),
        ];
        let probe = allocator_test_probe(&[
            allocator_test_symbol("malloc", 0x1100),
            allocator_test_symbol("runtime.mallocgc", 0x3100),
        ]);
        let snapshot = allocator_snapshot(&maps, &probe);
        assert_eq!(snapshot.implementation, "glibc / ptmalloc");
        assert_eq!(
            snapshot.allocation_frontends,
            [String::from("Go managed heap")]
        );
    }

    fn allocator_test_mapping(start: u64, end: u64, path: &str) -> ProcessMapping {
        ProcessMapping {
            start,
            end,
            permissions: String::from("r-xp"),
            path: path.to_owned(),
        }
    }

    fn allocator_test_symbol(name: &str, address: u64) -> AllocatorProbeSymbol {
        AllocatorProbeSymbol {
            name: name.to_owned(),
            address,
            indirect: false,
        }
    }

    fn allocator_test_probe(symbols: &[AllocatorProbeSymbol]) -> AllocatorProbe {
        AllocatorProbe {
            complete: true,
            dispatch_failures: 0,
            symbols: symbols.to_vec(),
        }
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
