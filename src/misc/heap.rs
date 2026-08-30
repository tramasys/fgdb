use std::{
    collections::{HashMap, HashSet},
    fs::File,
    io::Read,
    os::unix::fs::FileExt,
    path::Path,
};

use super::*;

const MAX_LIBC_BYTES: usize = 64 * 1024 * 1024;
const MAX_HEAP_READ_BYTES: usize = 32 * 1024 * 1024;
const MAX_ARENAS: usize = 256;
const MAX_CHUNKS: usize = 8_192;
const MAX_FREELIST_NODES: usize = 65_536;
const MAX_NODES_PER_BIN: usize = 4_096;
const MIN_SUPPORTED_GLIBC_MINOR: u32 = 15;
const MAX_SUPPORTED_GLIBC_MINOR: u32 = 44;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NativeHeapQuery {
    Arenas,
    Arena,
    Top,
    Chunks,
    Parsed,
    CompactBins,
    AllBins,
    TcacheBins,
    FastBins,
    UnsortedBin,
    SmallBins,
    LargeBins,
    Chunk(u64),
}

impl NativeHeapQuery {
    pub(crate) const fn title(self) -> &'static str {
        match self {
            Self::Arenas => "heap arenas",
            Self::Arena => "heap arena",
            Self::Top => "heap top",
            Self::Chunks => "heap chunks",
            Self::Parsed => "heap parse",
            Self::CompactBins => "heap bins compact",
            Self::AllBins => "heap bins",
            Self::TcacheBins => "heap bins tcache",
            Self::FastBins => "heap bins fast",
            Self::UnsortedBin => "heap bins unsorted",
            Self::SmallBins => "heap bins small",
            Self::LargeBins => "heap bins large",
            Self::Chunk(_) => "heap chunk",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct HeapDiscovery {
    pub main_arena: Option<u64>,
    pub malloc_hook: Option<u64>,
    pub tcache: Option<u64>,
    pub tls_bases: Vec<u64>,
}

#[derive(Clone, Debug)]
pub(crate) struct NativeHeapReadRequest {
    pub pid: u32,
    pub debugger_pid: u32,
    pub architecture: TargetArchitecture,
    pub endian: TargetEndian,
    pub pointer_bits: u32,
    pub query: NativeHeapQuery,
    pub discovery: HeapDiscovery,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct GlibcVersion {
    major: u32,
    minor: u32,
}

impl GlibcVersion {
    const fn at_least(self, minor: u32) -> bool {
        self.major > 2 || (self.major == 2 && self.minor >= minor)
    }
}

impl std::fmt::Display for GlibcVersion {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}.{}", self.major, self.minor)
    }
}

#[derive(Clone, Copy, Debug)]
struct GlibcLayout {
    version: GlibcVersion,
    architecture: TargetArchitecture,
    pointer_size: u64,
    malloc_alignment: u64,
    min_chunk_size: u64,
    num_fastbins: usize,
    fastbins: Option<u64>,
    top: u64,
    last_remainder: u64,
    bins: u64,
    binmap: u64,
    next: u64,
    next_free: Option<u64>,
    attached_threads: Option<u64>,
    system_mem: u64,
    max_system_mem: u64,
    arena_size: u64,
}

impl GlibcLayout {
    // These offsets mirror glibc's malloc_state transitions and Bata24 GEF's
    // no-debuginfo fallback: have_fastchunks in 2.27, 32-bit alignment changes
    // in 2.26, and fastbin removal in 2.43. Every selected layout is validated
    // against the live arena ring, top chunk, and representative bin heads
    // before any address is accepted as main_arena.
    fn new(
        version: GlibcVersion,
        architecture: TargetArchitecture,
        pointer_bits: u32,
    ) -> Result<Self, String> {
        let pointer_size = match pointer_bits {
            32 => 4,
            64 => 8,
            other => return Err(format!("Unsupported {other}-bit glibc target")),
        };
        let special_32 = pointer_size == 4
            && matches!(
                architecture,
                TargetArchitecture::X86
                    | TargetArchitecture::RiscV32
                    | TargetArchitecture::PowerPc32
            );
        let malloc_alignment = if pointer_size == 8 || (special_32 && version.at_least(26)) {
            16
        } else {
            8
        };
        let num_fastbins = if version.at_least(43) {
            0
        } else if special_32 && version.at_least(26) {
            11
        } else {
            10
        };
        let flags = 4_u64;
        let fastbins = (!version.at_least(43)).then(|| {
            if version.at_least(27) {
                align_up(12, pointer_size)
            } else {
                8
            }
        });
        let top = fastbins.map_or(flags + 4, |offset| {
            offset + pointer_size * u64::try_from(num_fastbins).unwrap_or(0)
        });
        let last_remainder = top + pointer_size;
        let bins = last_remainder + pointer_size;
        let binmap = bins + pointer_size * 254;
        let next = binmap + 16;
        let next_free = version.at_least(19).then_some(next + pointer_size);
        let attached_threads = version
            .at_least(23)
            .then(|| next_free.unwrap_or(next) + pointer_size);
        let system_mem = if let Some(offset) = attached_threads {
            offset + pointer_size
        } else if let Some(offset) = next_free {
            offset + pointer_size
        } else {
            next + pointer_size
        };
        let max_system_mem = system_mem + pointer_size;
        let arena_size = max_system_mem + pointer_size;
        Ok(Self {
            version,
            architecture,
            pointer_size,
            malloc_alignment,
            min_chunk_size: if pointer_size == 8 { 0x20 } else { 0x10 },
            num_fastbins,
            fastbins,
            top,
            last_remainder,
            bins,
            binmap,
            next,
            next_free,
            attached_threads,
            system_mem,
            max_system_mem,
            arena_size,
        })
    }

    fn main_heap_start_adjustment(self) -> u64 {
        let special_32 = self.pointer_size == 4
            && matches!(
                self.architecture,
                TargetArchitecture::X86
                    | TargetArchitecture::RiscV32
                    | TargetArchitecture::PowerPc32
            );
        u64::from(special_32 && self.version.at_least(26)) * 8
    }

    fn tcache_bin_count(self) -> usize {
        if self.version.at_least(42) { 76 } else { 64 }
    }

    fn tcache_count_size(self) -> u64 {
        if self.version.at_least(30) { 2 } else { 1 }
    }

    fn tcache_struct_size(self) -> u64 {
        let bins = u64::try_from(self.tcache_bin_count()).unwrap_or(0);
        bins * (self.tcache_count_size() + self.pointer_size)
    }

    fn tcache_chunk_size(self) -> u64 {
        request_to_chunk_size(
            self.tcache_struct_size(),
            self.pointer_size,
            self.malloc_alignment,
            self.min_chunk_size,
        )
    }

    const fn arena_alignment(self) -> u64 {
        if self.pointer_size == 4 { 8 } else { 16 }
    }

    fn fastbin_index_used(self, index: usize) -> bool {
        index < self.num_fastbins.min(7)
            && (!(self.pointer_size == 4
                && self.version.at_least(26)
                && matches!(
                    self.architecture,
                    TargetArchitecture::X86
                        | TargetArchitecture::RiscV32
                        | TargetArchitecture::PowerPc32
                ))
                || index.is_multiple_of(2))
    }

    fn fastbin_size(self, index: usize) -> u64 {
        if self.pointer_size == 4
            && self.version.at_least(26)
            && matches!(
                self.architecture,
                TargetArchitecture::X86
                    | TargetArchitecture::RiscV32
                    | TargetArchitecture::PowerPc32
            )
        {
            self.min_chunk_size + u64::try_from(index / 2).unwrap_or(0) * self.malloc_alignment
        } else {
            self.min_chunk_size + u64::try_from(index).unwrap_or(0) * self.malloc_alignment
        }
    }
}

#[derive(Clone, Debug)]
struct Arena {
    address: u64,
    main: bool,
    top: u64,
    last_remainder: u64,
    next: u64,
    next_free: Option<u64>,
    attached_threads: Option<u64>,
    system_mem: u64,
    max_system_mem: u64,
    heap_base: Option<u64>,
}

#[derive(Clone, Copy, Debug)]
struct Chunk {
    base: u64,
    user: u64,
    previous_size: u64,
    raw_size: u64,
    size: u64,
}

impl Chunk {
    const fn previous_in_use(self) -> bool {
        self.raw_size & 1 != 0
    }

    const fn mapped(self) -> bool {
        self.raw_size & 2 != 0
    }

    const fn non_main(self) -> bool {
        self.raw_size & 4 != 0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BinKind {
    Tcache,
    Fast,
    Unsorted,
    Small,
    Large,
}

#[derive(Clone, Debug)]
struct BinRecord {
    kind: BinKind,
    index: usize,
    expected_size: String,
    declared_count: Option<usize>,
    chunks: Vec<u64>,
    warning: Option<String>,
}

struct MemoryReader<'a> {
    file: File,
    mappings: &'a [ProcessMapping],
    endian: TargetEndian,
    pointer_size: usize,
    bytes_read: usize,
}

impl<'a> MemoryReader<'a> {
    fn new(
        root: &Path,
        mappings: &'a [ProcessMapping],
        endian: TargetEndian,
        pointer_bits: u32,
    ) -> Result<Self, String> {
        let pointer_size = usize::try_from(pointer_bits / 8)
            .unwrap_or_default()
            .clamp(4, 8);
        let file = File::open(root.join("mem"))
            .map_err(|error| format!("Cannot read the traced process memory: {error}"))?;
        Ok(Self {
            file,
            mappings,
            endian,
            pointer_size,
            bytes_read: 0,
        })
    }

    fn mapping(&self, address: u64) -> Option<&ProcessMapping> {
        let index = self
            .mappings
            .partition_point(|mapping| mapping.start <= address)
            .checked_sub(1)?;
        self.mappings
            .get(index)
            .filter(|mapping| mapping.permissions.starts_with('r') && address < mapping.end)
    }

    fn readable(&self, address: u64, length: usize) -> bool {
        let Ok(length) = u64::try_from(length) else {
            return false;
        };
        let Some(end) = address.checked_add(length) else {
            return false;
        };
        self.mapping(address)
            .is_some_and(|mapping| end <= mapping.end)
    }

    fn read(&mut self, address: u64, bytes: &mut [u8]) -> Result<(), String> {
        if !self.readable(address, bytes.len()) {
            return Err(format!(
                "Address range 0x{address:x}+0x{:x} is not readable",
                bytes.len()
            ));
        }
        self.bytes_read = self
            .bytes_read
            .checked_add(bytes.len())
            .ok_or_else(|| String::from("Heap read accounting overflowed"))?;
        if self.bytes_read > MAX_HEAP_READ_BYTES {
            return Err(format!(
                "Heap inspection exceeded its {} MiB memory-read budget",
                MAX_HEAP_READ_BYTES / (1024 * 1024)
            ));
        }
        let mut done = 0_usize;
        while done < bytes.len() {
            let offset = address
                .checked_add(u64::try_from(done).unwrap_or(u64::MAX))
                .ok_or_else(|| String::from("Heap read address overflowed"))?;
            let read = self
                .file
                .read_at(&mut bytes[done..], offset)
                .map_err(|error| format!("Cannot read inferior memory at 0x{offset:x}: {error}"))?;
            if read == 0 {
                return Err(format!("Short read from inferior memory at 0x{offset:x}"));
            }
            done += read;
        }
        Ok(())
    }

    fn word(&mut self, address: u64) -> Result<u64, String> {
        let mut bytes = [0_u8; 8];
        let size = self.pointer_size;
        self.read(address, &mut bytes[..size])?;
        read_word(&bytes[..size], self.endian)
            .ok_or_else(|| format!("Cannot decode target word at 0x{address:x}"))
    }

    fn u16(&mut self, address: u64) -> Result<u16, String> {
        let mut bytes = [0_u8; 2];
        self.read(address, &mut bytes)?;
        Ok(match self.endian {
            TargetEndian::Little => u16::from_le_bytes(bytes),
            TargetEndian::Big => u16::from_be_bytes(bytes),
        })
    }

    fn u8(&mut self, address: u64) -> Result<u8, String> {
        let mut byte = [0_u8; 1];
        self.read(address, &mut byte)?;
        Ok(byte[0])
    }
}

struct Inspector<'a> {
    reader: MemoryReader<'a>,
    mappings: &'a [ProcessMapping],
    layout: GlibcLayout,
    version: GlibcVersion,
    main_arena: u64,
    main_heap: Option<(u64, u64)>,
    tcache_hint: Option<u64>,
    tls_bases: &'a [u64],
    rows: Vec<HeapInspectionRow>,
    truncated: bool,
}

pub(crate) fn inspect_native_heap(
    request: NativeHeapReadRequest,
) -> Result<HeapInspectionSnapshot, String> {
    let root = crate::kernel::verified_proc_root(request.pid, request.debugger_pid)?;
    let (mappings, mappings_capped) = read_maps(&root.join("maps"))?;
    if mappings_capped {
        return Err(String::from(
            "The process has more mappings than the native heap reader can safely index",
        ));
    }
    let version = detect_glibc_version(&root, &mappings)?;
    if version.major != 2
        || version.minor < MIN_SUPPORTED_GLIBC_MINOR
        || version.minor > MAX_SUPPORTED_GLIBC_MINOR
    {
        return Err(format!(
            "Native ptmalloc decoding supports glibc 2.{MIN_SUPPORTED_GLIBC_MINOR} through 2.{MAX_SUPPORTED_GLIBC_MINOR}. Target uses {version}"
        ));
    }
    let layout = GlibcLayout::new(version, request.architecture, request.pointer_bits)?;
    let reader = MemoryReader::new(&root, &mappings, request.endian, request.pointer_bits)?;
    let main_heap = mappings
        .iter()
        .find(|mapping| mapping.path == "[heap]" && mapping.permissions.starts_with('r'))
        .map(|mapping| (mapping.start, mapping.end));
    let mut locator = Inspector {
        reader,
        mappings: &mappings,
        layout,
        version,
        main_arena: 0,
        main_heap,
        tcache_hint: request.discovery.tcache,
        tls_bases: &request.discovery.tls_bases,
        rows: Vec::new(),
        truncated: false,
    };
    locator.main_arena = locator.locate_main_arena(&request.discovery)?;
    locator.run(request.query)?;
    crate::kernel::verified_proc_root(request.pid, request.debugger_pid)?;

    let bytes_read = locator.reader.bytes_read;
    let summary = native_heap_summary(request.query, version, &locator.rows, bytes_read);
    Ok(HeapInspectionSnapshot {
        command: request.query.title().to_owned(),
        summary,
        diagnostic: None,
        rows: locator.rows,
        truncated: locator.truncated,
    })
}

impl Inspector<'_> {
    fn run(&mut self, query: NativeHeapQuery) -> Result<(), String> {
        let arenas = self.arenas()?;
        match query {
            NativeHeapQuery::Arenas => self.show_arenas(&arenas),
            NativeHeapQuery::Arena => self.show_arena(
                arenas
                    .first()
                    .ok_or_else(|| String::from("No initialized ptmalloc arena was found"))?,
            ),
            NativeHeapQuery::Top => self.show_top(
                arenas
                    .first()
                    .ok_or_else(|| String::from("No initialized ptmalloc arena was found"))?,
            )?,
            NativeHeapQuery::Chunks => self.show_chunks(&arenas, false)?,
            NativeHeapQuery::Parsed => self.show_chunks(&arenas, true)?,
            NativeHeapQuery::Chunk(address) => self.show_target_chunk(&arenas, address)?,
            NativeHeapQuery::CompactBins
            | NativeHeapQuery::AllBins
            | NativeHeapQuery::TcacheBins
            | NativeHeapQuery::FastBins
            | NativeHeapQuery::UnsortedBin
            | NativeHeapQuery::SmallBins
            | NativeHeapQuery::LargeBins => self.show_bins(&arenas, query)?,
        }
        Ok(())
    }

    fn locate_main_arena(&mut self, discovery: &HeapDiscovery) -> Result<u64, String> {
        if let Some(address) = discovery.main_arena
            && self.valid_arena_ring(address, true)
        {
            return Ok(address);
        }
        if !self.version.at_least(34)
            && let Some(hook) = discovery.malloc_hook
            && let Some(address) = self.arena_from_malloc_hook(hook)
            && self.valid_arena_ring(address, true)
        {
            return Ok(address);
        }
        for tls in self
            .tls_bases
            .iter()
            .copied()
            .filter(|address| *address != 0)
        {
            for index in 1_u64..=500 {
                let distance = match index.checked_mul(self.layout.pointer_size) {
                    Some(distance) => distance,
                    None => break,
                };
                for slot in [tls.checked_sub(distance), tls.checked_add(distance)]
                    .into_iter()
                    .flatten()
                {
                    let Ok(candidate) = self.reader.word(slot) else {
                        continue;
                    };
                    if candidate == 0 || !self.plausible_main_arena_mapping(candidate) {
                        continue;
                    }
                    if self.valid_arena_ring(candidate, true) {
                        return Ok(candidate);
                    }
                }
            }
        }
        Err(String::from(
            "Cannot locate glibc main_arena. Install matching libc debuginfo or stop on a thread whose TLS base GDB exposes. fgdb will not guess an unvalidated arena address.",
        ))
    }

    fn arena_from_malloc_hook(&self, hook: u64) -> Option<u64> {
        match self.layout.architecture {
            TargetArchitecture::X86 | TargetArchitecture::X86_64 => {
                Some(align_up(hook.checked_add(self.layout.pointer_size)?, 0x20))
            }
            TargetArchitecture::AArch64 => hook
                .checked_sub(self.layout.pointer_size.checked_mul(2)?)?
                .checked_sub(self.layout.arena_size),
            TargetArchitecture::Arm => hook
                .checked_sub(self.layout.pointer_size)?
                .checked_sub(self.layout.arena_size),
            _ => None,
        }
    }

    fn plausible_main_arena_mapping(&self, address: u64) -> bool {
        self.reader.mapping(address).is_some_and(|mapping| {
            mapping.permissions.contains('w')
                && (mapping.path.contains("libc")
                    || self.mappings.iter().any(|candidate| {
                        !mapping.path.is_empty()
                            && !mapping.path.starts_with('[')
                            && candidate.path == mapping.path
                            && candidate.permissions.contains('x')
                    }))
        })
    }

    fn valid_arena_ring(&mut self, start: u64, main: bool) -> bool {
        let mut current = start;
        let mut visited = HashSet::with_capacity(4);
        for _ in 0..MAX_ARENAS {
            if !visited.insert(current) {
                return current == start;
            }
            let Ok(arena) = self.read_arena(current, main && current == start) else {
                return false;
            };
            if arena.system_mem < 4096
                || arena.top == 0
                || !arena.top.is_multiple_of(self.layout.arena_alignment())
                || !self
                    .reader
                    .readable(arena.top, self.reader.pointer_size * 2)
                || !self.valid_top_chunk(&arena)
                || !self.valid_arena_bin_heads(arena.address)
            {
                return false;
            }
            if arena.next == start {
                return true;
            }
            if arena.next == 0
                || !self
                    .reader
                    .readable(arena.next, self.layout.arena_size as usize)
            {
                return false;
            }
            current = arena.next;
        }
        false
    }

    fn valid_top_chunk(&mut self, arena: &Arena) -> bool {
        self.read_chunk(arena.top).is_ok_and(|chunk| {
            self.plausible_chunk(&chunk) && chunk.size <= arena.system_mem && !chunk.mapped()
        })
    }

    fn valid_arena_bin_heads(&mut self, arena: u64) -> bool {
        for index in [0_u64, 1, 2, 31, 63, 126] {
            let Some(pair) = arena
                .checked_add(self.layout.bins)
                .and_then(|address| address.checked_add(index * self.layout.pointer_size * 2))
            else {
                return false;
            };
            let Some(head) = pair.checked_sub(self.layout.pointer_size * 2) else {
                return false;
            };
            let (Ok(forward), Ok(backward)) = (
                self.reader.word(pair),
                self.reader.word(pair + self.layout.pointer_size),
            ) else {
                return false;
            };
            for pointer in [forward, backward] {
                if pointer == 0
                    || (pointer != head
                        && (!pointer.is_multiple_of(self.layout.arena_alignment())
                            || !self.reader.readable(pointer, self.reader.pointer_size * 4)))
                {
                    return false;
                }
            }
        }
        true
    }

    fn arenas(&mut self) -> Result<Vec<Arena>, String> {
        let mut arenas = Vec::new();
        let mut visited = HashSet::with_capacity(4);
        let mut current = self.main_arena;
        while visited.insert(current) {
            if arenas.len() == MAX_ARENAS {
                return Err(format!(
                    "Arena ring exceeds the {MAX_ARENAS}-arena safety limit"
                ));
            }
            let arena = self.read_arena(current, current == self.main_arena)?;
            current = arena.next;
            arenas.push(arena);
            if current == self.main_arena {
                break;
            }
            if current == 0 {
                return Err(String::from(
                    "ptmalloc arena ring contains a null next pointer",
                ));
            }
        }
        if current != self.main_arena {
            return Err(format!(
                "ptmalloc arena ring loops at 0x{current:x} instead of main_arena"
            ));
        }
        Ok(arenas)
    }

    fn read_arena(&mut self, address: u64, main: bool) -> Result<Arena, String> {
        let field = |base: u64, offset: u64| {
            base.checked_add(offset)
                .ok_or_else(|| String::from("Arena field address overflowed"))
        };
        let top = self.reader.word(field(address, self.layout.top)?)?;
        let last_remainder = self
            .reader
            .word(field(address, self.layout.last_remainder)?)?;
        let next = self.reader.word(field(address, self.layout.next)?)?;
        let next_free = self
            .layout
            .next_free
            .map(|offset| self.reader.word(field(address, offset)?))
            .transpose()?;
        let attached_threads = self
            .layout
            .attached_threads
            .map(|offset| self.reader.word(field(address, offset)?))
            .transpose()?;
        let system_mem = self.reader.word(field(address, self.layout.system_mem)?)?;
        let max_system_mem = self
            .reader
            .word(field(address, self.layout.max_system_mem)?)?;
        let heap_base = if main {
            self.main_heap
                .map(|(start, _)| start + self.layout.main_heap_start_adjustment())
        } else {
            Some(align_up(
                address
                    .checked_add(self.layout.arena_size)
                    .ok_or_else(|| String::from("Thread-arena base overflowed"))?,
                self.layout.arena_alignment(),
            ))
        };
        Ok(Arena {
            address,
            main,
            top,
            last_remainder,
            next,
            next_free,
            attached_threads,
            system_mem,
            max_system_mem,
            heap_base,
        })
    }

    fn show_arenas(&mut self, arenas: &[Arena]) {
        for arena in arenas {
            self.push_row(
                "Arena",
                &format_address(arena.address),
                &format!("system {}", crate::kernel::format_bytes(arena.system_mem)),
                if arena.main { "main" } else { "thread" },
                &format!(
                    "heap {}  ·  top {}  ·  next {}  ·  attached {}  ·  max system {}",
                    arena
                        .heap_base
                        .map_or_else(|| String::from("uninitialized"), format_address),
                    format_address(arena.top),
                    format_address(arena.next),
                    arena
                        .attached_threads
                        .map_or_else(|| String::from("n/a"), |value| value.to_string()),
                    crate::kernel::format_bytes(arena.max_system_mem)
                ),
            );
        }
    }

    fn show_arena(&mut self, arena: &Arena) {
        self.push_row(
            "Layout",
            "glibc",
            &self.version.to_string(),
            "native",
            &format!(
                "{}-bit pointers  ·  alignment 0x{:x}  ·  malloc_state 0x{:x} bytes",
                self.layout.pointer_size * 8,
                self.layout.malloc_alignment,
                self.layout.arena_size
            ),
        );
        for (name, value) in [
            ("address", Some(arena.address)),
            ("heap_base", arena.heap_base),
            ("top", Some(arena.top)),
            ("last_remainder", Some(arena.last_remainder)),
            ("next", Some(arena.next)),
            ("next_free", arena.next_free),
            ("attached_threads", arena.attached_threads),
            ("system_mem", Some(arena.system_mem)),
            ("max_system_mem", Some(arena.max_system_mem)),
        ] {
            self.push_row(
                "Arena field",
                name,
                &value.map_or_else(|| String::from("n/a"), format_address),
                if arena.main { "main" } else { "thread" },
                "",
            );
        }
        if let Some(fastbins) = self.layout.fastbins {
            self.push_row(
                "Arena field",
                "fastbinsY",
                &format_address(arena.address + fastbins),
                &format!("{} slots", self.layout.num_fastbins),
                "",
            );
        } else {
            self.push_row(
                "Arena field",
                "fastbinsY",
                "not present",
                "glibc 2.43+",
                "Fastbins were removed from this malloc_state layout",
            );
        }
        self.push_row(
            "Arena field",
            "bins",
            &format_address(arena.address + self.layout.bins),
            "127 bins",
            &format!(
                "binmap at {}",
                format_address(arena.address + self.layout.binmap)
            ),
        );
    }

    fn show_top(&mut self, arena: &Arena) -> Result<(), String> {
        let chunk = self.read_chunk(arena.top)?;
        self.push_chunk(&chunk, "Top", &[String::from("top chunk")], false);
        Ok(())
    }

    fn show_chunks(&mut self, arenas: &[Arena], include_links: bool) -> Result<(), String> {
        let free = self.freelist_index(arenas)?;
        for arena in arenas {
            let chunks = self.chunks(arena)?;
            for chunk in chunks {
                let tags = free.get(&chunk.base).cloned().unwrap_or_default();
                let state = if chunk.base == arena.top {
                    "Top"
                } else if !tags.is_empty() {
                    "Freed"
                } else if chunk.mapped() {
                    "Mapped"
                } else if self.next_chunk_marks_in_use(&chunk) {
                    "Used"
                } else {
                    "Freed"
                };
                self.push_chunk(&chunk, state, &tags, include_links);
            }
        }
        Ok(())
    }

    fn show_target_chunk(&mut self, arenas: &[Arena], address: u64) -> Result<(), String> {
        let free = self.freelist_index(arenas)?;
        for arena in arenas {
            for chunk in self.chunks(arena)? {
                if address >= chunk.base && address < chunk.base.saturating_add(chunk.size) {
                    let tags = free.get(&chunk.base).cloned().unwrap_or_default();
                    let state = if chunk.base == arena.top {
                        "Top"
                    } else if !tags.is_empty() {
                        "Freed"
                    } else if self.next_chunk_marks_in_use(&chunk) {
                        "Used"
                    } else {
                        "Freed"
                    };
                    self.push_chunk(&chunk, state, &tags, true);
                    self.push_row(
                        "Address",
                        &format_address(address),
                        &format!("+0x{:x}", address - chunk.base),
                        "inside chunk",
                        &format!("user payload begins at {}", format_address(chunk.user)),
                    );
                    return Ok(());
                }
            }
        }
        for base in [
            address,
            address.saturating_sub(self.layout.pointer_size * 2),
        ] {
            if let Ok(chunk) = self.read_chunk(base)
                && self.plausible_chunk(&chunk)
                && address >= chunk.base
                && address < chunk.base.saturating_add(chunk.size)
            {
                let tags = free.get(&chunk.base).cloned().unwrap_or_default();
                self.push_chunk(
                    &chunk,
                    if tags.is_empty() { "Unknown" } else { "Freed" },
                    &tags,
                    true,
                );
                return Ok(());
            }
        }
        Err(format!(
            "0x{address:x} is not inside a bounded, structurally valid ptmalloc chunk"
        ))
    }

    fn chunks(&mut self, arena: &Arena) -> Result<Vec<Chunk>, String> {
        let Some(mut current) = arena.heap_base else {
            return Err(String::from("The ptmalloc heap is not initialized"));
        };
        if current > arena.top {
            return Err(format!(
                "Arena heap base {} is above top {}",
                format_address(current),
                format_address(arena.top)
            ));
        }
        let mut chunks = Vec::new();
        let mut seen = HashSet::new();
        while current <= arena.top {
            if chunks.len() == MAX_CHUNKS {
                self.truncated = true;
                break;
            }
            if !seen.insert(current) {
                return Err(format!("Chunk traversal loops at 0x{current:x}"));
            }
            let chunk = self.read_chunk(current)?;
            if !self.plausible_chunk(&chunk) {
                return Err(format!(
                    "Invalid chunk size 0x{:x} at 0x{:x}",
                    chunk.size, current
                ));
            }
            chunks.push(chunk);
            if current == arena.top || chunk.size == 0 {
                break;
            }
            let next = current
                .checked_add(chunk.size)
                .ok_or_else(|| String::from("Chunk address overflowed"))?;
            if next <= current || next > arena.top {
                return Err(format!(
                    "Chunk at 0x{current:x} advances beyond arena.top {}",
                    format_address(arena.top)
                ));
            }
            current = next;
        }
        Ok(chunks)
    }

    fn read_chunk(&mut self, base: u64) -> Result<Chunk, String> {
        let size_address = base
            .checked_add(self.layout.pointer_size)
            .ok_or_else(|| String::from("Chunk size address overflowed"))?;
        let previous_size = self.reader.word(base)?;
        let raw_size = self.reader.word(size_address)?;
        let user = base
            .checked_add(self.layout.pointer_size * 2)
            .ok_or_else(|| String::from("Chunk user address overflowed"))?;
        Ok(Chunk {
            base,
            user,
            previous_size,
            raw_size,
            size: raw_size & !7,
        })
    }

    fn plausible_chunk(&self, chunk: &Chunk) -> bool {
        chunk.size >= self.layout.min_chunk_size
            && chunk.size.is_multiple_of(self.layout.malloc_alignment)
            && chunk
                .base
                .checked_add(chunk.size)
                .is_some_and(|end| end > chunk.base)
    }

    fn next_chunk_marks_in_use(&mut self, chunk: &Chunk) -> bool {
        let Some(next) = chunk.base.checked_add(chunk.size) else {
            return false;
        };
        self.reader
            .word(next.saturating_add(self.layout.pointer_size))
            .is_ok_and(|raw| raw & 1 != 0)
    }

    fn push_chunk(&mut self, chunk: &Chunk, state: &str, tags: &[String], include_links: bool) {
        let mut flags = Vec::new();
        if chunk.previous_in_use() {
            flags.push("PREV_INUSE");
        }
        if chunk.mapped() {
            flags.push("IS_MMAPPED");
        }
        if chunk.non_main() {
            flags.push("NON_MAIN_ARENA");
        }
        let usable = if chunk.mapped() {
            chunk.size.saturating_sub(self.layout.pointer_size * 2)
        } else {
            chunk.size.saturating_sub(self.layout.pointer_size)
        };
        let mut details = format!(
            "user {}  ·  usable 0x{:x}  ·  prev_size 0x{:x}",
            format_address(chunk.user),
            usable,
            chunk.previous_size
        );
        if !flags.is_empty() {
            details.push_str("  ·  ");
            details.push_str(&flags.join(" | "));
        }
        if !tags.is_empty() {
            details.push_str("  ·  ");
            details.push_str(&tags.join(", "));
        }
        if include_links && state == "Freed" {
            if let Ok(forward) = self.reader.word(chunk.user) {
                details.push_str(&format!("  ·  fd/raw {}", format_address(forward)));
            }
            if let Ok(backward) = self
                .reader
                .word(chunk.user.saturating_add(self.layout.pointer_size))
            {
                details.push_str(&format!("  ·  bk/key {}", format_address(backward)));
            }
        }
        self.push_inspectable_row(
            "Chunk",
            &format_address(chunk.base),
            &format!("0x{:x}", chunk.size),
            state,
            &details,
            Some(chunk.base),
        );
    }

    fn show_bins(&mut self, arenas: &[Arena], query: NativeHeapQuery) -> Result<(), String> {
        let mut total_nodes = 0_usize;
        for arena in arenas {
            let bins = self.collect_bins(arena, &mut total_nodes)?;
            for bin in bins {
                let matches = match query {
                    NativeHeapQuery::CompactBins | NativeHeapQuery::AllBins => true,
                    NativeHeapQuery::TcacheBins => bin.kind == BinKind::Tcache,
                    NativeHeapQuery::FastBins => bin.kind == BinKind::Fast,
                    NativeHeapQuery::UnsortedBin => bin.kind == BinKind::Unsorted,
                    NativeHeapQuery::SmallBins => bin.kind == BinKind::Small,
                    NativeHeapQuery::LargeBins => bin.kind == BinKind::Large,
                    _ => false,
                };
                let occupied = !bin.chunks.is_empty() || bin.warning.is_some();
                if !matches || (query == NativeHeapQuery::CompactBins && !occupied) {
                    continue;
                }
                let name = match bin.kind {
                    BinKind::Tcache => "Tcache bin",
                    BinKind::Fast => "Fastbin",
                    BinKind::Unsorted => "Unsorted bin",
                    BinKind::Small => "Small bin",
                    BinKind::Large => "Large bin",
                };
                let parsed_count = bin.chunks.len();
                let metric = bin.declared_count.map_or_else(
                    || format!("{} chunk{}", parsed_count, plural(parsed_count)),
                    |declared| format!("{parsed_count} parsed  ·  {declared} declared"),
                );
                let details = if bin.chunks.is_empty() {
                    bin.warning.clone().unwrap_or_default()
                } else {
                    let addresses = bin
                        .chunks
                        .iter()
                        .map(|address| format_address(*address))
                        .collect::<Vec<_>>()
                        .join(" → ");
                    bin.warning.as_ref().map_or(addresses.clone(), |warning| {
                        format!("{addresses}  ·  {warning}")
                    })
                };
                self.push_inspectable_row(
                    name,
                    &format!("index {}", bin.index),
                    &format!("{}  ·  {metric}", bin.expected_size),
                    if bin.warning.is_some() {
                        "warning"
                    } else if bin.chunks.is_empty() {
                        "empty"
                    } else {
                        "occupied"
                    },
                    &details,
                    bin.chunks.first().copied(),
                );
            }
        }
        if self.rows.is_empty() {
            self.push_row(
                "Info",
                "",
                "",
                "empty",
                "No bins in this category are present in the target glibc layout",
            );
        }
        Ok(())
    }

    fn freelist_index(&mut self, arenas: &[Arena]) -> Result<HashMap<u64, Vec<String>>, String> {
        let mut index = HashMap::<u64, Vec<String>>::new();
        let mut total_nodes = 0_usize;
        for arena in arenas {
            for bin in self.collect_bins(arena, &mut total_nodes)? {
                let label = format!(
                    "{}[{}]",
                    match bin.kind {
                        BinKind::Tcache => "tcache",
                        BinKind::Fast => "fastbin",
                        BinKind::Unsorted => "unsorted",
                        BinKind::Small => "smallbin",
                        BinKind::Large => "largebin",
                    },
                    bin.index
                );
                for chunk in bin.chunks {
                    index.entry(chunk).or_default().push(label.clone());
                }
            }
        }
        Ok(index)
    }

    fn collect_bins(
        &mut self,
        arena: &Arena,
        total_nodes: &mut usize,
    ) -> Result<Vec<BinRecord>, String> {
        let mut bins = self.tcache_bins(arena, total_nodes)?;
        if let Some(offset) = self.layout.fastbins {
            for index in 0..self.layout.num_fastbins.min(7) {
                if !self.layout.fastbin_index_used(index) {
                    continue;
                }
                let head_address = arena
                    .address
                    .checked_add(offset)
                    .and_then(|address| {
                        address.checked_add(u64::try_from(index).ok()? * self.layout.pointer_size)
                    })
                    .ok_or_else(|| String::from("Fastbin address overflowed"))?;
                let head = self.reader.word(head_address)?;
                let (chunks, warning) = self.walk_singly(head, false, total_nodes)?;
                bins.push(BinRecord {
                    kind: BinKind::Fast,
                    index,
                    expected_size: format!("size 0x{:x}", self.layout.fastbin_size(index)),
                    declared_count: None,
                    chunks,
                    warning,
                });
            }
        }
        for index in 0_usize..127 {
            let pair = arena
                .address
                .checked_add(self.layout.bins)
                .and_then(|address| {
                    address.checked_add(u64::try_from(index).ok()? * self.layout.pointer_size * 2)
                })
                .ok_or_else(|| String::from("Bin address overflowed"))?;
            let head = pair
                .checked_sub(self.layout.pointer_size * 2)
                .ok_or_else(|| String::from("Bin head address underflowed"))?;
            let forward = self.reader.word(pair)?;
            let backward = self.reader.word(pair + self.layout.pointer_size)?;
            let (chunks, warning) = self.walk_double(forward, backward, head, total_nodes)?;
            let kind = if index == 0 {
                BinKind::Unsorted
            } else if index < 63 {
                BinKind::Small
            } else {
                BinKind::Large
            };
            bins.push(BinRecord {
                kind,
                index,
                expected_size: self.bin_expected_size(kind, index),
                declared_count: None,
                chunks,
                warning,
            });
        }
        Ok(bins)
    }

    fn tcache_bins(
        &mut self,
        arena: &Arena,
        total_nodes: &mut usize,
    ) -> Result<Vec<BinRecord>, String> {
        if !self.version.at_least(26) || !arena.main {
            return Ok(Vec::new());
        }
        let Some(tcache) = self.locate_tcache(arena)? else {
            return Ok(vec![BinRecord {
                kind: BinKind::Tcache,
                index: 0,
                expected_size: String::from("selected thread"),
                declared_count: None,
                chunks: Vec::new(),
                warning: Some(String::from(
                    "Selected-thread tcache is not initialized or could not be validated",
                )),
            }]);
        };
        let count = self.layout.tcache_bin_count();
        let count_size = self.layout.tcache_count_size();
        let entries = tcache
            .checked_add(u64::try_from(count).unwrap_or(0) * count_size)
            .ok_or_else(|| String::from("Tcache entries address overflowed"))?;
        let mut raw_counts = Vec::with_capacity(count);
        let mut heads = Vec::with_capacity(count);
        for index in 0..count {
            let index = u64::try_from(index).unwrap_or(0);
            let count_address = tcache + index * count_size;
            raw_counts.push(if count_size == 1 {
                usize::from(self.reader.u8(count_address)?)
            } else {
                usize::from(self.reader.u16(count_address)?)
            });
            heads.push(
                self.reader
                    .word(entries + index * self.layout.pointer_size)?,
            );
        }
        let fill_count = if self.version.at_least(42) {
            raw_counts.iter().copied().max().unwrap_or(0)
        } else {
            0
        };
        let mut bins = Vec::with_capacity(count);
        for index in 0..count {
            let declared = if self.version.at_least(42) {
                fill_count.saturating_sub(raw_counts[index])
            } else {
                raw_counts[index]
            };
            let (chunks, mut warning) = self.walk_singly(heads[index], true, total_nodes)?;
            if warning.is_none() && declared != chunks.len() {
                warning = Some(format!(
                    "metadata says {declared} entries, traversal found {}",
                    chunks.len()
                ));
            }
            bins.push(BinRecord {
                kind: BinKind::Tcache,
                index,
                expected_size: self.tcache_expected_size(index),
                declared_count: Some(declared),
                chunks,
                warning,
            });
        }
        Ok(bins)
    }

    fn locate_tcache(&mut self, arena: &Arena) -> Result<Option<u64>, String> {
        if let Some(candidate) = self.tcache_hint
            && candidate != 0
            && self.valid_tcache(candidate)
        {
            return Ok(Some(candidate));
        }
        if !self.version.at_least(42)
            && let Some(heap_base) = arena.heap_base
        {
            let mut first = [0_u8; 8];
            if self.reader.read(heap_base, &mut first).is_ok() {
                let candidate = heap_base + if first == [0; 8] { 0x10 } else { 0x8 };
                if self.valid_tcache(candidate) {
                    return Ok(Some(candidate));
                }
            }
        }
        let expected = self.layout.tcache_chunk_size();
        for tls in self.tls_bases.iter().copied() {
            let mut arena_slot = None;
            for index in 1_u64..=500 {
                let distance = index * self.layout.pointer_size;
                for slot in [tls.checked_sub(distance), tls.checked_add(distance)]
                    .into_iter()
                    .flatten()
                {
                    if self
                        .reader
                        .word(slot)
                        .is_ok_and(|value| value == arena.address)
                    {
                        arena_slot = Some(slot);
                        break;
                    }
                }
                if arena_slot.is_some() {
                    break;
                }
            }
            let Some(arena_slot) = arena_slot else {
                continue;
            };
            for index in 1_u64..=20 {
                let distance = index * self.layout.pointer_size;
                for slot in [
                    arena_slot.checked_sub(distance),
                    arena_slot.checked_add(distance),
                ]
                .into_iter()
                .flatten()
                {
                    let Ok(candidate) = self.reader.word(slot) else {
                        continue;
                    };
                    let size_address = candidate.checked_sub(self.layout.pointer_size);
                    if size_address
                        .and_then(|address| self.reader.word(address).ok())
                        .is_some_and(|raw| raw & !7 == expected)
                        && self.valid_tcache(candidate)
                    {
                        return Ok(Some(candidate));
                    }
                }
            }
        }
        Ok(None)
    }

    fn valid_tcache(&mut self, address: u64) -> bool {
        let size = match usize::try_from(self.layout.tcache_struct_size()) {
            Ok(size) => size,
            Err(_) => return false,
        };
        if !self.reader.readable(address, size) || !address.is_multiple_of(self.layout.pointer_size)
        {
            return false;
        }
        let bins = self.layout.tcache_bin_count();
        let count_bytes = self.layout.tcache_count_size() * u64::try_from(bins).unwrap_or(0);
        for index in 0..bins {
            let Ok(head) = self.reader.word(
                address
                    + count_bytes
                    + u64::try_from(index).unwrap_or(0) * self.layout.pointer_size,
            ) else {
                return false;
            };
            if head != 0
                && (!self.reader.readable(head, self.reader.pointer_size)
                    || head % self.layout.pointer_size != 0)
            {
                return false;
            }
        }
        true
    }

    fn walk_singly(
        &mut self,
        mut current: u64,
        user_pointer: bool,
        total_nodes: &mut usize,
    ) -> Result<(Vec<u64>, Option<String>), String> {
        let mut chunks = Vec::new();
        let mut seen = HashSet::new();
        while current != 0 {
            if chunks.len() == MAX_NODES_PER_BIN || *total_nodes == MAX_FREELIST_NODES {
                self.truncated = true;
                return Ok((chunks, Some(String::from("freelist traversal capped"))));
            }
            if !seen.insert(current) {
                return Ok((chunks, Some(format!("loop at {}", format_address(current)))));
            }
            let base = if user_pointer {
                match current.checked_sub(self.layout.pointer_size * 2) {
                    Some(base) => base,
                    None => return Ok((chunks, Some(String::from("pointer underflow")))),
                }
            } else {
                current
            };
            if !self.reader.readable(current, self.reader.pointer_size) {
                return Ok((
                    chunks,
                    Some(format!("unreadable node {}", format_address(current))),
                ));
            }
            chunks.push(base);
            *total_nodes += 1;
            let link_address = if user_pointer {
                current
            } else {
                match current.checked_add(self.layout.pointer_size * 2) {
                    Some(address) => address,
                    None => return Ok((chunks, Some(String::from("link address overflow")))),
                }
            };
            let raw = match self.reader.word(link_address) {
                Ok(raw) => raw,
                Err(error) => return Ok((chunks, Some(error))),
            };
            current = if self.version.at_least(32) {
                reveal_safe_link(raw, link_address)
            } else {
                raw
            };
        }
        Ok((chunks, None))
    }

    fn walk_double(
        &mut self,
        mut forward: u64,
        backward: u64,
        head: u64,
        total_nodes: &mut usize,
    ) -> Result<(Vec<u64>, Option<String>), String> {
        if forward == 0 && backward == 0 {
            return Ok((Vec::new(), Some(String::from("null bin head pointers"))));
        }
        if forward == head && backward == head {
            return Ok((Vec::new(), None));
        }
        let mut chunks = Vec::new();
        let mut seen = HashSet::new();
        let mut previous = head;
        while forward != head {
            if chunks.len() == MAX_NODES_PER_BIN || *total_nodes == MAX_FREELIST_NODES {
                self.truncated = true;
                return Ok((chunks, Some(String::from("bin traversal capped"))));
            }
            if forward == 0 || !seen.insert(forward) {
                return Ok((
                    chunks,
                    Some(format!("invalid or looping fd {}", format_address(forward))),
                ));
            }
            let user = match forward.checked_add(self.layout.pointer_size * 2) {
                Some(user) => user,
                None => return Ok((chunks, Some(String::from("bin node overflow")))),
            };
            let next = match self.reader.word(user) {
                Ok(next) => next,
                Err(error) => return Ok((chunks, Some(error))),
            };
            let back = match self.reader.word(user + self.layout.pointer_size) {
                Ok(back) => back,
                Err(error) => return Ok((chunks, Some(error))),
            };
            if back != previous {
                return Ok((
                    chunks,
                    Some(format!(
                        "bk mismatch at {} (expected {}, found {})",
                        format_address(forward),
                        format_address(previous),
                        format_address(back)
                    )),
                ));
            }
            chunks.push(forward);
            *total_nodes += 1;
            previous = forward;
            forward = next;
        }
        if previous != backward {
            return Ok((
                chunks,
                Some(String::from("bin tail does not match head.bk")),
            ));
        }
        Ok((chunks, None))
    }

    fn bin_expected_size(&self, kind: BinKind, index: usize) -> String {
        match kind {
            BinKind::Unsorted => String::from("mixed sizes"),
            BinKind::Small => format!(
                "size 0x{:x}",
                self.layout.min_chunk_size
                    + u64::try_from(index.saturating_sub(1)).unwrap_or(0)
                        * self.layout.malloc_alignment
            ),
            BinKind::Large => String::from("size range varies"),
            _ => String::new(),
        }
    }

    fn tcache_expected_size(&self, index: usize) -> String {
        if index < 64 {
            return format!(
                "size 0x{:x}",
                self.layout.min_chunk_size
                    + u64::try_from(index).unwrap_or(0) * self.layout.malloc_alignment
            );
        }
        let first = if self.layout.pointer_size == 8 {
            0x420
        } else if matches!(
            self.layout.architecture,
            TargetArchitecture::X86 | TargetArchitecture::RiscV32 | TargetArchitecture::PowerPc32
        ) {
            0x410
        } else {
            0x210
        };
        let shift = u32::try_from(index.saturating_sub(64)).unwrap_or(u32::MAX);
        let min = if shift == 0 {
            first
        } else {
            0x800_u64
                .checked_shl(shift.saturating_sub(1))
                .unwrap_or(u64::MAX)
        };
        let max = 0x800_u64.checked_shl(shift).unwrap_or(u64::MAX);
        format!("size 0x{min:x}–0x{max:x}")
    }

    fn push_row(&mut self, kind: &str, location: &str, metric: &str, state: &str, details: &str) {
        self.push_inspectable_row(kind, location, metric, state, details, None);
    }

    fn push_inspectable_row(
        &mut self,
        kind: &str,
        location: &str,
        metric: &str,
        state: &str,
        details: &str,
        inspect_address: Option<u64>,
    ) {
        if self.rows.len() == MAX_HEAP_INSPECTION_ROWS {
            self.truncated = true;
            return;
        }
        let mut row = heap_inspection_row(kind, location, metric, state, details);
        row.inspect_address = inspect_address;
        self.rows.push(row);
    }
}

fn native_heap_summary(
    query: NativeHeapQuery,
    version: GlibcVersion,
    rows: &[HeapInspectionRow],
    bytes_read: usize,
) -> String {
    let detail = match query {
        NativeHeapQuery::Arenas => {
            let arenas = rows.iter().filter(|row| row.kind == "Arena").count();
            let threads = rows.iter().filter(|row| row.state == "thread").count();
            format!(
                "{arenas} arena{}  ·  {threads} thread arena{}",
                plural(arenas),
                plural(threads)
            )
        }
        NativeHeapQuery::Arena => {
            let fields = rows.iter().filter(|row| row.kind == "Arena field").count();
            format!("{fields} arena field{}", plural(fields))
        }
        NativeHeapQuery::Top | NativeHeapQuery::Chunk(_) => {
            rows.iter().find(|row| row.kind == "Chunk").map_or_else(
                || String::from("no valid chunk"),
                |row| format!("{}  ·  {}  ·  {}", row.location, row.metric, row.state),
            )
        }
        NativeHeapQuery::Chunks | NativeHeapQuery::Parsed => {
            let chunks = rows.iter().filter(|row| row.kind == "Chunk").count();
            let used = rows.iter().filter(|row| row.state == "Used").count();
            let freed = rows.iter().filter(|row| row.state == "Freed").count();
            let bytes = rows
                .iter()
                .filter(|row| row.kind == "Chunk")
                .filter_map(|row| parse_hex_u64(&row.metric))
                .fold(0_u64, u64::saturating_add);
            format!(
                "{chunks} chunk{}  ·  {used} used  ·  {freed} free  ·  {} total",
                plural(chunks),
                crate::kernel::format_bytes(bytes)
            )
        }
        NativeHeapQuery::CompactBins
        | NativeHeapQuery::AllBins
        | NativeHeapQuery::TcacheBins
        | NativeHeapQuery::FastBins
        | NativeHeapQuery::UnsortedBin
        | NativeHeapQuery::SmallBins
        | NativeHeapQuery::LargeBins => {
            let bins = rows.iter().filter(|row| row.kind.contains("bin")).count();
            let occupied = rows.iter().filter(|row| row.state == "occupied").count();
            let warnings = rows.iter().filter(|row| row.state == "warning").count();
            format!(
                "{bins} bin{}  ·  {occupied} occupied  ·  {warnings} warning{}",
                plural(bins),
                plural(warnings)
            )
        }
    };
    format!(
        "glibc {version} ptmalloc  ·  {detail}  ·  {} read",
        crate::kernel::format_bytes(u64::try_from(bytes_read).unwrap_or(u64::MAX))
    )
}

fn parse_hex_u64(value: &str) -> Option<u64> {
    u64::from_str_radix(value.trim().strip_prefix("0x")?, 16).ok()
}

fn detect_glibc_version(root: &Path, mappings: &[ProcessMapping]) -> Result<GlibcVersion, String> {
    let mut paths = HashSet::new();
    for mapping in mappings {
        let path = mapping
            .path
            .strip_suffix(" (deleted)")
            .unwrap_or(&mapping.path);
        if !path.starts_with('/') || !path.contains("libc") || !paths.insert(path.to_owned()) {
            continue;
        }
        if let Some(version) = version_from_path(path) {
            return Ok(version);
        }
        if let Some(version) = version_from_mapping_file(root, mapping, path) {
            return Ok(version);
        }
    }
    // A statically linked glibc has no separate libc VMA. Only use the main
    // executable as a fallback when there was no libc mapping at all, and
    // still require glibc's embedded version marker before selecting a layout.
    if paths.is_empty() {
        let executable = mappings
            .iter()
            .find(|mapping| {
                mapping.permissions.contains('x')
                    && mapping.path.starts_with('/')
                    && !mapping.path.contains("ld-linux")
            })
            .map(|mapping| mapping.path.clone());
        if let Some(executable) = executable {
            let path = executable.strip_suffix(" (deleted)").unwrap_or(&executable);
            if let Some(mapping) = mappings.iter().find(|mapping| mapping.path == executable)
                && let Some(version) = version_from_mapping_file(root, mapping, path)
            {
                return Ok(version);
            }
        }
    }
    Err(String::from(
        "No supported glibc image/version was found in the inferior mappings, native ptmalloc decoding is not applicable",
    ))
}

fn version_from_mapping_file(
    root: &Path,
    mapping: &ProcessMapping,
    target_path: &str,
) -> Option<GlibcVersion> {
    let rooted = root.join("root").join(target_path.trim_start_matches('/'));
    if let Ok(Some(version)) = version_from_file(&rooted) {
        return Some(version);
    }
    let map_file = root
        .join("map_files")
        .join(format!("{:x}-{:x}", mapping.start, mapping.end));
    version_from_file(&map_file).ok().flatten()
}

fn version_from_file(path: &Path) -> std::io::Result<Option<GlibcVersion>> {
    const CHUNK_BYTES: usize = 64 * 1024;
    const OVERLAP_BYTES: usize = 32;
    let file = File::open(path)?;
    if file.metadata()?.len() > u64::try_from(MAX_LIBC_BYTES).unwrap_or(u64::MAX) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "mapped libc image exceeds the native heap reader's 64 MiB limit",
        ));
    }
    let mut file = file.take(u64::try_from(MAX_LIBC_BYTES).unwrap_or(u64::MAX));
    let mut buffer = vec![0_u8; CHUNK_BYTES + OVERLAP_BYTES];
    let mut retained = 0_usize;
    loop {
        let read = file.read(&mut buffer[retained..])?;
        if read == 0 {
            return Ok(version_from_bytes(&buffer[..retained]));
        }
        let available = retained + read;
        if let Some(version) = version_from_bytes(&buffer[..available]) {
            return Ok(Some(version));
        }
        retained = available.min(OVERLAP_BYTES);
        buffer.copy_within(available - retained..available, 0);
    }
}

fn version_from_path(path: &str) -> Option<GlibcVersion> {
    for marker in ["libc-", "libc_"] {
        if let Some((_, suffix)) = path.rsplit_once(marker)
            && let Some(version) = parse_version_prefix(suffix)
        {
            return Some(version);
        }
    }
    None
}

fn version_from_bytes(bytes: &[u8]) -> Option<GlibcVersion> {
    let marker = b"glibc ";
    bytes
        .windows(marker.len())
        .enumerate()
        .filter(|(_, window)| *window == marker)
        .find_map(|(index, _)| parse_version_prefix_bytes(&bytes[index + marker.len()..]))
}

fn parse_version_prefix(value: &str) -> Option<GlibcVersion> {
    parse_version_prefix_bytes(value.as_bytes())
}

fn parse_version_prefix_bytes(bytes: &[u8]) -> Option<GlibcVersion> {
    let major_end = bytes.iter().position(|byte| !byte.is_ascii_digit())?;
    if bytes.get(major_end) != Some(&b'.') {
        return None;
    }
    let minor_bytes = bytes.get(major_end + 1..)?;
    let minor_end = minor_bytes
        .iter()
        .position(|byte| !byte.is_ascii_digit())
        .unwrap_or(minor_bytes.len());
    if major_end == 0 || minor_end == 0 {
        return None;
    }
    Some(GlibcVersion {
        major: std::str::from_utf8(&bytes[..major_end])
            .ok()?
            .parse()
            .ok()?,
        minor: std::str::from_utf8(&minor_bytes[..minor_end])
            .ok()?
            .parse()
            .ok()?,
    })
}

const fn align_up(value: u64, alignment: u64) -> u64 {
    value.saturating_add(alignment.saturating_sub(1)) & !alignment.saturating_sub(1)
}

const fn request_to_chunk_size(
    request: u64,
    pointer_size: u64,
    alignment: u64,
    minimum: u64,
) -> u64 {
    let size = request
        .saturating_add(pointer_size)
        .saturating_add(alignment.saturating_sub(1))
        & !alignment.saturating_sub(1);
    if size < minimum { minimum } else { size }
}

fn format_address(address: u64) -> String {
    format!("0x{address:016x}")
}

const fn reveal_safe_link(stored: u64, storage_address: u64) -> u64 {
    stored ^ (storage_address >> 12)
}

#[cfg(test)]
mod tests {
    use std::{
        io::{BufRead, BufReader, Write},
        process::{Command, Stdio},
    };

    use super::*;

    #[test]
    fn parses_glibc_versions_without_accepting_noise() {
        assert_eq!(
            version_from_bytes(b"GNU C Library glibc 2.44 release"),
            Some(GlibcVersion {
                major: 2,
                minor: 44
            })
        );
        assert_eq!(
            version_from_path("/lib/libc-2.39.so"),
            Some(GlibcVersion {
                major: 2,
                minor: 39
            })
        );
        assert_eq!(version_from_bytes(b"glibc two.forty"), None);
    }

    #[test]
    fn malloc_state_layout_matches_known_x86_64_versions() {
        let old = GlibcLayout::new(
            GlibcVersion {
                major: 2,
                minor: 42,
            },
            TargetArchitecture::X86_64,
            64,
        )
        .unwrap();
        assert_eq!(old.fastbins, Some(16));
        assert_eq!(old.top, 96);
        let current = GlibcLayout::new(
            GlibcVersion {
                major: 2,
                minor: 44,
            },
            TargetArchitecture::X86_64,
            64,
        )
        .unwrap();
        assert_eq!(current.fastbins, None);
        assert_eq!(current.top, 8);
        assert_eq!(current.bins, 24);
        assert_eq!(current.next, 2072);
        assert_eq!(current.arena_size, 2112);
    }

    #[test]
    fn tcache_layout_tracks_glibc_transitions() {
        let v29 = GlibcLayout::new(
            GlibcVersion {
                major: 2,
                minor: 29,
            },
            TargetArchitecture::X86_64,
            64,
        )
        .unwrap();
        assert_eq!(v29.tcache_struct_size(), 576);
        let v44 = GlibcLayout::new(
            GlibcVersion {
                major: 2,
                minor: 44,
            },
            TargetArchitecture::X86_64,
            64,
        )
        .unwrap();
        assert_eq!(v44.tcache_struct_size(), 760);
        assert_eq!(v44.tcache_chunk_size(), 0x300);
    }

    #[test]
    fn x86_32_glibc_226_keeps_structure_and_chunk_alignment_distinct() {
        let layout = GlibcLayout::new(
            GlibcVersion {
                major: 2,
                minor: 26,
            },
            TargetArchitecture::X86,
            32,
        )
        .unwrap();
        assert_eq!(layout.malloc_alignment, 16);
        assert_eq!(layout.arena_alignment(), 8);
        assert_eq!(layout.num_fastbins, 11);
        assert_eq!(layout.fastbins, Some(8));
        assert_eq!(layout.top, 52);
        assert_eq!(
            (0..7)
                .filter(|index| layout.fastbin_index_used(*index))
                .collect::<Vec<_>>(),
            [0, 2, 4, 6]
        );
        assert_eq!(layout.fastbin_size(6), 0x40);
    }

    #[test]
    fn safe_linking_uses_the_link_field_address_and_decodes_null() {
        let storage = 0x0000_5555_5555_9010;
        assert_eq!(reveal_safe_link(storage >> 12, storage), 0);
        let next = 0x0000_5555_5555_9080;
        assert_eq!(reveal_safe_link(next ^ (storage >> 12), storage), next);
    }

    #[test]
    #[ignore = "requires GDB and target/debug-fixtures/c-misc-allocator-target"]
    fn reads_a_live_stripped_style_ptmalloc_heap_via_tls() {
        let fixture = Path::new("target/debug-fixtures/c-misc-allocator-target");
        assert!(fixture.exists(), "build the C debug fixtures first");
        let mut gdb = Command::new("gdb")
            .args(["-q", "-nx", "--interpreter=mi2"])
            .arg(fixture)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("start GDB");
        let debugger_pid = gdb.id();
        let mut input = gdb.stdin.take().expect("GDB stdin");
        let mut output = BufReader::new(gdb.stdout.take().expect("GDB stdout"));
        wait_for_mi(&mut output, "(gdb)");
        writeln!(input, "1-break-insert c_misc_allocator_target.c:85").unwrap();
        wait_for_mi(&mut output, "1^done");
        writeln!(input, "2-break-insert c_misc_allocator_target.c:96").unwrap();
        wait_for_mi(&mut output, "2^done");
        writeln!(input, "3-exec-run").unwrap();
        wait_for_mi(&mut output, "*stopped");
        writeln!(input, "4-list-thread-groups").unwrap();
        let groups = wait_for_mi(&mut output, "4^done");
        let pid = mi_decimal_field(&groups, "pid").expect("inferior PID");
        writeln!(input, "5-data-evaluate-expression \"(void *)$fs_base\"").unwrap();
        let tls = wait_for_mi(&mut output, "5^done");
        let tls = mi_hex_field(&tls, "value").expect("TLS base");

        let request = |query| NativeHeapReadRequest {
            pid,
            debugger_pid,
            architecture: TargetArchitecture::X86_64,
            endian: TargetEndian::Little,
            pointer_bits: 64,
            query,
            discovery: HeapDiscovery {
                tls_bases: vec![tls],
                ..HeapDiscovery::default()
            },
        };
        let arenas = inspect_native_heap(request(NativeHeapQuery::Arenas)).unwrap();
        assert!(arenas.rows.iter().any(|row| row.state == "main"));
        let chunks = inspect_native_heap(request(NativeHeapQuery::Chunks)).unwrap();
        assert!(chunks.rows.iter().filter(|row| row.kind == "Chunk").count() >= 3);
        let bins = inspect_native_heap(request(NativeHeapQuery::AllBins)).unwrap();
        assert!(bins.rows.iter().any(|row| row.kind.contains("bin")));

        writeln!(input, "6-exec-continue").unwrap();
        wait_for_mi(&mut output, "*stopped");
        let tcache = inspect_native_heap(request(NativeHeapQuery::TcacheBins)).unwrap();
        assert!(
            tcache
                .rows
                .iter()
                .any(|row| row.kind == "Tcache bin" && row.state == "occupied")
        );

        writeln!(input, "7-gdb-exit").unwrap();
        let _ = gdb.wait();
    }

    fn wait_for_mi(reader: &mut impl BufRead, needle: &str) -> String {
        let mut captured = String::new();
        loop {
            let mut line = String::new();
            assert_ne!(
                reader.read_line(&mut line).unwrap(),
                0,
                "GDB exited: {captured}"
            );
            captured.push_str(&line);
            if line.contains(needle) {
                return captured;
            }
        }
    }

    fn mi_decimal_field(record: &str, name: &str) -> Option<u32> {
        let marker = format!("{name}=\"");
        let value = record.split(&marker).nth(1)?.split('"').next()?;
        value.parse().ok()
    }

    fn mi_hex_field(record: &str, name: &str) -> Option<u64> {
        let marker = format!("{name}=\"0x");
        let value = record.split(&marker).nth(1)?.split(['"', ' ']).next()?;
        u64::from_str_radix(value, 16).ok()
    }
}
