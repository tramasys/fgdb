use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::OnceLock,
    time::SystemTime,
};

use goblin::{
    container::Ctx,
    elf::{
        Elf,
        note::NT_GNU_BUILD_ID,
        section_header::{SHN_XINDEX, SectionHeader},
    },
};

const MAX_ELF_SECTIONS: usize = 100_000;
const MAX_SECTION_HEADER_BYTES: usize = 16 * 1024 * 1024;
const MAX_SECTION_NAME_BYTES: usize = 16 * 1024 * 1024;
const MAX_METADATA_SECTION_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ModuleDebugMetadata {
    pub(crate) path: PathBuf,
    pub(crate) build_id: Option<String>,
    pub(crate) debuglink: Option<String>,
    pub(crate) debuglink_crc: Option<u32>,
    pub(crate) separate_debug_file: Option<PathBuf>,
    pub(crate) embedded_debug_info: bool,
    pub(crate) suggestion: Option<String>,
    pub(crate) error: Option<String>,
    pub(crate) file_size: Option<u64>,
    pub(crate) modified: Option<SystemTime>,
}

impl ModuleDebugMetadata {
    fn unavailable(path: &Path, error: impl Into<String>) -> Self {
        Self {
            path: path.to_path_buf(),
            build_id: None,
            debuglink: None,
            debuglink_crc: None,
            separate_debug_file: None,
            embedded_debug_info: false,
            suggestion: None,
            error: Some(error.into()),
            file_size: None,
            modified: None,
        }
    }

    fn unavailable_for_file(
        path: &Path,
        error: impl Into<String>,
        metadata: &std::fs::Metadata,
    ) -> Self {
        let mut unavailable = Self::unavailable(path, error);
        unavailable.file_size = Some(metadata.len());
        unavailable.modified = metadata.modified().ok();
        unavailable
    }
}

pub(crate) fn inspect_module(path: &Path) -> ModuleDebugMetadata {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) => {
            let message = format!("Cannot read host module: {error}");
            return match std::fs::metadata(path) {
                Ok(metadata) => ModuleDebugMetadata::unavailable_for_file(path, message, &metadata),
                Err(_) => ModuleDebugMetadata::unavailable(path, message),
            };
        }
    };
    let metadata = match file.metadata() {
        Ok(metadata) if metadata.is_file() => metadata,
        Ok(metadata) => {
            return ModuleDebugMetadata::unavailable_for_file(
                path,
                "The host path is not a regular file",
                &metadata,
            );
        }
        Err(error) => {
            return ModuleDebugMetadata::unavailable(
                path,
                format!("Cannot inspect host module: {error}"),
            );
        }
    };
    let elf = match inspect_elf_metadata(&mut file, metadata.len()) {
        Ok(elf) => elf,
        Err(error) => {
            return ModuleDebugMetadata::unavailable_for_file(path, error, &metadata);
        }
    };
    refresh_module_debug_file(ModuleDebugMetadata {
        path: path.to_path_buf(),
        build_id: elf.build_id,
        debuglink: elf.debuglink,
        debuglink_crc: elf.debuglink_crc,
        separate_debug_file: None,
        embedded_debug_info: elf.embedded_debug_info,
        suggestion: None,
        error: None,
        file_size: Some(metadata.len()),
        modified: metadata.modified().ok(),
    })
}

struct ElfDebugMetadata {
    build_id: Option<String>,
    debuglink: Option<String>,
    debuglink_crc: Option<u32>,
    embedded_debug_info: bool,
}

fn inspect_elf_metadata(file: &mut File, file_size: u64) -> Result<ElfDebugMetadata, String> {
    let header_bytes = read_file_range(file, 0, 64_u64.min(file_size), file_size, 64)
        .map_err(|error| format!("Cannot read ELF header: {error}"))?;
    let header = Elf::parse_header(&header_bytes)
        .map_err(|error| format!("Cannot parse ELF header: {error}"))?;
    if header.e_shoff == 0 {
        return Ok(ElfDebugMetadata {
            build_id: None,
            debuglink: None,
            debuglink_crc: None,
            embedded_debug_info: false,
        });
    }
    let context = Ctx::new(
        header
            .container()
            .map_err(|error| format!("Cannot parse ELF class: {error}"))?,
        header
            .endianness()
            .map_err(|error| format!("Cannot parse ELF byte order: {error}"))?,
    );
    let section_size = SectionHeader::size(context);
    let section_entry_size = usize::from(header.e_shentsize);
    if section_entry_size < section_size {
        return Err(format!(
            "Unsupported ELF section-header size {} (expected {section_size})",
            header.e_shentsize
        ));
    }

    let first_section = read_section_headers(
        file,
        file_size,
        header.e_shoff,
        1,
        section_entry_size,
        context,
    )?;
    let Some(null_section) = first_section.first() else {
        return Err(String::from("ELF section table is empty"));
    };
    let section_count = if header.e_shnum == 0 {
        usize::try_from(null_section.sh_size)
            .map_err(|_| String::from("ELF section count does not fit in memory"))?
    } else {
        usize::from(header.e_shnum)
    };
    if section_count == 0 {
        return Ok(ElfDebugMetadata {
            build_id: None,
            debuglink: None,
            debuglink_crc: None,
            embedded_debug_info: false,
        });
    }
    if section_count > MAX_ELF_SECTIONS {
        return Err(format!(
            "ELF section count {section_count} exceeds the safety limit of {MAX_ELF_SECTIONS}"
        ));
    }
    let sections = read_section_headers(
        file,
        file_size,
        header.e_shoff,
        section_count,
        section_entry_size,
        context,
    )?;
    if header.e_shstrndx == 0 {
        return Ok(ElfDebugMetadata {
            build_id: None,
            debuglink: None,
            debuglink_crc: None,
            embedded_debug_info: false,
        });
    }
    let string_index = if u32::from(header.e_shstrndx) == SHN_XINDEX {
        usize::try_from(null_section.sh_link)
            .map_err(|_| String::from("ELF section-name table index does not fit in memory"))?
    } else {
        usize::from(header.e_shstrndx)
    };
    let string_section = sections
        .get(string_index)
        .ok_or_else(|| String::from("ELF section-name table index is out of range"))?;
    let section_names = read_file_range(
        file,
        string_section.sh_offset,
        string_section.sh_size,
        file_size,
        MAX_SECTION_NAME_BYTES,
    )
    .map_err(|error| format!("Cannot read ELF section names: {error}"))?;

    let mut embedded_debug_info = false;
    let mut build_id_section = None;
    let mut debuglink_section = None;
    for section in &sections {
        match section_name(&section_names, section.sh_name) {
            Some(".debug_info" | ".zdebug_info") => embedded_debug_info = true,
            Some(".note.gnu.build-id") => build_id_section = Some(section),
            Some(".gnu_debuglink") => debuglink_section = Some(section),
            _ => {}
        }
    }
    let build_id = if let Some(section) = build_id_section {
        let bytes = read_metadata_section(file, file_size, section)
            .map_err(|error| format!("Cannot read ELF build ID: {error}"))?;
        gnu_build_id(
            &bytes,
            context.is_little_endian(),
            usize::try_from(section.sh_addralign).unwrap_or(4),
        )
    } else {
        None
    };
    let (debuglink, debuglink_crc) = if let Some(section) = debuglink_section {
        let bytes = read_metadata_section(file, file_size, section)
            .map_err(|error| format!("Cannot read ELF debuglink: {error}"))?;
        gnu_debuglink(&bytes, context.is_little_endian()).unwrap_or_default()
    } else {
        (None, None)
    };
    Ok(ElfDebugMetadata {
        build_id,
        debuglink,
        debuglink_crc,
        embedded_debug_info,
    })
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

fn section_name(names: &[u8], offset: usize) -> Option<&str> {
    let name = names.get(offset..)?;
    let end = name.iter().position(|byte| *byte == 0)?;
    std::str::from_utf8(&name[..end]).ok()
}

pub(crate) fn refresh_module_debug_file(mut metadata: ModuleDebugMetadata) -> ModuleDebugMetadata {
    if metadata.error.is_some() {
        return metadata;
    }
    metadata.separate_debug_file = find_separate_debug_file(
        &metadata.path,
        metadata.debuglink.as_deref(),
        metadata.build_id.as_deref(),
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
        if note_type == NT_GNU_BUILD_ID && name == b"GNU" {
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
    build_id: Option<&str>,
) -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(debuglink) = debuglink.filter(|debuglink| valid_debuglink(debuglink)) {
        let parent = module.parent().unwrap_or_else(|| Path::new("."));
        candidates.push(parent.join(debuglink));
        candidates.push(parent.join(".debug").join(debuglink));
        if module.is_absolute() {
            let relative_parent = parent.strip_prefix("/").unwrap_or(parent);
            candidates.push(
                Path::new("/usr/lib/debug")
                    .join(relative_parent)
                    .join(debuglink),
            );
        }
    }
    if let Some(build_id) = build_id.filter(|build_id| build_id.len() > 2) {
        let (prefix, suffix) = build_id.split_at(2);
        candidates.push(
            Path::new("/usr/lib/debug/.build-id")
                .join(prefix)
                .join(format!("{suffix}.debug")),
        );
        if let Some(cache) = std::env::var_os("HOME") {
            candidates.push(
                PathBuf::from(cache)
                    .join(".cache/debuginfod_client")
                    .join(build_id)
                    .join("debuginfo"),
            );
        }
    }
    candidates.into_iter().find(|candidate| candidate.is_file())
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
    }
}
