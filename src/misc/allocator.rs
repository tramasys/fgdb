use std::path::Path;

use super::{ProcessMapping, mapping_containing};

mod inspect;
mod probe;

pub(crate) use inspect::{allocator_inspection_script, parse_allocator_inspection};
pub(crate) use probe::{allocator_probe_script, parse_allocator_probe};

/// A decoder capability, distinct from ownership of the process's C bindings.
/// Multiple runtimes can coexist with a language-specific global allocator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HeapBackend {
    Glibc,
    Musl,
    Jemalloc,
    Tcmalloc,
    Mimalloc,
}

impl HeapBackend {
    pub fn key(self) -> &'static str {
        match self {
            Self::Glibc => "glibc",
            Self::Musl => "musl",
            Self::Jemalloc => "jemalloc",
            Self::Tcmalloc => "tcmalloc",
            Self::Mimalloc => "mimalloc",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Glibc => "glibc / ptmalloc",
            Self::Musl => "musl malloc",
            Self::Jemalloc => "jemalloc",
            Self::Tcmalloc => "tcmalloc",
            Self::Mimalloc => "mimalloc",
        }
    }

    pub fn scope(self) -> &'static str {
        match self {
            Self::Glibc => {
                "Validated native arenas, chunks and bins. Tcache belongs to the selected thread."
            }
            Self::Musl => {
                "mallocng groups and size classes, or legacy bins. Requires allocator debug symbols."
            }
            Self::Jemalloc => {
                "Initialized arenas and page allocator counters. Requires allocator debug symbols."
            }
            Self::Tcmalloc => {
                "gperftools page heap and thread caches, or Google TCMalloc globals. Requires allocator debug symbols."
            }
            Self::Mimalloc => {
                "Thread heaps, page queues and block counts. Requires allocator debug symbols."
            }
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct AllocatorSnapshot {
    pub selected_backend: Option<HeapBackend>,
    pub available_backends: Vec<HeapBackend>,
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
    probe_function("gnu_get_libc_version"),
    AllocatorProbeSpec {
        name: "main_arena",
        expression: "&main_arena",
    },
    probe_function("__libc_malloc_impl"),
    probe_function("__malloc_allzerop"),
    probe_function("__bin_chunk"),
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
    probe_function("_rjem_malloc"),
    probe_function("_rjem_mallctl"),
    probe_function("_rjem_je_malloc"),
    probe_function("_rjem_je_mallctl"),
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
    AllocatorProbeSpec {
        name: "__rustc::__rust_alloc",
        expression: "&'__rustc::__rust_alloc'",
    },
    AllocatorProbeSpec {
        name: "__rustc::__rg_alloc",
        expression: "&'__rustc::__rg_alloc'",
    },
    AllocatorProbeSpec {
        name: "__rustc::__rdl_alloc",
        expression: "&'__rustc::__rdl_alloc'",
    },
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

impl AllocatorRegion {
    pub fn size(&self) -> u64 {
        self.end.saturating_sub(self.start)
    }
}

pub(super) fn allocator_snapshot(
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

        if symbol.indirect || mapping.is_none() {
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
            malloc_mapping.map_or_else(
                || "an unknown mapping".to_owned(),
                |mapping| mapping_display_name(&mapping.path),
            ),
            free_mapping.map_or_else(
                || "an unknown mapping".to_owned(),
                |mapping| mapping_display_name(&mapping.path),
            )
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
            "{} optional GDB symbol probes were skipped or could not be queued",
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
        } else if writable_private
            && (mapping.path.is_empty() || mapping.path.starts_with("[anon:"))
        {
            anonymous_writable_bytes =
                anonymous_writable_bytes.saturating_add(mapping.end.saturating_sub(mapping.start));

            Some(String::from(if mapping.path == "[anon:mimalloc]" {
                "mimalloc-tagged memory"
            } else {
                "anonymous writable (possible arena)"
            }))
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
        selected_backend: selected_family.and_then(AllocatorFamily::backend),
        available_backends: runtime_families
            .iter()
            .filter_map(|family| family.backend())
            .collect(),
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
    fn backend(self) -> Option<HeapBackend> {
        match self {
            Self::Glibc => Some(HeapBackend::Glibc),
            Self::Musl => Some(HeapBackend::Musl),
            Self::Jemalloc => Some(HeapBackend::Jemalloc),
            Self::Tcmalloc => Some(HeapBackend::Tcmalloc),
            Self::Mimalloc => Some(HeapBackend::Mimalloc),
            _ => None,
        }
    }

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
        // __libc_malloc also exists in musl and is not an identifying marker.
        "gnu_get_libc_version" | "main_arena" => Some(AllocatorFamily::Glibc),
        "__malloc_context" | "__libc_malloc_impl" | "__malloc_allzerop" | "__bin_chunk" => {
            Some(AllocatorFamily::Musl)
        }
        "__uClibc_main" => Some(AllocatorFamily::Uclibc),
        "android_mallopt" => Some(AllocatorFamily::Bionic),
        "mallctl" | "malloc_stats_print" | "je_malloc" | "je_mallctl" | "_rjem_malloc"
        | "_rjem_mallctl" | "_rjem_je_malloc" | "_rjem_je_mallctl" => {
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
        "__rust_alloc" | "__rustc::__rust_alloc" => Some("Rust allocation shim"),
        "__rg_alloc" | "__rustc::__rg_alloc" => Some("Rust custom global allocator"),
        "__rdl_alloc" | "__rustc::__rdl_alloc" => Some("Rust standard-library allocator"),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn separates_rust_backends_from_c_bindings_and_shared_libc_symbols() {
        let maps = [
            allocator_test_mapping(0x1000, 0x2000, "/opt/bin/rust-service"),
            allocator_test_mapping(0x3000, 0x4000, "/usr/lib/libc.so.6"),
        ];

        let probe = allocator_test_probe(&[
            allocator_test_symbol("malloc", 0x3100),
            allocator_test_symbol("free", 0x3200),
            allocator_test_symbol("_rjem_mallctl", 0x1100),
            allocator_test_symbol("__rustc::__rg_alloc", 0x1200),
        ]);

        let snapshot = allocator_snapshot(&maps, &probe);
        assert_eq!(snapshot.selected_backend, Some(HeapBackend::Glibc));
        assert!(snapshot.available_backends.contains(&HeapBackend::Jemalloc));
        assert_eq!(
            snapshot.allocation_frontends,
            ["Rust custom global allocator"]
        );

        let probe = allocator_test_probe(&[
            allocator_test_symbol("malloc", 0x1100),
            allocator_test_symbol("free", 0x1200),
            allocator_test_symbol("__libc_malloc", 0x1300),
            allocator_test_symbol("__libc_malloc_impl", 0x1400),
        ]);

        let snapshot = allocator_snapshot(&maps[..1], &probe);
        assert_eq!(snapshot.selected_backend, Some(HeapBackend::Musl));
        assert_eq!(snapshot.available_backends, [HeapBackend::Musl]);
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
                ProcessMapping {
                    start: 0xb000,
                    end: 0xc000,
                    permissions: String::from("rw-p"),
                    path: String::from("[anon:mimalloc]"),
                },
            ],
            &AllocatorProbe::default(),
        );

        assert_eq!(snapshot.implementation, "jemalloc");
        assert_eq!(snapshot.detection_basis, "loaded module evidence");
        assert_eq!(snapshot.heap_bytes, 0x2000);
        assert_eq!(snapshot.anonymous_writable_bytes, 0x5000);
        assert_eq!(snapshot.regions.len(), 4);
        assert_eq!(snapshot.regions[3].role, "mimalloc-tagged memory");
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
}
