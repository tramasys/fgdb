use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

use crate::{
    bounded::{FileIdentity, open_regular_file},
    performance::BoundedLruCache,
};

use goblin::{
    container::Ctx,
    elf::{
        Elf,
        note::NT_GNU_BUILD_ID,
        program_header::{PT_NOTE, ProgramHeader},
        section_header::{SHF_COMPRESSED, SHN_XINDEX, SHT_NOBITS, SHT_NOTE, SectionHeader},
    },
};

const MAX_ELF_SECTIONS: usize = 100_000;
const MAX_SECTION_HEADER_BYTES: usize = 16 * 1024 * 1024;
const MAX_SECTION_NAME_BYTES: usize = 16 * 1024 * 1024;
const MAX_METADATA_SECTION_BYTES: usize = 1024 * 1024;
const MAX_NOTE_SCAN_BYTES: usize = 4 * 1024 * 1024;
const DEBUGLINK_CRC_BUFFER_BYTES: usize = 64 * 1024;
const MAX_DEBUGLINK_CRC_CACHE_ENTRIES: usize = 32;
const GNU_DEBUGLINK_CRC_TABLE: [u32; 256] = gnu_debuglink_crc_table();

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DebugFileSearch {
    pub directories: Vec<PathBuf>,
    pub caches: Vec<PathBuf>,
}

impl Default for DebugFileSearch {
    fn default() -> Self {
        let caches = if let Some(path) = std::env::var_os("DEBUGINFOD_CACHE_PATH") {
            vec![PathBuf::from(path)]
        } else {
            let mut paths = Vec::new();

            if let Some(path) = std::env::var_os("XDG_CACHE_HOME") {
                paths.push(PathBuf::from(path).join("debuginfod_client"));
            }

            if let Some(path) = std::env::var_os("HOME") {
                paths.push(PathBuf::from(&path).join(".cache/debuginfod_client"));
                paths.push(PathBuf::from(path).join(".debuginfod_client_cache"));
            }

            paths
        };

        Self {
            directories: vec![PathBuf::from("/usr/lib/debug")],
            caches,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ModuleDebugMetadata {
    pub(crate) path: PathBuf,
    pub(crate) build_id: Option<String>,
    pub(crate) debuglink: Option<String>,
    pub(crate) debuglink_crc: Option<u32>,
    pub(crate) separate_debug_file: Option<PathBuf>,
    pub(crate) rejected_debug_files: Vec<String>,
    pub(crate) embedded_debug_info: bool,
    pub(crate) suggestion: Option<String>,
    pub(crate) error: Option<String>,
    pub(crate) file_identity: Option<FileIdentity>,
}

impl ModuleDebugMetadata {
    fn unavailable(path: &Path, error: impl Into<String>) -> Self {
        Self {
            path: path.to_path_buf(),
            build_id: None,
            debuglink: None,
            debuglink_crc: None,
            separate_debug_file: None,
            rejected_debug_files: Vec::new(),
            embedded_debug_info: false,
            suggestion: None,
            error: Some(error.into()),
            file_identity: None,
        }
    }

    fn unavailable_for_file(
        path: &Path,
        error: impl Into<String>,
        metadata: &std::fs::Metadata,
    ) -> Self {
        let mut unavailable = Self::unavailable(path, error);
        unavailable.file_identity = FileIdentity::from_metadata(path, metadata).ok();

        unavailable
    }
}

#[cfg(test)]
fn inspect_module(path: &Path) -> ModuleDebugMetadata {
    inspect_module_with_search(path, &DebugFileSearch::default(), &|| true)
}

pub(crate) fn inspect_module_with_search(
    path: &Path,
    search: &DebugFileSearch,
    is_current: &impl Fn() -> bool,
) -> ModuleDebugMetadata {
    inspect_module_for_build_id(path, search, None, is_current)
}

/// GDB's build ID remains useful when a remote or deleted module cannot be
/// opened on the host. Local lookup must not require a network request.
pub(crate) fn inspect_module_for_build_id(
    path: &Path,
    search: &DebugFileSearch,
    expected_build_id: Option<&str>,
    is_current: &impl Fn() -> bool,
) -> ModuleDebugMetadata {
    let mut metadata = read_module_metadata(path, is_current);

    if metadata.build_id.is_none() {
        metadata.build_id = expected_build_id.map(str::to_owned);

        if expected_build_id.is_some() {
            // Only the ID from GDB is trusted here. A different file may now
            // occupy the host path, so do not use its DWARF or debuglink CRC.
            metadata.embedded_debug_info = false;
            metadata.debuglink = None;
            metadata.debuglink_crc = None;
        }
    }

    if expected_build_id.is_some_and(|expected| metadata.build_id.as_deref() != Some(expected)) {
        metadata.error = Some(String::from(
            "The host module does not match GDB's build ID",
        ));
        return metadata;
    }

    refresh_module_debug_file_with_search(metadata, search, is_current)
}

fn read_module_metadata(path: &Path, is_current: &impl Fn() -> bool) -> ModuleDebugMetadata {
    if !is_current() {
        return ModuleDebugMetadata::unavailable(path, "Symbol request cancelled");
    }

    let (mut file, metadata) = match open_regular_file(path) {
        Ok(opened) => opened,
        Err(error) => {
            return ModuleDebugMetadata::unavailable(
                path,
                format!("Cannot read host module: {error}"),
            );
        }
    };

    let file_identity = match FileIdentity::from_metadata(path, &metadata) {
        Ok(identity) => identity,
        Err(error) => {
            return ModuleDebugMetadata::unavailable(
                path,
                format!("Cannot identify host module: {error}"),
            );
        }
    };

    let elf = match inspect_elf_metadata(&mut file, metadata.len(), is_current) {
        Ok(elf) => elf,
        Err(error) => return ModuleDebugMetadata::unavailable_for_file(path, error, &metadata),
    };

    let after = file
        .metadata()
        .and_then(|metadata| FileIdentity::from_metadata(path, &metadata))
        .ok();

    if after.as_ref() != Some(&file_identity) {
        return ModuleDebugMetadata::unavailable(
            path,
            "Host module changed or became inaccessible during inspection",
        );
    }

    ModuleDebugMetadata {
        path: path.to_path_buf(),
        build_id: elf.build_id,
        debuglink: elf.debuglink,
        debuglink_crc: elf.debuglink_crc,
        separate_debug_file: None,
        rejected_debug_files: Vec::new(),
        embedded_debug_info: elf.embedded_debug_info,
        suggestion: None,
        error: None,
        file_identity: Some(file_identity),
    }
}

struct ElfDebugMetadata {
    build_id: Option<String>,
    debuglink: Option<String>,
    debuglink_crc: Option<u32>,
    embedded_debug_info: bool,
}

fn inspect_elf_metadata(
    file: &mut File,
    file_size: u64,
    is_current: &impl Fn() -> bool,
) -> Result<ElfDebugMetadata, String> {
    check_current(is_current)?;
    let header_bytes = read_file_range(file, 0, 64_u64.min(file_size), file_size, 64)?;
    let header = Elf::parse_header(&header_bytes)
        .map_err(|error| format!("Cannot parse ELF header: {error}"))?;

    let context = Ctx::new(
        header.container().map_err(|error| error.to_string())?,
        header.endianness().map_err(|error| error.to_string())?,
    );

    let mut result = ElfDebugMetadata {
        build_id: None,
        debuglink: None,
        debuglink_crc: None,
        embedded_debug_info: false,
    };

    let sections = if header.e_shoff == 0 {
        Vec::new()
    } else {
        let entry_size = usize::from(header.e_shentsize);

        if entry_size < SectionHeader::size(context) {
            return Err(String::from("Invalid ELF section-header size"));
        }

        let first = read_section_headers(file, file_size, header.e_shoff, 1, entry_size, context)?;
        let count = if header.e_shnum == 0 {
            usize::try_from(first[0].sh_size).map_err(|_| "ELF section count overflow")?
        } else {
            usize::from(header.e_shnum)
        };

        if count > MAX_ELF_SECTIONS {
            return Err(String::from("ELF section count exceeds its safety limit"));
        }

        check_current(is_current)?;
        read_section_headers(file, file_size, header.e_shoff, count, entry_size, context)?
    };

    let names = if header.e_shstrndx == 0 {
        Vec::new()
    } else {
        let index = if u32::from(header.e_shstrndx) == SHN_XINDEX {
            sections
                .first()
                .ok_or("Missing extended ELF section index")?
                .sh_link as usize
        } else {
            usize::from(header.e_shstrndx)
        };

        let section = sections
            .get(index)
            .ok_or("ELF section-name table index is out of range")?;
        check_current(is_current)?;
        read_file_range(
            file,
            section.sh_offset,
            section.sh_size,
            file_size,
            MAX_SECTION_NAME_BYTES,
        )?
    };

    let mut note_budget = MAX_NOTE_SCAN_BYTES;
    let mut has_abbreviations = false;

    for section in &sections {
        check_current(is_current)?;

        if section.sh_type == SHT_NOTE && result.build_id.is_none() {
            result.build_id = read_build_id_note(
                file,
                file_size,
                section.sh_offset,
                section.sh_size,
                section.sh_addralign,
                context.is_little_endian(),
                &mut note_budget,
            )?;
        }

        if section_named(&names, section.sh_name, b".debug_info")
            || section_named(&names, section.sh_name, b".zdebug_info")
        {
            result.embedded_debug_info |= dwarf_section_present(
                file,
                file_size,
                section,
                context,
                section_named(&names, section.sh_name, b".zdebug_info"),
            )?;
        } else if section_named(&names, section.sh_name, b".debug_abbrev")
            || section_named(&names, section.sh_name, b".zdebug_abbrev")
        {
            has_abbreviations |= section.sh_type != SHT_NOBITS
                && section.sh_size > 0
                && section
                    .sh_offset
                    .checked_add(section.sh_size)
                    .is_some_and(|end| end <= file_size);
        } else if section_named(&names, section.sh_name, b".gnu_debuglink") {
            note_budget = note_budget
                .checked_sub(
                    usize::try_from(section.sh_size).map_err(|_| "ELF debuglink size overflow")?,
                )
                .ok_or("ELF metadata exceeds its scan budget")?;
            let bytes = read_metadata_section(file, file_size, section)?;
            (result.debuglink, result.debuglink_crc) =
                gnu_debuglink(&bytes, context.is_little_endian()).unwrap_or_default();
        }
    }

    result.embedded_debug_info &= has_abbreviations;

    // Section headers are optional in a stripped ELF. Build IDs are note
    // contents, not section names, and can also be found through PT_NOTE.
    if result.build_id.is_none() && header.e_phoff != 0 {
        let count = if header.e_phnum == u16::MAX {
            sections
                .first()
                .ok_or("Missing extended ELF program count")?
                .sh_info as usize
        } else {
            usize::from(header.e_phnum)
        };

        let size = usize::from(header.e_phentsize);

        if size < ProgramHeader::size(context) || count > MAX_ELF_SECTIONS {
            return Err(String::from("Invalid ELF program-header table"));
        }

        let bytes = read_file_range(
            file,
            header.e_phoff,
            size.checked_mul(count)
                .ok_or("ELF program table overflow")? as u64,
            file_size,
            MAX_SECTION_HEADER_BYTES,
        )?;

        for entry in bytes.chunks_exact(size) {
            check_current(is_current)?;
            let segment = ProgramHeader::parse(entry, 0, 1, context)
                .map_err(|error| error.to_string())?
                .remove(0);

            if segment.p_type == PT_NOTE {
                result.build_id = read_build_id_note(
                    file,
                    file_size,
                    segment.p_offset,
                    segment.p_filesz,
                    segment.p_align,
                    context.is_little_endian(),
                    &mut note_budget,
                )?;

                if result.build_id.is_some() {
                    break;
                }
            }
        }
    }

    check_current(is_current)?;
    Ok(result)
}

fn check_current(is_current: &impl Fn() -> bool) -> Result<(), String> {
    is_current()
        .then_some(())
        .ok_or_else(|| String::from("Symbol request cancelled"))
}

/// Only a few fixed names are needed. Comparing their terminating NUL bounds
/// work per section even when many offsets share an unterminated string.
fn section_named(names: &[u8], offset: usize, expected: &[u8]) -> bool {
    names.get(offset..).is_some_and(|name| {
        name.get(..expected.len()) == Some(expected) && name.get(expected.len()) == Some(&0)
    })
}

fn read_build_id_note(
    file: &mut File,
    file_size: u64,
    offset: u64,
    size: u64,
    alignment: u64,
    little_endian: bool,
    budget: &mut usize,
) -> Result<Option<String>, String> {
    let count = usize::try_from(size).map_err(|_| "ELF note size overflow")?;
    *budget = budget
        .checked_sub(count)
        .ok_or("ELF notes exceed their scan budget")?;
    let bytes = read_file_range(file, offset, size, file_size, MAX_METADATA_SECTION_BYTES)?;
    Ok(gnu_build_id(
        &bytes,
        little_endian,
        usize::try_from(alignment).unwrap_or(4),
    ))
}

/// Establish the presence of DWARF, not complete type usability. GDB owns
/// decompression, DIE decoding and resolution of split-DWARF dependencies.
fn dwarf_section_present(
    file: &mut File,
    file_size: u64,
    section: &SectionHeader,
    context: Ctx,
    legacy_compressed: bool,
) -> Result<bool, String> {
    if section.sh_type == SHT_NOBITS
        || section.sh_size == 0
        || section
            .sh_offset
            .checked_add(section.sh_size)
            .is_none_or(|end| end > file_size)
    {
        return Ok(false);
    }

    let bytes = read_file_range(
        file,
        section.sh_offset,
        section.sh_size.min(32),
        file_size,
        32,
    )?;

    if legacy_compressed {
        return Ok(bytes.starts_with(b"ZLIB")
            && bytes.len() > 12
            && u64::from_be_bytes(bytes[4..12].try_into().unwrap()) > 0);
    }

    if section.sh_flags & u64::from(SHF_COMPRESSED) != 0 {
        let size = if context.is_big() { 24 } else { 12 };

        if bytes.len() <= size {
            return Ok(false);
        }

        let uncompressed = if context.is_big() {
            let length = bytes[8..16].try_into().unwrap();

            if context.is_little_endian() {
                u64::from_le_bytes(length)
            } else {
                u64::from_be_bytes(length)
            }
        } else {
            u64::from(read_elf_u32(&bytes[4..8], context.is_little_endian()).unwrap())
        };

        return Ok(uncompressed > 0
            && read_elf_u32(&bytes[..4], context.is_little_endian())
                .is_some_and(|kind| matches!(kind, 1 | 2)));
    }

    let Some(initial) = bytes
        .get(..4)
        .and_then(|bytes| read_elf_u32(bytes, context.is_little_endian()))
    else {
        return Ok(false);
    };

    let (length, start, offset_size) = if initial == u32::MAX {
        let Some(length) = bytes.get(4..12) else {
            return Ok(false);
        };
        let length = if context.is_little_endian() {
            u64::from_le_bytes(length.try_into().unwrap())
        } else {
            u64::from_be_bytes(length.try_into().unwrap())
        };
        (length, 12, 8)
    } else if initial >= 0xfffffff0 {
        return Ok(false);
    } else {
        (u64::from(initial), 4, 4)
    };

    let Some(version) = bytes.get(start..start + 2) else {
        return Ok(false);
    };
    let version = if context.is_little_endian() {
        u16::from_le_bytes(version.try_into().unwrap())
    } else {
        u16::from_be_bytes(version.try_into().unwrap())
    };

    let minimum = offset_size + if version == 5 { 5 } else { 4 };
    Ok((2..=5).contains(&version)
        && length >= minimum
        && length
            .checked_add(start as u64)
            .is_some_and(|end| end <= section.sh_size))
}

fn read_section_headers(
    file: &mut File,
    file_size: u64,
    offset: u64,
    count: usize,
    entry_size: usize,
    context: Ctx,
) -> Result<Vec<SectionHeader>, String> {
    let byte_count = entry_size
        .checked_mul(count)
        .ok_or_else(|| String::from("ELF section-table size overflow"))?;

    if byte_count > MAX_SECTION_HEADER_BYTES {
        return Err(format!(
            "ELF section table exceeds the {MAX_SECTION_HEADER_BYTES}-byte safety limit"
        ));
    }

    let bytes = read_file_range(file, offset, byte_count as u64, file_size, byte_count)
        .map_err(|error| format!("Cannot read ELF section table: {error}"))?;

    if entry_size == SectionHeader::size(context) {
        return SectionHeader::parse_from(&bytes, 0, count, context)
            .map_err(|error| format!("Cannot parse ELF section table: {error}"));
    }

    bytes
        .chunks_exact(entry_size)
        .map(|entry| {
            SectionHeader::parse_from(entry, 0, 1, context)
                .map_err(|error| format!("Cannot parse ELF section table: {error}"))?
                .into_iter()
                .next()
                .ok_or_else(|| String::from("ELF section-header entry is empty"))
        })
        .collect()
}

fn read_metadata_section(
    file: &mut File,
    file_size: u64,
    section: &SectionHeader,
) -> Result<Vec<u8>, String> {
    read_file_range(
        file,
        section.sh_offset,
        section.sh_size,
        file_size,
        MAX_METADATA_SECTION_BYTES,
    )
}

fn read_file_range(
    file: &mut File,
    offset: u64,
    size: u64,
    file_size: u64,
    limit: usize,
) -> Result<Vec<u8>, String> {
    let size = usize::try_from(size).map_err(|_| String::from("file range is too large"))?;

    if size > limit {
        return Err(format!("file range exceeds the {limit}-byte safety limit"));
    }

    let end = offset
        .checked_add(size as u64)
        .ok_or_else(|| String::from("file range overflows"))?;

    if end > file_size {
        return Err(String::from(
            "file range extends past the end of the module",
        ));
    }

    let mut bytes = vec![0_u8; size];

    file.seek(SeekFrom::Start(offset))
        .map_err(|error| error.to_string())?;

    file.read_exact(&mut bytes)
        .map_err(|error| error.to_string())?;

    Ok(bytes)
}

pub(crate) fn refresh_module_debug_file_with_search(
    mut metadata: ModuleDebugMetadata,
    search: &DebugFileSearch,
    is_current: &impl Fn() -> bool,
) -> ModuleDebugMetadata {
    metadata.rejected_debug_files.clear();
    metadata.separate_debug_file = find_separate_debug_file(
        &metadata.path,
        metadata.debuglink.as_deref(),
        metadata.debuglink_crc,
        metadata.build_id.as_deref(),
        search,
        is_current,
        &mut metadata.rejected_debug_files,
    );

    metadata.suggestion = (!metadata.embedded_debug_info && metadata.separate_debug_file.is_none())
        .then(|| debug_package_suggestion(&metadata.path, metadata.build_id.as_deref()));

    metadata
}

fn gnu_build_id(data: &[u8], little_endian: bool, alignment: usize) -> Option<String> {
    let alignment = if alignment.is_power_of_two() && alignment >= 4 {
        alignment
    } else {
        4
    };

    let mut offset = 0_usize;

    while offset.checked_add(12)? <= data.len() {
        let name_size = read_elf_u32(data.get(offset..offset + 4)?, little_endian)? as usize;

        let description_size =
            read_elf_u32(data.get(offset + 4..offset + 8)?, little_endian)? as usize;

        let note_type = read_elf_u32(data.get(offset + 8..offset + 12)?, little_endian)?;
        let name_start = offset + 12;
        let name_end = name_start.checked_add(name_size)?;
        let description_start = align_up(name_end, alignment)?;
        let description_end = description_start.checked_add(description_size)?;
        let name = data.get(name_start..name_end)?;
        let name = name.strip_suffix(&[0]).unwrap_or(name);
        let description = data.get(description_start..description_end)?;

        if note_type == NT_GNU_BUILD_ID && name == b"GNU" && (2..=128).contains(&description.len())
        {
            return Some(hexadecimal(description));
        }

        offset = align_up(description_end, alignment)?;
    }

    None
}

fn gnu_debuglink(data: &[u8], little_endian: bool) -> Option<(Option<String>, Option<u32>)> {
    let name_end = data.iter().position(|byte| *byte == 0)?;
    let name = std::str::from_utf8(&data[..name_end]).ok()?.trim();
    let name = (!name.is_empty()).then(|| name.to_owned());
    let crc_start = name_end.checked_add(4)? & !3;

    let crc = data
        .get(crc_start..crc_start.checked_add(4)?)
        .and_then(|crc| read_elf_u32(crc, little_endian));

    Some((name, crc))
}

fn read_elf_u32(bytes: &[u8], little_endian: bool) -> Option<u32> {
    let bytes = <[u8; 4]>::try_from(bytes).ok()?;

    Some(if little_endian {
        u32::from_le_bytes(bytes)
    } else {
        u32::from_be_bytes(bytes)
    })
}

fn align_up(value: usize, alignment: usize) -> Option<usize> {
    value
        .checked_add(alignment.checked_sub(1)?)
        .map(|value| value & !(alignment - 1))
}

fn find_separate_debug_file(
    module: &Path,
    debuglink: Option<&str>,
    debuglink_crc: Option<u32>,
    build_id: Option<&str>,
    search: &DebugFileSearch,
    is_current: &impl Fn() -> bool,
    rejected: &mut Vec<String>,
) -> Option<PathBuf> {
    let mut debuglink_candidates = Vec::new();

    if let Some(debuglink) = debuglink.filter(|debuglink| valid_debuglink(debuglink)) {
        let parent = module.parent().unwrap_or_else(|| Path::new("."));
        debuglink_candidates.push(parent.join(debuglink));
        debuglink_candidates.push(parent.join(".debug").join(debuglink));

        if module.is_absolute() {
            let relative_parent = parent.strip_prefix("/").unwrap_or(parent);

            debuglink_candidates.extend(
                search
                    .directories
                    .iter()
                    .take(64)
                    .map(|directory| directory.join(relative_parent).join(debuglink)),
            );
        }
    }

    let mut build_id_candidates = Vec::new();

    if let Some(build_id) = build_id.filter(|id| {
        (4..=256).contains(&id.len())
            && id.len().is_multiple_of(2)
            && id.bytes().all(|byte| byte.is_ascii_hexdigit())
    }) {
        let (prefix, suffix) = build_id.split_at(2);

        build_id_candidates.extend(search.directories.iter().take(64).map(|directory| {
            directory
                .join(".build-id")
                .join(prefix)
                .join(format!("{suffix}.debug"))
        }));
        build_id_candidates.extend(
            search
                .caches
                .iter()
                .take(4)
                .map(|cache| cache.join(build_id).join("debuginfo")),
        );
    }

    select_separate_debug_file_while(
        debuglink_candidates,
        debuglink_crc,
        build_id_candidates,
        build_id,
        is_current,
        rejected,
    )
}

#[cfg(test)]
fn select_separate_debug_file(
    debuglink_candidates: impl IntoIterator<Item = PathBuf>,
    debuglink_crc: Option<u32>,
    build_id_candidates: impl IntoIterator<Item = PathBuf>,
    expected_build_id: Option<&str>,
) -> Option<PathBuf> {
    select_separate_debug_file_while(
        debuglink_candidates,
        debuglink_crc,
        build_id_candidates,
        expected_build_id,
        &|| true,
        &mut Vec::new(),
    )
}

fn select_separate_debug_file_while(
    debuglink_candidates: impl IntoIterator<Item = PathBuf>,
    debuglink_crc: Option<u32>,
    build_id_candidates: impl IntoIterator<Item = PathBuf>,
    expected_build_id: Option<&str>,
    is_current: &impl Fn() -> bool,
    rejected: &mut Vec<String>,
) -> Option<PathBuf> {
    let candidates = build_id_candidates
        .into_iter()
        .filter(|_| expected_build_id.is_some())
        .map(|path| (path, None))
        .chain(
            debuglink_candidates
                .into_iter()
                .filter(|_| debuglink_crc.is_some())
                .map(|path| (path, debuglink_crc)),
        );

    for (candidate, crc) in candidates {
        if !is_current() {
            break;
        }

        // Missing paths are normal during a search. Preserve a bounded set of
        // rejection reasons for files that actually exist, then try the next.
        if !candidate.try_exists().unwrap_or(false) {
            continue;
        }

        match validate_debug_file(&candidate, expected_build_id, crc, is_current) {
            Ok(()) => return Some(candidate),
            Err(error) if rejected.len() < 4 => {
                rejected.push(format!("{}: {error}", candidate.display()))
            }
            Err(_) => {}
        }
    }

    None
}

#[cfg(test)]
fn candidate_build_id(path: &Path) -> Result<Option<String>, String> {
    let is_current = &|| true;
    let (mut file, metadata) = open_regular_file(path).map_err(|error| error.to_string())?;

    inspect_elf_metadata(&mut file, metadata.len(), is_current).map(|metadata| metadata.build_id)
}

/// Validate before handing a separate file to GDB. A filename match is not
/// sufficient, and a matching stripped binary is not a usable debug file.
pub(crate) fn validate_debug_file(
    path: &Path,
    expected_build_id: Option<&str>,
    expected_crc: Option<u32>,
    is_current: &impl Fn() -> bool,
) -> Result<(), String> {
    if !is_current() {
        return Err(String::from("Symbol request cancelled"));
    }

    let (mut file, metadata) = open_regular_file(path).map_err(|error| error.to_string())?;

    let elf = inspect_elf_metadata(&mut file, metadata.len(), is_current)?;

    if !elf.embedded_debug_info {
        return Err(String::from(
            "The matching file does not contain supported DWARF debug information",
        ));
    }

    match (expected_build_id, elf.build_id.as_deref(), expected_crc) {
        (Some(expected), Some(actual), _) if expected == actual => Ok(()),
        (Some(_), Some(_), _) => Err(String::from(
            "Debug-file build ID does not match the loaded module",
        )),
        (_, _, Some(expected))
            if cached_gnu_debuglink_crc_while(path, is_current)
                .is_ok_and(|actual| actual == expected) =>
        {
            Ok(())
        }
        _ => Err(String::from(
            "Could not verify the debug file against the loaded module",
        )),
    }
}

#[cfg(test)]
fn gnu_debuglink_crc(path: &Path) -> io::Result<u32> {
    gnu_debuglink_crc_while(path, &|| true)
}

fn gnu_debuglink_crc_while(path: &Path, is_current: &impl Fn() -> bool) -> io::Result<u32> {
    #[cfg(test)]
    record_debuglink_crc_calculation(path);
    let (mut file, _) = open_regular_file(path)?;
    let mut buffer = [0_u8; DEBUGLINK_CRC_BUFFER_BYTES];
    let mut crc = u32::MAX;

    loop {
        if !is_current() {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "Debug metadata scan superseded",
            ));
        }

        let read = file.read(&mut buffer)?;

        if read == 0 {
            break;
        }

        crc = update_gnu_debuglink_crc(crc, &buffer[..read]);
    }

    Ok(!crc)
}

type DebuglinkCrcCache = BoundedLruCache<FileIdentity, u32>;

fn debuglink_crc_cache() -> &'static Mutex<DebuglinkCrcCache> {
    static CACHE: OnceLock<Mutex<DebuglinkCrcCache>> = OnceLock::new();

    CACHE.get_or_init(|| Mutex::new(DebuglinkCrcCache::new(MAX_DEBUGLINK_CRC_CACHE_ENTRIES)))
}

#[cfg(test)]
fn cached_gnu_debuglink_crc(path: &Path) -> io::Result<u32> {
    cached_gnu_debuglink_crc_while(path, &|| true)
}

fn cached_gnu_debuglink_crc_while(path: &Path, is_current: &impl Fn() -> bool) -> io::Result<u32> {
    let before = FileIdentity::read(path)?;

    if let Some(crc) = debuglink_crc_cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get_cloned(&before)
    {
        return Ok(crc);
    }

    let crc = gnu_debuglink_crc_while(path, is_current)?;
    let after = FileIdentity::read(path)?;

    if before != after {
        return Err(io::Error::other(
            "debug file changed while its GNU debuglink CRC was calculated",
        ));
    }

    debuglink_crc_cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(before, crc);

    Ok(crc)
}

#[cfg(test)]
fn debuglink_crc_calculations_by_path() -> &'static Mutex<std::collections::HashMap<PathBuf, usize>>
{
    static CALCULATIONS: OnceLock<Mutex<std::collections::HashMap<PathBuf, usize>>> =
        OnceLock::new();

    CALCULATIONS.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

#[cfg(test)]
fn record_debuglink_crc_calculation(path: &Path) {
    let mut calculations = debuglink_crc_calculations_by_path()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    *calculations.entry(path.to_owned()).or_default() += 1;
}

#[cfg(test)]
fn debuglink_crc_calculations(path: &Path) -> usize {
    debuglink_crc_calculations_by_path()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(path)
        .copied()
        .unwrap_or(0)
}

fn update_gnu_debuglink_crc(mut crc: u32, bytes: &[u8]) -> u32 {
    for byte in bytes {
        let index = ((crc ^ u32::from(*byte)) & 0xff) as usize;
        crc = GNU_DEBUGLINK_CRC_TABLE[index] ^ (crc >> 8);
    }

    crc
}

const fn gnu_debuglink_crc_table() -> [u32; 256] {
    let mut table = [0_u32; 256];
    let mut index = 0_usize;
    while index < table.len() {
        let mut value = index as u32;
        let mut bit = 0;
        while bit < 8 {
            value = if value & 1 != 0 {
                0xedb8_8320 ^ (value >> 1)
            } else {
                value >> 1
            };
            bit += 1;
        }

        table[index] = value;
        index += 1;
    }

    table
}

fn valid_debuglink(debuglink: &str) -> bool {
    let path = Path::new(debuglink);

    !debuglink.is_empty()
        && path.file_name().is_some_and(|name| name == debuglink)
        && path.components().count() == 1
}

fn debug_package_suggestion(path: &Path, build_id: Option<&str>) -> String {
    let module = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("this module");

    let distribution = distribution_id();

    match distribution {
        "arch" | "endeavouros" | "manjaro" => format!(
            "Enable the configured debuginfod service or install the debug package owning {module}"
        ),
        "debian" | "ubuntu" | "linuxmint" => {
            format!("Install the dbgsym/debug package owning {module} (use dpkg -S to identify it)")
        }
        "fedora" | "rhel" | "centos" => {
            format!("Use dnf debuginfo-install for the package owning {module}")
        }
        "opensuse" | "opensuse-leap" | "opensuse-tumbleweed" => {
            format!("Install the -debuginfo package owning {module}")
        }
        _ if build_id.is_some() => {
            format!("Use debuginfod or install the distribution debug-symbol package for {module}")
        }
        _ => format!("Install the distribution debug-symbol package for {module}"),
    }
}

fn distribution_id() -> &'static str {
    static DISTRIBUTION_ID: OnceLock<String> = OnceLock::new();

    DISTRIBUTION_ID
        .get_or_init(|| {
            std::fs::read_to_string("/etc/os-release")
                .unwrap_or_default()
                .lines()
                .find_map(|line| line.strip_prefix("ID="))
                .map(|id| id.trim_matches(['\'', '"']).to_owned())
                .unwrap_or_default()
        })
        .as_str()
}

fn hexadecimal(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut output = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        let _ = write!(output, "{byte:02x}");
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::SystemTime;

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new(label: &str) -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "fgdb-{label}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&path).expect("create test directory");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn crc_bytes(chunks: &[&[u8]]) -> u32 {
        let crc = chunks
            .iter()
            .fold(u32::MAX, |crc, bytes| update_gnu_debuglink_crc(crc, bytes));
        !crc
    }

    fn metadata_elf(with_debug: bool) -> Vec<u8> {
        let names = b"\0.shstrtab\0.note.custom\0.debug_info\0.debug_abbrev\0";
        let mut note = Vec::new();
        note.extend_from_slice(&4_u32.to_le_bytes());
        note.extend_from_slice(&4_u32.to_le_bytes());
        note.extend_from_slice(&NT_GNU_BUILD_ID.to_le_bytes());
        note.extend_from_slice(b"GNU\0\xde\xad\xbe\xef");
        let info = if with_debug {
            &b"\x09\0\0\0\x05\0\x01\x08\0\0\0\0\x01"[..]
        } else {
            &[]
        };
        let mut bytes = vec![0_u8; 64 + 5 * 64];
        bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
        bytes[16..18].copy_from_slice(&2_u16.to_le_bytes());
        bytes[18..20].copy_from_slice(&62_u16.to_le_bytes());
        bytes[20..24].copy_from_slice(&1_u32.to_le_bytes());
        bytes[40..48].copy_from_slice(&64_u64.to_le_bytes());
        bytes[52..54].copy_from_slice(&64_u16.to_le_bytes());
        bytes[58..60].copy_from_slice(&64_u16.to_le_bytes());
        bytes[60..62].copy_from_slice(&5_u16.to_le_bytes());
        bytes[62..64].copy_from_slice(&1_u16.to_le_bytes());

        for (index, (name, kind, data)) in [
            (1_u32, 3_u32, &names[..]),
            (11, SHT_NOTE, &note[..]),
            (24, 1, info),
            (36, 1, &b"\x01\x11\0\0\0\0"[..]),
        ]
        .into_iter()
        .enumerate()
        {
            let offset = bytes.len() as u64;
            let start = 64 + (index + 1) * 64;
            bytes[start..start + 4].copy_from_slice(&name.to_le_bytes());
            bytes[start + 4..start + 8].copy_from_slice(&kind.to_le_bytes());
            bytes[start + 24..start + 32].copy_from_slice(&offset.to_le_bytes());
            bytes[start + 32..start + 40].copy_from_slice(&(data.len() as u64).to_le_bytes());
            bytes[start + 48..start + 56].copy_from_slice(&4_u64.to_le_bytes());
            bytes.extend_from_slice(data);
        }

        bytes
    }

    #[test]
    fn module_metadata_rejects_a_file_changed_during_inspection() {
        let directory = TestDirectory::new("metadata-changed");
        let path = directory.path().join("module");
        std::fs::write(&path, metadata_elf(true)).unwrap();
        let checks = std::cell::Cell::new(0);

        let metadata = read_module_metadata(&path, &|| {
            checks.set(checks.get() + 1);

            if checks.get() == 2 {
                std::fs::write(&path, metadata_elf(false)).unwrap();
            }

            true
        });

        assert!(
            metadata
                .error
                .as_deref()
                .is_some_and(|error| error.contains("changed"))
        );

        assert!(metadata.file_identity.is_none());
        assert!(metadata.build_id.is_none());
        assert!(!metadata.embedded_debug_info);
    }

    #[test]
    fn metadata_checks_presence_cancellation_and_notes_without_section_names() {
        let directory = TestDirectory::new("metadata-contract");
        let path = directory.path().join("module");
        std::fs::write(&path, metadata_elf(true)).unwrap();
        let metadata = inspect_module(&path);
        assert!(metadata.embedded_debug_info, "{metadata:?}");
        assert_eq!(metadata.build_id.as_deref(), Some("deadbeef"));
        assert!(
            inspect_module_with_search(&path, &DebugFileSearch::default(), &|| false)
                .error
                .is_some()
        );
        assert!(!section_named(&vec![b'x'; 1024 * 1024], 0, b".debug_info"));
        std::fs::write(&path, metadata_elf(false)).unwrap();
        assert!(!inspect_module(&path).embedded_debug_info);

        let mut bytes = metadata_elf(true);
        let note = bytes[64 + 2 * 64 + 24..64 + 2 * 64 + 32]
            .try_into()
            .unwrap();
        let offset = u64::from_le_bytes(note);
        bytes[32..40].copy_from_slice(&64_u64.to_le_bytes());
        bytes[40..48].fill(0);
        bytes[54..56].copy_from_slice(&56_u16.to_le_bytes());
        bytes[56..58].copy_from_slice(&1_u16.to_le_bytes());
        bytes[60..64].fill(0);
        bytes[64..120].fill(0);
        bytes[64..68].copy_from_slice(&PT_NOTE.to_le_bytes());
        bytes[72..80].copy_from_slice(&offset.to_le_bytes());
        bytes[96..104].copy_from_slice(&20_u64.to_le_bytes());
        bytes[112..120].copy_from_slice(&4_u64.to_le_bytes());
        std::fs::write(&path, bytes).unwrap();
        assert_eq!(inspect_module(&path).build_id.as_deref(), Some("deadbeef"));
    }

    #[test]
    fn rejects_unusable_candidates_and_searches_without_the_host_module() {
        let directory = TestDirectory::new("debug-candidate-fallback");
        let invalid = directory.path().join("invalid.debug");
        let valid = directory.path().join("valid.debug");
        std::fs::write(&invalid, metadata_elf(false)).unwrap();
        std::fs::write(&valid, metadata_elf(true)).unwrap();
        let mut rejected = Vec::new();

        assert_eq!(
            select_separate_debug_file_while(
                [valid.clone()],
                Some(gnu_debuglink_crc(&valid).unwrap()),
                [invalid],
                Some("deadbeef"),
                &|| true,
                &mut rejected,
            ),
            Some(valid.clone())
        );
        assert_eq!(rejected.len(), 1);
        let cache = directory.path().join("deadbeef");
        std::fs::create_dir(&cache).unwrap();
        std::fs::copy(&valid, cache.join("debuginfo")).unwrap();

        let search = DebugFileSearch {
            directories: Vec::new(),
            caches: vec![directory.path().to_owned()],
        };
        let metadata = inspect_module_for_build_id(
            &directory.path().join("missing-module"),
            &search,
            Some("deadbeef"),
            &|| true,
        );

        assert_eq!(metadata.separate_debug_file, Some(cache.join("debuginfo")));
    }

    #[test]
    fn reads_build_id_and_debug_sections_from_the_current_executable() {
        let executable = std::env::current_exe().expect("test executable path");
        let metadata = inspect_module(&executable);
        assert!(metadata.error.is_none(), "{:?}", metadata.error);
        assert!(metadata.build_id.is_some());
        assert!(metadata.embedded_debug_info);
    }

    #[test]
    fn reports_unavailable_modules_without_panicking() {
        let metadata = inspect_module(Path::new("/definitely/not/an/fgdb/module"));
        assert!(metadata.error.is_some());
        assert!(metadata.build_id.is_none());
    }

    #[test]
    fn accepts_only_plain_debuglink_filenames() {
        assert!(valid_debuglink("libexample.so.debug"));
        assert!(!valid_debuglink(""));
        assert!(!valid_debuglink("../libexample.so.debug"));
        assert!(!valid_debuglink("symbols/libexample.so.debug"));
        assert!(!valid_debuglink("/tmp/libexample.so.debug"));
    }

    #[test]
    fn parses_build_id_notes_without_reading_an_entire_elf() {
        let mut note = Vec::new();
        note.extend_from_slice(&4_u32.to_le_bytes());
        note.extend_from_slice(&4_u32.to_le_bytes());
        note.extend_from_slice(&NT_GNU_BUILD_ID.to_le_bytes());
        note.extend_from_slice(b"GNU\0");
        note.extend_from_slice(&[0xde, 0xad, 0xbe, 0xef]);
        assert_eq!(gnu_build_id(&note, true, 4).as_deref(), Some("deadbeef"));
    }

    #[test]
    fn parses_debuglink_crc_in_the_target_byte_order() {
        let mut little = b"sample.debug\0".to_vec();
        little.resize(align_up(little.len(), 4).unwrap(), 0);
        little.extend_from_slice(&0x1234_5678_u32.to_le_bytes());
        assert_eq!(
            gnu_debuglink(&little, true),
            Some((Some(String::from("sample.debug")), Some(0x1234_5678)))
        );

        let mut big = b"sample.debug\0".to_vec();
        big.resize(align_up(big.len(), 4).unwrap(), 0);
        big.extend_from_slice(&0x1234_5678_u32.to_be_bytes());
        assert_eq!(
            gnu_debuglink(&big, false),
            Some((Some(String::from("sample.debug")), Some(0x1234_5678)))
        );

        let mut truncated = b"sample.debug\0".to_vec();
        truncated.resize(align_up(truncated.len(), 4).unwrap(), 0);
        assert_eq!(
            gnu_debuglink(&truncated, true),
            Some((Some(String::from("sample.debug")), None))
        );
    }

    #[test]
    fn computes_the_gnu_debuglink_crc_incrementally() {
        assert_eq!(crc_bytes(&[b""]), 0);
        assert_eq!(crc_bytes(&[b"123456789"]), 0xcbf4_3926);
        assert_eq!(
            crc_bytes(&[b"123", b"456", b"789"]),
            0xcbf4_3926,
            "chunk boundaries must not affect the GNU CRC"
        );
    }

    #[test]
    fn accepts_only_debuglink_candidates_with_matching_contents() {
        let directory = TestDirectory::new("debuglink-match");
        let candidate = directory.path().join("sample.debug");
        std::fs::write(&candidate, metadata_elf(true)).unwrap();
        let expected = gnu_debuglink_crc(&candidate).unwrap();

        assert_eq!(
            select_separate_debug_file([candidate.clone()], Some(expected), [], None),
            Some(candidate.clone())
        );
        assert_eq!(
            select_separate_debug_file([candidate], Some(expected ^ 1), [], None),
            None
        );
    }

    #[test]
    fn continues_after_a_debuglink_crc_mismatch() {
        let directory = TestDirectory::new("debuglink-fallback");
        let first = directory.path().join("first.debug");
        let second = directory.path().join("second.debug");
        std::fs::write(&first, b"stale debug information").unwrap();
        std::fs::write(&second, metadata_elf(true)).unwrap();
        let expected = gnu_debuglink_crc(&second).unwrap();

        assert_eq!(
            select_separate_debug_file([first, second.clone()], Some(expected), [], None),
            Some(second)
        );
    }

    #[test]
    fn missing_or_malformed_debuglinks_do_not_produce_false_matches() {
        let directory = TestDirectory::new("debuglink-malformed");
        let missing = directory.path().join("missing.debug");
        let existing = directory.path().join("existing.debug");
        std::fs::write(&existing, b"unvalidated debug information").unwrap();

        assert_eq!(
            select_separate_debug_file([missing], Some(0x1234_5678), [], None),
            None
        );
        assert_eq!(
            select_separate_debug_file([existing], None, [], None),
            None,
            "a truncated debuglink CRC must not weaken candidate validation"
        );
    }

    #[test]
    fn accepts_a_build_id_candidate_with_the_expected_embedded_id() {
        let candidate = std::env::current_exe().unwrap();
        let expected = candidate_build_id(&candidate).unwrap().unwrap();

        assert_eq!(
            select_separate_debug_file([], None, [candidate.clone()], Some(&expected)),
            Some(candidate)
        );
    }

    #[test]
    fn rejects_a_build_id_candidate_with_a_different_embedded_id() {
        let candidate = std::env::current_exe().unwrap();
        let embedded = candidate_build_id(&candidate).unwrap().unwrap();
        let mut expected = embedded.into_bytes();
        expected[0] = if expected[0] == b'0' { b'1' } else { b'0' };
        let expected = String::from_utf8(expected).unwrap();

        assert_eq!(
            select_separate_debug_file([], None, [candidate], Some(&expected)),
            None
        );
    }

    #[test]
    fn rejects_a_build_id_candidate_without_an_embedded_id() {
        let directory = TestDirectory::new("build-id-missing");
        let candidate = directory.path().join("without-build-id.debug");
        let executable = std::env::current_exe().unwrap();
        let mut header = crate::bounded::read_prefix(&executable, 64).unwrap();

        match header.get(4).copied() {
            Some(1) => header[32..36].fill(0),
            Some(2) => header[40..48].fill(0),
            class => panic!("unexpected test executable ELF class {class:?}"),
        }

        std::fs::write(&candidate, header).unwrap();

        assert_eq!(
            select_separate_debug_file([], None, [candidate], Some("deadbeef")),
            None
        );
    }

    #[test]
    fn rejects_a_malformed_build_id_candidate() {
        let directory = TestDirectory::new("build-id-malformed");
        let candidate = directory.path().join("malformed.debug");
        std::fs::write(&candidate, b"not an ELF file").unwrap();

        assert_eq!(
            select_separate_debug_file([], None, [candidate], Some("deadbeef")),
            None
        );
    }

    #[test]
    fn caches_crc_for_an_unchanged_debug_file_and_reloads_changes() {
        let directory = TestDirectory::new("debuglink-cache");
        let candidate = directory.path().join("cached.debug");
        std::fs::write(&candidate, b"first debug contents").unwrap();
        let calculations = debuglink_crc_calculations(&candidate);
        let first = cached_gnu_debuglink_crc(&candidate).unwrap();
        assert_eq!(cached_gnu_debuglink_crc(&candidate).unwrap(), first);
        assert_eq!(debuglink_crc_calculations(&candidate), calculations + 1);
        std::fs::write(&candidate, b"different debug contents with a new size").unwrap();
        let second = cached_gnu_debuglink_crc(&candidate).unwrap();
        assert_ne!(first, second);
        assert_eq!(debuglink_crc_calculations(&candidate), calculations + 2);
    }

    #[test]
    fn crc_cache_reloads_same_size_edits_with_restored_modification_time() {
        let directory = TestDirectory::new("debuglink-cache-restored-time");
        let candidate = directory.path().join("cached.debug");
        std::fs::write(&candidate, b"first").unwrap();
        let modified = std::fs::metadata(&candidate).unwrap().modified().unwrap();
        let first = cached_gnu_debuglink_crc(&candidate).unwrap();
        std::fs::write(&candidate, b"other").unwrap();

        File::options()
            .write(true)
            .open(&candidate)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(modified))
            .unwrap();

        let expected = gnu_debuglink_crc(&candidate).unwrap();
        assert_ne!(first, expected);
        assert_eq!(cached_gnu_debuglink_crc(&candidate).unwrap(), expected);
    }

    #[test]
    fn crc_read_failures_do_not_poison_the_cache() {
        let directory = TestDirectory::new("debuglink-cache-failure");
        let candidate = directory.path().join("later.debug");
        assert!(cached_gnu_debuglink_crc(&candidate).is_err());
        std::fs::write(&candidate, vec![b'x'; DEBUGLINK_CRC_BUFFER_BYTES * 3]).unwrap();
        let chunks = std::cell::Cell::new(0);
        let cancelled = cached_gnu_debuglink_crc_while(&candidate, &|| {
            chunks.set(chunks.get() + 1);
            chunks.get() < 2
        });
        assert_eq!(cancelled.unwrap_err().kind(), io::ErrorKind::Interrupted);
        assert!(
            debuglink_crc_cache()
                .lock()
                .unwrap()
                .get_cloned(&FileIdentity::read(&candidate).unwrap())
                .is_none()
        );
        assert_eq!(
            cached_gnu_debuglink_crc(&candidate).unwrap(),
            gnu_debuglink_crc(&candidate).unwrap()
        );
    }

    #[test]
    fn crc_cache_is_bounded_and_evicts_the_oldest_identity() {
        let identity = |inode| FileIdentity {
            path: PathBuf::from(format!("/debug/{inode}")),
            size: inode,
            modified: SystemTime::UNIX_EPOCH,
            device: 1,
            inode,
            changed: (0, 0),
        };

        let mut cache = DebuglinkCrcCache::new(2);
        cache.insert(identity(1), 1);
        cache.insert(identity(2), 2);
        cache.insert(identity(3), 3);
        assert_eq!(cache.keys().count(), 2);
        assert_eq!(cache.get_cloned(&identity(1)), None);
        assert_eq!(cache.get_cloned(&identity(2)), Some(2));
        assert_eq!(cache.get_cloned(&identity(3)), Some(3));
    }

    #[test]
    fn crc_cache_misses_when_strong_file_identity_changes() {
        let original = FileIdentity {
            path: PathBuf::from("/debug/module.debug"),
            size: 10,
            modified: SystemTime::UNIX_EPOCH,
            device: 1,
            inode: 2,
            changed: (0, 0),
        };

        let mut changed_time = original.clone();
        changed_time.modified = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1);
        let mut changed_inode = original.clone();
        changed_inode.inode += 1;
        let mut cache = DebuglinkCrcCache::new(4);
        cache.insert(original, 42);
        assert_eq!(cache.get_cloned(&changed_time), None);
        assert_eq!(cache.get_cloned(&changed_inode), None);
    }
}
