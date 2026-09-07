//! Streamed ELF core-dump metadata and note decoding.

use super::*;

const MAX_CORE_PROGRAM_HEADERS: usize = 65_536;
const MAX_CORE_PROGRAM_HEADER_BYTES: u64 = 64 * 1024 * 1024;
const MAX_CORE_NOTES: usize = 65_536;
const MAX_CORE_NOTE_BYTES: usize = 16 * 1024 * 1024;
const MAX_CORE_NOTE_NAME_BYTES: usize = 4096;
const MAX_CORE_FILES: usize = 65_536;

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

#[cfg(test)]
mod tests {
    use super::*;

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
