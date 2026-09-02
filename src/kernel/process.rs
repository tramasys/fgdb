use std::{
    collections::{HashMap, HashSet, VecDeque},
    fs, io,
    path::Path,
    time::{Duration, Instant},
};

use super::*;
use crate::debugger::{TargetArchitecture, TargetEndian};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ProcStat {
    pub start_time: u64,
    pub minor_faults: u64,
    pub major_faults: u64,
    pub user_ticks: u64,
    pub system_ticks: u64,
    pub priority: i64,
    pub nice: i64,
    pub processor: Option<u32>,
}

#[derive(Default)]
struct ThreadStatus {
    name: String,
    state: String,
    affinity: String,
    pending: u64,
    blocked: u64,
    voluntary_switches: String,
    involuntary_switches: String,
}

pub(super) fn populate_process(
    snapshot: &mut KernelSnapshot,
    root: &Path,
    status: &HashMap<String, String>,
) {
    push_status(&mut snapshot.process, status, "Name", "Name");
    push_status(&mut snapshot.process, status, "State", "State");
    snapshot.process.push(fact("PID", snapshot.pid.to_string()));

    for (source, label) in [
        ("Tgid", "Thread group"),
        ("PPid", "Parent PID"),
        ("TracerPid", "Tracer PID"),
        ("Threads", "Threads"),
        ("Umask", "Umask"),
        ("Uid", "UIDs (real/effective/saved/fs)"),
        ("Gid", "GIDs (real/effective/saved/fs)"),
        ("NSpid", "PID namespace IDs"),
    ] {
        push_status(&mut snapshot.process, status, source, label);
    }

    push_read_link(&mut snapshot.process, root, "exe", "Executable");
    push_read_link(&mut snapshot.process, root, "cwd", "Working directory");

    if let Ok(command_line) = crate::bounded::read_bytes(&root.join("cmdline"), 2 * 1024 * 1024) {
        let mut joined = String::with_capacity(command_line.len());

        for argument in command_line
            .split(|byte| *byte == 0)
            .filter(|argument| !argument.is_empty())
        {
            if !joined.is_empty() {
                joined.push(' ');
            }

            joined.push_str(&String::from_utf8_lossy(argument));
        }

        if !joined.is_empty() {
            snapshot.process.push(fact("Command line", joined));
        }
    }

    if let Ok(syscall) = crate::bounded::read_string(&root.join("syscall"), MAX_PROC_TEXT_BYTES) {
        snapshot.process.push(fact(
            "Kernel syscall state",
            decode_syscall(syscall.trim(), executable_architecture(root)),
        ));
    }
}

pub(super) fn populate_scheduler(
    snapshot: &mut KernelSnapshot,
    root: &Path,
    status: &HashMap<String, String>,
    stat: Option<&ProcStat>,
) {
    if let Ok(input) = crate::bounded::read_string(&root.join("schedstat"), MAX_PROC_TEXT_BYTES)
        && let Some((runtime_ns, wait_ns, timeslices)) = parse_schedstat(&input)
    {
        snapshot.metrics.sched_runtime_ns = runtime_ns;
        snapshot.metrics.sched_wait_ns = wait_ns;
        snapshot.metrics.sched_timeslices = timeslices;
        snapshot.metrics.schedstat_available = true;

        snapshot.scheduler.push(fact(
            "Main-thread CPU execution",
            format_duration_ns(runtime_ns),
        ));

        snapshot.scheduler.push(fact(
            "Main-thread run-queue wait",
            format_duration_ns(wait_ns),
        ));

        snapshot
            .scheduler
            .push(fact("Main-thread timeslices", timeslices.to_string()));
    }

    for (source, label) in [
        ("Cpus_allowed_list", "Allowed CPUs"),
        ("Mems_allowed_list", "Allowed NUMA nodes"),
        ("voluntary_ctxt_switches", "Voluntary switches"),
        ("nonvoluntary_ctxt_switches", "Involuntary switches"),
    ] {
        push_status(&mut snapshot.scheduler, status, source, label);
    }

    snapshot.metrics.voluntary_switches = status
        .get("voluntary_ctxt_switches")
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);

    snapshot.metrics.involuntary_switches = status
        .get("nonvoluntary_ctxt_switches")
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);

    if let Some(stat) = stat {
        snapshot.scheduler.push(fact(
            "Current CPU",
            stat.processor
                .map_or_else(|| String::from("unknown"), |cpu| cpu.to_string()),
        ));

        snapshot
            .scheduler
            .push(fact("Static priority", stat.priority.to_string()));

        snapshot.scheduler.push(fact("Nice", stat.nice.to_string()));
    }

    if let Ok(sched) = read_key_values(&root.join("sched")) {
        for (source, label) in [
            ("se.sum_exec_runtime", "CPU runtime"),
            ("se.nr_migrations", "CPU migrations"),
            ("nr_switches", "Scheduler switches"),
            ("prio", "Dynamic priority"),
        ] {
            push_status(&mut snapshot.scheduler, &sched, source, label);
        }

        if let Some(policy) = sched.get("policy") {
            snapshot.scheduler.push(fact(
                "Policy",
                format!("{} ({policy})", scheduler_policy(policy)),
            ));
        }
    }

    if let Ok(wchan) = crate::bounded::read_string(&root.join("wchan"), MAX_PROC_TEXT_BYTES) {
        let wchan = wchan.trim();

        if !wchan.is_empty() && wchan != "0" {
            snapshot.scheduler.push(fact("Kernel wait channel", wchan));
        }
    }
}

fn parse_schedstat(input: &str) -> Option<(u64, u64, u64)> {
    let mut fields = input.split_whitespace();

    Some((
        fields.next()?.parse().ok()?,
        fields.next()?.parse().ok()?,
        fields.next()?.parse().ok()?,
    ))
}

pub(super) fn populate_security(
    snapshot: &mut KernelSnapshot,
    root: &Path,
    status: &HashMap<String, String>,
) {
    push_status(
        &mut snapshot.security,
        status,
        "NoNewPrivs",
        "No new privileges",
    );

    if let Some(seccomp) = status.get("Seccomp") {
        let mode = match seccomp.as_str() {
            "0" => "disabled",
            "1" => "strict",
            "2" => "filter",
            _ => "unknown",
        };

        snapshot
            .security
            .push(fact("Seccomp", format!("{mode} ({seccomp})")));
    }

    push_status(
        &mut snapshot.security,
        status,
        "Seccomp_filters",
        "Seccomp filters",
    );

    for (source, label) in [
        ("CapInh", "Inheritable capabilities"),
        ("CapPrm", "Permitted capabilities"),
        ("CapEff", "Effective capabilities"),
        ("CapBnd", "Capability bounding set"),
        ("CapAmb", "Ambient capabilities"),
    ] {
        if let Some(capabilities) = status.get(source) {
            snapshot
                .security
                .push(fact(label, decode_capabilities(capabilities)));
        }
    }

    for (source, label) in [
        ("CoreDumping", "Core dump in progress"),
        ("THP_enabled", "THP enabled"),
        ("Speculation_Store_Bypass", "Store-bypass mitigation"),
        ("SpeculationIndirectBranch", "Indirect-branch mitigation"),
    ] {
        push_status(&mut snapshot.security, status, source, label);
    }

    if let Ok(context) =
        crate::bounded::read_string(&root.join("attr/current"), MAX_PROC_TEXT_BYTES)
    {
        let context = context.trim();

        if !context.is_empty() {
            snapshot.security.push(fact("LSM context", context));
        }
    }
}

pub(super) fn populate_io(snapshot: &mut KernelSnapshot, root: &Path) {
    let Ok(io) = read_key_values(&root.join("io")) else {
        return;
    };

    for (source, label, bytes) in [
        ("rchar", "Bytes returned by reads", true),
        ("wchar", "Bytes supplied to writes", true),
        ("read_bytes", "Storage bytes read", true),
        ("write_bytes", "Storage bytes written", true),
        ("cancelled_write_bytes", "Cancelled writes", true),
        ("syscr", "Read syscalls", false),
        ("syscw", "Write syscalls", false),
    ] {
        if let Some(value) = io.get(source) {
            let display = if bytes {
                value
                    .parse::<u64>()
                    .map(format_bytes)
                    .unwrap_or_else(|_| value.clone())
            } else {
                value.clone()
            };

            snapshot.io.push(fact(label, display));
        }
    }

    snapshot.metrics.read_characters = parse_u64(&io, "rchar");
    snapshot.metrics.write_characters = parse_u64(&io, "wchar");
    snapshot.metrics.read_bytes = parse_u64(&io, "read_bytes");
    snapshot.metrics.write_bytes = parse_u64(&io, "write_bytes");
    snapshot.metrics.cancelled_write_bytes = parse_u64(&io, "cancelled_write_bytes");
    snapshot.metrics.read_syscalls = parse_u64(&io, "syscr");
    snapshot.metrics.write_syscalls = parse_u64(&io, "syscw");
}

pub(super) fn populate_isolation(
    snapshot: &mut KernelSnapshot,
    root: &Path,
    status: &HashMap<String, String>,
) {
    for (source, label) in [
        ("NStgid", "Thread-group namespace IDs"),
        ("NSpgid", "Process-group namespace IDs"),
        ("NSsid", "Session namespace IDs"),
    ] {
        push_status(&mut snapshot.isolation, status, source, label);
    }

    if let Ok(entries) = fs::read_dir(root.join("ns")) {
        let mut namespaces = entries
            .filter_map(Result::ok)
            .filter_map(|entry| {
                fs::read_link(entry.path()).ok().map(|target| {
                    (
                        entry.file_name().to_string_lossy().into_owned(),
                        target.to_string_lossy().into_owned(),
                    )
                })
            })
            .collect::<Vec<_>>();

        namespaces.sort_unstable_by(|left, right| left.0.cmp(&right.0));

        snapshot.isolation.extend(
            namespaces
                .into_iter()
                .map(|(name, target)| fact(format!("{name} namespace"), target)),
        );
    }

    if let Ok(cgroups) = crate::bounded::read_string(&root.join("cgroup"), MAX_PROC_TEXT_BYTES) {
        snapshot.isolation.extend(
            cgroups
                .lines()
                .enumerate()
                .map(|(index, cgroup)| fact(format!("Cgroup {}", index + 1), cgroup)),
        );
    }
}

pub(super) fn populate_runtime(snapshot: &mut KernelSnapshot, root: &Path) {
    let elf = crate::bounded::read_prefix(&root.join("exe"), 64)
        .ok()
        .and_then(|bytes| parse_elf_identity(&bytes));

    if let Some(elf) = &elf {
        snapshot
            .runtime
            .push(fact("Executable format", &elf.description));

        snapshot.runtime.push(fact(
            "Position independent",
            if elf.pie {
                "yes (ET_DYN)"
            } else {
                "no (ET_EXEC)"
            },
        ));
    }

    if let Ok(value) = crate::bounded::read_string(
        &root
            .parent()
            .unwrap_or(root)
            .join("sys/kernel/randomize_va_space"),
        MAX_PROC_TEXT_BYTES,
    ) {
        let description = match value.trim() {
            "0" => "disabled",
            "1" => "conservative",
            "2" => "full",
            _ => "unknown",
        };

        snapshot.runtime.push(fact(
            "Kernel ASLR policy",
            format!("{description} ({})", value.trim()),
        ));
    }

    if let Ok(personality) =
        crate::bounded::read_string(&root.join("personality"), MAX_PROC_TEXT_BYTES)
    {
        snapshot.runtime.push(fact(
            "Personality",
            format!("0x{}", personality.trim().trim_start_matches("0x")),
        ));
    }

    if let (Ok(auxv), Some(elf)) = (
        crate::bounded::read_bytes(&root.join("auxv"), 1024 * 1024),
        elf.as_ref(),
    ) {
        let word_size = elf.word_size;
        let endian = elf.endian;
        let address_width = word_size.saturating_mul(2);

        for (kind, value) in parse_auxv(&auxv, word_size, endian) {
            if let Some((label, pointer)) = auxv_label(kind) {
                snapshot.runtime.push(fact(
                    label,
                    if pointer {
                        format!("0x{value:0address_width$x}")
                    } else {
                        value.to_string()
                    },
                ));
            }
        }
    }
}

pub(super) fn populate_threads_and_signals(
    snapshot: &mut KernelSnapshot,
    root: &Path,
    process_status: &HashMap<String, String>,
) {
    const MAX_THREADS: usize = 8_192;

    let Ok(entries) = fs::read_dir(root.join("task")) else {
        populate_signals(snapshot, process_status, &[0; 64], &[0; 64]);
        return;
    };

    let architecture = executable_architecture(root);
    let deadline = Instant::now() + Duration::from_millis(500);
    let mut threads = Vec::new();
    let mut thread_pending = [0_usize; 64];
    let mut thread_blocked = [0_usize; 64];

    for (index, entry) in entries.filter_map(Result::ok).enumerate() {
        if index >= MAX_THREADS {
            snapshot.warnings.push(format!(
                "Thread details were truncated at {MAX_THREADS} entries"
            ));

            break;
        }

        if Instant::now() >= deadline {
            snapshot.warnings.push(String::from(
                "Thread details were truncated after the 500 ms snapshot budget",
            ));

            break;
        }

        let Some(tid) = entry.file_name().to_string_lossy().parse::<u32>().ok() else {
            continue;
        };

        let thread_root = entry.path();

        let Ok(status) = read_thread_status(&thread_root.join("status")) else {
            continue;
        };

        accumulate_signal_mask(&mut thread_pending, status.pending);
        accumulate_signal_mask(&mut thread_blocked, status.blocked);
        let stat = read_proc_stat(&thread_root.join("stat"));
        let policy = read_thread_policy(&thread_root.join("sched"));

        let wait_channel =
            crate::bounded::read_string(&thread_root.join("wchan"), MAX_PROC_TEXT_BYTES)
                .unwrap_or_default()
                .trim()
                .to_owned();

        let syscall =
            crate::bounded::read_string(&thread_root.join("syscall"), MAX_PROC_TEXT_BYTES)
                .map(|value| decode_syscall(value.trim(), architecture))
                .unwrap_or_default();

        let schedstat =
            crate::bounded::read_string(&thread_root.join("schedstat"), MAX_PROC_TEXT_BYTES)
                .ok()
                .and_then(|value| parse_schedstat(&value));

        threads.push(KernelThread {
            tid,
            name: status.name,
            state: status.state,
            cpu: stat
                .as_ref()
                .and_then(|stat| stat.processor)
                .map_or_else(String::new, |cpu| cpu.to_string()),
            policy,
            priority: stat
                .as_ref()
                .map(|stat| format!("{} / nice {}", stat.priority, stat.nice))
                .unwrap_or_default(),
            affinity: status.affinity,
            wait_channel,
            syscall,
            switches: format!(
                "{} voluntary · {} involuntary",
                status.voluntary_switches, status.involuntary_switches
            ),
            runtime_ns: schedstat.map(|value| value.0),
            runqueue_wait_ns: schedstat.map(|value| value.1),
            timeslices: schedstat.map(|value| value.2),
        });
    }

    threads.sort_unstable_by_key(|thread| thread.tid);
    snapshot.threads = threads;
    populate_signals(snapshot, process_status, &thread_pending, &thread_blocked);
}

fn read_thread_status(path: &Path) -> io::Result<ThreadStatus> {
    let input = crate::bounded::read_string(path, 64 * 1024)?;

    let mut status = ThreadStatus {
        voluntary_switches: String::from("0"),
        involuntary_switches: String::from("0"),
        ..ThreadStatus::default()
    };

    for line in input.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };

        let value = value.trim();

        match key {
            "Name" => status.name = value.to_owned(),
            "State" => status.state = value.to_owned(),
            "Cpus_allowed_list" => status.affinity = value.to_owned(),
            "SigPnd" => status.pending = u64::from_str_radix(value, 16).unwrap_or(0),
            "SigBlk" => status.blocked = u64::from_str_radix(value, 16).unwrap_or(0),
            "voluntary_ctxt_switches" => status.voluntary_switches = value.to_owned(),
            "nonvoluntary_ctxt_switches" => status.involuntary_switches = value.to_owned(),
            _ => {}
        }
    }

    Ok(status)
}

fn read_thread_policy(path: &Path) -> String {
    crate::bounded::read_string(path, 64 * 1024)
        .ok()
        .and_then(|input| {
            input.lines().find_map(|line| {
                let (key, value) = line.split_once(':')?;

                (key.trim() == "policy").then(|| scheduler_policy(value.trim()).to_owned())
            })
        })
        .unwrap_or_default()
}

fn accumulate_signal_mask(counts: &mut [usize; 64], mask: u64) {
    for (bit, count) in counts.iter_mut().enumerate() {
        if mask & (1_u64 << bit) != 0 {
            *count += 1;
        }
    }
}

fn populate_signals(
    snapshot: &mut KernelSnapshot,
    status: &HashMap<String, String>,
    thread_pending: &[usize; 64],
    thread_blocked: &[usize; 64],
) {
    let process_pending = signal_mask(status, "ShdPnd");
    let ignored = signal_mask(status, "SigIgn");
    let caught = signal_mask(status, "SigCgt");
    snapshot.signals = (1_u8..=64)
        .map(|number| {
            let bit = 1_u64 << (number - 1);
            KernelSignal {
                number,
                name: signal_name(number),
                pending_process: process_pending & bit != 0,
                pending_threads: thread_pending[usize::from(number - 1)],
                blocked_threads: thread_blocked[usize::from(number - 1)],
                ignored: ignored & bit != 0,
                caught: caught & bit != 0,
            }
        })
        .collect();
}

pub(super) fn populate_hierarchy(
    snapshot: &mut KernelSnapshot,
    root: &Path,
    status: &HashMap<String, String>,
) {
    const HIERARCHY_BUDGET: Duration = Duration::from_millis(250);

    let Some(proc_root) = root.parent() else {
        return;
    };

    let mut ancestors = Vec::new();

    let mut parent = status
        .get("PPid")
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(0);

    let mut visited = HashSet::new();

    for _ in 0..8 {
        if parent == 0 || !visited.insert(parent) {
            break;
        }

        let Some(info) = process_summary(proc_root, parent) else {
            break;
        };

        parent = info.parent_pid;
        ancestors.push(info);
    }

    ancestors.reverse();
    let ancestor_count = ancestors.len();

    for (depth, mut process) in ancestors.into_iter().enumerate() {
        process.depth = depth as u8;

        process.relation = if depth + 1 == ancestor_count {
            String::from("Parent")
        } else {
            String::from("Ancestor")
        };

        snapshot.process_tree.push(process);
    }

    snapshot.process_tree.push(KernelProcess {
        pid: snapshot.pid,
        parent_pid: status
            .get("PPid")
            .and_then(|value| value.parse().ok())
            .unwrap_or(0),
        depth: ancestor_count as u8,
        relation: String::from("Target"),
        name: process_display_name(root, status),
        state: status.get("State").cloned().unwrap_or_default(),
        threads: status.get("Threads").cloned().unwrap_or_default(),
    });

    let mut queue = VecDeque::from([(snapshot.pid, ancestor_count as u8 + 1)]);
    let mut seen = HashSet::from([snapshot.pid]);
    let deadline = Instant::now() + HIERARCHY_BUDGET;

    while let Some((parent, depth)) = queue.pop_front() {
        if snapshot.process_tree.len() >= 256 || Instant::now() >= deadline {
            break;
        }

        for child in child_processes(proc_root, parent, deadline) {
            if !seen.insert(child) {
                continue;
            }

            if let Some(mut info) = process_summary(proc_root, child) {
                info.depth = depth;

                info.relation = if depth == ancestor_count as u8 + 1 {
                    String::from("Child")
                } else {
                    String::from("Descendant")
                };

                snapshot.process_tree.push(info);
                queue.push_back((child, depth.saturating_add(1)));
            }
        }
    }
}

pub(super) fn read_key_values(path: &Path) -> io::Result<HashMap<String, String>> {
    Ok(crate::bounded::read_string(path, 1024 * 1024)?
        .lines()
        .filter_map(|line| line.split_once(':'))
        .map(|(key, value)| (key.trim().to_owned(), value.trim().to_owned()))
        .collect())
}

pub(super) fn read_proc_stat(path: &Path) -> Option<ProcStat> {
    let stat = crate::bounded::read_string(path, 64 * 1024).ok()?;
    let mut minor_faults = None;
    let mut major_faults = None;
    let mut user_ticks = None;
    let mut system_ticks = None;
    let mut priority = None;
    let mut nice = None;
    let mut start_time = None;
    let mut processor = None;

    for (index, value) in stat.rsplit_once(") ")?.1.split_whitespace().enumerate() {
        match index {
            7 => minor_faults = value.parse().ok(),
            9 => major_faults = value.parse().ok(),
            11 => user_ticks = value.parse().ok(),
            12 => system_ticks = value.parse().ok(),
            15 => priority = value.parse().ok(),
            16 => nice = value.parse().ok(),
            19 => start_time = value.parse().ok(),
            36 => {
                processor = value.parse().ok();
                break;
            }
            _ => {}
        }
    }

    Some(ProcStat {
        minor_faults: minor_faults?,
        major_faults: major_faults?,
        user_ticks: user_ticks?,
        system_ticks: system_ticks?,
        priority: priority?,
        nice: nice?,
        start_time: start_time?,
        processor,
    })
}

fn parse_u64(values: &HashMap<String, String>, key: &str) -> u64 {
    values
        .get(key)
        .and_then(|value| value.parse().ok())
        .unwrap_or(0)
}

fn signal_mask(status: &HashMap<String, String>, key: &str) -> u64 {
    status
        .get(key)
        .and_then(|value| u64::from_str_radix(value.trim_start_matches("0x"), 16).ok())
        .unwrap_or(0)
}

fn process_summary(proc_root: &Path, pid: u32) -> Option<KernelProcess> {
    let root = proc_root.join(pid.to_string());
    let status = read_key_values(&root.join("status")).ok()?;

    Some(KernelProcess {
        pid,
        parent_pid: status
            .get("PPid")
            .and_then(|value| value.parse().ok())
            .unwrap_or(0),
        depth: 0,
        relation: String::new(),
        name: process_display_name(&root, &status),
        state: status.get("State").cloned().unwrap_or_default(),
        threads: status.get("Threads").cloned().unwrap_or_default(),
    })
}

fn process_display_name(root: &Path, status: &HashMap<String, String>) -> String {
    let kernel_name = status.get("Name").cloned().unwrap_or_default();

    if kernel_name.len() < 15 {
        return kernel_name;
    }

    let argv0 = crate::bounded::read_prefix(&root.join("cmdline"), 64 * 1024)
        .ok()
        .and_then(|bytes| {
            let argument = bytes.split(|byte| *byte == 0).next()?;
            let argument = String::from_utf8_lossy(argument);

            Path::new(argument.as_ref())
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        });

    let executable = fs::read_link(root.join("exe")).ok().and_then(|path| {
        path.file_name()
            .map(|name| name.to_string_lossy().into_owned())
    });

    [argv0, executable]
        .into_iter()
        .flatten()
        .find(|candidate| {
            candidate.len() > kernel_name.len()
                && candidate.as_bytes().starts_with(kernel_name.as_bytes())
        })
        .unwrap_or(kernel_name)
}

fn child_processes(proc_root: &Path, pid: u32, deadline: Instant) -> Vec<u32> {
    const MAX_CHILDREN: usize = 256;
    let task = proc_root.join(pid.to_string()).join("task");

    let Ok(entries) = fs::read_dir(task) else {
        return Vec::new();
    };

    let mut children = Vec::new();
    let mut seen = HashSet::new();

    'threads: for entry in entries.filter_map(Result::ok) {
        if Instant::now() >= deadline {
            break;
        }

        let Ok(list) = crate::bounded::read_string(&entry.path().join("children"), 64 * 1024)
        else {
            continue;
        };

        for child in list
            .split_whitespace()
            .filter_map(|pid| pid.parse::<u32>().ok())
        {
            if seen.insert(child) {
                children.push(child);
            }

            if children.len() >= MAX_CHILDREN {
                break 'threads;
            }
        }
    }

    children.sort_unstable();

    children
}

fn push_read_link(destination: &mut Vec<KernelFact>, root: &Path, entry: &str, label: &str) {
    if let Ok(target) = fs::read_link(root.join(entry)) {
        destination.push(fact(label, target.to_string_lossy()));
    }
}

fn scheduler_policy(policy: &str) -> &'static str {
    match policy.trim() {
        "0" => "SCHED_OTHER",
        "1" => "SCHED_FIFO",
        "2" => "SCHED_RR",
        "3" => "SCHED_BATCH",
        "5" => "SCHED_IDLE",
        "6" => "SCHED_DEADLINE",
        "7" => "SCHED_EXT",
        _ => "unknown",
    }
}

fn decode_syscall(value: &str, architecture: TargetArchitecture) -> String {
    let mut fields = value.split_whitespace();

    let Some(number) = fields.next() else {
        return String::new();
    };

    let Ok(number) = number.parse::<i64>() else {
        return value.to_owned();
    };

    if number < 0 {
        return String::from("not in a syscall");
    }

    let number = architecture.normalize_syscall_number(number as u64);
    let name = architecture.syscall_name(number);
    let mut arguments = String::new();

    for argument in fields.take(6) {
        if !arguments.is_empty() {
            arguments.push(' ');
        }

        arguments.push_str(argument);
    }

    if arguments.is_empty() {
        format!("{name} ({number})")
    } else {
        format!("{name} ({number}) · {arguments}")
    }
}

fn executable_architecture(root: &Path) -> TargetArchitecture {
    crate::bounded::read_prefix(&root.join("exe"), 40)
        .ok()
        .and_then(|bytes| parse_elf_identity(&bytes))
        .map_or(TargetArchitecture::Unknown, |elf| elf.architecture)
}

fn signal_name(number: u8) -> String {
    match number {
        1 => "SIGHUP",
        2 => "SIGINT",
        3 => "SIGQUIT",
        4 => "SIGILL",
        5 => "SIGTRAP",
        6 => "SIGABRT",
        7 => "SIGBUS",
        8 => "SIGFPE",
        9 => "SIGKILL",
        10 => "SIGUSR1",
        11 => "SIGSEGV",
        12 => "SIGUSR2",
        13 => "SIGPIPE",
        14 => "SIGALRM",
        15 => "SIGTERM",
        16 => "SIGSTKFLT",
        17 => "SIGCHLD",
        18 => "SIGCONT",
        19 => "SIGSTOP",
        20 => "SIGTSTP",
        21 => "SIGTTIN",
        22 => "SIGTTOU",
        23 => "SIGURG",
        24 => "SIGXCPU",
        25 => "SIGXFSZ",
        26 => "SIGVTALRM",
        27 => "SIGPROF",
        28 => "SIGWINCH",
        29 => "SIGIO",
        30 => "SIGPWR",
        31 => "SIGSYS",
        32 | 33 => return format!("SIG{number} (reserved)"),
        34..=64 => return format!("SIGRTMIN+{}", number - 34),
        _ => return format!("SIG{number}"),
    }
    .to_owned()
}

struct ElfIdentity {
    description: String,
    word_size: usize,
    architecture: TargetArchitecture,
    endian: TargetEndian,
    pie: bool,
}

fn parse_elf_identity(bytes: &[u8]) -> Option<ElfIdentity> {
    let (architecture, endian, pointer_bits) = TargetArchitecture::from_elf_ident(bytes)?;
    let word_size = usize::try_from(pointer_bits / 8).ok()?;
    let little = endian == TargetEndian::Little;

    let read_u16 = |offset| {
        let data: [u8; 2] = bytes.get(offset..offset + 2)?.try_into().ok()?;

        Some(if little {
            u16::from_le_bytes(data)
        } else {
            u16::from_be_bytes(data)
        })
    };

    let kind = read_u16(16)?;

    Some(ElfIdentity {
        description: format!(
            "ELF{} · {} · {} endian",
            word_size * 8,
            architecture.display_name(),
            if little { "little" } else { "big" },
        ),
        word_size,
        architecture,
        endian,
        pie: kind == 3,
    })
}

fn parse_auxv(bytes: &[u8], word_size: usize, endian: TargetEndian) -> Vec<(u64, u64)> {
    let pair_size = word_size.saturating_mul(2);

    if !matches!(word_size, 4 | 8) {
        return Vec::new();
    }

    let read = |slice: &[u8]| match word_size {
        4 => Some(u64::from(endian.decode_u32(slice.try_into().ok()?))),
        8 => Some(endian.decode_u64(slice.try_into().ok()?)),
        _ => None,
    };

    let mut entries = Vec::new();

    for pair in bytes.chunks_exact(pair_size) {
        let Some(kind) = read(&pair[..word_size]) else {
            break;
        };

        if kind == 0 {
            break;
        }

        let Some(value) = read(&pair[word_size..]) else {
            break;
        };

        entries.push((kind, value));
    }

    entries
}

fn auxv_label(kind: u64) -> Option<(&'static str, bool)> {
    Some(match kind {
        3 => ("Program headers", true),
        4 => ("Program-header size", false),
        5 => ("Program-header count", false),
        6 => ("Page size", false),
        7 => ("Interpreter base", true),
        8 => ("ELF flags", false),
        9 => ("Entry point", true),
        11 => ("Real UID", false),
        12 => ("Effective UID", false),
        13 => ("Real GID", false),
        14 => ("Effective GID", false),
        15 => ("Platform string", true),
        16 => ("Hardware capabilities", true),
        17 => ("Clock ticks/second", false),
        23 => ("Secure execution", false),
        24 => ("Base platform", true),
        25 => ("Random seed address", true),
        26 => ("Hardware capabilities 2", true),
        31 => ("Executable-name address", true),
        33 => ("vDSO base", true),
        51 => ("Minimum signal-stack size", false),
        _ => return None,
    })
}

fn decode_capabilities(value: &str) -> String {
    const NAMES: [&str; 41] = [
        "CHOWN",
        "DAC_OVERRIDE",
        "DAC_READ_SEARCH",
        "FOWNER",
        "FSETID",
        "KILL",
        "SETGID",
        "SETUID",
        "SETPCAP",
        "LINUX_IMMUTABLE",
        "NET_BIND_SERVICE",
        "NET_BROADCAST",
        "NET_ADMIN",
        "NET_RAW",
        "IPC_LOCK",
        "IPC_OWNER",
        "SYS_MODULE",
        "SYS_RAWIO",
        "SYS_CHROOT",
        "SYS_PTRACE",
        "SYS_PACCT",
        "SYS_ADMIN",
        "SYS_BOOT",
        "SYS_NICE",
        "SYS_RESOURCE",
        "SYS_TIME",
        "SYS_TTY_CONFIG",
        "MKNOD",
        "LEASE",
        "AUDIT_WRITE",
        "AUDIT_CONTROL",
        "SETFCAP",
        "MAC_OVERRIDE",
        "MAC_ADMIN",
        "SYSLOG",
        "WAKE_ALARM",
        "BLOCK_SUSPEND",
        "AUDIT_READ",
        "PERFMON",
        "BPF",
        "CHECKPOINT_RESTORE",
    ];

    let Ok(mask) = u64::from_str_radix(value.trim_start_matches("0x"), 16) else {
        return value.to_owned();
    };

    let enabled = NAMES
        .iter()
        .enumerate()
        .filter(|(bit, _)| mask & (1_u64 << bit) != 0)
        .map(|(_, name)| *name)
        .collect::<Vec<_>>();

    if enabled.is_empty() {
        format!("0x{mask:x} · none")
    } else {
        format!("0x{mask:x} · {}", enabled.join(" "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_stat_after_parenthesized_name() {
        let mut tail = vec!["0"; 37];
        tail[0] = "T";
        tail[7] = "7";
        tail[9] = "3";
        tail[11] = "11";
        tail[12] = "12";
        tail[15] = "20";
        tail[16] = "5";
        tail[19] = "99";
        tail[36] = "6";
        let fields = format!("1 (a name) {}", tail.join(" "));
        let path = std::env::temp_dir().join(format!("fgdb-stat-{}", std::process::id()));
        fs::write(&path, fields).unwrap();
        let stat = read_proc_stat(&path).unwrap();
        let _ = fs::remove_file(path);
        assert_eq!(stat.minor_faults, 7);
        assert_eq!(stat.start_time, 99);
        assert_eq!(stat.processor, Some(6));
    }

    #[test]
    fn decodes_signal_and_capability_masks() {
        assert_eq!(signal_name(11), "SIGSEGV");
        assert_eq!(signal_name(34), "SIGRTMIN+0");
        assert!(decode_capabilities("0000000000002000").contains("NET_RAW"));

        assert_eq!(
            parse_schedstat("1200000 34000 8\n"),
            Some((1_200_000, 34_000, 8))
        );
    }

    #[test]
    fn parses_32_and_64_bit_elf_architectures_and_auxv_byte_order() {
        let mut arm = [0_u8; 64];
        arm[..4].copy_from_slice(b"\x7fELF");
        arm[4] = 1;
        arm[5] = 1;
        arm[16..18].copy_from_slice(&2_u16.to_le_bytes());
        arm[18..20].copy_from_slice(&40_u16.to_le_bytes());
        let identity = parse_elf_identity(&arm).unwrap();
        assert_eq!(identity.architecture, TargetArchitecture::Arm);
        assert_eq!(identity.word_size, 4);
        let mut ppc64 = [0_u8; 64];
        ppc64[..4].copy_from_slice(b"\x7fELF");
        ppc64[4] = 2;
        ppc64[5] = 2;
        ppc64[16..18].copy_from_slice(&2_u16.to_be_bytes());
        ppc64[18..20].copy_from_slice(&21_u16.to_be_bytes());
        let identity = parse_elf_identity(&ppc64).unwrap();
        assert_eq!(identity.architecture, TargetArchitecture::PowerPc64);
        assert_eq!(identity.endian, TargetEndian::Big);

        let auxv = [
            6_u32.to_be_bytes(),
            4096_u32.to_be_bytes(),
            0_u32.to_be_bytes(),
            0_u32.to_be_bytes(),
        ]
        .concat();
        assert_eq!(parse_auxv(&auxv, 4, TargetEndian::Big), [(6, 4096)]);

        let trailing_after_null = [
            auxv,
            9_u32.to_be_bytes().to_vec(),
            0x1234_u32.to_be_bytes().to_vec(),
        ]
        .concat();

        assert_eq!(
            parse_auxv(&trailing_after_null, 4, TargetEndian::Big),
            [(6, 4096)]
        );

        assert!(parse_auxv(&trailing_after_null, 16, TargetEndian::Big).is_empty());
    }
}
