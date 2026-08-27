use std::{
    collections::{HashMap, HashSet},
    fs, io,
    net::{Ipv4Addr, Ipv6Addr},
    path::Path,
    time::{Duration, Instant},
};

use super::*;

pub(super) fn populate_constraints(snapshot: &mut KernelSnapshot, root: &Path) {
    for (entry, label) in [
        ("oom_score", "OOM score"),
        ("oom_score_adj", "OOM score adjustment"),
        ("autogroup", "Scheduler autogroup"),
    ] {
        if let Ok(value) = fs::read_to_string(root.join(entry)) {
            snapshot.constraints.push(fact(label, value.trim()));
        }
    }
    let Ok(cgroups) = fs::read_to_string(root.join("cgroup")) else {
        return;
    };
    let Some(path) = cgroups.lines().find_map(|line| line.strip_prefix("0::")) else {
        snapshot
            .constraints
            .push(fact("Cgroup resources", "Legacy/hybrid hierarchy"));
        return;
    };
    snapshot.constraints.push(fact("Cgroup v2 path", path));
    let cgroup_root = root
        .join("root/sys/fs/cgroup")
        .join(path.trim_start_matches('/'));
    let mut memory_current = None;
    let mut memory_limit = None;
    for (entry, label, bytes) in [
        ("memory.current", "Cgroup memory current", true),
        ("memory.max", "Cgroup memory maximum", true),
        ("memory.high", "Cgroup memory high", true),
        ("memory.swap.current", "Cgroup swap current", true),
        ("memory.swap.max", "Cgroup swap maximum", true),
        ("memory.low", "Cgroup memory low", true),
        ("cpu.max", "Cgroup CPU quota / period", false),
        ("cpu.weight", "Cgroup CPU weight", false),
        ("cpuset.cpus.effective", "Effective cgroup CPUs", false),
        (
            "cpuset.mems.effective",
            "Effective cgroup NUMA nodes",
            false,
        ),
        ("pids.current", "Processes current", false),
        ("pids.max", "Processes maximum", false),
    ] {
        if let Ok(value) = fs::read_to_string(cgroup_root.join(entry)) {
            let value = value.trim();
            if entry == "memory.current" {
                memory_current = value.parse::<u64>().ok();
                if let Some(current) = memory_current {
                    snapshot.metrics.cgroup_memory_current = current;
                    snapshot.metrics.cgroup_memory_current_available = true;
                }
            } else if entry == "memory.max" && value != "max" {
                memory_limit = value.parse::<u64>().ok();
            }
            let display = if bytes && value != "max" {
                value
                    .parse::<u64>()
                    .map(format_bytes)
                    .unwrap_or_else(|_| value.to_owned())
            } else {
                value.to_owned()
            };
            snapshot.constraints.push(fact(label, display));
        }
    }
    if let (Some(current), Some(limit)) = (memory_current, memory_limit) {
        let used = if limit == 0 {
            0.0
        } else {
            current as f64 / limit as f64 * 100.0
        };
        snapshot.constraints.push(fact(
            "Cgroup memory headroom",
            format!(
                "{} remaining · {used:.1}% used",
                format_bytes(limit.saturating_sub(current))
            ),
        ));
    }
    for (entry, label) in [
        ("memory.events", "Memory events"),
        ("cpu.stat", "CPU control accounting"),
        ("io.stat", "Cgroup I/O accounting"),
    ] {
        if let Ok(value) = fs::read_to_string(cgroup_root.join(entry)) {
            snapshot
                .constraints
                .push(fact(label, compact_lines(&value)));
            if entry == "memory.events" {
                let events = parse_flat_counters(&value);
                snapshot.metrics.cgroup_memory_high = counter(&events, "high");
                snapshot.metrics.cgroup_memory_max = counter(&events, "max");
                snapshot.metrics.cgroup_oom = counter(&events, "oom");
                snapshot.metrics.cgroup_oom_kill = counter(&events, "oom_kill");
                snapshot.metrics.cgroup_metrics_available = true;
            } else if entry == "cpu.stat" {
                let cpu = parse_flat_counters(&value);
                snapshot.metrics.cgroup_cpu_usage_us = counter(&cpu, "usage_usec");
                snapshot.metrics.cgroup_cpu_throttled_us = counter(&cpu, "throttled_usec");
                snapshot.metrics.cgroup_cpu_nr_throttled = counter(&cpu, "nr_throttled");
                snapshot.metrics.cgroup_metrics_available = true;
            }
        }
    }
    if let Ok(value) = fs::read_to_string(cgroup_root.join("memory.stat")) {
        let memory = parse_flat_counters(&value);
        snapshot.metrics.cgroup_pgfault = counter(&memory, "pgfault");
        snapshot.metrics.cgroup_pgmajfault = counter(&memory, "pgmajfault");
        snapshot.metrics.cgroup_workingset_refault = counter(&memory, "workingset_refault_anon")
            .saturating_add(counter(&memory, "workingset_refault_file"));
        snapshot.metrics.cgroup_pgscan = counter_or_sum(
            &memory,
            "pgscan",
            &["pgscan_kswapd", "pgscan_direct", "pgscan_khugepaged"],
        );
        snapshot.metrics.cgroup_pgsteal = counter_or_sum(
            &memory,
            "pgsteal",
            &["pgsteal_kswapd", "pgsteal_direct", "pgsteal_khugepaged"],
        );
        snapshot.metrics.cgroup_metrics_available = true;
        for (key, label) in [
            ("anon", "Cgroup anonymous memory"),
            ("file", "Cgroup file cache"),
            ("kernel", "Cgroup kernel memory"),
            ("pagetables", "Cgroup page tables"),
            ("slab", "Cgroup slab memory"),
        ] {
            if let Some(value) = memory.get(key) {
                snapshot.constraints.push(fact(label, format_bytes(*value)));
            }
        }
        snapshot.constraints.push(fact(
            "Cgroup fault / reclaim counters",
            format!(
                "faults {} · major {} · refaults {} · scanned {} · reclaimed {}",
                snapshot.metrics.cgroup_pgfault,
                snapshot.metrics.cgroup_pgmajfault,
                snapshot.metrics.cgroup_workingset_refault,
                snapshot.metrics.cgroup_pgscan,
                snapshot.metrics.cgroup_pgsteal,
            ),
        ));
    }
    for (entry, label) in [
        ("cpu.pressure", "CPU pressure"),
        ("memory.pressure", "Memory pressure"),
        ("io.pressure", "I/O pressure"),
    ] {
        if let Ok(value) = fs::read_to_string(cgroup_root.join(entry)) {
            snapshot.constraints.push(fact(label, compact_psi(&value)));
            let total = psi_total(&value, "some").unwrap_or(0);
            match entry {
                "cpu.pressure" => snapshot.metrics.cgroup_cpu_pressure_us = total,
                "memory.pressure" => snapshot.metrics.cgroup_memory_pressure_us = total,
                "io.pressure" => snapshot.metrics.cgroup_io_pressure_us = total,
                _ => {}
            }
        }
    }
    let proc_root = root.parent().unwrap_or(root);
    for (entry, label) in [
        ("cpu", "System CPU pressure"),
        ("memory", "System memory pressure"),
        ("io", "System I/O pressure"),
    ] {
        if let Ok(value) = fs::read_to_string(proc_root.join("pressure").join(entry)) {
            snapshot.constraints.push(fact(label, compact_psi(&value)));
        }
    }
}

pub(super) fn populate_descriptors(snapshot: &mut KernelSnapshot, root: &Path) {
    match read_file_descriptors(root) {
        Ok((descriptors, truncated)) => {
            snapshot.file_descriptors = descriptors;
            if truncated {
                snapshot.warnings.push(String::from(
                    "File descriptor details were truncated at 16,384 entries",
                ));
            }
        }
        Err(error) => snapshot
            .warnings
            .push(format!("Open file descriptors unavailable: {error}")),
    }
}

pub(super) fn populate_limits(snapshot: &mut KernelSnapshot, root: &Path) {
    match fs::read_to_string(root.join("limits")) {
        Ok(limits) => snapshot.limits = parse_limits(&limits),
        Err(error) => snapshot
            .warnings
            .push(format!("Resource limits unavailable: {error}")),
    }
}

pub(super) fn populate_kernel_policy(snapshot: &mut KernelSnapshot, root: &Path) {
    let proc_root = root.parent().unwrap_or(root);
    if let Ok(value) = fs::read_to_string(proc_root.join("sys/kernel/kptr_restrict")) {
        snapshot
            .advanced
            .push(fact("Kernel-pointer visibility", value.trim()));
    }
}

fn read_file_descriptors(root: &Path) -> io::Result<(Vec<KernelFileDescriptor>, bool)> {
    const MAX_FILE_DESCRIPTORS: usize = 16_384;
    const MAX_FDINFO_BYTES: usize = 64 * 1024;
    let mut truncated = false;
    let deadline = Instant::now() + Duration::from_millis(500);
    let mut descriptors = Vec::new();
    for entry in fs::read_dir(root.join("fd"))?.filter_map(Result::ok) {
        if descriptors.len() >= MAX_FILE_DESCRIPTORS || Instant::now() >= deadline {
            truncated = true;
            break;
        }
        let Some(number) = entry.file_name().to_string_lossy().parse::<u32>().ok() else {
            continue;
        };
        let Some(target) = fs::read_link(entry.path())
            .ok()
            .map(|target| target.to_string_lossy().into_owned())
        else {
            continue;
        };
        let info = crate::bounded::read_prefix(
            &root.join("fdinfo").join(number.to_string()),
            MAX_FDINFO_BYTES,
        )
        .ok()
        .map(|bytes| parse_descriptor_info(&String::from_utf8_lossy(&bytes)))
        .unwrap_or_default();
        descriptors.push((number, target, info));
    }
    let socket_inodes = descriptors
        .iter()
        .filter_map(|(_, target, _)| socket_inode(target))
        .collect::<HashSet<_>>();
    let sockets = read_socket_endpoints(root, &socket_inodes);
    drop(socket_inodes);
    let mut descriptors = descriptors
        .into_iter()
        .map(|(number, target, info)| {
            let access = match info.flags.map(|flags| flags & 3) {
                Some(0) => "read",
                Some(1) => "write",
                Some(2) => "read/write",
                _ => "unknown",
            }
            .to_owned();
            let socket = socket_inode(&target).and_then(|inode| sockets.get(inode));
            let details = match (socket, info.details.is_empty()) {
                (Some(socket), false) => format!("{socket} · {}", info.details),
                (Some(socket), true) => socket.clone(),
                (None, _) => info.details,
            };
            KernelFileDescriptor {
                number,
                kind: descriptor_kind(&target).to_owned(),
                access,
                flags: descriptor_flags(info.flags),
                position: info.position,
                target,
                details,
            }
        })
        .collect::<Vec<_>>();
    descriptors.sort_unstable_by_key(|descriptor| descriptor.number);
    Ok((descriptors, truncated))
}

#[derive(Default)]
struct DescriptorInfo {
    flags: Option<u64>,
    position: Option<u64>,
    details: String,
}

fn parse_descriptor_info(fdinfo: &str) -> DescriptorInfo {
    let mut info = DescriptorInfo::default();
    let mut detail_count = 0_usize;
    for line in fdinfo.lines() {
        let trimmed = line.trim();
        let (key, value) = trimmed.split_once(':').unwrap_or((trimmed, ""));
        match key {
            "flags" => info.flags = u64::from_str_radix(value.trim(), 8).ok(),
            "pos" => info.position = value.trim().parse().ok(),
            _ => {}
        }
        if matches!(
            key,
            "mnt_id"
                | "ino"
                | "eventfd-count"
                | "eventfd-id"
                | "sigmask"
                | "Pid"
                | "clockid"
                | "ticks"
                | "settime flags"
                | "tfd"
                | "inotify wd"
                | "SAME_MNT_ID"
                | "sq_entries"
                | "cq_entries"
        ) {
            if !info.details.is_empty() {
                info.details.push_str(" · ");
            }
            push_compact_whitespace(&mut info.details, trimmed);
            detail_count += 1;
        }
        if detail_count >= 10 {
            info.details.push_str(" · …");
            break;
        }
    }
    info
}

fn descriptor_kind(target: &str) -> &'static str {
    if target.starts_with("socket:") {
        "socket"
    } else if target.starts_with("pipe:") {
        "pipe"
    } else if target.contains("eventpoll") {
        "epoll"
    } else if target.contains("eventfd") {
        "eventfd"
    } else if target.contains("inotify") || target.contains("fanotify") {
        "filesystem notify"
    } else if target.contains("io_uring") {
        "io_uring"
    } else if target.contains("signalfd") {
        "signalfd"
    } else if target.contains("timerfd") {
        "timerfd"
    } else if target.contains("pidfd") {
        "pidfd"
    } else if target.starts_with("anon_inode:") {
        "anon inode"
    } else if target.starts_with("memfd:") || target.contains("/memfd:") {
        "memfd"
    } else if target.starts_with('/') {
        "file"
    } else {
        "other"
    }
}

fn socket_inode(target: &str) -> Option<&str> {
    target.strip_prefix("socket:[")?.strip_suffix(']')
}

fn read_socket_endpoints(root: &Path, wanted: &HashSet<&str>) -> HashMap<String, String> {
    let mut endpoints = HashMap::new();
    if wanted.is_empty() {
        return endpoints;
    }
    for (entry, protocol, ipv6) in [
        ("tcp", "TCP", false),
        ("tcp6", "TCP6", true),
        ("udp", "UDP", false),
        ("udp6", "UDP6", true),
    ] {
        let Ok(input) =
            crate::bounded::read_string(&root.join("net").join(entry), 16 * 1024 * 1024)
        else {
            continue;
        };
        for line in input.lines().skip(1) {
            let mut fields = line.split_whitespace();
            let Some(local_field) = fields.nth(1) else {
                continue;
            };
            let Some(remote_field) = fields.next() else {
                continue;
            };
            let Some(state_field) = fields.next() else {
                continue;
            };
            let Some(inode) = fields.nth(5) else {
                continue;
            };
            if !wanted.contains(inode) {
                continue;
            }
            let local =
                decode_endpoint(local_field, ipv6).unwrap_or_else(|| local_field.to_owned());
            let remote =
                decode_endpoint(remote_field, ipv6).unwrap_or_else(|| remote_field.to_owned());
            let state = socket_state(state_field);
            endpoints.insert(
                inode.to_owned(),
                format!("{protocol} {local} → {remote} · {state}"),
            );
        }
    }
    if let Ok(input) = crate::bounded::read_string(&root.join("net/unix"), 16 * 1024 * 1024) {
        for line in input.lines().skip(1) {
            let mut fields = line.split_whitespace();
            let Some(socket_type) = fields.nth(4) else {
                continue;
            };
            let Some(state) = fields.next() else {
                continue;
            };
            let Some(inode) = fields.next() else {
                continue;
            };
            if !wanted.contains(inode) {
                continue;
            }
            let path = fields.next().unwrap_or("unnamed");
            endpoints.insert(
                inode.to_owned(),
                format!("UNIX {path} · state {state} · type {socket_type}"),
            );
        }
    }
    endpoints
}

fn descriptor_flags(flags: Option<u64>) -> String {
    let Some(flags) = flags else {
        return String::from("—");
    };
    let mut names = Vec::new();
    if flags & 0o4010000 == 0o4010000 {
        names.push("SYNC");
    } else if flags & 0o10000 != 0 {
        names.push("DSYNC");
    }
    for (mask, name) in [
        (0o2000, "APPEND"),
        (0o4000, "NONBLOCK"),
        (0o20000, "ASYNC"),
        (0o40000, "DIRECT"),
        (0o100000, "LARGEFILE"),
        (0o200000, "DIRECTORY"),
        (0o400000, "NOFOLLOW"),
        (0o1000000, "NOATIME"),
        (0o2000000, "CLOEXEC"),
        (0o10000000, "PATH"),
        (0o20000000, "TMPFILE"),
    ] {
        if flags & mask == mask {
            names.push(name);
        }
    }
    if names.is_empty() {
        format!("0{flags:o}")
    } else {
        format!("0{flags:o} · {}", names.join(" "))
    }
}

fn decode_endpoint(value: &str, ipv6: bool) -> Option<String> {
    let (address, port) = value.split_once(':')?;
    let port = u16::from_str_radix(port, 16).ok()?;
    if ipv6 {
        let mut bytes = [0_u8; 16];
        let (chunks, remainder) = address.as_bytes().as_chunks::<8>();
        if !remainder.is_empty() || chunks.len() != 4 {
            return None;
        }
        for (index, chunk) in chunks.iter().enumerate() {
            let word = u32::from_str_radix(std::str::from_utf8(chunk).ok()?, 16).ok()?;
            bytes[index * 4..index * 4 + 4].copy_from_slice(&word.to_ne_bytes());
        }
        Some(format!("[{}]:{port}", Ipv6Addr::from(bytes)))
    } else {
        let word = u32::from_str_radix(address, 16).ok()?;
        Some(format!("{}:{port}", Ipv4Addr::from(word.to_ne_bytes())))
    }
}

fn socket_state(state: &str) -> &'static str {
    match state {
        "01" => "ESTABLISHED",
        "02" => "SYN_SENT",
        "03" => "SYN_RECV",
        "04" => "FIN_WAIT1",
        "05" => "FIN_WAIT2",
        "06" => "TIME_WAIT",
        "07" => "CLOSE",
        "08" => "CLOSE_WAIT",
        "09" => "LAST_ACK",
        "0A" => "LISTEN",
        "0B" => "CLOSING",
        "0C" => "NEW_SYN_RECV",
        _ => "UNKNOWN",
    }
}

fn parse_limits(input: &str) -> Vec<KernelLimit> {
    input
        .lines()
        .skip(1)
        .filter_map(|line| {
            let resource = fixed_column(line, 0, 25);
            let soft = fixed_column(line, 25, 46);
            let hard = fixed_column(line, 46, 67);
            let units = line.get(67..).unwrap_or_default().trim();
            (!resource.is_empty()).then(|| KernelLimit {
                resource: resource.to_owned(),
                soft: soft.to_owned(),
                hard: hard.to_owned(),
                units: units.to_owned(),
            })
        })
        .collect()
}

fn fixed_column(line: &str, start: usize, end: usize) -> &str {
    line.get(start.min(line.len())..end.min(line.len()))
        .unwrap_or_default()
        .trim()
}

fn push_compact_whitespace(output: &mut String, value: &str) {
    for word in value.split_whitespace() {
        if !output.is_empty() && !output.ends_with(" · ") {
            output.push(' ');
        }
        output.push_str(word);
    }
}

fn compact_lines(value: &str) -> String {
    value
        .lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" · ")
}

fn compact_psi(value: &str) -> String {
    value
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let scope = fields.next()?;
            let selected = fields
                .filter(|field| field.starts_with("avg10=") || field.starts_with("total="))
                .collect::<Vec<_>>()
                .join(" ");
            Some(format!("{scope} {selected}"))
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

fn parse_flat_counters(input: &str) -> HashMap<String, u64> {
    input
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            Some((fields.next()?.to_owned(), fields.next()?.parse().ok()?))
        })
        .collect()
}

fn counter(values: &HashMap<String, u64>, key: &str) -> u64 {
    values.get(key).copied().unwrap_or(0)
}

fn counter_or_sum(values: &HashMap<String, u64>, total: &str, parts: &[&str]) -> u64 {
    values
        .get(total)
        .copied()
        .unwrap_or_else(|| parts.iter().map(|key| counter(values, key)).sum())
}

fn psi_total(input: &str, scope: &str) -> Option<u64> {
    input.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        (fields.next()? == scope).then(|| {
            fields.find_map(|field| {
                field
                    .strip_prefix("total=")
                    .and_then(|value| value.parse().ok())
            })
        })?
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_limits_and_socket_endpoints() {
        let limits = parse_limits(
            "Limit                     Soft Limit           Hard Limit           Units\n\
             Max open files            1024                 1048576              files\n",
        );
        assert_eq!(limits[0].resource, "Max open files");
        assert_eq!(limits[0].hard, "1048576");
        assert_eq!(
            decode_endpoint("0100007F:1F90", false).as_deref(),
            Some("127.0.0.1:8080")
        );
        assert_eq!(
            descriptor_flags(Some(0o2004002)),
            "02004002 · NONBLOCK CLOEXEC"
        );
        assert_eq!(descriptor_flags(Some(0)), "00");
        assert_eq!(descriptor_flags(None), "—");

        let info = parse_descriptor_info(
            "pos:\t17\nflags:\t02004002\nmnt_id:\t  12   34\nino:\t99\nignored:\tlarge\n",
        );
        assert_eq!(info.position, Some(17));
        assert_eq!(info.flags, Some(0o2004002));
        assert_eq!(info.details, "mnt_id: 12 34 · ino: 99");
    }

    #[test]
    fn compacts_pressure_without_losing_scope() {
        assert_eq!(
            compact_psi("some avg10=0.10 avg60=0.20 total=4\nfull avg10=0.00 total=0\n"),
            "some avg10=0.10 total=4 · full avg10=0.00 total=0"
        );
        assert_eq!(
            psi_total("some avg10=0.10 total=42\nfull avg10=0.0 total=4\n", "some"),
            Some(42)
        );
        let counters = parse_flat_counters("pgfault 12\npgscan_direct 3\npgscan_kswapd 4\n");
        assert_eq!(
            counter_or_sum(&counters, "pgscan", &["pgscan_direct", "pgscan_kswapd"]),
            7
        );
    }
}
