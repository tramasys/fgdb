//! Bounded stopped-process lock and wait-dependency inspection.

use super::*;

const MAX_TASKS: usize = 4096;

mod evidence;
pub(crate) use evidence::{LockObservation, LockOwnership};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct LockSnapshot {
    pub threads_scanned: usize,
    pub waits: Vec<LockWait>,
    pub dependencies: Vec<LockDependency>,
    pub deadlocks: Vec<DeadlockCycle>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct LockWait {
    pub tid: u32,
    pub thread: String,
    pub state: String,
    pub address: Option<u64>,
    pub operation: String,
    pub expected: Option<u64>,
    pub details: String,
    pub operation_flags: Option<u64>,
    pub observation: LockObservation,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LockDependency {
    pub waiter_tid: u32,
    pub waiter: String,
    pub owner_tid: u32,
    pub owner: String,
    pub address: u64,
    pub futex_value: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DeadlockCycle {
    pub tids: Vec<u32>,
    pub description: String,
}

pub(super) fn read_locks(
    root: &Path,
    architecture: TargetArchitecture,
    endian: Option<TargetEndian>,
    maps: &[ProcessMapping],
) -> LockSnapshot {
    let mut snapshot = LockSnapshot::default();
    let mut thread_names = HashMap::new();
    let task_root = root.join("task");
    let mut unavailable_syscalls = 0;

    let entries = match std::fs::read_dir(&task_root) {
        Ok(entries) => entries,
        Err(error) => {
            snapshot
                .warnings
                .push(format!("Cannot enumerate {}: {error}", task_root.display()));

            return snapshot;
        }
    };

    for (index, entry) in entries.flatten().enumerate() {
        if index == MAX_TASKS {
            snapshot
                .warnings
                .push(format!("Lock inspection was capped at {MAX_TASKS} threads"));

            break;
        }

        let Some(tid) = entry
            .file_name()
            .to_str()
            .and_then(|value| value.parse::<u32>().ok())
        else {
            continue;
        };

        snapshot.threads_scanned += 1;
        let task = entry.path();

        let thread = crate::bounded::read_string(&task.join("comm"), 4096)
            .unwrap_or_default()
            .trim()
            .to_owned();

        thread_names.insert(tid, thread.clone());
        let state = read_thread_state(&task.join("status"));

        let wchan = crate::bounded::read_string(&task.join("wchan"), 4096)
            .unwrap_or_default()
            .trim()
            .to_owned();

        let syscall = crate::bounded::read_string(&task.join("syscall"), 64 * 1024).ok();
        unavailable_syscalls += usize::from(syscall.is_none());

        if let Some(wait) = syscall
            .as_deref()
            .and_then(|line| parse_lock_wait(tid, &thread, &state, &wchan, line, architecture))
        {
            snapshot.waits.push(wait);
        } else if wchan.contains("futex")
            && !syscall
                .as_deref()
                .is_some_and(|line| is_non_waiting_futex_syscall(line, architecture))
        {
            snapshot.waits.push(LockWait {
                tid,
                thread,
                state,
                address: None,
                operation: String::from("futex wait"),
                expected: None,
                details: format!("kernel wait channel {wchan}. Syscall arguments unavailable"),
                operation_flags: None,
                observation: LockObservation::default(),
            });
        }
    }

    snapshot
        .waits
        .sort_by_key(|wait| (wait.address.unwrap_or(u64::MAX), wait.tid));

    snapshot.dependencies = derive_lock_dependencies(
        root,
        &mut snapshot.waits,
        &thread_names,
        endian,
        maps,
        &mut snapshot.warnings,
    );

    snapshot.deadlocks = find_deadlock_cycles(&snapshot.dependencies);

    let cycle_tids = snapshot
        .deadlocks
        .iter()
        .flat_map(|cycle| cycle.tids.iter().copied())
        .collect::<HashSet<_>>();

    for wait in &mut snapshot.waits {
        wait.observation.cycle = cycle_tids.contains(&wait.tid);
    }

    if unavailable_syscalls > 0 {
        snapshot.warnings.push(format!("Syscall arguments were unreadable for {unavailable_syscalls} threads. Some waits and owners may be absent"));
    }

    if architecture == TargetArchitecture::Unknown {
        snapshot.warnings.push(
            "The target architecture is unknown. Futex syscall arguments could not be decoded"
                .into(),
        );
    }

    snapshot
}

fn derive_lock_dependencies(
    root: &Path,
    waits: &mut [LockWait],
    thread_names: &HashMap<u32, String>,
    endian: Option<TargetEndian>,
    maps: &[ProcessMapping],
    warnings: &mut Vec<String>,
) -> Vec<LockDependency> {
    let memory_path = root.join("mem");
    let memory = File::open(&memory_path);
    let mut words = HashMap::new();
    let mut dependencies = Vec::new();

    for wait in waits {
        let Some(address) = wait.address else {
            continue;
        };

        // A contended address is read once, so all of its waiters share the
        // same word observation and additional waiters do not add target reads.
        let (word, mapping) = words.entry(address).or_insert_with(|| {
            let mapping = maps
                .iter()
                .find(|mapping| mapping.start <= address && address < mapping.end)
                .cloned();

            (
                evidence::read_word(memory.as_ref(), address, endian),
                mapping,
            )
        });

        wait.observation.mapping.clone_from(mapping);

        wait.observation.ownership = match word {
            Ok(value) => {
                wait.observation.word = Some(*value);

                evidence::ownership(wait, *value, thread_names)
            }

            Err(reason) => reason.clone(),
        };

        let Some(owner_tid) = wait.observation.ownership.owner() else {
            continue;
        };

        dependencies.push(LockDependency {
            waiter_tid: wait.tid,
            waiter: wait.thread.clone(),
            owner_tid,
            owner: thread_names
                .get(&owner_tid)
                .cloned()
                .unwrap_or_else(|| String::from("<unnamed>")),
            address,
            futex_value: wait.observation.word.unwrap_or_default(),
        });
    }

    dependencies.sort_by_key(|edge| (edge.waiter_tid, edge.owner_tid, edge.address));
    dependencies.dedup();
    let unreadable = words.values().filter(|(word, _)| word.is_err()).count();

    if unreadable > 0 {
        warnings.push(format!("Current futex words are unavailable at {unreadable} wait addresses. Select a waiter for details"));
    }

    dependencies
}

fn find_deadlock_cycles(dependencies: &[LockDependency]) -> Vec<DeadlockCycle> {
    let edges = dependencies
        .iter()
        .map(|edge| (edge.waiter_tid, edge.owner_tid))
        .collect::<HashMap<_, _>>();

    let mut starts = edges.keys().copied().collect::<Vec<_>>();
    starts.sort_unstable();
    let mut canonical_cycles = HashSet::new();
    let mut cycles = Vec::new();
    // Each thread has at most one current wait edge. Retire complete paths
    // instead of traversing every suffix of a long chain again.
    let mut processed = HashSet::new();

    for start in starts {
        if processed.contains(&start) {
            continue;
        }

        let mut path = Vec::new();
        let mut positions = HashMap::new();
        let mut current = start;

        while let Some(&next) = edges.get(&current) {
            if let Some(&position) = positions.get(&current) {
                let mut cycle = path[position..].to_vec();

                let rotation = cycle
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, tid)| *tid)
                    .map(|(index, _)| index)
                    .unwrap_or(0);

                cycle.rotate_left(rotation);

                if canonical_cycles.insert(cycle.clone()) {
                    let mut chain = cycle.iter().map(u32::to_string).collect::<Vec<_>>();
                    chain.push(cycle[0].to_string());

                    cycles.push(DeadlockCycle {
                        tids: cycle,
                        description: format!("TID {}", chain.join(" waits for TID ")),
                    });
                }

                break;
            }

            if processed.contains(&current) {
                break;
            }

            positions.insert(current, path.len());
            path.push(current);
            current = next;
        }

        processed.extend(path);
    }

    cycles.sort_by(|left, right| left.tids.cmp(&right.tids));

    cycles
}

fn read_thread_state(path: &Path) -> String {
    crate::bounded::read_string(path, 64 * 1024)
        .ok()
        .and_then(|text| {
            text.lines()
                .find_map(|line| line.strip_prefix("State:").map(str::trim))
                .map(str::to_owned)
        })
        .unwrap_or_else(|| String::from("unknown"))
}

fn parse_lock_wait(
    tid: u32,
    thread: &str,
    state: &str,
    wchan: &str,
    syscall: &str,
    architecture: TargetArchitecture,
) -> Option<LockWait> {
    let values = syscall
        .split_whitespace()
        .take(7)
        .map(parse_kernel_number)
        .collect::<Option<Vec<_>>>()?;

    let (&number, arguments) = values.split_first()?;
    let name = architecture.syscall_name(number);

    if name != "futex" && name != "futex_waitv" {
        return None;
    }

    if name == "futex_waitv" {
        let vector = arguments.first().copied();
        let count = arguments.get(1).copied();
        let flags = arguments.get(2).copied().unwrap_or(0);

        let mut details = vector.map_or_else(
            || String::from("wait vector address unavailable"),
            |vector| format!("wait vector at 0x{vector:x}"),
        );

        if flags != 0 {
            let _ = write!(details, " with flags 0x{flags:x}");
        }

        if !wchan.is_empty() && wchan != "0" {
            let _ = write!(details, "  {wchan}");
        }

        return Some(LockWait {
            tid,
            thread: thread.to_owned(),
            state: state.to_owned(),
            // This syscall points at an array of futex_waitv descriptors, not
            // at a futex word. Reporting the descriptor array as a lock address
            // could create a false owner edge when its first bytes resemble a
            // thread ID.
            address: None,
            operation: String::from("FUTEX_WAITV"),
            expected: count,
            details,
            operation_flags: Some(flags),
            observation: LockObservation::default(),
        });
    }

    let operation = arguments.get(1).copied().unwrap_or(0);
    let base = operation & 0x7f;

    if !is_futex_wait_operation(base) {
        return None;
    }

    let private = operation & 0x80 != 0;
    let realtime = operation & 0x100 != 0;
    let mut flags = Vec::new();

    if private {
        flags.push("private");
    }

    if realtime {
        flags.push("realtime clock");
    }

    if !wchan.is_empty() && wchan != "0" {
        flags.push(wchan);
    }

    Some(LockWait {
        tid,
        thread: thread.to_owned(),
        state: state.to_owned(),
        address: arguments.first().copied(),
        operation: futex_operation(base).to_owned(),
        expected: matches!(base, 0 | 9 | 11)
            .then(|| arguments.get(2).copied())
            .flatten(),
        details: flags.join("  "),
        operation_flags: Some(operation & !0x7f),
        observation: LockObservation::default(),
    })
}

fn is_non_waiting_futex_syscall(syscall: &str, architecture: TargetArchitecture) -> bool {
    let mut values = syscall.split_whitespace().take(3).map(parse_kernel_number);

    let Some(number) = values.next().flatten() else {
        return false;
    };

    if architecture.syscall_name(number) != "futex" {
        return false;
    }

    let Some(operation) = values.nth(1).flatten() else {
        return false;
    };

    !is_futex_wait_operation(operation & 0x7f)
}

fn is_futex_wait_operation(operation: u64) -> bool {
    matches!(operation, 0 | 6 | 9 | 11 | 13)
}

fn parse_kernel_number(value: &str) -> Option<u64> {
    value.strip_prefix("0x").map_or_else(
        || value.parse().ok(),
        |value| u64::from_str_radix(value, 16).ok(),
    )
}

fn futex_operation(operation: u64) -> &'static str {
    match operation {
        0 => "FUTEX_WAIT",
        1 => "FUTEX_WAKE",
        3 => "FUTEX_REQUEUE",
        4 => "FUTEX_CMP_REQUEUE",
        5 => "FUTEX_WAKE_OP",
        6 => "FUTEX_LOCK_PI",
        7 => "FUTEX_UNLOCK_PI",
        8 => "FUTEX_TRYLOCK_PI",
        9 => "FUTEX_WAIT_BITSET",
        10 => "FUTEX_WAKE_BITSET",
        11 => "FUTEX_WAIT_REQUEUE_PI",
        12 => "FUTEX_CMP_REQUEUE_PI",
        13 => "FUTEX_LOCK_PI2",
        _ => "FUTEX_UNKNOWN",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_mapping_cycle_evidence_and_incomplete_wait_coverage() {
        let root =
            gtk::glib::mkdtemp(std::env::temp_dir().join("fgdb-lock-snapshot-XXXXXX")).unwrap();

        let mut memory = [0_u8; 64];

        for (tid, address, owner) in [(10, 16, 20), (20, 32, 10)] {
            let task = root.join("task").join(tid.to_string());
            std::fs::create_dir_all(&task).unwrap();
            std::fs::write(task.join("comm"), format!("worker-{tid}")).unwrap();
            std::fs::write(task.join("status"), "State:\tT (tracing stop)\n").unwrap();
            std::fs::write(task.join("wchan"), "futex_wait_queue").unwrap();
            let word = 0x8000_0000_u32 | owner;
            memory[address..address + 4].copy_from_slice(&word.to_le_bytes());

            std::fs::write(
                task.join("syscall"),
                format!("202 0x{address:x} 0x80 0x{word:x} 0 0 0"),
            )
            .unwrap();
        }

        std::fs::write(root.join("mem"), memory).unwrap();

        let maps = [ProcessMapping {
            start: 0,
            end: 64,
            permissions: "rw-p".into(),
            path: "[test mapping]".into(),
        }];

        let read = || {
            read_locks(
                &root,
                TargetArchitecture::X86_64,
                Some(TargetEndian::Little),
                &maps,
            )
        };

        let snapshot = read();
        assert_eq!(snapshot.deadlocks.len(), 1);
        assert_eq!(snapshot.deadlocks[0].tids, [10, 20]);
        assert!(snapshot.warnings.is_empty());

        for wait in snapshot.waits {
            assert!(wait.observation.cycle);
            assert!(wait.observation.word.is_some());
            assert_eq!(wait.observation.mapping.as_ref(), Some(&maps[0]));

            assert!(matches!(
                wait.observation.ownership,
                LockOwnership::RobustCandidate(_)
            ));
        }

        std::fs::remove_file(root.join("task/10/syscall")).unwrap();
        std::fs::remove_file(root.join("mem")).unwrap();
        let snapshot = read();
        assert!(snapshot.deadlocks.is_empty());

        assert!(
            snapshot
                .warnings
                .iter()
                .any(|warning| warning.contains("unreadable for 1 threads"))
        );

        assert!(
            snapshot.waits.iter().any(|wait| wait.address.is_none()
                && wait.observation.ownership == LockOwnership::NoAddress)
        );

        assert!(
            snapshot
                .waits
                .iter()
                .any(|wait| matches!(wait.observation.ownership, LockOwnership::Unreadable(_)))
        );

        std::fs::remove_dir_all(root).unwrap();
    }

    fn dependency(waiter_tid: u32, owner_tid: u32) -> LockDependency {
        LockDependency {
            waiter_tid,
            waiter: format!("thread-{waiter_tid}"),
            owner_tid,
            owner: format!("thread-{owner_tid}"),
            address: 0x1000 + u64::from(waiter_tid),
            futex_value: owner_tid,
        }
    }

    #[test]
    fn detects_and_deduplicates_wait_for_cycles() {
        let cycles = find_deadlock_cycles(&[
            dependency(10, 20),
            dependency(20, 30),
            dependency(30, 10),
            dependency(40, 20),
        ]);

        assert_eq!(cycles.len(), 1);
        assert_eq!(cycles[0].tids, [10, 20, 30]);

        assert_eq!(
            cycles[0].description,
            "TID 10 waits for TID 20 waits for TID 30 waits for TID 10"
        );
    }

    #[test]
    fn does_not_report_acyclic_wait_chains_as_deadlocks() {
        assert!(find_deadlock_cycles(&[dependency(10, 20), dependency(30, 20)]).is_empty());
    }

    #[test]
    fn recognizes_only_scanned_thread_ids_as_futex_owners() {
        let root = std::env::temp_dir().join(format!(
            "fgdb-lock-owner-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));

        std::fs::create_dir_all(&root).unwrap();
        let mut memory = [0_u8; 64];
        memory[16..20].copy_from_slice(&0x8000_0014_u32.to_le_bytes());
        std::fs::write(root.join("mem"), memory).unwrap();

        let mut waits = [LockWait {
            tid: 10,
            thread: String::from("waiter"),
            state: String::from("sleeping"),
            address: Some(16),
            operation: String::from("FUTEX_WAIT"),
            expected: Some(0x8000_0014),
            details: String::new(),
            operation_flags: None,
            observation: LockObservation::default(),
        }];

        let names = HashMap::from([(10, String::from("waiter")), (20, String::from("owner"))]);
        let mut warnings = Vec::new();

        let dependencies = derive_lock_dependencies(
            &root,
            &mut waits,
            &names,
            Some(TargetEndian::Little),
            &[],
            &mut warnings,
        );

        std::fs::remove_dir_all(&root).unwrap();
        assert!(warnings.is_empty());
        assert_eq!(dependencies.len(), 1);
        assert_eq!(dependencies[0].waiter_tid, 10);
        assert_eq!(dependencies[0].owner_tid, 20);
        assert_eq!(dependencies[0].futex_value, 0x8000_0014);
    }

    #[test]
    fn does_not_treat_requeue_pi_condition_words_as_mutex_owners() {
        let root = std::env::temp_dir().join(format!(
            "fgdb-lock-requeue-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));

        std::fs::create_dir_all(&root).unwrap();
        let mut memory = [0_u8; 64];
        memory[16..20].copy_from_slice(&0x8000_0014_u32.to_le_bytes());
        std::fs::write(root.join("mem"), memory).unwrap();

        let mut waits = [LockWait {
            tid: 10,
            thread: String::from("waiter"),
            state: String::from("sleeping"),
            address: Some(16),
            operation: String::from("FUTEX_WAIT_REQUEUE_PI"),
            expected: Some(0x8000_0014),
            details: String::new(),
            operation_flags: None,
            observation: LockObservation::default(),
        }];

        let names = HashMap::from([(10, String::from("waiter")), (20, String::from("owner"))]);

        let dependencies = derive_lock_dependencies(
            &root,
            &mut waits,
            &names,
            Some(TargetEndian::Little),
            &[],
            &mut Vec::new(),
        );

        std::fs::remove_dir_all(&root).unwrap();
        assert!(dependencies.is_empty());
    }

    #[test]
    fn parses_futex_wait_and_preserves_flags() {
        let wait = parse_lock_wait(
            17,
            "worker",
            "S (sleeping)",
            "futex_wait_queue",
            "202 0x1234 0x80 7 0 0 0 0 0",
            TargetArchitecture::X86_64,
        )
        .unwrap();

        assert_eq!(wait.address, Some(0x1234));
        assert_eq!(wait.operation, "FUTEX_WAIT");
        assert_eq!(wait.expected, Some(7));
        assert!(wait.details.contains("private"));
    }

    #[test]
    fn does_not_treat_a_futex_wait_vector_as_a_lock_word() {
        let wait = parse_lock_wait(
            17,
            "worker",
            "S (sleeping)",
            "futex_wait_multiple",
            "449 0x12340000 3 0x2 0 0 0",
            TargetArchitecture::X86_64,
        )
        .unwrap();

        assert_eq!(wait.operation, "FUTEX_WAITV");
        assert_eq!(wait.address, None);
        assert_eq!(wait.expected, Some(3));
        assert!(wait.details.contains("0x12340000"));
        assert!(wait.details.contains("flags 0x2"));
    }

    #[test]
    fn ignores_non_blocking_futex_operations() {
        let syscall = "202 0x12340000 1 1 0 0 0";

        assert!(
            parse_lock_wait(
                17,
                "worker",
                "R (running)",
                "futex_wake",
                syscall,
                TargetArchitecture::X86_64,
            )
            .is_none()
        );

        assert!(is_non_waiting_futex_syscall(
            syscall,
            TargetArchitecture::X86_64
        ));
    }
}
