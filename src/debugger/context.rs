use super::{MemoryBlock, MemoryKind, Register, StackEntry, StackFrame};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MemoryRegion {
    pub start: u64,
    pub end: u64,
    pub permissions: String,
    pub path: Option<String>,
    pub kind: MemoryKind,
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
    let value = value.split_whitespace().next()?.strip_prefix("0x")?;
    u64::from_str_radix(value, 16).ok()
}

pub(crate) fn is_pointer_register(name: &str) -> bool {
    matches!(
        name,
        "rax"
            | "rbx"
            | "rcx"
            | "rdx"
            | "rsp"
            | "rbp"
            | "rsi"
            | "rdi"
            | "rip"
            | "r8"
            | "r9"
            | "r10"
            | "r11"
            | "r12"
            | "r13"
            | "r14"
            | "r15"
            | "fs_base"
            | "gs_base"
            | "eax"
            | "ebx"
            | "ecx"
            | "edx"
            | "esp"
            | "ebp"
            | "esi"
            | "edi"
            | "eip"
    )
}

pub(crate) fn read_memory_regions(pid: u32) -> Vec<MemoryRegion> {
    let Ok(maps) = std::fs::read_to_string(format!("/proc/{pid}/maps")) else {
        return Vec::new();
    };
    maps.lines().filter_map(parse_memory_region).collect()
}

pub(crate) fn build_stack_entries(
    memory: &MemoryBlock,
    word_size: usize,
    registers: &[Register],
    frames: &[StackFrame],
    regions: &[MemoryRegion],
) -> Vec<StackEntry> {
    memory
        .bytes
        .chunks_exact(word_size)
        .enumerate()
        .map(|(index, bytes)| {
            let mut word = [0_u8; 8];
            word[..word_size].copy_from_slice(bytes);
            let value = u64::from_le_bytes(word);
            let address = memory.begin + (index * word_size) as u64;
            let address_registers = matching_registers(registers, address);
            let value_registers = if value == 0 {
                Vec::new()
            } else {
                matching_registers(registers, value)
            };
            let return_frame = frames
                .iter()
                .filter(|frame| frame.level > 0)
                .find(|frame| pointer_address(&frame.address) == Some(value))
                .map(|frame| frame.level);
            let region = region_for_address(regions, value);
            let memory_kind = if region.is_none() && looks_like_string_word(value) {
                MemoryKind::String
            } else {
                region.map_or(MemoryKind::None, |region| region.kind)
            };
            StackEntry {
                address,
                offset: index * word_size,
                index,
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

pub(crate) fn looks_like_string_word(value: u64) -> bool {
    value
        .to_le_bytes()
        .iter()
        .take_while(|byte| byte.is_ascii_graphic() || **byte == b' ')
        .count()
        >= 4
}

fn matching_registers(registers: &[Register], address: u64) -> Vec<String> {
    registers
        .iter()
        .filter(|register| {
            is_pointer_register(&register.name) && pointer_address(&register.value) == Some(address)
        })
        .map(|register| register.name.clone())
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
    })
}

fn region_for_address(regions: &[MemoryRegion], address: u64) -> Option<&MemoryRegion> {
    regions.iter().find(|region| region.contains(address))
}

fn region_description(region: &MemoryRegion) -> String {
    region.description()
}

#[cfg(test)]
mod tests {
    use super::{MemoryRegion, build_stack_entries, parse_memory_region, pointer_address};
    use crate::debugger::{MemoryBlock, MemoryKind, Register, StackFrame};

    #[test]
    fn extracts_addresses_from_symbolic_values() {
        assert_eq!(pointer_address("0x40116f <main+15>"), Some(0x40116f));
        assert_eq!(pointer_address("[loop detected]"), None);
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
        let entries = build_stack_entries(&memory, 8, &[], &[], &[]);
        assert_eq!(entries.len(), words.len());
        assert_eq!(entries[0].memory_kind, MemoryKind::String);
        assert_eq!(entries[11].offset, 88);
    }
}
