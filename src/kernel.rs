use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

mod elf;
mod memory;
mod process;
mod resources;
mod startup;

pub(crate) use startup::{
    ProcessArgument, ProcessEnvironment, ProcessStartupSnapshot, read_process_startup,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct KernelFact {
    pub label: String,
    pub value: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct KernelSnapshot {
    pub pid: u32,
    pub captured_at_millis: u64,
    pub capture_duration_micros: u64,
    pub changes: Vec<KernelFact>,
    pub cgroup_changes: Vec<KernelFact>,
    pub diagnostics: Vec<KernelFact>,
    pub process: Vec<KernelFact>,
    pub memory: Vec<KernelFact>,
    pub memory_accounting: Option<KernelMemoryAccounting>,
    pub scheduler: Vec<KernelFact>,
    pub security: Vec<KernelFact>,
    pub io: Vec<KernelFact>,
    pub isolation: Vec<KernelFact>,
    pub constraints: Vec<KernelFact>,
    pub runtime: Vec<KernelFact>,
    pub advanced: Vec<KernelFact>,
    pub tls_modules: Vec<KernelTlsModule>,
    pub mappings: Vec<KernelMapping>,
    pub mapping_changes: Vec<KernelMappingChange>,
    pub mapping_summary: Vec<KernelFact>,
    pub file_descriptors: Vec<KernelFileDescriptor>,
    pub limits: Vec<KernelLimit>,
    pub threads: Vec<KernelThread>,
    pub signals: Vec<KernelSignal>,
    pub process_tree: Vec<KernelProcess>,
    pub warnings: Vec<String>,
    pub tls_metadata_scanned: bool,
    pub comparison_ready: bool,
    identity: Option<u64>,
    metrics: KernelMetrics,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct KernelBaseline {
    pid: u32,
    captured_at_millis: u64,
    identity: Option<u64>,
    metrics: KernelMetrics,
    mappings: Vec<KernelMappingBaseline>,
}

/// The subset of a VMA needed for stop-to-stop comparison.
///
/// Keeping this separate from [`KernelMapping`] matters for large processes:
/// the live mapping model contains display-only NUMA, page-sampling and smaps
/// fields which would otherwise remain allocated for the entire next stop.
#[derive(Clone, Debug, PartialEq, Eq)]
struct KernelMappingBaseline {
    start: u64,
    end: u64,
    permissions: String,
    offset: u64,
    device: String,
    inode: u64,
    path: Option<String>,
    size: u64,
    rss: u64,
    pss: u64,
    private_rss: u64,
    private_dirty: u64,
    referenced: u64,
    swap: u64,
    huge: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct KernelTlsModule {
    pub module: String,
    pub path: String,
    pub role: String,
    pub template_address: u64,
    pub initialized_bytes: u64,
    pub total_bytes: u64,
    pub alignment: u64,
    pub symbol_count: usize,
    pub symbols: Vec<KernelTlsSymbol>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct KernelTlsSymbol {
    pub name: String,
    pub offset: u64,
    pub size: u64,
    pub binding: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct KernelMemoryAccounting {
    pub page_size: u64,
    pub statm_virtual_bytes: Option<u64>,
    pub statm_rss: Option<u64>,
    pub virtual_bytes: u64,
    pub rss: u64,
    pub pss: u64,
    pub private_clean: u64,
    pub private_dirty: u64,
    pub shared_clean: u64,
    pub shared_dirty: u64,
    pub swap: u64,
    pub anon_huge_pages: u64,
    pub anonymous: u64,
    pub referenced: u64,
    pub lazy_free: u64,
    pub locked: u64,
    pub ksm: u64,
    pub file_pmd_mapped: u64,
    pub shmem_pmd_mapped: u64,
    pub shared_hugetlb: u64,
    pub private_hugetlb: u64,
    pub page_tables: u64,
    pub pinned: u64,
    pub categories: Vec<KernelMemoryCategory>,
}

impl KernelMemoryAccounting {
    pub(crate) fn unique_rss(&self) -> u64 {
        self.private_clean.saturating_add(self.private_dirty)
    }

    pub(crate) fn shared_rss(&self) -> u64 {
        self.shared_clean.saturating_add(self.shared_dirty)
    }

    pub(crate) fn huge_bytes(&self) -> u64 {
        self.anon_huge_pages
            .saturating_add(self.file_pmd_mapped)
            .saturating_add(self.shmem_pmd_mapped)
            .saturating_add(self.shared_hugetlb)
            .saturating_add(self.private_hugetlb)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct KernelMemoryCategory {
    pub category: String,
    pub mappings: usize,
    pub virtual_bytes: u64,
    pub rss: u64,
    pub pss: u64,
    pub private_clean: u64,
    pub private_dirty: u64,
    pub shared_clean: u64,
    pub shared_dirty: u64,
    pub swap: u64,
    pub details: String,
}

impl KernelMemoryCategory {
    pub(crate) fn unique_rss(&self) -> u64 {
        self.private_clean.saturating_add(self.private_dirty)
    }

    pub(crate) fn shared_rss(&self) -> u64 {
        self.shared_clean.saturating_add(self.shared_dirty)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct KernelMapping {
    pub start: u64,
    pub end: u64,
    pub permissions: String,
    pub offset: u64,
    pub device: String,
    pub inode: u64,
    pub path: Option<String>,
    pub size: u64,
    pub rss: u64,
    pub pss: u64,
    pub shared_clean: u64,
    pub shared_dirty: u64,
    pub private_clean: u64,
    pub private_dirty: u64,
    pub anonymous: u64,
    pub referenced: u64,
    pub swap: u64,
    pub swap_pss: u64,
    pub locked: u64,
    pub anon_huge_pages: u64,
    pub pss_dirty: u64,
    pub ksm: u64,
    pub lazy_free: u64,
    pub file_pmd_mapped: u64,
    pub shmem_pmd_mapped: u64,
    pub shared_hugetlb: u64,
    pub private_hugetlb: u64,
    pub kernel_page_size: u64,
    pub mmu_page_size: u64,
    pub thp_eligible: bool,
    pub vm_flags: String,
    pub numa_policy: String,
    pub numa_nodes: String,
    pub page_sample: String,
}

impl KernelMapping {
    pub(crate) fn private_bytes(&self) -> u64 {
        self.private_clean.saturating_add(self.private_dirty)
    }

    pub(crate) fn shared_bytes(&self) -> u64 {
        self.shared_clean.saturating_add(self.shared_dirty)
    }

    pub(crate) fn huge_bytes(&self) -> u64 {
        self.anon_huge_pages
            .saturating_add(self.file_pmd_mapped)
            .saturating_add(self.shmem_pmd_mapped)
            .saturating_add(self.shared_hugetlb)
            .saturating_add(self.private_hugetlb)
    }

    fn comparison_baseline(&self) -> KernelMappingBaseline {
        KernelMappingBaseline {
            start: self.start,
            end: self.end,
            permissions: self.permissions.clone(),
            offset: self.offset,
            device: self.device.clone(),
            inode: self.inode,
            path: self.path.clone(),
            size: self.size,
            rss: self.rss,
            pss: self.pss,
            private_rss: self.private_bytes(),
            private_dirty: self.private_dirty,
            referenced: self.referenced,
            swap: self.swap,
            huge: self.huge_bytes(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct KernelMappingChange {
    pub status: String,
    pub start: u64,
    pub end: u64,
    pub permissions: String,
    pub device: String,
    pub inode: u64,
    pub path: Option<String>,
    pub size_delta: i128,
    pub rss_delta: i128,
    pub pss_delta: i128,
    pub private_delta: i128,
    pub dirty_delta: i128,
    pub referenced_delta: i128,
    pub swap_delta: i128,
    pub huge_delta: i128,
}

impl KernelMappingChange {
    fn impact(&self) -> u128 {
        self.size_delta.unsigned_abs()
            + self.rss_delta.unsigned_abs()
            + self.pss_delta.unsigned_abs()
            + self.private_delta.unsigned_abs()
            + self.dirty_delta.unsigned_abs()
            + self.referenced_delta.unsigned_abs()
            + self.swap_delta.unsigned_abs()
            + self.huge_delta.unsigned_abs()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct KernelFileDescriptor {
    pub number: u32,
    pub kind: String,
    pub access: String,
    pub flags: String,
    pub position: Option<u64>,
    pub target: String,
    pub details: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct KernelLimit {
    pub resource: String,
    pub soft: String,
    pub hard: String,
    pub units: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct KernelThread {
    pub tid: u32,
    pub name: String,
    pub state: String,
    pub cpu: String,
    pub policy: String,
    pub priority: String,
    pub affinity: String,
    pub wait_channel: String,
    pub syscall: String,
    pub switches: String,
    pub runtime_ns: Option<u64>,
    pub runqueue_wait_ns: Option<u64>,
    pub timeslices: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct KernelSignal {
    pub number: u8,
    pub name: String,
    pub pending_process: bool,
    pub pending_threads: usize,
    pub blocked_threads: usize,
    pub ignored: bool,
    pub caught: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct KernelProcess {
    pub pid: u32,
    pub parent_pid: u32,
    pub depth: u8,
    pub relation: String,
    pub name: String,
    pub state: String,
    pub threads: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct KernelMetrics {
    virtual_bytes: u64,
    rss: u64,
    pss: u64,
    private_rss: u64,
    shared_rss: u64,
    swap: u64,
    minor_faults: u64,
    major_faults: u64,
    user_ticks: u64,
    system_ticks: u64,
    voluntary_switches: u64,
    involuntary_switches: u64,
    read_characters: u64,
    write_characters: u64,
    read_bytes: u64,
    write_bytes: u64,
    cancelled_write_bytes: u64,
    read_syscalls: u64,
    write_syscalls: u64,
    mappings: u64,
    descriptors: u64,
    sched_runtime_ns: u64,
    sched_wait_ns: u64,
    sched_timeslices: u64,
    schedstat_available: bool,
    cgroup_memory_high: u64,
    cgroup_memory_max: u64,
    cgroup_oom: u64,
    cgroup_oom_kill: u64,
    cgroup_memory_current: u64,
    cgroup_memory_current_available: bool,
    cgroup_cpu_usage_us: u64,
    cgroup_cpu_throttled_us: u64,
    cgroup_cpu_nr_throttled: u64,
    cgroup_pgfault: u64,
    cgroup_pgmajfault: u64,
    cgroup_workingset_refault: u64,
    cgroup_pgscan: u64,
    cgroup_pgsteal: u64,
    cgroup_cpu_pressure_us: u64,
    cgroup_memory_pressure_us: u64,
    cgroup_io_pressure_us: u64,
    cgroup_metrics_available: bool,
}

pub(crate) fn verified_proc_root(pid: u32, debugger_pid: u32) -> Result<PathBuf, String> {
    let root = PathBuf::from(format!("/proc/{pid}"));
    let status = process::read_key_values(&root.join("status"))
        .map_err(|error| format!("Cannot inspect /proc/{pid}/status: {error}"))?;
    verify_tracer(pid, debugger_pid, &status)?;
    Ok(root)
}

/// Returns the ABI encoded by the traced process' executable.
///
/// GDB often reports its architecture and byte order as `auto`, especially
/// before the inferior has stopped. Reading the already-verified `/proc` entry
/// gives local sessions an authoritative fallback without trusting an
/// arbitrary PID supplied by debugger output.
pub(crate) fn read_local_target_abi(
    pid: u32,
    debugger_pid: u32,
) -> Option<(
    crate::debugger::TargetArchitecture,
    crate::debugger::TargetEndian,
    u32,
)> {
    let root = verified_proc_root(pid, debugger_pid).ok()?;
    let bytes = crate::bounded::read_prefix(&root.join("exe"), 40).ok()?;
    crate::debugger::TargetArchitecture::from_elf_ident(&bytes)
}

fn verify_tracer(
    pid: u32,
    debugger_pid: u32,
    status: &HashMap<String, String>,
) -> Result<(), String> {
    let tracer = status
        .get("TracerPid")
        .and_then(|value| value.parse::<u32>().ok())
        .ok_or_else(|| format!("/proc/{pid}/status did not expose TracerPid"))?;
    if tracer != debugger_pid {
        return Err(format!(
            "PID {pid} is not a local inferior traced by this GDB process (expected tracer {debugger_pid}, found {tracer})"
        ));
    }
    Ok(())
}

pub(crate) fn read_snapshot(
    pid: u32,
    debugger_pid: u32,
    include_tls_metadata: bool,
) -> Result<KernelSnapshot, String> {
    let root = verified_proc_root(pid, debugger_pid)?;
    let snapshot = read_snapshot_from(&root, pid, include_tls_metadata)?;
    // Revalidate ownership after the potentially expensive reads. This closes
    // the window where GDB detaches (or the PID is recycled) during collection.
    verified_proc_root(pid, debugger_pid)?;
    Ok(snapshot)
}

fn read_snapshot_from(
    root: &Path,
    pid: u32,
    include_tls_metadata: bool,
) -> Result<KernelSnapshot, String> {
    let started = Instant::now();
    let status = process::read_key_values(&root.join("status"))
        .map_err(|error| format!("Cannot inspect /proc/{pid}/status: {error}"))?;
    let stat = process::read_proc_stat(&root.join("stat"))
        .ok_or_else(|| format!("Cannot establish the identity of process {pid}"))?;
    let identity = Some(stat.start_time);
    let mut snapshot = KernelSnapshot {
        pid,
        identity,
        ..KernelSnapshot::default()
    };

    process::populate_process(&mut snapshot, root, &status);
    memory::populate_mappings(&mut snapshot, root);
    memory::populate_memory(&mut snapshot, root, &status);
    memory::populate_numa(&mut snapshot, root);
    memory::populate_page_samples(&mut snapshot, root);
    if include_tls_metadata {
        elf::populate_tls_metadata(&mut snapshot, root);
        snapshot.tls_metadata_scanned = true;
    }
    process::populate_scheduler(&mut snapshot, root, &status, Some(&stat));
    process::populate_security(&mut snapshot, root, &status);
    process::populate_io(&mut snapshot, root);
    process::populate_isolation(&mut snapshot, root, &status);
    process::populate_runtime(&mut snapshot, root);
    process::populate_threads_and_signals(&mut snapshot, root, &status);
    process::populate_hierarchy(&mut snapshot, root, &status);
    resources::populate_constraints(&mut snapshot, root);
    resources::populate_descriptors(&mut snapshot, root);
    resources::populate_limits(&mut snapshot, root);
    resources::populate_kernel_policy(&mut snapshot, root);
    populate_diagnostics(&mut snapshot);

    snapshot.metrics.minor_faults = stat.minor_faults;
    snapshot.metrics.major_faults = stat.major_faults;
    snapshot.metrics.user_ticks = stat.user_ticks;
    snapshot.metrics.system_ticks = stat.system_ticks;
    snapshot.metrics.mappings = snapshot.mappings.len() as u64;
    snapshot.metrics.descriptors = snapshot.file_descriptors.len() as u64;

    let after = process::read_proc_stat(&root.join("stat"))
        .ok_or_else(|| format!("Process {pid} disappeared while its snapshot was being read"))?;
    if stat.start_time != after.start_time {
        return Err(String::from(
            "The inferior changed while its procfs snapshot was being read",
        ));
    }
    snapshot.captured_at_millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        });
    snapshot.capture_duration_micros =
        u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
    Ok(snapshot)
}

impl KernelSnapshot {
    #[cfg(test)]
    pub(crate) fn compare_with(&mut self, previous: Option<&Self>) {
        let baseline = previous.map(Self::baseline);
        self.compare_with_baseline(baseline.as_ref());
    }

    pub(crate) fn baseline(&self) -> KernelBaseline {
        KernelBaseline {
            pid: self.pid,
            captured_at_millis: self.captured_at_millis,
            identity: self.identity,
            metrics: self.metrics,
            mappings: self
                .mappings
                .iter()
                .map(KernelMapping::comparison_baseline)
                .collect(),
        }
    }

    pub(crate) fn compare_with_baseline(&mut self, previous: Option<&KernelBaseline>) {
        self.changes.clear();
        self.cgroup_changes.clear();
        self.mapping_changes.clear();
        self.mapping_summary.clear();
        self.comparison_ready = false;
        let Some(previous) = previous.filter(|old| {
            old.pid == self.pid && old.identity.is_some() && old.identity == self.identity
        }) else {
            self.changes.push(fact(
                "Baseline",
                "Captured; values will be compared at the next stop or refresh",
            ));
            return;
        };
        self.comparison_ready = true;
        if let Some(elapsed_millis) = self
            .captured_at_millis
            .checked_sub(previous.captured_at_millis)
            .filter(|elapsed| *elapsed > 0)
        {
            self.changes.push(fact(
                "Snapshot interval",
                format_duration_ns(elapsed_millis.saturating_mul(1_000_000)),
            ));
        }
        let old = &previous.metrics;
        let new = &self.metrics;
        for (label, before, after, bytes) in [
            (
                "Virtual address space",
                old.virtual_bytes,
                new.virtual_bytes,
                true,
            ),
            ("Resident memory", old.rss, new.rss, true),
            (
                "Process-private resident memory (USS)",
                old.private_rss,
                new.private_rss,
                true,
            ),
            (
                "Shared resident memory",
                old.shared_rss,
                new.shared_rss,
                true,
            ),
            ("Proportional resident memory", old.pss, new.pss, true),
            ("Swap", old.swap, new.swap, true),
            ("Storage read", old.read_bytes, new.read_bytes, true),
            ("Storage written", old.write_bytes, new.write_bytes, true),
            (
                "Bytes returned by reads",
                old.read_characters,
                new.read_characters,
                true,
            ),
            (
                "Bytes supplied to writes",
                old.write_characters,
                new.write_characters,
                true,
            ),
            (
                "Cancelled writes",
                old.cancelled_write_bytes,
                new.cancelled_write_bytes,
                true,
            ),
            (
                "Minor page faults",
                old.minor_faults,
                new.minor_faults,
                false,
            ),
            (
                "Major page faults",
                old.major_faults,
                new.major_faults,
                false,
            ),
            ("User CPU ticks", old.user_ticks, new.user_ticks, false),
            (
                "Kernel CPU ticks",
                old.system_ticks,
                new.system_ticks,
                false,
            ),
            (
                "Voluntary switches",
                old.voluntary_switches,
                new.voluntary_switches,
                false,
            ),
            (
                "Involuntary switches",
                old.involuntary_switches,
                new.involuntary_switches,
                false,
            ),
            ("Read syscalls", old.read_syscalls, new.read_syscalls, false),
            (
                "Write syscalls",
                old.write_syscalls,
                new.write_syscalls,
                false,
            ),
            ("Mappings", old.mappings, new.mappings, false),
            ("Open descriptors", old.descriptors, new.descriptors, false),
        ] {
            let delta = if bytes {
                format_byte_delta(before, after)
            } else {
                format_count_delta(before, after)
            };
            if delta != "—" {
                self.changes.push(fact(label, delta));
            }
        }
        if old.schedstat_available && new.schedstat_available {
            for (label, before, after) in [
                (
                    "Main-thread CPU execution time",
                    old.sched_runtime_ns,
                    new.sched_runtime_ns,
                ),
                (
                    "Main-thread run-queue wait",
                    old.sched_wait_ns,
                    new.sched_wait_ns,
                ),
            ] {
                let delta = format_duration_delta(before, after, 1);
                if delta != "—" {
                    self.changes.push(fact(label, delta));
                }
            }
            let timeslices = format_count_delta(old.sched_timeslices, new.sched_timeslices);
            if timeslices != "—" {
                self.changes
                    .push(fact("Main-thread scheduler timeslices", timeslices));
            }
            if let (Some(runtime), Some(wait)) = (
                new.sched_runtime_ns.checked_sub(old.sched_runtime_ns),
                new.sched_wait_ns.checked_sub(old.sched_wait_ns),
            ) && runtime.saturating_add(wait) > 0
            {
                self.changes.push(fact(
                    "Main-thread run-queue wait share",
                    format!(
                        "{:.1}% · wait / (execution + wait)",
                        wait as f64 / runtime.saturating_add(wait) as f64 * 100.0
                    ),
                ));
            }
        }
        if old.cgroup_metrics_available && new.cgroup_metrics_available {
            if old.cgroup_memory_current_available && new.cgroup_memory_current_available {
                let delta = format_byte_delta(old.cgroup_memory_current, new.cgroup_memory_current);
                if delta != "—" {
                    self.cgroup_changes.push(fact("Cgroup memory usage", delta));
                }
            }
            for (label, before, after) in [
                (
                    "Cgroup memory.high events",
                    old.cgroup_memory_high,
                    new.cgroup_memory_high,
                ),
                (
                    "Cgroup memory.max events",
                    old.cgroup_memory_max,
                    new.cgroup_memory_max,
                ),
                ("Cgroup OOM events", old.cgroup_oom, new.cgroup_oom),
                ("Cgroup OOM kills", old.cgroup_oom_kill, new.cgroup_oom_kill),
                ("Cgroup page faults", old.cgroup_pgfault, new.cgroup_pgfault),
                (
                    "Cgroup major page faults",
                    old.cgroup_pgmajfault,
                    new.cgroup_pgmajfault,
                ),
                (
                    "Cgroup workingset refaults",
                    old.cgroup_workingset_refault,
                    new.cgroup_workingset_refault,
                ),
                ("Cgroup pages scanned", old.cgroup_pgscan, new.cgroup_pgscan),
                (
                    "Cgroup pages reclaimed",
                    old.cgroup_pgsteal,
                    new.cgroup_pgsteal,
                ),
                (
                    "Cgroup CPU throttling events",
                    old.cgroup_cpu_nr_throttled,
                    new.cgroup_cpu_nr_throttled,
                ),
            ] {
                let delta = format_count_delta(before, after);
                if delta != "—" {
                    self.cgroup_changes.push(fact(label, delta));
                }
            }
            for (label, before, after) in [
                (
                    "Cgroup CPU usage",
                    old.cgroup_cpu_usage_us,
                    new.cgroup_cpu_usage_us,
                ),
                (
                    "Cgroup CPU throttled time",
                    old.cgroup_cpu_throttled_us,
                    new.cgroup_cpu_throttled_us,
                ),
                (
                    "Cgroup CPU pressure stall",
                    old.cgroup_cpu_pressure_us,
                    new.cgroup_cpu_pressure_us,
                ),
                (
                    "Cgroup memory pressure stall",
                    old.cgroup_memory_pressure_us,
                    new.cgroup_memory_pressure_us,
                ),
                (
                    "Cgroup I/O pressure stall",
                    old.cgroup_io_pressure_us,
                    new.cgroup_io_pressure_us,
                ),
            ] {
                let delta = format_duration_delta(before, after, 1_000);
                if delta != "—" {
                    self.cgroup_changes.push(fact(label, delta));
                }
            }
        }
        if !self
            .changes
            .iter()
            .any(|change| change.label != "Snapshot interval")
        {
            self.changes
                .push(fact("Process-wide counters", "No changes"));
        }
        self.mapping_changes = compare_mappings(&previous.mappings, &self.mappings);
        self.mapping_summary = summarize_mapping_changes(&self.mapping_changes);
    }
}

fn populate_diagnostics(snapshot: &mut KernelSnapshot) {
    let executable = snapshot
        .mappings
        .iter()
        .filter(|mapping| mapping.permissions.contains('x'))
        .count();
    let writable_executable = snapshot
        .mappings
        .iter()
        .filter(|mapping| mapping.permissions.contains('w') && mapping.permissions.contains('x'))
        .count();
    let anonymous_executable = snapshot
        .mappings
        .iter()
        .filter(|mapping| mapping.path.is_none() && mapping.permissions.contains('x'))
        .count();
    snapshot.diagnostics.push(fact(
        "Executable mappings",
        format!(
            "{executable} total · {writable_executable} writable+executable · {anonymous_executable} anonymous"
        ),
    ));

    if let Some(memory) = snapshot.memory_accounting.as_ref() {
        let not_referenced = memory.rss.saturating_sub(memory.referenced);
        let referenced_share = if memory.rss == 0 {
            0.0
        } else {
            memory.referenced as f64 / memory.rss as f64 * 100.0
        };
        snapshot.diagnostics.push(fact(
            "Working-set signal",
            format!(
                "{} referenced ({referenced_share:.1}% of RSS) · {} not marked referenced",
                format_bytes(memory.referenced),
                format_bytes(not_referenced),
            ),
        ));
        let swapped_mappings = snapshot
            .mappings
            .iter()
            .filter(|mapping| mapping.swap > 0)
            .count();
        snapshot.diagnostics.push(fact(
            "Reclaim / pinning",
            format!(
                "swap {} in {swapped_mappings} VMAs · lazy-free {} · locked {} · pinned {}",
                format_bytes(memory.swap),
                format_bytes(memory.lazy_free),
                format_bytes(memory.locked),
                format_bytes(memory.pinned),
            ),
        ));

        let mut page_sizes = snapshot
            .mappings
            .iter()
            .flat_map(|mapping| [mapping.kernel_page_size, mapping.mmu_page_size])
            .filter(|size| *size > 0)
            .collect::<Vec<_>>();
        page_sizes.sort_unstable();
        page_sizes.dedup();
        snapshot.diagnostics.push(fact(
            "Page-size mix",
            format!(
                "{} · huge/PMD-backed {} · {} THP-eligible VMAs",
                page_sizes
                    .into_iter()
                    .map(format_bytes)
                    .collect::<Vec<_>>()
                    .join(" / "),
                format_bytes(memory.huge_bytes()),
                snapshot
                    .mappings
                    .iter()
                    .filter(|mapping| mapping.thp_eligible)
                    .count(),
            ),
        ));

        let allocation_categories = ["Heap", "Anonymous / JIT"]
            .into_iter()
            .filter_map(|name| {
                memory
                    .categories
                    .iter()
                    .find(|category| category.category == name)
                    .map(|category| {
                        format!(
                            "{name}: VSS {} · RSS {} · USS {}",
                            format_bytes(category.virtual_bytes),
                            format_bytes(category.rss),
                            format_bytes(category.unique_rss()),
                        )
                    })
            })
            .collect::<Vec<_>>();
        if !allocation_categories.is_empty() {
            snapshot.diagnostics.push(fact(
                "Allocation backing",
                allocation_categories.join("  |  "),
            ));
        }
        if let Some(mapping) = snapshot
            .mappings
            .iter()
            .max_by_key(|mapping| mapping.private_bytes())
            .filter(|mapping| mapping.private_bytes() > 0)
        {
            snapshot.diagnostics.push(fact(
                "Largest private mapping",
                format!(
                    "0x{:016x}–0x{:016x} · USS {} · RSS {} · {}",
                    mapping.start,
                    mapping.end,
                    format_bytes(mapping.private_bytes()),
                    format_bytes(mapping.rss),
                    mapping.path.as_deref().unwrap_or("anonymous"),
                ),
            ));
        }
    }

    let soft_fd_limit = snapshot
        .limits
        .iter()
        .find(|limit| limit.resource == "Max open files")
        .and_then(|limit| limit.soft.parse::<u64>().ok());
    let open_descriptors = snapshot.file_descriptors.len() as u64;
    snapshot.diagnostics.push(fact(
        "Open-file headroom",
        soft_fd_limit.map_or_else(
            || format!("{open_descriptors} open · soft limit unavailable"),
            |limit| {
                format!(
                    "{open_descriptors} / {limit} · {} remaining",
                    limit.saturating_sub(open_descriptors)
                )
            },
        ),
    ));
}

fn summarize_mapping_changes(changes: &[KernelMappingChange]) -> Vec<KernelFact> {
    type MappingMetric = (&'static str, fn(&KernelMappingChange) -> i128);

    if changes.is_empty() {
        return Vec::new();
    }
    let count = |status: &str| {
        changes
            .iter()
            .filter(|change| change.status.split(" / ").any(|part| part == status))
            .count()
    };
    let mut facts = vec![fact(
        "VMA lifecycle",
        format!(
            "{} new · {} unmapped · {} resized · {} protection changes · {} accounting-only",
            count("NEW"),
            count("UNMAPPED"),
            count("RESIZED"),
            count("PROTECTION"),
            count("CHANGED"),
        ),
    )];
    let metrics: [MappingMetric; 8] = [
        ("Virtual mapping churn", |change: &KernelMappingChange| {
            change.size_delta
        }),
        ("Resident memory churn", |change: &KernelMappingChange| {
            change.rss_delta
        }),
        (
            "Proportional memory churn",
            |change: &KernelMappingChange| change.pss_delta,
        ),
        (
            "Process-private memory churn",
            |change: &KernelMappingChange| change.private_delta,
        ),
        (
            "Private dirty memory churn",
            |change: &KernelMappingChange| change.dirty_delta,
        ),
        ("Referenced memory churn", |change: &KernelMappingChange| {
            change.referenced_delta
        }),
        ("Swap churn", |change: &KernelMappingChange| {
            change.swap_delta
        }),
        ("Huge-page churn", |change: &KernelMappingChange| {
            change.huge_delta
        }),
    ];
    for (label, value) in metrics {
        let gained = changes
            .iter()
            .map(value)
            .filter(|delta| *delta > 0)
            .sum::<i128>();
        let released = changes
            .iter()
            .map(value)
            .filter(|delta| *delta < 0)
            .sum::<i128>();
        if gained != 0 || released != 0 {
            facts.push(fact(
                label,
                format!(
                    "gained {} · released {} · net {}",
                    format_signed_bytes(gained),
                    format_signed_bytes(released),
                    format_signed_bytes(gained + released),
                ),
            ));
        }
    }
    facts
}

fn compare_mappings(
    before: &[KernelMappingBaseline],
    after: &[KernelMapping],
) -> Vec<KernelMappingChange> {
    let mut previous = before
        .iter()
        .map(|mapping| (baseline_mapping_key(mapping), mapping))
        .collect::<HashMap<_, _>>();
    let mut changes = Vec::new();
    for mapping in after {
        let old = previous.remove(&current_mapping_key(mapping));
        let status = match old {
            None => "NEW",
            Some(old) if old.end != mapping.end && old.permissions != mapping.permissions => {
                "RESIZED / PROTECTION"
            }
            Some(old) if old.end != mapping.end => "RESIZED",
            Some(old) if old.permissions != mapping.permissions => "PROTECTION",
            Some(_) => "CHANGED",
        };
        if let Some(change) = mapping_change(status, old, Some(mapping))
            && (old.is_none() || mapping_change_has_delta(&change))
        {
            changes.push(change);
        }
    }
    changes.extend(
        previous
            .into_values()
            .filter_map(|mapping| mapping_change("UNMAPPED", Some(mapping), None)),
    );
    changes.sort_by_key(|change| std::cmp::Reverse(change.impact()));
    changes
}

fn baseline_mapping_key(mapping: &KernelMappingBaseline) -> (u64, u64, &str, u64, Option<&str>) {
    (
        mapping.start,
        mapping.offset,
        &mapping.device,
        mapping.inode,
        mapping.path.as_deref(),
    )
}

fn current_mapping_key(mapping: &KernelMapping) -> (u64, u64, &str, u64, Option<&str>) {
    (
        mapping.start,
        mapping.offset,
        &mapping.device,
        mapping.inode,
        mapping.path.as_deref(),
    )
}

fn mapping_change(
    status: &str,
    before: Option<&KernelMappingBaseline>,
    after: Option<&KernelMapping>,
) -> Option<KernelMappingChange> {
    let (start, end, permissions, device, inode, path) = if let Some(mapping) = after {
        (
            mapping.start,
            mapping.end,
            mapping.permissions.as_str(),
            mapping.device.as_str(),
            mapping.inode,
            mapping.path.as_deref(),
        )
    } else {
        let mapping = before?;
        (
            mapping.start,
            mapping.end,
            mapping.permissions.as_str(),
            mapping.device.as_str(),
            mapping.inode,
            mapping.path.as_deref(),
        )
    };
    let delta = |before: u64, after: u64| i128::from(after) - i128::from(before);
    let before_value = |value: fn(&KernelMappingBaseline) -> u64| before.map_or(0, value);
    let after_value = |value: fn(&KernelMapping) -> u64| after.map_or(0, value);
    Some(KernelMappingChange {
        status: status.to_owned(),
        start,
        end,
        permissions: permissions.to_owned(),
        device: device.to_owned(),
        inode,
        path: path.map(str::to_owned),
        size_delta: delta(
            before_value(|mapping| mapping.size),
            after_value(|mapping| mapping.size),
        ),
        rss_delta: delta(
            before_value(|mapping| mapping.rss),
            after_value(|mapping| mapping.rss),
        ),
        pss_delta: delta(
            before_value(|mapping| mapping.pss),
            after_value(|mapping| mapping.pss),
        ),
        private_delta: delta(
            before_value(|mapping| mapping.private_rss),
            after_value(KernelMapping::private_bytes),
        ),
        dirty_delta: delta(
            before_value(|mapping| mapping.private_dirty),
            after_value(|mapping| mapping.private_dirty),
        ),
        referenced_delta: delta(
            before_value(|mapping| mapping.referenced),
            after_value(|mapping| mapping.referenced),
        ),
        swap_delta: delta(
            before_value(|mapping| mapping.swap),
            after_value(|mapping| mapping.swap),
        ),
        huge_delta: delta(
            before_value(|mapping| mapping.huge),
            after_value(KernelMapping::huge_bytes),
        ),
    })
}

fn mapping_change_has_delta(change: &KernelMappingChange) -> bool {
    change.status != "CHANGED"
        || [
            change.size_delta,
            change.rss_delta,
            change.pss_delta,
            change.private_delta,
            change.dirty_delta,
            change.referenced_delta,
            change.swap_delta,
            change.huge_delta,
        ]
        .into_iter()
        .any(|delta| delta != 0)
}

fn format_count_delta(before: u64, after: u64) -> String {
    let delta = i128::from(after) - i128::from(before);
    if delta > 0 {
        format!("+{delta}")
    } else if delta < 0 {
        format!("−{}", -delta)
    } else {
        String::from("—")
    }
}

fn format_byte_delta(before: u64, after: u64) -> String {
    match after.cmp(&before) {
        std::cmp::Ordering::Greater => format!("+{}", format_bytes(after - before)),
        std::cmp::Ordering::Less => format!("−{}", format_bytes(before - after)),
        std::cmp::Ordering::Equal => String::from("—"),
    }
}

fn format_signed_bytes(bytes: i128) -> String {
    if bytes == 0 {
        return String::from("—");
    }
    let magnitude = u64::try_from(bytes.unsigned_abs()).unwrap_or(u64::MAX);
    format!(
        "{}{}",
        if bytes > 0 { "+" } else { "−" },
        format_bytes(magnitude)
    )
}

fn format_duration_delta(before: u64, after: u64, nanos_per_unit: u64) -> String {
    match after.cmp(&before) {
        std::cmp::Ordering::Greater => format!(
            "+{}",
            format_duration_ns(after.saturating_sub(before).saturating_mul(nanos_per_unit))
        ),
        std::cmp::Ordering::Less => format!(
            "−{}",
            format_duration_ns(before.saturating_sub(after).saturating_mul(nanos_per_unit))
        ),
        std::cmp::Ordering::Equal => String::from("—"),
    }
}

pub(crate) fn format_duration_ns(nanos: u64) -> String {
    if nanos >= 1_000_000_000 {
        format!("{:.3} s", nanos as f64 / 1_000_000_000.0)
    } else if nanos >= 1_000_000 {
        format!("{:.3} ms", nanos as f64 / 1_000_000.0)
    } else if nanos >= 1_000 {
        format!("{:.3} µs", nanos as f64 / 1_000.0)
    } else {
        format!("{nanos} ns")
    }
}

pub(super) fn parse_proc_quantity(value: &str) -> Option<u64> {
    let mut fields = value.split_whitespace();
    let value = fields.next()?.parse::<u64>().ok()?;
    match fields.next() {
        Some("kB") | Some("KB") => value.checked_mul(1024),
        Some("mB") | Some("MB") => value.checked_mul(1024 * 1024),
        _ => Some(value),
    }
}

pub(super) fn push_status(
    destination: &mut Vec<KernelFact>,
    source: &HashMap<String, String>,
    key: &str,
    label: &str,
) {
    if let Some(value) = source.get(key) {
        destination.push(fact(label, value));
    }
}

pub(super) fn fact(label: impl Into<String>, value: impl Into<String>) -> KernelFact {
    KernelFact {
        label: label.into(),
        value: value.into(),
    }
}

pub(crate) fn format_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    const GIB: f64 = MIB * 1024.0;
    let bytes_float = bytes as f64;
    if bytes_float >= GIB {
        format!("{:.2} GiB", bytes_float / GIB)
    } else if bytes_float >= MIB {
        format!("{:.2} MiB", bytes_float / MIB)
    } else if bytes_float >= KIB {
        format!("{:.1} KiB", bytes_float / KIB)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_procfs_data_not_traced_by_this_debugger() {
        let status = HashMap::from([(String::from("TracerPid"), String::from("42"))]);
        assert!(verify_tracer(7, 42, &status).is_ok());
        assert!(
            verify_tracer(7, 41, &status)
                .unwrap_err()
                .contains("found 42")
        );
    }

    #[test]
    fn computes_signed_snapshot_deltas() {
        let mut old = KernelSnapshot {
            pid: 4,
            identity: Some(7),
            ..KernelSnapshot::default()
        };
        old.metrics.rss = 8192;
        old.metrics.major_faults = 3;
        let mut new = old.clone();
        new.metrics.rss = 4096;
        new.metrics.major_faults = 5;
        new.compare_with(Some(&old));
        assert_eq!(
            new.changes
                .iter()
                .find(|fact| fact.label == "Resident memory")
                .map(|fact| fact.value.as_str()),
            Some("−4.0 KiB")
        );
        assert!(
            new.changes
                .iter()
                .any(|fact| fact.label == "Major page faults" && fact.value == "+2")
        );
    }

    #[test]
    fn keeps_process_deltas_focused_and_labels_group_wide_activity() {
        let mut old = KernelSnapshot {
            pid: 4,
            identity: Some(7),
            captured_at_millis: 1_000,
            ..KernelSnapshot::default()
        };
        old.metrics.cgroup_metrics_available = true;
        old.metrics.cgroup_cpu_usage_us = 100;
        let mut new = old.clone();
        new.captured_at_millis = 1_250;
        new.metrics.cgroup_cpu_usage_us = 175;
        new.compare_with(Some(&old));

        assert!(
            new.changes
                .iter()
                .any(|fact| fact.label == "Snapshot interval" && fact.value == "250.000 ms")
        );
        assert!(
            new.changes
                .iter()
                .any(|fact| fact.label == "Process-wide counters" && fact.value == "No changes")
        );
        assert!(
            !new.changes
                .iter()
                .any(|fact| fact.label.starts_with("Cgroup"))
        );
        assert!(
            new.cgroup_changes
                .iter()
                .any(|fact| fact.label == "Cgroup CPU usage" && fact.value == "+75.000 µs")
        );
    }

    #[test]
    fn classifies_and_sorts_mapping_changes() {
        let original = KernelMapping {
            start: 0x1000,
            end: 0x2000,
            permissions: String::from("rw-p"),
            path: Some(String::from("[heap]")),
            size: 4096,
            rss: 4096,
            private_dirty: 4096,
            referenced: 4096,
            mmu_page_size: 4096,
            ..KernelMapping::default()
        };
        let mut old = KernelSnapshot {
            pid: 4,
            identity: Some(7),
            mappings: vec![original.clone()],
            ..KernelSnapshot::default()
        };
        old.metrics.mappings = 1;
        let mut grown = original;
        grown.end = 0x4000;
        grown.size = 12 * 1024;
        grown.rss = 8 * 1024;
        grown.private_dirty = 8 * 1024;
        grown.referenced = 8 * 1024;
        let mut new = KernelSnapshot {
            pid: 4,
            identity: Some(7),
            mappings: vec![grown],
            ..KernelSnapshot::default()
        };
        new.compare_with(Some(&old));

        assert!(new.comparison_ready);
        assert_eq!(new.mapping_changes.len(), 1);
        assert_eq!(new.mapping_changes[0].status, "RESIZED");
        assert_eq!(new.mapping_changes[0].size_delta, 8 * 1024);
        assert_eq!(new.mapping_changes[0].private_delta, 4 * 1024);
        assert!(new.mapping_summary.iter().any(|fact| {
            fact.label == "Virtual mapping churn"
                && fact.value == "gained +8.0 KiB · released — · net +8.0 KiB"
        }));
        assert!(
            new.mapping_summary
                .iter()
                .any(|fact| { fact.label == "VMA lifecycle" && fact.value.contains("1 resized") })
        );
    }

    #[test]
    fn baseline_keeps_comparison_data_without_heavy_mapping_annotations() {
        let snapshot = KernelSnapshot {
            mappings: vec![KernelMapping {
                start: 0x1000,
                end: 0x2000,
                permissions: String::from("rw-p"),
                device: String::from("00:01"),
                path: Some(String::from("[heap]")),
                size: 4096,
                rss: 4096,
                private_dirty: 4096,
                vm_flags: String::from("rd wr mr mw me ac sd"),
                numa_nodes: String::from("N0=1"),
                page_sample: String::from("large diagnostic payload"),
                ..KernelMapping::default()
            }],
            ..KernelSnapshot::default()
        };

        let baseline = snapshot.baseline();
        let mapping = &baseline.mappings[0];
        assert_eq!(mapping.path.as_deref(), Some("[heap]"));
        assert_eq!(mapping.private_dirty, 4096);
        assert_eq!(mapping.private_rss, 4096);
        assert!(
            std::mem::size_of::<KernelMappingBaseline>() < std::mem::size_of::<KernelMapping>()
        );
    }

    #[test]
    fn formats_debugger_scale_durations() {
        assert_eq!(format_duration_ns(420), "420 ns");
        assert_eq!(format_duration_ns(42_000), "42.000 µs");
        assert_eq!(format_duration_ns(42_000_000), "42.000 ms");
        assert_eq!(format_duration_ns(2_000_000_000), "2.000 s");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn reads_a_live_procfs_snapshot() {
        let pid = std::process::id();
        let snapshot =
            read_snapshot_from(&PathBuf::from(format!("/proc/{pid}")), pid, true).unwrap();
        assert_eq!(snapshot.pid, std::process::id());
        assert!(!snapshot.process.is_empty());
        assert!(!snapshot.mappings.is_empty());
        let memory = snapshot.memory_accounting.as_ref().unwrap();
        assert!(memory.page_size > 0);
        assert!(!memory.categories.is_empty());
        assert!(memory.unique_rss() <= memory.rss);
        assert!(memory.shared_rss() <= memory.rss);
        assert!(!snapshot.limits.is_empty());
        assert!(!snapshot.threads.is_empty());
        assert!(!snapshot.signals.is_empty());
    }
}
