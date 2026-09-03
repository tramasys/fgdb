use std::{
    collections::VecDeque,
    fs::File,
    io::{self, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::SystemTime,
};

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

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
const DEBUGLINK_CRC_BUFFER_BYTES: usize = 64 * 1024;
const MAX_DEBUGLINK_CRC_CACHE_ENTRIES: usize = 32;
const GNU_DEBUGLINK_CRC_TABLE: [u32; 256] = gnu_debuglink_crc_table();

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
        metadata.debuglink_crc,
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
    debuglink_crc: Option<u32>,
    build_id: Option<&str>,
) -> Option<PathBuf> {
    let mut debuglink_candidates = Vec::new();

    if let Some(debuglink) = debuglink.filter(|debuglink| valid_debuglink(debuglink)) {
        let parent = module.parent().unwrap_or_else(|| Path::new("."));
        debuglink_candidates.push(parent.join(debuglink));
        debuglink_candidates.push(parent.join(".debug").join(debuglink));

        if module.is_absolute() {
            let relative_parent = parent.strip_prefix("/").unwrap_or(parent);

            debuglink_candidates.push(
                Path::new("/usr/lib/debug")
                    .join(relative_parent)
                    .join(debuglink),
            );
        }
    }

    let mut build_id_candidates = Vec::new();

    if let Some(build_id) = build_id.filter(|build_id| build_id.len() > 2) {
        let (prefix, suffix) = build_id.split_at(2);

        build_id_candidates.push(
            Path::new("/usr/lib/debug/.build-id")
                .join(prefix)
                .join(format!("{suffix}.debug")),
        );

        if let Some(cache) = std::env::var_os("HOME") {
            build_id_candidates.push(
                PathBuf::from(cache)
                    .join(".cache/debuginfod_client")
                    .join(build_id)
                    .join("debuginfo"),
            );
        }
    }

    select_separate_debug_file(
        debuglink_candidates,
        debuglink_crc,
        build_id_candidates,
        build_id,
    )
}

fn select_separate_debug_file(
    debuglink_candidates: impl IntoIterator<Item = PathBuf>,
    debuglink_crc: Option<u32>,
    build_id_candidates: impl IntoIterator<Item = PathBuf>,
    expected_build_id: Option<&str>,
) -> Option<PathBuf> {
    if let Some(expected_crc) = debuglink_crc {
        for candidate in debuglink_candidates {
            if candidate.is_file()
                && cached_gnu_debuglink_crc(&candidate).is_ok_and(|crc| crc == expected_crc)
            {
                return Some(candidate);
            }
        }
    }

    let expected_build_id = expected_build_id?;

    build_id_candidates.into_iter().find(|candidate| {
        candidate_build_id(candidate)
            .is_ok_and(|build_id| build_id.as_deref() == Some(expected_build_id))
    })
}

fn candidate_build_id(path: &Path) -> Result<Option<String>, String> {
    let mut file = File::open(path).map_err(|error| error.to_string())?;
    let metadata = file.metadata().map_err(|error| error.to_string())?;

    if !metadata.is_file() {
        return Err(String::from("Build-ID candidate is not a regular file"));
    }

    inspect_elf_metadata(&mut file, metadata.len()).map(|metadata| metadata.build_id)
}

fn gnu_debuglink_crc(path: &Path) -> io::Result<u32> {
    #[cfg(test)]
    record_debuglink_crc_calculation(path);
    let mut file = File::open(path)?;
    let mut buffer = [0_u8; DEBUGLINK_CRC_BUFFER_BYTES];
    let mut crc = u32::MAX;

    loop {
        let read = file.read(&mut buffer)?;

        if read == 0 {
            break;
        }

        crc = update_gnu_debuglink_crc(crc, &buffer[..read]);
    }

    Ok(!crc)
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DebuglinkFileIdentity {
    path: PathBuf,
    size: u64,
    modified: SystemTime,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

impl DebuglinkFileIdentity {
    fn read(path: &Path) -> io::Result<Self> {
        let metadata = std::fs::metadata(path)?;

        Ok(Self {
            path: path.to_owned(),
            size: metadata.len(),
            modified: metadata.modified()?,
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
        })
    }
}

struct DebuglinkCrcCache {
    entries: VecDeque<(DebuglinkFileIdentity, u32)>,
    capacity: usize,
}

impl DebuglinkCrcCache {
    fn new(capacity: usize) -> Self {
        Self {
            entries: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    fn get(&mut self, identity: &DebuglinkFileIdentity) -> Option<u32> {
        let index = self
            .entries
            .iter()
            .position(|(cached, _)| cached == identity)?;

        let entry = self.entries.remove(index)?;
        let crc = entry.1;
        self.entries.push_back(entry);

        Some(crc)
    }

    fn insert(&mut self, identity: DebuglinkFileIdentity, crc: u32) {
        if self.capacity == 0 {
            return;
        }

        if let Some(index) = self
            .entries
            .iter()
            .position(|(cached, _)| cached == &identity)
        {
            self.entries.remove(index);
        }

        self.entries.push_back((identity, crc));

        while self.entries.len() > self.capacity {
            self.entries.pop_front();
        }
    }
}

fn debuglink_crc_cache() -> &'static Mutex<DebuglinkCrcCache> {
    static CACHE: OnceLock<Mutex<DebuglinkCrcCache>> = OnceLock::new();

    CACHE.get_or_init(|| Mutex::new(DebuglinkCrcCache::new(MAX_DEBUGLINK_CRC_CACHE_ENTRIES)))
}

fn cached_gnu_debuglink_crc(path: &Path) -> io::Result<u32> {
    let before = DebuglinkFileIdentity::read(path)?;

    if let Some(crc) = debuglink_crc_cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&before)
    {
        return Ok(crc);
    }

    let crc = gnu_debuglink_crc(path)?;
    let after = DebuglinkFileIdentity::read(path)?;

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
        std::fs::write(&candidate, b"matching debug information").unwrap();
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
        std::fs::write(&second, b"matching debug information").unwrap();
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
    fn crc_read_failures_do_not_poison_the_cache() {
        let directory = TestDirectory::new("debuglink-cache-failure");
        let candidate = directory.path().join("later.debug");
        assert!(cached_gnu_debuglink_crc(&candidate).is_err());
        std::fs::write(&candidate, b"now available").unwrap();
        assert_eq!(
            cached_gnu_debuglink_crc(&candidate).unwrap(),
            gnu_debuglink_crc(&candidate).unwrap()
        );
    }

    #[test]
    fn crc_cache_is_bounded_and_evicts_the_oldest_identity() {
        let identity = |inode| DebuglinkFileIdentity {
            path: PathBuf::from(format!("/debug/{inode}")),
            size: inode,
            modified: SystemTime::UNIX_EPOCH,
            #[cfg(unix)]
            device: 1,
            #[cfg(unix)]
            inode,
        };
        let mut cache = DebuglinkCrcCache::new(2);
        cache.insert(identity(1), 1);
        cache.insert(identity(2), 2);
        cache.insert(identity(3), 3);
        assert_eq!(cache.entries.len(), 2);
        assert_eq!(cache.get(&identity(1)), None);
        assert_eq!(cache.get(&identity(2)), Some(2));
        assert_eq!(cache.get(&identity(3)), Some(3));
    }

    #[test]
    fn crc_cache_misses_when_strong_file_identity_changes() {
        let original = DebuglinkFileIdentity {
            path: PathBuf::from("/debug/module.debug"),
            size: 10,
            modified: SystemTime::UNIX_EPOCH,
            #[cfg(unix)]
            device: 1,
            #[cfg(unix)]
            inode: 2,
        };
        let mut changed_time = original.clone();
        changed_time.modified = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1);
        let mut changed_inode = original.clone();
        #[cfg(unix)]
        {
            changed_inode.inode += 1;
        }

        #[cfg(not(unix))]
        {
            changed_inode.size += 1;
        }

        let mut cache = DebuglinkCrcCache::new(4);
        cache.insert(original, 42);
        assert_eq!(cache.get(&changed_time), None);
        assert_eq!(cache.get(&changed_inode), None);
    }
}
