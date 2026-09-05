use super::{
    MemoryBlock, MemoryKind, Register, StackEntry, StackFrame, TargetArchitecture, TargetEndian,
    thread_id_argument,
};

/// Immutable identity for data collected at one debugger stop.
///
/// GDB keeps an implicit global thread and frame selection. UI callbacks and
/// terminal commands can change that selection while an earlier group of MI
/// requests is still in flight, so stopped-state requests must carry their own
/// explicit context and responses must be rejected once this identity is no
/// longer current.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StopContext {
    transport_epoch: u64,
    generation: u64,
    inferior_id: Option<String>,
    thread_id: String,
    frame_level: u32,
}

impl StopContext {
    pub(crate) fn new(
        transport_epoch: u64,
        generation: u64,
        inferior_id: Option<String>,
        thread_id: String,
        frame_level: u32,
    ) -> Option<Self> {
        thread_id_argument(&thread_id)?;

        Some(Self {
            transport_epoch,
            generation,
            inferior_id,
            thread_id,
            frame_level,
        })
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    pub(crate) fn inferior_id(&self) -> Option<&str> {
        self.inferior_id.as_deref()
    }

    pub(crate) fn thread_id(&self) -> &str {
        &self.thread_id
    }

    pub(crate) fn frame_level(&self) -> u32 {
        self.frame_level
    }

    /// Add an explicit thread selector to an MI command.
    pub(crate) fn scope_thread(&self, command: &str) -> String {
        scope_mi_command(command, &self.thread_id, None)
    }

    /// Add explicit thread and frame selectors to an MI command.
    pub(crate) fn scope_frame(&self, command: &str) -> String {
        scope_mi_command(command, &self.thread_id, Some(self.frame_level))
    }
}

fn scope_mi_command(command: &str, thread_id: &str, frame_level: Option<u32>) -> String {
    let (operation, arguments) = command
        .split_once(' ')
        .map_or((command, ""), |(operation, arguments)| {
            (operation, arguments)
        });

    let mut scoped = format!("{operation} --thread {thread_id}");

    if let Some(frame_level) = frame_level {
        use std::fmt::Write as _;

        let _ = write!(scoped, " --frame {frame_level}");
    }

    if !arguments.is_empty() {
        scoped.push(' ');
        scoped.push_str(arguments);
    }

    scoped
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MemoryRegion {
    pub start: u64,
    pub end: u64,
    pub permissions: String,
    pub path: Option<String>,
    pub kind: MemoryKind,
    pub referenced_by: Vec<String>,
}

pub(crate) fn annotate_memory_regions(
    regions: &mut [MemoryRegion],
    registers: &[Register],
    architecture: TargetArchitecture,
) {
    for region in regions.iter_mut() {
        region.referenced_by.clear();
    }

    for register in registers
        .iter()
        .filter(|register| is_pointer_register(&register.name, architecture))
    {
        let Some(address) = pointer_address(&register.value) else {
            continue;
        };

        if let Some(index) = region_index(regions, address) {
            regions[index]
                .referenced_by
                .push(format!("${}", register.name));
        }
    }
}

impl MemoryRegion {
    pub fn contains(&self, address: u64) -> bool {
        self.start <= address && address < self.end
    }

    pub fn description(&self) -> String {
        match self.path.as_deref() {
            Some(path) => format!("{} · {path}", self.permissions),
            None => self.permissions.clone(),
        }
    }
}

pub(crate) fn pointer_address(value: &str) -> Option<u64> {
    // GDB uses several pointer renderings depending on the language, pretty
    // printer, and command. Accept both a bare address and forms such as
    // `(Node *) 0x1234`, while stopping before symbols and punctuation.
    let bytes = value.as_bytes();

    for start in 0..bytes.len().saturating_sub(1) {
        if bytes[start] != b'0' || !matches!(bytes[start + 1], b'x' | b'X') {
            continue;
        }

        let digits = value[start + 2..]
            .bytes()
            .take_while(u8::is_ascii_hexdigit)
            .count();

        if digits == 0 {
            continue;
        }

        let end = start + 2 + digits;

        if let Ok(address) = u64::from_str_radix(&value[start + 2..end], 16) {
            return Some(address);
        }
    }

    None
}

pub(crate) fn is_pointer_register(name: &str, architecture: TargetArchitecture) -> bool {
    architecture.is_address_register(name)
}

pub(crate) fn read_memory_regions(pid: u32, debugger_pid: u32) -> Vec<MemoryRegion> {
    const MAX_MAPS_BYTES: usize = 16 * 1024 * 1024;

    let Ok(maps) = crate::kernel::read_verified_local_proc(pid, debugger_pid, |target| {
        crate::bounded::read_string(&target.root().join("maps"), MAX_MAPS_BYTES)
            .map_err(|error| format!("Cannot read /proc/{pid}/maps: {error}"))
    }) else {
        return Vec::new();
    };

    maps.lines()
        .take(32_768)
        .filter_map(parse_memory_region)
        .collect()
}

pub(crate) fn build_stack_entries(
    memory: &MemoryBlock,
    word_size: usize,
    endian: TargetEndian,
    architecture: TargetArchitecture,
    registers: &[Register],
    frames: &[StackFrame],
    regions: &[MemoryRegion],
) -> Vec<StackEntry> {
    // Stack words are target pointers. Reject corrupt or unsupported ABI
    // widths here as a last line of defence. Slicing the fixed u64 buffer with
    // a wider value would otherwise panic.
    if !matches!(word_size, 4 | 8) {
        return Vec::new();
    }

    // Register and frame addresses are invariant across the stack window.
    // Decode them once instead of reparsing the same hexadecimal strings for
    // every stack word.
    let register_addresses = registers
        .iter()
        .filter(|register| is_pointer_register(&register.name, architecture))
        .filter_map(|register| {
            pointer_address(&register.value).map(|address| (address, register.name.as_str()))
        })
        .collect::<Vec<_>>();

    let return_addresses = frames
        .iter()
        .filter(|frame| frame.level > 0)
        .filter_map(|frame| pointer_address(&frame.address).map(|address| (address, frame.level)))
        .collect::<Vec<_>>();

    memory
        .bytes
        .chunks_exact(word_size)
        .enumerate()
        .map(|(index, bytes)| {
            let mut word = [0_u8; 8];
            word[..word_size].copy_from_slice(bytes);

            let value = match endian {
                TargetEndian::Little => u64::from_le_bytes(word),
                TargetEndian::Big => {
                    word.rotate_right(8 - word_size);

                    u64::from_be_bytes(word)
                }
            };

            let address = memory.begin.saturating_add((index * word_size) as u64);
            let address_registers = matching_registers(&register_addresses, address);

            let value_registers = if value == 0 {
                Vec::new()
            } else {
                matching_registers(&register_addresses, value)
            };

            let return_frame = return_addresses
                .iter()
                .find_map(|(address, level)| (*address == value).then_some(*level));

            let region = memory_region_for_address(regions, value);

            let memory_kind =
                if region.is_none() && looks_like_string_word(value, endian, word_size) {
                    MemoryKind::String
                } else {
                    region.map_or(MemoryKind::None, |region| region.kind)
                };

            StackEntry {
                address,
                offset: index * word_size,
                index,
                pointer_bits: u32::try_from(word_size * 8).unwrap_or(64),
                endian,
                value: format!("0x{value:x}"),
                pointer_chain: Vec::new(),
                address_registers,
                value_registers,
                return_frame,
                memory_kind,
                region: region.map(region_description),
            }
        })
        .collect()
}

pub(crate) fn looks_like_string_word(value: u64, endian: TargetEndian, word_size: usize) -> bool {
    let word_bytes = endian.word_bytes(value);

    let bytes = match endian {
        TargetEndian::Little => word_bytes.get(..word_size),
        TargetEndian::Big => word_bytes.get(8_usize.saturating_sub(word_size)..),
    };

    bytes
        .unwrap_or(&word_bytes)
        .iter()
        .take_while(|byte| byte.is_ascii_graphic() || **byte == b' ')
        .count()
        >= 4
}

fn matching_registers(registers: &[(u64, &str)], address: u64) -> Vec<String> {
    registers
        .iter()
        .filter(|(register_address, _)| *register_address == address)
        .map(|(_, name)| (*name).to_owned())
        .collect()
}

fn parse_memory_region(line: &str) -> Option<MemoryRegion> {
    let mut fields = line.split_whitespace();
    let (start, end) = fields.next()?.split_once('-')?;
    let start = u64::from_str_radix(start, 16).ok()?;
    let end = u64::from_str_radix(end, 16).ok()?;
    let permissions = fields.next()?.to_owned();
    fields.next()?;
    fields.next()?;
    fields.next()?;

    let path = fields.next().map(|first| {
        let mut path = first.to_owned();

        for component in fields {
            path.push(' ');
            path.push_str(component);
        }

        path
    });

    let kind = match path.as_deref() {
        Some("[stack]") => MemoryKind::Stack,
        Some("[heap]") => MemoryKind::Heap,
        _ if permissions.contains('x') && permissions.contains('w') => MemoryKind::Rwx,
        _ if permissions.contains('x') => MemoryKind::Code,
        _ if permissions.contains('w') => MemoryKind::Writable,
        _ if permissions.contains('r') => MemoryKind::ReadOnly,
        _ => MemoryKind::None,
    };

    Some(MemoryRegion {
        start,
        end,
        permissions,
        path,
        kind,
        referenced_by: Vec::new(),
    })
}

pub(crate) fn memory_region_for_address(
    regions: &[MemoryRegion],
    address: u64,
) -> Option<&MemoryRegion> {
    region_index(regions, address).and_then(|index| regions.get(index))
}

fn region_index(regions: &[MemoryRegion], address: u64) -> Option<usize> {
    let index = regions
        .partition_point(|region| region.start <= address)
        .checked_sub(1)?;

    regions[index].contains(address).then_some(index)
}

fn region_description(region: &MemoryRegion) -> String {
    region.description()
}

#[cfg(test)]
mod tests {
    use super::{
        MemoryRegion, StopContext, annotate_memory_regions, build_stack_entries,
        memory_region_for_address, parse_memory_region, pointer_address,
    };
    use crate::debugger::{
        MemoryBlock, MemoryKind, Register, StackFrame, TargetArchitecture, TargetEndian,
    };

    #[test]
    fn stop_context_scopes_mi_commands_without_reordering_arguments() {
        let context =
            StopContext::new(7, 11, Some(String::from("i2")), String::from("2.19"), 3).unwrap();

        assert_eq!(
            context.scope_thread("-stack-list-frames 0 24"),
            "-stack-list-frames --thread 2.19 0 24"
        );

        assert_eq!(
            context.scope_frame("-stack-list-variables --simple-values"),
            "-stack-list-variables --thread 2.19 --frame 3 --simple-values"
        );

        assert_eq!(context.transport_epoch, 7);
        assert_eq!(context.generation(), 11);
        assert_eq!(context.inferior_id(), Some("i2"));
    }

    #[test]
    fn stop_context_rejects_unsafe_thread_identifiers() {
        assert!(StopContext::new(1, 1, None, String::from("1 --all"), 0).is_none());
    }

    #[test]
    fn extracts_addresses_from_symbolic_values() {
        assert_eq!(pointer_address("0x40116f <main+15>"), Some(0x40116f));

        assert_eq!(
            pointer_address("(Node *) 0X40116f <main+15>"),
            Some(0x40116f)
        );

        assert_eq!(pointer_address("@0x2a"), Some(0x2a));
        assert_eq!(pointer_address("(void *) 0x0"), Some(0));
        assert_eq!(pointer_address("[loop detected]"), None);
        assert_eq!(pointer_address("0x"), None);
    }

    #[test]
    fn finds_sorted_memory_regions_at_exact_half_open_boundaries() {
        let regions = [
            parse_memory_region("1000-2000 r--p 00000000 00:00 0").unwrap(),
            parse_memory_region("3000-4000 rw-p 00000000 00:00 0").unwrap(),
            parse_memory_region("8000-9000 r-xp 00000000 00:00 0").unwrap(),
        ];

        assert_eq!(
            memory_region_for_address(&regions, 0x1000).map(|region| region.start),
            Some(0x1000)
        );

        assert_eq!(
            memory_region_for_address(&regions, 0x3fff).map(|region| region.start),
            Some(0x3000)
        );

        assert!(memory_region_for_address(&regions, 0x2000).is_none());
        assert!(memory_region_for_address(&regions, 0x7000).is_none());
        assert!(memory_region_for_address(&regions, 0x9000).is_none());
    }

    #[test]
    fn classifies_and_correlates_stack_words() {
        let region =
            parse_memory_region("00400000-00402000 r-xp 00000000 00:00 0 /tmp/program").unwrap();

        assert_eq!(region.kind, MemoryKind::Code);
        assert_eq!(region.path.as_deref(), Some("/tmp/program"));
        let stack = parse_memory_region("7fff0000-80000000 rw-p 00000000 00:00 0 [stack]").unwrap();
        assert_eq!(stack.kind, MemoryKind::Stack);
        let anonymous = parse_memory_region("7f000000-7f001000 rw-p 00000000 00:00 0").unwrap();
        assert_eq!(anonymous.kind, MemoryKind::Writable);
        assert!(anonymous.path.is_none());

        let spaced =
            parse_memory_region("00400000-00402000 r-xp 00000000 00:00 0 /tmp/debug build/program")
                .unwrap();

        assert_eq!(spaced.path.as_deref(), Some("/tmp/debug build/program"));

        let memory = MemoryBlock {
            begin: 0x1000,
            bytes: 0x401000_u64.to_le_bytes().to_vec(),
        };

        let registers = vec![
            Register {
                name: String::from("rsp"),
                value: String::from("0x1000"),
                pointer_chain: Vec::new(),
            },
            Register {
                name: String::from("rsi"),
                value: String::from("0x401000"),
                pointer_chain: Vec::new(),
            },
        ];

        let mut mapped_regions = vec![region.clone(), stack.clone()];
        annotate_memory_regions(&mut mapped_regions, &registers, TargetArchitecture::X86_64);
        assert_eq!(mapped_regions[0].referenced_by, ["$rsi"]);
        assert!(mapped_regions[1].referenced_by.is_empty());

        let frames = vec![StackFrame {
            level: 1,
            address: String::from("0x401000"),
            function: String::from("caller"),
            architecture: Some(String::from("i386:x86-64")),
            file: None,
            fullname: None,
            line: None,
        }];

        let entries = build_stack_entries(
            &memory,
            8,
            TargetEndian::Little,
            TargetArchitecture::X86_64,
            &registers,
            &frames,
            &[MemoryRegion { ..region }],
        );

        assert_eq!(entries[0].address_registers, ["rsp"]);
        assert_eq!(entries[0].value_registers, ["rsi"]);
        assert_eq!(entries[0].return_frame, Some(1));
        assert_eq!(entries[0].memory_kind, MemoryKind::Code);
    }

    #[test]
    fn builds_the_full_requested_stack_window_and_marks_inline_ascii() {
        let words = [
            u64::from_le_bytes(*b"ABCDEFGH"),
            1,
            2,
            3,
            4,
            5,
            6,
            7,
            8,
            9,
            10,
            11,
        ];

        let memory = MemoryBlock {
            begin: 0x7fff_0000,
            bytes: words.into_iter().flat_map(u64::to_le_bytes).collect(),
        };

        let entries = build_stack_entries(
            &memory,
            8,
            TargetEndian::Little,
            TargetArchitecture::X86_64,
            &[],
            &[],
            &[],
        );

        assert_eq!(entries.len(), words.len());
        assert_eq!(entries[0].memory_kind, MemoryKind::String);
        assert_eq!(entries[11].offset, 88);
    }

    #[test]
    fn decodes_big_endian_stack_words() {
        let memory = MemoryBlock {
            begin: 0x1000,
            bytes: 0x1234_5678_u32.to_be_bytes().to_vec(),
        };

        let entries = build_stack_entries(
            &memory,
            4,
            TargetEndian::Big,
            TargetArchitecture::PowerPc32,
            &[],
            &[],
            &[],
        );

        assert_eq!(entries[0].value, "0x12345678");
    }

    #[test]
    fn recognizes_inline_ascii_in_big_endian_32_bit_words() {
        let memory = MemoryBlock {
            begin: 0x1000,
            bytes: b"TEXT".to_vec(),
        };

        let entries = build_stack_entries(
            &memory,
            4,
            TargetEndian::Big,
            TargetArchitecture::PowerPc32,
            &[],
            &[],
            &[],
        );

        assert_eq!(entries[0].memory_kind, MemoryKind::String);
    }
}
