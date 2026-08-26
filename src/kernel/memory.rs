use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs,
    fs::File,
    os::unix::fs::FileExt,
    path::Path,
};

use super::*;

pub(super) fn populate_mappings(snapshot: &mut KernelSnapshot, root: &Path) {
    match fs::read_to_string(root.join("smaps")) {
        Ok(smaps) => snapshot.mappings = parse_smaps(&smaps),
        Err(error) => snapshot.warnings.push(format!(
            "Detailed mappings unavailable: {error}. Kernel ptrace/procfs permissions may deny access."
        )),
    }
}

pub(super) fn populate_memory(
    snapshot: &mut KernelSnapshot,
    root: &Path,
    status: &HashMap<String, String>,
) {
    if snapshot.mappings.is_empty() {
        for (source, label) in [
            ("VmSize", "Address space (VSS)"),
            ("VmRSS", "Resident memory (RSS)"),
            ("RssAnon", "Anonymous RSS"),
            ("RssFile", "File RSS"),
            ("RssShmem", "Shared-memory RSS"),
            ("VmSwap", "Swap"),
        ] {
            push_memory_status(&mut snapshot.memory, status, source, label);
        }
        snapshot.metrics.rss = status
            .get("VmRSS")
            .and_then(|value| parse_proc_quantity(value))
            .unwrap_or(0);
        snapshot.metrics.virtual_bytes = status
            .get("VmSize")
            .and_then(|value| parse_proc_quantity(value))
            .unwrap_or(0);
        snapshot.metrics.swap = status
            .get("VmSwap")
            .and_then(|value| parse_proc_quantity(value))
            .unwrap_or(0);
    } else {
        let executable = fs::read_link(root.join("exe")).ok();
        let mut accounting = summarize_mappings(&snapshot.mappings, executable.as_deref());
        accounting.page_tables = status
            .get("VmPTE")
            .and_then(|value| parse_proc_quantity(value))
            .unwrap_or(0);
        accounting.pinned = status
            .get("VmPin")
            .and_then(|value| parse_proc_quantity(value))
            .unwrap_or(0);
        if let Ok(statm) = fs::read_to_string(root.join("statm"))
            && let Some((virtual_bytes, rss)) = parse_statm(&statm, accounting.page_size)
        {
            accounting.statm_virtual_bytes = Some(virtual_bytes);
            accounting.statm_rss = Some(rss);
        }
        snapshot.metrics.virtual_bytes = accounting.virtual_bytes;
        snapshot.metrics.rss = accounting.rss;
        snapshot.metrics.pss = accounting.pss;
        snapshot.metrics.private_rss = accounting.unique_rss();
        snapshot.metrics.shared_rss = accounting.shared_rss();
        snapshot.metrics.swap = accounting.swap;
        for (label, value) in [
            ("Address space (VSS)", accounting.virtual_bytes),
            ("Resident memory (RSS)", accounting.rss),
            (
                "Process-private resident memory (USS)",
                accounting.unique_rss(),
            ),
            ("Private clean", accounting.private_clean),
            ("Private dirty", accounting.private_dirty),
            ("Shared resident memory", accounting.shared_rss()),
            ("Proportional resident memory (PSS)", accounting.pss),
            ("Swap", accounting.swap),
            ("Anonymous memory", accounting.anonymous),
            ("Referenced memory", accounting.referenced),
            ("Lazy-free memory", accounting.lazy_free),
            ("Locked memory", accounting.locked),
            ("Page tables", accounting.page_tables),
            ("Pinned memory", accounting.pinned),
            ("Anonymous huge pages", accounting.anon_huge_pages),
        ] {
            snapshot.memory.push(fact(label, format_bytes(value)));
        }
        for (label, numerator, denominator) in [
            (
                "Resident share of address space",
                accounting.rss,
                accounting.virtual_bytes,
            ),
            (
                "Process-private share of RSS",
                accounting.unique_rss(),
                accounting.rss,
            ),
            (
                "Dirty share of private RSS",
                accounting.private_dirty,
                accounting.unique_rss(),
            ),
            (
                "Referenced share of RSS",
                accounting.referenced,
                accounting.rss,
            ),
        ] {
            if denominator > 0 {
                snapshot.memory.push(fact(
                    label,
                    format!("{:.1}%", numerator as f64 / denominator as f64 * 100.0),
                ));
            }
        }
        let resident_mappings = snapshot
            .mappings
            .iter()
            .filter(|mapping| mapping.rss > 0)
            .count();
        snapshot.memory.push(fact(
            "Resident mappings",
            format!("{resident_mappings} of {} VMAs", snapshot.mappings.len()),
        ));
        let thp_eligible = snapshot
            .mappings
            .iter()
            .filter(|mapping| mapping.thp_eligible)
            .count();
        snapshot.advanced.push(fact(
            "Huge-page coverage",
            format!(
                "{} backed · {thp_eligible} THP-eligible VMAs",
                format_bytes(accounting.huge_bytes())
            ),
        ));
        snapshot.memory_accounting = Some(accounting);
    }
    for (source, label) in [("VmPeak", "Peak virtual"), ("VmHWM", "Peak resident")] {
        push_memory_status(&mut snapshot.memory, status, source, label);
    }
}

fn parse_statm(input: &str, page_size: u64) -> Option<(u64, u64)> {
    let mut fields = input.split_whitespace();
    let virtual_pages = fields.next()?.parse::<u64>().ok()?;
    let resident_pages = fields.next()?.parse::<u64>().ok()?;
    Some((
        virtual_pages.checked_mul(page_size)?,
        resident_pages.checked_mul(page_size)?,
    ))
}

fn summarize_mappings(
    mappings: &[KernelMapping],
    executable: Option<&Path>,
) -> KernelMemoryAccounting {
    let page_size = mappings
        .iter()
        .filter_map(|mapping| (mapping.mmu_page_size > 0).then_some(mapping.mmu_page_size))
        .min()
        .unwrap_or(4096);
    let mut accounting = KernelMemoryAccounting {
        page_size,
        ..KernelMemoryAccounting::default()
    };
    let mut categories = BTreeMap::<&'static str, (KernelMemoryCategory, BTreeSet<String>)>::new();
    for mapping in mappings {
        accounting.virtual_bytes = accounting.virtual_bytes.saturating_add(mapping.size);
        accounting.rss = accounting.rss.saturating_add(mapping.rss);
        accounting.pss = accounting.pss.saturating_add(mapping.pss);
        accounting.private_clean = accounting
            .private_clean
            .saturating_add(mapping.private_clean);
        accounting.private_dirty = accounting
            .private_dirty
            .saturating_add(mapping.private_dirty);
        accounting.shared_clean = accounting.shared_clean.saturating_add(mapping.shared_clean);
        accounting.shared_dirty = accounting.shared_dirty.saturating_add(mapping.shared_dirty);
        accounting.swap = accounting.swap.saturating_add(mapping.swap);
        accounting.anon_huge_pages = accounting
            .anon_huge_pages
            .saturating_add(mapping.anon_huge_pages);
        accounting.anonymous = accounting.anonymous.saturating_add(mapping.anonymous);
        accounting.referenced = accounting.referenced.saturating_add(mapping.referenced);
        accounting.lazy_free = accounting.lazy_free.saturating_add(mapping.lazy_free);
        accounting.locked = accounting.locked.saturating_add(mapping.locked);
        accounting.ksm = accounting.ksm.saturating_add(mapping.ksm);
        accounting.file_pmd_mapped = accounting
            .file_pmd_mapped
            .saturating_add(mapping.file_pmd_mapped);
        accounting.shmem_pmd_mapped = accounting
            .shmem_pmd_mapped
            .saturating_add(mapping.shmem_pmd_mapped);
        accounting.shared_hugetlb = accounting
            .shared_hugetlb
            .saturating_add(mapping.shared_hugetlb);
        accounting.private_hugetlb = accounting
            .private_hugetlb
            .saturating_add(mapping.private_hugetlb);

        let category = mapping_category(mapping, executable);
        let (summary, paths) = categories.entry(category).or_insert_with(|| {
            (
                KernelMemoryCategory {
                    category: category.to_owned(),
                    ..KernelMemoryCategory::default()
                },
                BTreeSet::new(),
            )
        });
        summary.mappings += 1;
        summary.virtual_bytes = summary.virtual_bytes.saturating_add(mapping.size);
        summary.rss = summary.rss.saturating_add(mapping.rss);
        summary.pss = summary.pss.saturating_add(mapping.pss);
        summary.private_clean = summary.private_clean.saturating_add(mapping.private_clean);
        summary.private_dirty = summary.private_dirty.saturating_add(mapping.private_dirty);
        summary.shared_clean = summary.shared_clean.saturating_add(mapping.shared_clean);
        summary.shared_dirty = summary.shared_dirty.saturating_add(mapping.shared_dirty);
        summary.swap = summary.swap.saturating_add(mapping.swap);
        paths.insert(
            mapping
                .path
                .clone()
                .unwrap_or_else(|| String::from("anonymous mappings")),
        );
    }
    let mut categories = categories
        .into_values()
        .map(|(mut summary, paths)| {
            summary.details = summarize_paths(&paths);
            summary
        })
        .collect::<Vec<_>>();
    categories.sort_by_key(|category| memory_category_order(&category.category));
    accounting.categories = categories;
    accounting
}

fn mapping_category(mapping: &KernelMapping, executable: Option<&Path>) -> &'static str {
    let Some(path) = mapping.path.as_deref() else {
        return "Anonymous / JIT";
    };
    let path_without_deleted = path.strip_suffix(" (deleted)").unwrap_or(path);
    if executable
        .and_then(Path::to_str)
        .map(|path| path.strip_suffix(" (deleted)").unwrap_or(path))
        == Some(path_without_deleted)
    {
        return "Main executable";
    }
    if path == "[heap]" {
        return "Heap";
    }
    if path.starts_with("[stack") {
        return "Stacks";
    }
    if path.starts_with("[anon:") {
        return "Anonymous / JIT";
    }
    if path.starts_with("/memfd:")
        || path.starts_with("memfd:")
        || path.starts_with("/dev/shm/")
        || path.starts_with("/SYSV")
    {
        return "Shared memory / memfd";
    }
    if path.starts_with('[') {
        return "Kernel / special";
    }
    let file_name = Path::new(path_without_deleted)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(path_without_deleted);
    if file_name.contains(".so") || file_name.starts_with("ld-linux") {
        "Shared libraries"
    } else {
        "Mapped files"
    }
}

fn summarize_paths(paths: &BTreeSet<String>) -> String {
    const SHOWN_PATHS: usize = 3;
    let mut summary = paths
        .iter()
        .take(SHOWN_PATHS)
        .cloned()
        .collect::<Vec<_>>()
        .join(" · ");
    if paths.len() > SHOWN_PATHS {
        summary.push_str(&format!(" · +{} more", paths.len() - SHOWN_PATHS));
    }
    summary
}

fn memory_category_order(category: &str) -> u8 {
    match category {
        "Main executable" => 0,
        "Heap" => 1,
        "Stacks" => 2,
        "Anonymous / JIT" => 3,
        "Shared memory / memfd" => 4,
        "Shared libraries" => 5,
        "Mapped files" => 6,
        "Kernel / special" => 7,
        _ => 8,
    }
}

pub(super) fn populate_numa(snapshot: &mut KernelSnapshot, root: &Path) {
    let Ok(input) = fs::read_to_string(root.join("numa_maps")) else {
        return;
    };
    let parsed = parse_numa_maps(&input);
    let mut totals = HashMap::<u32, u64>::new();
    for mapping in &mut snapshot.mappings {
        let Some((policy, nodes, details)) = parsed.get(&mapping.start) else {
            continue;
        };
        mapping.numa_policy.clone_from(policy);
        mapping.numa_nodes = nodes
            .iter()
            .map(|(node, pages)| format!("N{node}={pages}"))
            .collect::<Vec<_>>()
            .join(" ");
        for (node, pages) in nodes {
            *totals.entry(*node).or_default() += pages;
        }
        if !details.is_empty() {
            mapping.numa_nodes.push_str(&format!(" · {details}"));
        }
    }
    if totals.is_empty() {
        snapshot
            .advanced
            .push(fact("NUMA residency", "Unavailable or no resident pages"));
    } else {
        let mut totals = totals.into_iter().collect::<Vec<_>>();
        totals.sort_unstable_by_key(|(node, _)| *node);
        snapshot.advanced.push(fact(
            "NUMA residency",
            totals
                .into_iter()
                .map(|(node, pages)| format!("N{node} {pages} pages"))
                .collect::<Vec<_>>()
                .join(" · "),
        ));
    }
}

pub(super) fn populate_page_samples(snapshot: &mut KernelSnapshot, root: &Path) {
    const MAX_SAMPLED_MAPPINGS: usize = 1024;
    let Ok(file) = File::open(root.join("pagemap")) else {
        snapshot.advanced.push(fact(
            "Page-table inspection",
            "Unavailable (procfs permissions or kernel configuration)",
        ));
        return;
    };
    let page_size = snapshot
        .mappings
        .iter()
        .find_map(|mapping| (mapping.mmu_page_size > 0).then_some(mapping.mmu_page_size))
        .unwrap_or(4096);
    let proc_root = root.parent().unwrap_or(root);
    let page_flags = File::open(proc_root.join("kpageflags")).ok();
    let page_counts = File::open(proc_root.join("kpagecount")).ok();
    let mut disclosed_pfn = false;
    let mut sampled = 0_usize;
    let mut probed = 0_usize;
    let mut skipped_by_budget = 0_usize;
    for mapping in &mut snapshot.mappings {
        if mapping.size == 0 || mapping.rss == 0 {
            continue;
        }
        if probed >= MAX_SAMPLED_MAPPINGS {
            mapping.page_sample = String::from("not sampled · snapshot budget reached");
            skipped_by_budget += 1;
            continue;
        }
        probed += 1;
        if let Some(sample) = sample_mapping(
            &file,
            page_flags.as_ref(),
            page_counts.as_ref(),
            mapping.start,
            mapping.end,
            page_size,
        ) {
            disclosed_pfn |= sample.contains("PFN 0x");
            mapping.page_sample = sample;
            sampled += 1;
        }
    }
    let access = if disclosed_pfn {
        "Available; per-mapping samples include PFN and physical address"
    } else if sampled > 0 {
        "Metadata available; PFNs are masked by kernel permissions"
    } else {
        "Readable, but no mapping sample was available"
    };
    snapshot
        .advanced
        .push(fact("Page-table inspection", access));
    snapshot.advanced.push(fact(
        "Sampling scope",
        if skipped_by_budget == 0 {
            String::from("Up to four evenly spaced probes per resident VMA")
        } else {
            format!(
                "Up to four probes per resident VMA · {skipped_by_budget} VMAs skipped after the {MAX_SAMPLED_MAPPINGS}-mapping responsiveness budget"
            )
        },
    ));
}

fn sample_mapping(
    file: &File,
    page_flags: Option<&File>,
    page_counts: Option<&File>,
    start: u64,
    end: u64,
    page_size: u64,
) -> Option<String> {
    let pages = end.saturating_sub(start).div_ceil(page_size);
    for index in sample_page_indices(pages) {
        let address = start.saturating_add(index.saturating_mul(page_size));
        let offset = (address / page_size).checked_mul(8)?;
        let mut bytes = [0_u8; 8];
        if file.read_at(&mut bytes, offset).ok()? != bytes.len() {
            return None;
        }
        let entry = u64::from_ne_bytes(bytes);
        let present = entry & (1_u64 << 63) != 0;
        let swapped = entry & (1_u64 << 62) != 0;
        if !present && !swapped {
            continue;
        }
        let mut fields = vec![
            format!("0x{address:016x}"),
            if present {
                String::from("resident")
            } else {
                String::from("swapped")
            },
        ];
        let payload = entry & ((1_u64 << 55) - 1);
        if present && payload != 0 {
            fields.push(format!("PFN 0x{payload:x}"));
            fields.push(format!(
                "physical 0x{:x}",
                payload.saturating_mul(page_size)
            ));
            if let Some(count) = page_counts.and_then(|file| read_page_word(file, payload)) {
                fields.push(format!("mapcount {count}"));
            }
            if let Some(flags) = page_flags.and_then(|file| read_page_word(file, payload)) {
                let flags = decode_page_flags(flags);
                if !flags.is_empty() {
                    fields.push(flags);
                }
            }
        }
        if entry & (1_u64 << 61) != 0 {
            fields.push(String::from("file/shared"));
        }
        if entry & (1_u64 << 57) != 0 {
            fields.push(String::from("uffd-wp"));
        }
        if entry & (1_u64 << 58) != 0 {
            fields.push(String::from("guard"));
        }
        if entry & (1_u64 << 56) != 0 {
            fields.push(String::from("exclusive"));
        }
        if entry & (1_u64 << 55) != 0 {
            fields.push(String::from("soft-dirty"));
        }
        return Some(fields.join(" · "));
    }
    Some(String::from("sampled pages not resident"))
}

fn sample_page_indices(pages: u64) -> Vec<u64> {
    if pages == 0 {
        return Vec::new();
    }
    let mut indices = vec![0, pages / 3, pages.saturating_mul(2) / 3, pages - 1];
    indices.sort_unstable();
    indices.dedup();
    indices
}

fn read_page_word(file: &File, page_frame: u64) -> Option<u64> {
    let mut bytes = [0_u8; 8];
    let offset = page_frame.checked_mul(8)?;
    (file.read_at(&mut bytes, offset).ok()? == bytes.len()).then(|| u64::from_ne_bytes(bytes))
}

fn decode_page_flags(mask: u64) -> String {
    const FLAGS: [&str; 27] = [
        "locked",
        "error",
        "referenced",
        "uptodate",
        "dirty",
        "lru",
        "active",
        "slab",
        "writeback",
        "reclaim",
        "buddy",
        "mmap",
        "anonymous",
        "swap-cache",
        "swap-backed",
        "compound-head",
        "compound-tail",
        "huge",
        "unevictable",
        "hwpoison",
        "no-page",
        "ksm",
        "thp",
        "offline",
        "zero-page",
        "idle",
        "page-table",
    ];
    let names = FLAGS
        .iter()
        .enumerate()
        .filter(|(bit, _)| mask & (1_u64 << bit) != 0)
        .map(|(_, name)| *name)
        .collect::<Vec<_>>();
    if names.is_empty() {
        String::new()
    } else {
        format!("flags {}", names.join(","))
    }
}

fn parse_smaps(input: &str) -> Vec<KernelMapping> {
    let mut mappings = Vec::new();
    let mut current = None;
    for line in input.lines() {
        if let Some(mapping) = parse_mapping_header(line) {
            if let Some(previous) = current.replace(mapping) {
                mappings.push(previous);
            }
            continue;
        }
        let Some(mapping) = current.as_mut() else {
            continue;
        };
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        if key == "VmFlags" {
            mapping.vm_flags = value.trim().to_owned();
            continue;
        }
        if key == "THPeligible" {
            mapping.thp_eligible = value.trim() == "1";
            continue;
        }
        let bytes = parse_proc_quantity(value).unwrap_or(0);
        match key {
            "Size" => mapping.size = bytes,
            "Rss" => mapping.rss = bytes,
            "Pss" => mapping.pss = bytes,
            "Pss_Dirty" => mapping.pss_dirty = bytes,
            "Shared_Clean" => mapping.shared_clean = bytes,
            "Shared_Dirty" => mapping.shared_dirty = bytes,
            "Private_Clean" => mapping.private_clean = bytes,
            "Private_Dirty" => mapping.private_dirty = bytes,
            "Anonymous" => mapping.anonymous = bytes,
            "Referenced" => mapping.referenced = bytes,
            "KSM" => mapping.ksm = bytes,
            "LazyFree" => mapping.lazy_free = bytes,
            "Swap" => mapping.swap = bytes,
            "SwapPss" => mapping.swap_pss = bytes,
            "Locked" => mapping.locked = bytes,
            "AnonHugePages" => mapping.anon_huge_pages = bytes,
            "FilePmdMapped" => mapping.file_pmd_mapped = bytes,
            "ShmemPmdMapped" => mapping.shmem_pmd_mapped = bytes,
            "Shared_Hugetlb" => mapping.shared_hugetlb = bytes,
            "Private_Hugetlb" => mapping.private_hugetlb = bytes,
            "KernelPageSize" => mapping.kernel_page_size = bytes,
            "MMUPageSize" => mapping.mmu_page_size = bytes,
            _ => {}
        }
    }
    if let Some(mapping) = current {
        mappings.push(mapping);
    }
    mappings
}

fn parse_mapping_header(line: &str) -> Option<KernelMapping> {
    let mut fields = line.split_whitespace();
    let (start, end) = fields.next()?.split_once('-')?;
    let start = u64::from_str_radix(start, 16).ok()?;
    let end = u64::from_str_radix(end, 16).ok()?;
    let permissions = fields.next()?.to_owned();
    let offset = u64::from_str_radix(fields.next()?, 16).ok()?;
    let device = fields.next()?.to_owned();
    let inode = fields.next()?.parse().ok()?;
    let path = {
        let path = fields.collect::<Vec<_>>().join(" ");
        (!path.is_empty()).then_some(path)
    };
    Some(KernelMapping {
        start,
        end,
        permissions,
        offset,
        device,
        inode,
        path,
        ..KernelMapping::default()
    })
}

type NumaMap = HashMap<u64, (String, Vec<(u32, u64)>, String)>;

fn parse_numa_maps(input: &str) -> NumaMap {
    input
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let address = u64::from_str_radix(fields.next()?, 16).ok()?;
            let policy = fields.next().unwrap_or("default").to_owned();
            let mut nodes = Vec::new();
            let mut details = Vec::new();
            for field in fields {
                if let Some((node, pages)) = field.split_once('=')
                    && let Some(node) = node.strip_prefix('N').and_then(|node| node.parse().ok())
                    && let Ok(pages) = pages.parse()
                {
                    nodes.push((node, pages));
                } else if field.starts_with("anon=")
                    || field.starts_with("dirty=")
                    || field.starts_with("kernelpagesize_kB=")
                {
                    details.push(field);
                }
            }
            Some((address, (policy, nodes, details.join(" "))))
        })
        .collect()
}

fn push_memory_status(
    destination: &mut Vec<KernelFact>,
    source: &HashMap<String, String>,
    key: &str,
    label: &str,
) {
    if let Some(value) = source.get(key) {
        destination.push(fact(
            label,
            parse_proc_quantity(value)
                .map(format_bytes)
                .unwrap_or_else(|| value.clone()),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_structured_smaps_and_numa_data() {
        let mappings = parse_smaps(
            "00400000-00402000 r-xp 00000000 08:01 42 /tmp/a b\n\
             Size: 8 kB\nRss: 6 kB\nPss: 3 kB\nPss_Dirty: 2 kB\nPrivate_Dirty: 2 kB\n\
             Anonymous: 4 kB\nReferenced: 6 kB\nLazyFree: 1 kB\nKSM: 1 kB\n\
             FilePmdMapped: 2 kB\nKernelPageSize: 4 kB\nMMUPageSize: 4 kB\n\
             THPeligible: 1\nVmFlags: rd ex\n",
        );
        assert_eq!(mappings[0].path.as_deref(), Some("/tmp/a b"));
        assert_eq!(mappings[0].device, "08:01");
        assert_eq!(mappings[0].inode, 42);
        assert_eq!(mappings[0].size, 8192);
        assert_eq!(mappings[0].pss_dirty, 2048);
        assert_eq!(mappings[0].anonymous, 4096);
        assert_eq!(mappings[0].referenced, 6144);
        assert_eq!(mappings[0].lazy_free, 1024);
        assert_eq!(mappings[0].huge_bytes(), 2048);
        assert_eq!(mappings[0].kernel_page_size, 4096);
        assert!(mappings[0].thp_eligible);
        let numa = parse_numa_maps(
            "00400000 interleave:0-1 file=/tmp/a N0=3 N1=2 dirty=1 kernelpagesize_kB=4\n",
        );
        let (policy, nodes, _) = numa.get(&0x0040_0000).unwrap();
        assert_eq!(policy, "interleave:0-1");
        assert_eq!(nodes, &vec![(0, 3), (1, 2)]);
    }

    #[test]
    fn attributes_unique_and_shared_pages_by_mapping_type() {
        let mappings = parse_smaps(
            "00400000-00402000 r-xp 00000000 08:01 42 /tmp/fgdb-target\n\
             Size: 8 kB\nRss: 8 kB\nPss: 8 kB\nPrivate_Clean: 4 kB\n\
             Private_Dirty: 4 kB\nMMUPageSize: 4 kB\n\
             70000000-70003000 r-xp 00000000 08:01 43 /usr/lib/libfixture.so.1\n\
             Size: 12 kB\nRss: 8 kB\nPss: 4 kB\nShared_Clean: 8 kB\n\
             MMUPageSize: 4 kB\n\
             71000000-71004000 rw-p 00000000 00:00 0\n\
             Size: 16 kB\nRss: 4 kB\nPss: 4 kB\nPrivate_Dirty: 4 kB\n\
             Swap: 4 kB\nMMUPageSize: 4 kB\n",
        );
        let accounting = summarize_mappings(&mappings, Some(Path::new("/tmp/fgdb-target")));

        assert_eq!(accounting.page_size, 4096);
        assert_eq!(accounting.virtual_bytes, 36 * 1024);
        assert_eq!(accounting.rss, 20 * 1024);
        assert_eq!(accounting.unique_rss(), 12 * 1024);
        assert_eq!(accounting.shared_rss(), 8 * 1024);
        assert_eq!(accounting.swap, 4 * 1024);
        assert_eq!(
            accounting
                .categories
                .iter()
                .map(|category| category.category.as_str())
                .collect::<Vec<_>>(),
            vec!["Main executable", "Anonymous / JIT", "Shared libraries"]
        );
        assert_eq!(accounting.categories[0].unique_rss(), 8 * 1024);
        assert_eq!(accounting.categories[1].swap, 4 * 1024);
        assert_eq!(accounting.categories[2].shared_rss(), 8 * 1024);
    }

    #[test]
    fn converts_statm_page_counters_to_bytes() {
        assert_eq!(
            parse_statm("751 454 427 1 0 52 0\n", 4096),
            Some((3_076_096, 1_859_584))
        );
        assert_eq!(parse_statm("invalid", 4096), None);
        assert_eq!(sample_page_indices(1), vec![0]);
        assert_eq!(sample_page_indices(12), vec![0, 4, 8, 11]);
    }
}
