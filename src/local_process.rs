//! Host process discovery and attach validation, independent of GTK and GDB.

mod procfs;

use std::{
    collections::HashMap,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

pub(crate) use procfs::{capture_identity, validate_identity};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ProcessIdentity {
    pub pid: u32,
    pub start_time: u64,
}

#[derive(PartialEq, Eq)]
pub(crate) struct Process {
    pub identity: ProcessIdentity,
    pub uid: Option<u32>,
    pub executable: String,
    pub command: String,
    pub owner: String,
    pub detail: String,
    pub search: String,
}

impl Process {
    pub(crate) fn executable_name(&self) -> &str {
        self.executable
            .rsplit('/')
            .next()
            .unwrap_or(&self.executable)
    }
}

pub(crate) struct Snapshot {
    pub processes: Vec<Process>,
    pub skipped: usize,
    pub limited: bool,
}

pub(crate) fn scan(cancelled: &AtomicBool, excluded: &[u32]) -> Result<Snapshot, String> {
    scan_at(Path::new("/proc"), cancelled, excluded)
}

fn scan_at(root: &Path, cancelled: &AtomicBool, excluded: &[u32]) -> Result<Snapshot, String> {
    const MAX_PROCESSES: usize = 32_768;
    const MAX_TEXT_BYTES: usize = 32 * 1024 * 1024;
    let entries =
        std::fs::read_dir(root).map_err(|error| format!("Cannot list local processes: {error}"))?;

    let owners = local_owners();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut bytes = 0;
    let mut snapshot = Snapshot {
        processes: Vec::new(),
        skipped: 0,
        limited: false,
    };

    for entry in entries {
        if cancelled.load(Ordering::Relaxed) {
            return Err(String::from("Process discovery cancelled"));
        }

        if Instant::now() >= deadline
            || snapshot.processes.len() >= MAX_PROCESSES
            || bytes >= MAX_TEXT_BYTES
        {
            snapshot.limited = true;
            break;
        }

        let Ok(entry) = entry else {
            snapshot.skipped += 1;
            continue;
        };

        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
            .filter(|pid| *pid > 0 && *pid <= i32::MAX as u32)
        else {
            continue;
        };

        if excluded.contains(&pid) {
            continue;
        }

        match procfs::read_process(&entry.path(), pid, &owners) {
            Ok(process) => {
                bytes += process.executable.len()
                    + process.command.len()
                    + process.owner.len()
                    + process.detail.len()
                    + process.search.len();
                snapshot.processes.push(process);
            }
            Err(_) => snapshot.skipped += 1,
        }
    }

    snapshot
        .processes
        .sort_unstable_by_key(|process| process.identity.pid);

    Ok(snapshot)
}

fn local_owners() -> HashMap<u32, String> {
    let Ok(text) = crate::bounded::read_string(Path::new("/etc/passwd"), 1024 * 1024) else {
        return HashMap::new();
    };

    text.lines()
        .filter_map(|line| {
            let mut fields = line.split(':');
            let name = fields.next()?;
            fields.next()?;
            let uid = fields.next()?.parse().ok()?;
            Some((uid, procfs::display_text(name)))
        })
        .collect()
}

/// Preserve the backend's reason and add guidance, never change host policy.
pub(crate) fn attach_error(pid: u32, reason: &str) -> String {
    let lower = reason.to_ascii_lowercase();
    let mut message = format!("Could not attach to PID {pid}: {reason}");

    if lower.contains("operation not permitted") || lower.contains("permission denied") {
        message.push_str("\nAttach requires ptrace permission. The same owner alone may not be sufficient. Check whether another debugger is attached, whether the process is dumpable, and any container or security-policy restrictions.");

        let scope =
            crate::bounded::read_string(Path::new("/proc/sys/kernel/yama/ptrace_scope"), 32).ok();

        match scope.as_deref().map(str::trim) {
            Some("1") => message.push_str("\nYama ptrace_scope is 1. Launch the program through fgdb, or have the target explicitly authorize this debugger with PR_SET_PTRACER. Otherwise an administrator must grant appropriate ptrace permission."),
            Some("2") => message.push_str("\nYama ptrace_scope is 2. The debugger needs CAP_SYS_PTRACE in the target's user namespace."),
            Some("3") => message.push_str("\nYama ptrace_scope is 3. This host forbids attach, including for privileged debuggers."),
            _ => {}
        }
    } else if lower.contains("no such process") {
        message.push_str("\nThe process may have exited or may be outside the debugger's PID namespace. Refresh the process list and select it again.");
    }

    message
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_is_bounded_cancellable_and_does_not_need_a_debugger() {
        let cancelled = AtomicBool::new(false);
        let snapshot = scan(&cancelled, &[]).unwrap();
        let own = snapshot
            .processes
            .iter()
            .find(|process| process.identity.pid == std::process::id())
            .unwrap();

        assert_eq!(own.uid, Some(rustix::process::getuid().as_raw()));
        assert!(!own.executable.is_empty());
        assert!(!own.command.is_empty());
        assert!(own.search.contains(&std::process::id().to_string()));
        cancelled.store(true, Ordering::Relaxed);
        assert!(scan(&cancelled, &[]).is_err());
    }

    #[test]
    fn attach_errors_retain_the_backend_reason() {
        let message = attach_error(42, "ptrace: Operation not permitted");
        assert!(message.contains("PID 42"));
        assert!(message.contains("Operation not permitted"));
        assert!(message.contains("same owner alone"));
        assert!(attach_error(42, "No such process").contains("Refresh"));
        assert_eq!(
            attach_error(42, "unexpected response"),
            "Could not attach to PID 42: unexpected response"
        );
    }
}
