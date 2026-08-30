use std::sync::Arc;

use super::mi::{MiListItem, MiRecord, MiResult, MiValue, result_field};
use super::target::{TargetArchitecture, TargetEndian};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StackFrame {
    pub level: u32,
    pub address: String,
    pub function: String,
    pub architecture: Option<String>,
    pub file: Option<String>,
    pub fullname: Option<String>,
    pub line: Option<u32>,
}

impl StackFrame {
    pub fn source_path(&self) -> Option<&str> {
        self.fullname.as_deref().or(self.file.as_deref())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Variable {
    pub name: String,
    pub value: String,
    pub type_name: Option<String>,
    pub varobj: Option<String>,
    pub num_children: usize,
    pub has_more: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VariableUpdate {
    pub varobj: String,
    pub value: Option<String>,
    pub in_scope: Option<bool>,
    pub type_changed: bool,
    pub new_type: Option<String>,
    pub new_num_children: Option<usize>,
    pub has_more: Option<bool>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ValueTypeKind {
    Integer,
    Float,
    Enum,
    Boolean,
    Character,
    #[default]
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnumVariant {
    pub name: String,
    pub value: String,
}

/// Target-derived facts used by value editors. This is intentionally fetched
/// on demand: expanding a large locals tree must not issue one GDB query per
/// value merely to populate the debugger view.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ValueTypeMetadata {
    pub kind: ValueTypeKind,
    pub bits: Option<u32>,
    pub signed: Option<bool>,
    pub language: Option<String>,
    pub raw_bytes: Option<Vec<u8>>,
    pub enum_variants: Vec<EnumVariant>,
}

impl Variable {
    pub fn is_pointer(&self) -> bool {
        self.type_name.as_deref().is_some_and(|type_name| {
            let type_name = type_name.trim();
            type_name.contains('*') || type_name.starts_with('&') || type_name.ends_with('&')
        })
    }

    pub fn can_expand(&self) -> bool {
        self.varobj.is_some()
            && (self.num_children > 0
                || self.has_more
                || (self.is_pointer()
                    && !matches!(
                        self.value.trim(),
                        "0" | "0x0" | "nullptr" | "<not available>" | "<optimized out>"
                    )))
    }

    /// Scalar values returned by `-stack-list-variables --simple-values` are
    /// already complete and can be assigned by expression. Reserve GDB
    /// variable objects for values that can actually benefit from expansion.
    pub fn needs_variable_object(&self) -> bool {
        self.is_pointer() || self.value == "<not available>"
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Register {
    pub name: String,
    pub value: String,
    pub pointer_chain: Vec<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MemoryKind {
    Code,
    Heap,
    Stack,
    Writable,
    ReadOnly,
    Rwx,
    String,
    #[default]
    None,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemoryBlock {
    pub begin: u64,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceFile {
    pub file: String,
    pub fullname: Option<String>,
    pub line: u32,
}

impl SourceFile {
    pub fn source_path(&self) -> &str {
        self.fullname.as_deref().unwrap_or(&self.file)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StackEntry {
    pub address: u64,
    pub offset: usize,
    pub index: usize,
    pub pointer_bits: u32,
    pub endian: TargetEndian,
    pub value: String,
    pub pointer_chain: Vec<String>,
    pub address_registers: Vec<String>,
    pub value_registers: Vec<String>,
    pub return_frame: Option<u32>,
    pub memory_kind: MemoryKind,
    pub region: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThreadInfo {
    pub id: String,
    pub target_id: String,
    pub name: Option<String>,
    pub state: String,
    pub core: Option<String>,
    pub frame: Option<StackFrame>,
    pub pc_symbol: Option<String>,
    pub current: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SharedLibrary {
    pub target_name: String,
    pub host_name: Option<String>,
    pub symbols_loaded: bool,
    pub from: Option<String>,
    pub to: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Instruction {
    pub address: String,
    pub function: String,
    pub offset: String,
    pub opcodes: Option<String>,
    pub text: String,
    pub source: Option<Arc<SourceFile>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Breakpoint {
    pub number: String,
    pub kind: String,
    pub enabled: bool,
    pub condition: Option<String>,
    pub address: Option<String>,
    pub function: Option<String>,
    pub file: Option<String>,
    pub fullname: Option<String>,
    pub line: Option<u32>,
    pub original_location: Option<String>,
    pub catch_type: Option<String>,
    pub disposition: Option<String>,
    pub hit_count: u64,
    pub ignore_count: u64,
    pub thread: Option<String>,
    pub inferior: Option<String>,
    pub pending: Option<String>,
    pub commands: Vec<String>,
    pub parent_number: Option<String>,
    pub location_count: usize,
}

impl Breakpoint {
    pub fn source_path(&self) -> Option<&str> {
        self.fullname.as_deref().or(self.file.as_deref())
    }

    pub fn is_watchpoint(&self) -> bool {
        self.kind.contains("watchpoint")
    }

    pub fn is_catchpoint(&self) -> bool {
        self.kind.contains("catchpoint")
    }

    pub fn is_signal_catchpoint(&self) -> bool {
        self.is_catchpoint()
            && (self.catch_type.as_deref() == Some("signal")
                || self.original_location.as_deref().is_some_and(|location| {
                    location.starts_with("SIG") || location == "<any signal>"
                }))
    }

    pub fn command_number(&self) -> &str {
        self.number
            .split_once('.')
            .map_or(self.number.as_str(), |(root, _)| root)
    }

    pub fn is_location(&self) -> bool {
        self.parent_number.is_some()
    }

    pub fn is_logpoint(&self) -> bool {
        self.kind == "dprintf"
            || (self
                .commands
                .first()
                .is_some_and(|command| command == "silent")
                && self
                    .commands
                    .last()
                    .is_some_and(|command| matches!(command.trim(), "continue" | "cont" | "c")))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceLocation {
    pub function: String,
    pub file: String,
    pub fullname: Option<String>,
    pub line: u32,
}

impl SourceLocation {
    pub fn source_path(&self) -> &str {
        self.fullname.as_deref().unwrap_or(&self.file)
    }
}

pub fn stack_frames(record: &MiRecord) -> Vec<StackFrame> {
    record
        .field("stack")
        .and_then(MiValue::as_list)
        .into_iter()
        .flatten()
        .filter_map(|item| match item {
            MiListItem::Result(result) if result.name == "frame" => result.value.as_tuple(),
            MiListItem::Value(MiValue::Tuple(tuple)) => Some(tuple.as_slice()),
            _ => None,
        })
        .filter_map(stack_frame)
        .collect()
}

pub fn current_frame(record: &MiRecord) -> Option<StackFrame> {
    record
        .field("frame")
        .and_then(MiValue::as_tuple)
        .and_then(stack_frame)
}

pub fn variables(record: &MiRecord) -> Vec<Variable> {
    record
        .field("variables")
        .and_then(MiValue::as_list)
        .into_iter()
        .flatten()
        .filter_map(tuple_from_item)
        .filter_map(|tuple| {
            Some(Variable {
                name: constant(tuple, "name")?.to_owned(),
                value: constant(tuple, "value")
                    .unwrap_or("<not available>")
                    .to_owned(),
                type_name: owned_constant(tuple, "type"),
                varobj: None,
                num_children: 0,
                has_more: false,
            })
        })
        .collect()
}

pub fn variable_object(record: &MiRecord, display_name: &str) -> Option<Variable> {
    Some(Variable {
        name: display_name.to_owned(),
        value: record
            .field("value")
            .and_then(MiValue::as_const)
            .unwrap_or("<not available>")
            .to_owned(),
        type_name: record
            .field("type")
            .and_then(MiValue::as_const)
            .map(str::to_owned),
        varobj: Some(record.field("name")?.as_const()?.to_owned()),
        num_children: record
            .field("numchild")
            .and_then(MiValue::as_const)
            .and_then(|value| value.parse().ok())
            .unwrap_or(0),
        has_more: record.field("has_more").and_then(MiValue::as_const) == Some("1"),
    })
}

pub fn variable_updates(record: &MiRecord) -> Vec<VariableUpdate> {
    record
        .field("changelist")
        .and_then(MiValue::as_list)
        .into_iter()
        .flatten()
        .filter_map(tuple_from_item)
        .filter_map(|tuple| {
            let in_scope = match constant(tuple, "in_scope") {
                Some("true") => Some(true),
                Some("false" | "invalid") => Some(false),
                _ => None,
            };
            Some(VariableUpdate {
                varobj: constant(tuple, "name")?.to_owned(),
                value: owned_constant(tuple, "value"),
                in_scope,
                type_changed: constant(tuple, "type_changed") == Some("true"),
                new_type: owned_constant(tuple, "new_type"),
                new_num_children: constant(tuple, "new_num_children")
                    .and_then(|value| value.parse().ok()),
                has_more: constant(tuple, "has_more").map(|value| value == "1"),
            })
        })
        .collect()
}

pub fn variable_children(record: &MiRecord) -> Vec<Variable> {
    record
        .field("children")
        .and_then(MiValue::as_list)
        .into_iter()
        .flatten()
        .filter_map(tuple_from_item)
        .filter_map(|tuple| {
            let expression = constant(tuple, "exp").or_else(|| constant(tuple, "name"))?;
            Some(Variable {
                name: variable_child_name(expression),
                value: constant(tuple, "value")
                    .unwrap_or("<not available>")
                    .to_owned(),
                type_name: owned_constant(tuple, "type"),
                varobj: owned_constant(tuple, "name"),
                num_children: constant(tuple, "numchild")
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(0),
                has_more: constant(tuple, "has_more") == Some("1"),
            })
        })
        .collect()
}

pub fn variable_children_have_more(record: &MiRecord) -> bool {
    record.field("has_more").and_then(MiValue::as_const) == Some("1")
}

fn variable_child_name(expression: &str) -> String {
    if !expression.is_empty()
        && expression
            .chars()
            .all(|character| character.is_ascii_digit())
    {
        format!("[{expression}]")
    } else {
        expression.to_owned()
    }
}

pub fn variable_path_expression(record: &MiRecord) -> Option<String> {
    record
        .field("path_expr")
        .and_then(MiValue::as_const)
        .map(str::to_owned)
}

pub fn register_names(record: &MiRecord) -> Vec<String> {
    record
        .field("register-names")
        .and_then(MiValue::as_list)
        .into_iter()
        .flatten()
        .filter_map(|item| match item {
            MiListItem::Value(value) => value.as_const(),
            MiListItem::Result(_) => None,
        })
        .map(str::to_owned)
        .collect()
}

pub fn compact_register_numbers(names: &[String], architecture: TargetArchitecture) -> Vec<usize> {
    const MAX_REGISTER_VALUES: usize = 256;
    let architecture = architecture.effective_for_registers(names);
    if architecture == TargetArchitecture::Unknown {
        return names
            .iter()
            .enumerate()
            // Unknown architectures must degrade to a complete, bounded
            // snapshot. A partial list based on generic aliases can silently
            // hide the registers needed to identify the target later.
            .filter(|(_, name)| !name.is_empty())
            .map(|(number, _)| number)
            .take(MAX_REGISTER_VALUES)
            .collect();
    }
    let has_preferred = names
        .iter()
        .any(|name| architecture.is_core_register(name) || architecture.is_thread_pointer(name));
    if has_preferred {
        // Always keep a complete first-stop snapshot. On x86, request only
        // the widest advertised SIMD alias (ZMM, YMM, or XMM), rather than
        // transferring overlapping views of the same register file.
        let x86_vector_prefix = match architecture {
            TargetArchitecture::X86 | TargetArchitecture::X86_64 => {
                if names.iter().any(|name| vector_register(name, "zmm")) {
                    Some("zmm")
                } else if names.iter().any(|name| vector_register(name, "ymm")) {
                    Some("ymm")
                } else {
                    Some("xmm")
                }
            }
            _ => None,
        };
        return names
            .iter()
            .enumerate()
            .filter(|(_, name)| {
                architecture.is_core_register(name)
                    || architecture.is_thread_pointer(name)
                    || x86_vector_prefix.is_some_and(|prefix| vector_register(name, prefix))
                    || (matches!(
                        architecture,
                        TargetArchitecture::X86 | TargetArchitecture::X86_64
                    ) && name.as_str() == "mxcsr")
                    || (x86_vector_prefix.is_none() && architecture.is_vector_register(name))
                    || architecture.is_floating_register(name)
            })
            .map(|(number, _)| number)
            .take(MAX_REGISTER_VALUES)
            .collect();
    }

    names
        .iter()
        .enumerate()
        .filter(|(_, name)| !name.is_empty())
        .map(|(number, _)| number)
        .take(MAX_REGISTER_VALUES)
        .collect()
}

fn vector_register(name: &str, prefix: &str) -> bool {
    name.strip_prefix(prefix).is_some_and(|index| {
        !index.is_empty() && index.chars().all(|character| character.is_ascii_digit())
    })
}

pub fn registers(record: &MiRecord, names: &[String]) -> Vec<Register> {
    record
        .field("register-values")
        .and_then(MiValue::as_list)
        .into_iter()
        .flatten()
        .filter_map(tuple_from_item)
        .filter_map(|tuple| {
            let number = constant(tuple, "number")?.parse::<usize>().ok()?;
            let name = names.get(number)?.to_owned();
            if name.is_empty() {
                return None;
            }
            Some(Register {
                name,
                value: constant(tuple, "value")
                    .unwrap_or("<not available>")
                    .to_owned(),
                pointer_chain: Vec::new(),
            })
        })
        .collect()
}

pub fn evaluated_value(record: &MiRecord) -> Option<String> {
    record
        .field("value")
        .and_then(MiValue::as_const)
        .map(str::to_owned)
}

pub fn inferior_pid(record: &MiRecord) -> Option<u32> {
    record
        .field("groups")
        .and_then(MiValue::as_list)
        .into_iter()
        .flatten()
        .filter_map(tuple_from_item)
        .find_map(|tuple| constant(tuple, "pid").and_then(|pid| pid.parse().ok()))
}

pub fn memory_block(record: &MiRecord) -> Option<MemoryBlock> {
    let tuple = record
        .field("memory")
        .and_then(MiValue::as_list)?
        .iter()
        .filter_map(tuple_from_item)
        .next()?;
    let begin = parse_hex(constant(tuple, "begin")?)?;
    let contents = constant(tuple, "contents")?;
    let (pairs, remainder) = contents.as_bytes().as_chunks::<2>();
    if !remainder.is_empty() {
        return None;
    }
    let bytes = pairs
        .iter()
        .map(|pair| {
            let high = hex_nibble(pair[0])?;
            let low = hex_nibble(pair[1])?;
            Some((high << 4) | low)
        })
        .collect::<Option<Vec<_>>>()?;
    Some(MemoryBlock { begin, bytes })
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn parse_hex(value: &str) -> Option<u64> {
    u64::from_str_radix(value.strip_prefix("0x")?, 16).ok()
}

pub fn threads(record: &MiRecord) -> Vec<ThreadInfo> {
    let current = record
        .field("current-thread-id")
        .and_then(MiValue::as_const);
    record
        .field("threads")
        .and_then(MiValue::as_list)
        .into_iter()
        .flatten()
        .filter_map(tuple_from_item)
        .filter_map(|tuple| {
            let id = constant(tuple, "id")?.to_owned();
            Some(ThreadInfo {
                current: current == Some(id.as_str()),
                id,
                target_id: constant(tuple, "target-id").unwrap_or("unknown").to_owned(),
                name: owned_constant(tuple, "name"),
                state: constant(tuple, "state").unwrap_or("unknown").to_owned(),
                core: owned_constant(tuple, "core"),
                frame: result_field(tuple, "frame")
                    .and_then(MiValue::as_tuple)
                    .and_then(stack_frame),
                pc_symbol: None,
            })
        })
        .collect()
}

pub fn instructions(record: &MiRecord) -> Vec<Instruction> {
    let Some(items) = record.field("asm_insns").and_then(MiValue::as_list) else {
        return Vec::new();
    };
    let mut instructions = Vec::new();
    for tuple in items.iter().filter_map(tuple_from_item) {
        let source = source_file(tuple);
        if let Some(nested) = result_field(tuple, "line_asm_insn").and_then(MiValue::as_list) {
            instructions.extend(
                nested
                    .iter()
                    .filter_map(tuple_from_item)
                    .filter_map(|tuple| instruction(tuple, source.clone())),
            );
        } else if let Some(instruction) = instruction(tuple, source) {
            instructions.push(instruction);
        }
    }
    instructions
}

fn instruction(tuple: &[MiResult], source: Option<Arc<SourceFile>>) -> Option<Instruction> {
    Some(Instruction {
        address: constant(tuple, "address")?.to_owned(),
        function: constant(tuple, "func-name").unwrap_or("??").to_owned(),
        // Address-range disassembly has no containing symbol and therefore no
        // meaningful function offset. Keep that distinct from a real `+0`
        // function boundary.
        offset: constant(tuple, "offset").unwrap_or("").to_owned(),
        opcodes: owned_constant(tuple, "opcodes"),
        text: constant(tuple, "inst")?.to_owned(),
        source,
    })
}

fn source_file(tuple: &[MiResult]) -> Option<Arc<SourceFile>> {
    Some(Arc::new(SourceFile {
        file: constant(tuple, "file")?.to_owned(),
        fullname: owned_constant(tuple, "fullname"),
        line: constant(tuple, "line")?.parse().ok()?,
    }))
}

pub fn shared_libraries(record: &MiRecord) -> Vec<SharedLibrary> {
    record
        .field("shared-libraries")
        .and_then(MiValue::as_list)
        .into_iter()
        .flatten()
        .filter_map(tuple_from_item)
        .filter_map(|tuple| {
            let range = result_field(tuple, "ranges")
                .and_then(MiValue::as_list)
                .into_iter()
                .flatten()
                .find_map(tuple_from_item);
            Some(SharedLibrary {
                target_name: constant(tuple, "target-name")?.to_owned(),
                host_name: owned_constant(tuple, "host-name"),
                symbols_loaded: constant(tuple, "symbols-loaded") == Some("1"),
                from: range.and_then(|range| owned_constant(range, "from")),
                to: range.and_then(|range| owned_constant(range, "to")),
            })
        })
        .collect()
}

pub fn breakpoints(record: &MiRecord) -> Vec<Breakpoint> {
    let Some(table) = record.field("BreakpointTable").and_then(MiValue::as_tuple) else {
        return Vec::new();
    };
    result_field(table, "body")
        .and_then(MiValue::as_list)
        .into_iter()
        .flatten()
        .filter_map(tuple_from_item)
        .flat_map(expand_breakpoint)
        .collect()
}

pub fn inserted_breakpoints(record: &MiRecord) -> Vec<Breakpoint> {
    record
        .field("bkpt")
        .and_then(MiValue::as_tuple)
        .map(expand_breakpoint)
        .unwrap_or_default()
}

pub fn executable_source_lines(record: &MiRecord) -> Vec<u32> {
    let mut lines = record
        .field("lines")
        .and_then(MiValue::as_list)
        .into_iter()
        .flatten()
        .filter_map(tuple_from_item)
        .filter_map(|tuple| constant(tuple, "line")?.parse().ok())
        .filter(|line| *line > 0)
        .collect::<Vec<_>>();
    lines.sort_unstable();
    lines.dedup();
    lines
}

pub fn source_locations(record: &MiRecord) -> Vec<SourceLocation> {
    let Some(symbols) = record.field("symbols").and_then(MiValue::as_tuple) else {
        return Vec::new();
    };
    result_field(symbols, "debug")
        .and_then(MiValue::as_list)
        .into_iter()
        .flatten()
        .filter_map(tuple_from_item)
        .flat_map(|file| {
            let filename = constant(file, "filename").unwrap_or("source").to_owned();
            let fullname = owned_constant(file, "fullname");
            result_field(file, "symbols")
                .and_then(MiValue::as_list)
                .into_iter()
                .flatten()
                .filter_map(tuple_from_item)
                .filter_map(move |symbol| {
                    Some(SourceLocation {
                        function: constant(symbol, "name")?.to_owned(),
                        file: filename.clone(),
                        fullname: fullname.clone(),
                        line: constant(symbol, "line")?.parse().ok()?,
                    })
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

pub fn current_source(record: &MiRecord) -> Option<SourceFile> {
    Some(SourceFile {
        file: record.field("file")?.as_const()?.to_owned(),
        fullname: record
            .field("fullname")
            .and_then(MiValue::as_const)
            .map(str::to_owned),
        line: record.field("line")?.as_const()?.parse().ok()?,
    })
}

pub fn has_exact_command_completion(record: &MiRecord, command: &str) -> bool {
    record.is_done() && record.field("completion").and_then(MiValue::as_const) == Some(command)
}

fn expand_breakpoint(tuple: &[MiResult]) -> Vec<Breakpoint> {
    let Some(mut parent) = breakpoint(tuple) else {
        return Vec::new();
    };
    let mut locations: Vec<_> = result_field(tuple, "locations")
        .and_then(MiValue::as_list)
        .into_iter()
        .flatten()
        .filter_map(tuple_from_item)
        .filter_map(breakpoint)
        .collect();
    if locations.is_empty() {
        vec![parent]
    } else {
        parent.location_count = locations.len();
        for location in &mut locations {
            location.parent_number = Some(parent.number.clone());
            if location.condition.is_none() {
                location.condition.clone_from(&parent.condition);
            }
            if location.original_location.is_none() {
                location
                    .original_location
                    .clone_from(&parent.original_location);
            }
            if location.catch_type.is_none() {
                location.catch_type.clone_from(&parent.catch_type);
            }
            if location.disposition.is_none() {
                location.disposition.clone_from(&parent.disposition);
            }
            if location.hit_count == 0 {
                location.hit_count = parent.hit_count;
            }
            if location.ignore_count == 0 {
                location.ignore_count = parent.ignore_count;
            }
            if location.thread.is_none() {
                location.thread.clone_from(&parent.thread);
            }
            if location.inferior.is_none() {
                location.inferior.clone_from(&parent.inferior);
            }
            if location.commands.is_empty() {
                location.commands.clone_from(&parent.commands);
            }
        }
        let mut expanded = Vec::with_capacity(locations.len() + 1);
        expanded.push(parent);
        expanded.extend(locations);
        expanded
    }
}

fn breakpoint(tuple: &[MiResult]) -> Option<Breakpoint> {
    Some(Breakpoint {
        number: constant(tuple, "number")?.to_owned(),
        kind: constant(tuple, "type").unwrap_or("breakpoint").to_owned(),
        enabled: constant(tuple, "enabled") != Some("n"),
        condition: owned_constant(tuple, "cond"),
        address: owned_constant(tuple, "addr"),
        function: owned_constant(tuple, "func"),
        file: owned_constant(tuple, "file"),
        fullname: owned_constant(tuple, "fullname"),
        line: constant(tuple, "line").and_then(|line| line.parse().ok()),
        original_location: owned_constant(tuple, "original-location")
            .or_else(|| owned_constant(tuple, "what"))
            .or_else(|| owned_constant(tuple, "exp")),
        catch_type: owned_constant(tuple, "catch-type"),
        disposition: owned_constant(tuple, "disp"),
        hit_count: constant(tuple, "times")
            .and_then(|times| times.parse().ok())
            .unwrap_or(0),
        ignore_count: constant(tuple, "ignore")
            .and_then(|count| count.parse().ok())
            .unwrap_or(0),
        thread: owned_constant(tuple, "thread"),
        inferior: owned_constant(tuple, "inferior"),
        pending: owned_constant(tuple, "pending"),
        commands: result_field(tuple, "script")
            .and_then(MiValue::as_list)
            .into_iter()
            .flatten()
            .filter_map(|item| match item {
                MiListItem::Value(MiValue::Const(command)) => Some(command.clone()),
                MiListItem::Result(_) | MiListItem::Value(MiValue::Tuple(_) | MiValue::List(_)) => {
                    None
                }
            })
            .collect(),
        parent_number: None,
        location_count: 0,
    })
}

fn stack_frame(tuple: &[MiResult]) -> Option<StackFrame> {
    Some(StackFrame {
        level: constant(tuple, "level")?.parse().ok()?,
        address: constant(tuple, "addr").unwrap_or("?").to_owned(),
        function: constant(tuple, "func").unwrap_or("??").to_owned(),
        architecture: owned_constant(tuple, "arch"),
        file: owned_constant(tuple, "file"),
        fullname: owned_constant(tuple, "fullname"),
        line: constant(tuple, "line").and_then(|line| line.parse().ok()),
    })
}

fn tuple_from_item(item: &MiListItem) -> Option<&[MiResult]> {
    match item {
        MiListItem::Value(MiValue::Tuple(tuple)) => Some(tuple),
        MiListItem::Result(result) => result.value.as_tuple(),
        MiListItem::Value(MiValue::Const(_) | MiValue::List(_)) => None,
    }
}

fn constant<'a>(tuple: &'a [MiResult], name: &str) -> Option<&'a str> {
    result_field(tuple, name).and_then(MiValue::as_const)
}

fn owned_constant(tuple: &[MiResult], name: &str) -> Option<String> {
    constant(tuple, name).map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::{
        breakpoints, compact_register_numbers, current_source, has_exact_command_completion,
        inferior_pid, inserted_breakpoints, instructions, memory_block, register_names, registers,
        shared_libraries, source_locations, stack_frames, threads, variable_children,
        variable_children_have_more, variable_object, variable_path_expression, variable_updates,
        variables,
    };
    use crate::debugger::mi::parse_record;
    use crate::debugger::{TargetArchitecture, TargetEndian};

    #[test]
    fn parses_target_byte_order_descriptions() {
        assert_eq!(
            TargetEndian::from_gdb_description("auto (currently little endian)"),
            Some(TargetEndian::Little)
        );
        assert_eq!(
            TargetEndian::from_gdb_description("The target is set to big endian"),
            Some(TargetEndian::Big)
        );
    }

    #[test]
    fn distinguishes_exact_cli_commands_from_prefix_matches() {
        let exact = parse_record(
            r#"1^done,completion="dt",matches=["dt","dtor-dump"],max_completions_reached="0""#,
        )
        .unwrap();
        assert!(has_exact_command_completion(&exact, "dt"));
        assert!(!has_exact_command_completion(&exact, "dtor"));

        let nested = parse_record(
            r#"2^done,completion="heap bins",matches=["heap bins","heap bins-simple"],max_completions_reached="0""#,
        )
        .unwrap();
        assert!(has_exact_command_completion(&nested, "heap bins"));
        assert!(!has_exact_command_completion(&nested, "heap bins-simple"));

        let missing = parse_record(r#"3^done,matches=[],max_completions_reached="0""#).unwrap();
        assert!(!has_exact_command_completion(&missing, "future-calls"));
    }

    #[test]
    fn converts_debugger_models() {
        let frames = stack_frames(
            &parse_record(r#"1^done,stack=[frame={level="0",addr="0x12",func="main",file="a.c",fullname="/tmp/a.c",line="9"}]"#).unwrap(),
        );
        assert_eq!(frames[0].function, "main");
        assert_eq!(frames[0].line, Some(9));

        let locals =
            variables(&parse_record(r#"2^done,variables=[{name="answer",value="42"}]"#).unwrap());
        assert_eq!(locals[0].value, "42");
        assert!(!locals[0].needs_variable_object());

        let expandable = variables(
            &parse_record(
                r#"2^done,variables=[{name="state",type="Demo"},{name="next",type="Demo *",value="0x12"}]"#,
            )
            .unwrap(),
        );
        assert!(
            expandable
                .iter()
                .all(|variable| variable.needs_variable_object())
        );

        let names =
            register_names(&parse_record(r#"3^done,register-names=["rax","rbx"]"#).unwrap());
        let values = registers(
            &parse_record(r#"4^done,register-values=[{number="1",value="0xff"}]"#).unwrap(),
            &names,
        );
        assert_eq!(values[0].name, "rbx");
        assert_eq!(
            compact_register_numbers(
                &[
                    String::from("rax"),
                    String::from("xmm0"),
                    String::from("rip"),
                    String::from("eax"),
                    String::new(),
                ],
                TargetArchitecture::X86_64,
            ),
            [0, 1, 2]
        );
        assert_eq!(
            compact_register_numbers(
                &[
                    String::from("rax"),
                    String::from("xmm0"),
                    String::from("ymm0"),
                    String::from("zmm0"),
                    String::from("st0"),
                    String::from("mxcsr"),
                ],
                TargetArchitecture::X86_64,
            ),
            [0, 3, 4, 5]
        );
        assert_eq!(
            compact_register_numbers(
                &[
                    String::from("x0"),
                    String::from("x31"),
                    String::from("pc"),
                    String::from("f0"),
                    String::from("v0"),
                ],
                TargetArchitecture::RiscV32,
            ),
            [0, 1, 2, 3, 4]
        );
        assert_eq!(
            compact_register_numbers(
                &[String::from("pc"), String::from("v0"), String::from("f0")],
                TargetArchitecture::Unknown,
            ),
            [0, 1, 2]
        );
        let mut powerpc = (0..32).map(|index| format!("r{index}")).collect::<Vec<_>>();
        powerpc.extend((0..32).map(|index| format!("f{index}")));
        powerpc.extend((0..32).map(|index| format!("vr{index}")));
        powerpc.extend((0..64).map(|index| format!("vs{index}")));
        powerpc.extend([String::from("pc"), String::from("msr")]);
        let selected = compact_register_numbers(&powerpc, TargetArchitecture::PowerPc64);
        assert_eq!(selected.len(), 162);
        assert_eq!(selected.last(), Some(&161));

        let thread_list = threads(&parse_record(r#"5^done,threads=[{id="1",target-id="Thread 1",name="main",state="stopped",core="3",frame={level="0",addr="0x12",func="main"}}],current-thread-id="1""#).unwrap());
        assert!(thread_list[0].current);
        assert_eq!(thread_list[0].core.as_deref(), Some("3"));

        let disassembly = instructions(&parse_record(r#"6^done,asm_insns=[{address="0x12",func-name="main",offset="0",opcodes="90",inst="nop"}]"#).unwrap());
        assert_eq!(disassembly[0].text, "nop");
        assert!(disassembly[0].source.is_none());

        let symbol_less = instructions(
            &parse_record(r#"6^done,asm_insns=[{address="0x13",opcodes="c3",inst="ret"}]"#)
                .unwrap(),
        );
        assert_eq!(symbol_less[0].function, "??");
        assert!(symbol_less[0].offset.is_empty());

        let mixed = instructions(&parse_record(r#"7^done,asm_insns=[src_and_asm_line={line="42",file="main.c",fullname="/tmp/main.c",line_asm_insn=[{address="0x20",func-name="main",offset="4",opcodes="c3",inst="ret"}]}]"#).unwrap());
        assert_eq!(mixed.len(), 1);
        assert_eq!(mixed[0].address, "0x20");
        assert_eq!(mixed[0].source.as_ref().map(|source| source.line), Some(42));
        assert_eq!(
            mixed[0].source.as_ref().map(|source| source.source_path()),
            Some("/tmp/main.c")
        );

        let libraries = shared_libraries(&parse_record(r#"6^done,shared-libraries=[{target-name="/usr/lib/libc.so.6",host-name="/usr/lib/libc.so.6",symbols-loaded="1",ranges=[{from="0x7000",to="0x9000"}]}]"#).unwrap());
        assert_eq!(libraries.len(), 1);
        assert_eq!(libraries[0].target_name, "/usr/lib/libc.so.6");
        assert!(libraries[0].symbols_loaded);
        assert_eq!(libraries[0].from.as_deref(), Some("0x7000"));

        let process = parse_record(
            r#"7^done,groups=[{id="i1",type="process",pid="1234",executable="/tmp/a"}]"#,
        )
        .unwrap();
        assert_eq!(inferior_pid(&process), Some(1234));

        let memory = memory_block(
            &parse_record(
                r#"8^done,memory=[{begin="0x1000",offset="0x0",end="0x1004",contents="0102feff"}]"#,
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(memory.begin, 0x1000);
        assert_eq!(memory.bytes, [1, 2, 0xfe, 0xff]);
        let uppercase = memory_block(
            &parse_record(
                r#"9^done,memory=[{begin="0x2000",offset="0x0",end="0x2002",contents="A0fF"}]"#,
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(uppercase.bytes, [0xa0, 0xff]);
        assert!(
            memory_block(
                &parse_record(
                    r#"10^done,memory=[{begin="0x2000",offset="0x0",end="0x2001",contents="g0"}]"#,
                )
                .unwrap()
            )
            .is_none()
        );
    }

    #[test]
    fn converts_breakpoint_table() {
        let record = parse_record(r#"5^done,BreakpointTable={nr_rows="1",body=[bkpt={number="1",type="breakpoint",disp="keep",enabled="y",addr="0x1",func="main",file="a.c",fullname="/tmp/a.c",line="12",cond="count == 4",times="7",thread="2",original-location="main"}]}"#).unwrap();
        let parsed_breakpoints = breakpoints(&record);
        assert_eq!(parsed_breakpoints.len(), 1);
        assert_eq!(parsed_breakpoints[0].number, "1");
        assert_eq!(parsed_breakpoints[0].kind, "breakpoint");
        assert_eq!(parsed_breakpoints[0].line, Some(12));
        assert_eq!(parsed_breakpoints[0].disposition.as_deref(), Some("keep"));
        assert_eq!(parsed_breakpoints[0].hit_count, 7);
        assert_eq!(parsed_breakpoints[0].thread.as_deref(), Some("2"));
        assert_eq!(
            parsed_breakpoints[0].condition.as_deref(),
            Some("count == 4")
        );
        let mut child_location = parsed_breakpoints[0].clone();
        child_location.number = String::from("4.2");
        assert_eq!(child_location.command_number(), "4");

        let watchpoints = breakpoints(&parse_record(r#"6^done,BreakpointTable={nr_rows="1",body=[bkpt={number="2",type="hw watchpoint",disp="keep",enabled="y",what="counter",cond="counter > 3"}]}"#).unwrap());
        assert!(watchpoints[0].is_watchpoint());
        assert_eq!(watchpoints[0].original_location.as_deref(), Some("counter"));
        assert_eq!(watchpoints[0].condition.as_deref(), Some("counter > 3"));

        let catchpoints = breakpoints(&parse_record(r#"7^done,BreakpointTable={body=[bkpt={number="3",type="catchpoint",enabled="y",what="exception throw",catch-type="throw"},bkpt={number="4",type="catchpoint",enabled="y",what="SIGSEGV",catch-type="signal"}]}"#).unwrap());
        assert_eq!(catchpoints[0].catch_type.as_deref(), Some("throw"));
        assert!(!catchpoints[0].is_signal_catchpoint());
        assert!(catchpoints[1].is_signal_catchpoint());

        let multi = breakpoints(&parse_record(r#"8^done,BreakpointTable={body=[bkpt={number="5",type="breakpoint",disp="keep",enabled="y",addr="<MULTIPLE>",times="2",ignore="3",script={"silent","printf \"hit\\n\"","continue"},original-location="Payload::Payload",locations=[{number="5.1",enabled="y",addr="0x10",func="Payload::Payload()",file="a.cpp",line="9"},{number="5.2",enabled="n",addr="0x20",func="Payload::Payload(Payload&&)",file="a.cpp",line="9"}]}]}"#).unwrap());
        assert_eq!(multi.len(), 3);
        assert_eq!(multi[0].location_count, 2);
        assert_eq!(multi[0].ignore_count, 3);
        assert!(multi[0].is_logpoint());
        assert_eq!(multi[0].commands.len(), 3);
        assert_eq!(multi[1].parent_number.as_deref(), Some("5"));
        assert_eq!(multi[2].parent_number.as_deref(), Some("5"));
        assert!(!multi[2].enabled);

        let pending = breakpoints(&parse_record(r#"9^done,BreakpointTable={body=[bkpt={number="6",type="breakpoint",disp="keep",enabled="y",addr="<PENDING>",pending="future_function",times="0",original-location="future_function"}]}"#).unwrap());
        assert_eq!(pending[0].pending.as_deref(), Some("future_function"));
        assert_eq!(pending[0].address.as_deref(), Some("<PENDING>"));
    }

    #[test]
    fn converts_inserted_breakpoints_and_executable_lines() {
        let inserted = inserted_breakpoints(
            &parse_record(
                r#"8^done,bkpt={number="3",type="breakpoint",enabled="y",addr="0x401100",func="main",file="main.c",fullname="/tmp/main.c",line="12"}"#,
            )
            .unwrap(),
        );
        assert_eq!(inserted.len(), 1);
        assert_eq!(inserted[0].number, "3");
        assert_eq!(inserted[0].line, Some(12));

        assert_eq!(
            super::executable_source_lines(
                &parse_record(
                    r#"9^done,lines=[{pc="0x1",line="12"},{pc="0x2",line="10"},{pc="0x3",line="12"},{pc="0x4",line="0"}]"#,
                )
                .unwrap()
            ),
            [10, 12]
        );
    }

    #[test]
    fn converts_typed_variable_objects_and_children() {
        let root = parse_record(
            r#"10^done,name="var1",numchild="2",value="{x = 1, nested = {...}}",type="struct Demo",has_more="0""#,
        )
        .unwrap();
        let root = variable_object(&root, "demo").unwrap();
        assert_eq!(root.name, "demo");
        assert_eq!(root.type_name.as_deref(), Some("struct Demo"));
        assert_eq!(root.varobj.as_deref(), Some("var1"));
        assert_eq!(root.num_children, 2);
        assert!(root.can_expand());

        let reference = variable_object(
            &parse_record(
                r#"10^done,name="var2",numchild="0",value="@0x12",type="const Demo &",has_more="0""#,
            )
            .unwrap(),
            "reference",
        )
        .unwrap();
        assert!(reference.is_pointer());
        assert!(reference.needs_variable_object());

        let dynamic = variable_object(
            &parse_record(
                r#"10^done,name="var3",numchild="0",value="std::vector of length 3",type="std::vector<int>",dynamic="1",has_more="1""#,
            )
            .unwrap(),
            "numbers",
        )
        .unwrap();
        assert_eq!(dynamic.num_children, 0);
        assert!(dynamic.has_more);
        assert!(dynamic.can_expand());

        let children_record = parse_record(
            r#"11^done,numchild="2",children=[child={name="var1.x",exp="x",numchild="0",type="int",value="1"},child={name="var1.nested",exp="nested",numchild="1",type="struct Inner",value="{...}",has_more="0"}],has_more="1""#,
        )
        .unwrap();
        assert!(variable_children_have_more(&children_record));
        let children = variable_children(&children_record);
        assert_eq!(children.len(), 2);
        assert_eq!(children[0].name, "x");
        assert_eq!(children[0].type_name.as_deref(), Some("int"));
        assert_eq!(children[1].varobj.as_deref(), Some("var1.nested"));
        assert!(children[1].can_expand());

        let array_children = variable_children(
            &parse_record(
                r#"13^done,numchild="1",children=[child={name="var3.0",exp="0",numchild="0",type="int",value="7"}],has_more="0""#,
            )
            .unwrap(),
        );
        assert_eq!(array_children[0].name, "[0]");

        let path = parse_record(r#"12^done,path_expr="demo.next""#).unwrap();
        assert_eq!(
            variable_path_expression(&path).as_deref(),
            Some("demo.next")
        );

        let null_pointer = super::Variable {
            name: String::from("pointer"),
            value: String::from("0x0"),
            type_name: Some(String::from("Demo *")),
            varobj: Some(String::from("var2")),
            num_children: 0,
            has_more: false,
        };
        assert!(!null_pointer.can_expand());
    }

    #[test]
    fn converts_incremental_variable_object_updates() {
        let record = parse_record(
            r#"14^done,changelist=[{name="var1",value="std::vector of length 4",in_scope="true",type_changed="false",new_num_children="4",has_more="1"},{name="var2",in_scope="invalid",type_changed="true",new_type="long"}]"#,
        )
        .unwrap();
        let updates = variable_updates(&record);
        assert_eq!(updates.len(), 2);
        assert_eq!(updates[0].varobj, "var1");
        assert_eq!(updates[0].value.as_deref(), Some("std::vector of length 4"));
        assert_eq!(updates[0].in_scope, Some(true));
        assert!(!updates[0].type_changed);
        assert_eq!(updates[0].new_num_children, Some(4));
        assert_eq!(updates[0].has_more, Some(true));
        assert_eq!(updates[1].in_scope, Some(false));
        assert!(updates[1].type_changed);
        assert_eq!(updates[1].new_type.as_deref(), Some("long"));
    }

    #[test]
    fn converts_symbol_source_locations() {
        let record = parse_record(r#"7^done,symbols={debug=[{filename="src/main.rs",fullname="/tmp/src/main.rs",symbols=[{line="14",name="demo::run",type="void ()",description="void demo::run();"}]}]}"#).unwrap();
        let locations = source_locations(&record);
        assert_eq!(locations.len(), 1);
        assert_eq!(locations[0].function, "demo::run");
        assert_eq!(locations[0].source_path(), "/tmp/src/main.rs");
        assert_eq!(locations[0].line, 14);
    }

    #[test]
    fn converts_current_executable_source() {
        let record = parse_record(
            r#"8^done,line="4",file="src/main.c",fullname="/tmp/project/src/main.c",macro-info="0""#,
        )
        .unwrap();
        let source = current_source(&record).unwrap();
        assert_eq!(source.source_path(), "/tmp/project/src/main.c");
        assert_eq!(source.line, 4);
    }
}
