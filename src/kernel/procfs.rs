use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct VerifiedProcTarget {
    root: PathBuf,
    pid: u32,
    debugger_pid: u32,
    start_time: u64,
}

impl VerifiedProcTarget {
    fn establish(pid: u32, debugger_pid: u32) -> Result<Self, String> {
        let root = PathBuf::from(format!("/proc/{pid}"));
        let start_time = observe_identity(&root, pid, debugger_pid, None, "checked")?;
        Ok(Self {
            root,
            pid,
            debugger_pid,
            start_time,
        })
    }

    pub(super) fn root(&self) -> &Path {
        &self.root
    }

    pub(super) fn start_time(&self) -> u64 {
        self.start_time
    }

    fn revalidate(&self) -> Result<(), String> {
        observe_identity(
            &self.root,
            self.pid,
            self.debugger_pid,
            Some(self.start_time),
            "data was being read",
        )?;
        Ok(())
    }
}

pub(crate) fn verified_proc_root(pid: u32, debugger_pid: u32) -> Result<PathBuf, String> {
    VerifiedProcTarget::establish(pid, debugger_pid).map(|target| target.root)
}

pub(super) fn read_verified_local_proc<T>(
    pid: u32,
    debugger_pid: u32,
    read: impl FnOnce(&VerifiedProcTarget) -> Result<T, String>,
) -> Result<T, String> {
    let target = VerifiedProcTarget::establish(pid, debugger_pid)?;
    let value = read(&target)?;
    target.revalidate()?;
    Ok(value)
}

/// Returns the ABI encoded by the traced process' executable.
///
/// GDB often reports its architecture and byte order as `auto`, especially
/// before the inferior has stopped. Reading the verified `/proc` entry gives
/// local sessions an authoritative fallback without trusting an arbitrary PID.
pub(crate) fn read_local_target_abi(
    pid: u32,
    debugger_pid: u32,
) -> Option<(
    crate::debugger::TargetArchitecture,
    crate::debugger::TargetEndian,
    u32,
)> {
    read_verified_local_proc(pid, debugger_pid, |target| {
        let bytes = crate::bounded::read_prefix(&target.root().join("exe"), 40)
            .map_err(|error| format!("Cannot inspect /proc/{pid}/exe: {error}"))?;
        crate::debugger::TargetArchitecture::from_elf_ident(&bytes)
            .ok_or_else(|| format!("Cannot identify the executable ABI for process {pid}"))
    })
    .ok()
}

pub(crate) fn read_local_parent_pid(pid: u32, debugger_pid: u32) -> Option<u32> {
    read_verified_local_proc(pid, debugger_pid, |target| {
        let status = crate::bounded::read_string(&target.root().join("status"), 1024 * 1024)
            .map_err(|error| format!("Cannot inspect /proc/{pid}/status: {error}"))?;
        status
            .lines()
            .find_map(|line| line.strip_prefix("PPid:"))
            .and_then(|value| value.trim().parse().ok())
            .ok_or_else(|| format!("/proc/{pid}/status did not expose PPid"))
    })
    .ok()
}

fn observe_identity(
    root: &Path,
    pid: u32,
    debugger_pid: u32,
    expected_start_time: Option<u64>,
    operation: &str,
) -> Result<u64, String> {
    let before = super::process::read_proc_stat(&root.join("stat")).map(|stat| stat.start_time);
    let status = crate::bounded::read_string(&root.join("status"), 1024 * 1024)
        .map_err(|error| format!("Cannot inspect /proc/{pid}/status: {error}"))?;
    let tracer = tracer_pid(&status);
    let after = super::process::read_proc_stat(&root.join("stat")).map(|stat| stat.start_time);
    validate_identity_observation(
        pid,
        debugger_pid,
        expected_start_time,
        before,
        tracer,
        after,
        operation,
    )
}

fn tracer_pid(status: &str) -> Option<u32> {
    status
        .lines()
        .find_map(|line| line.strip_prefix("TracerPid:"))
        .and_then(|value| value.trim().parse().ok())
}

fn validate_identity_observation(
    pid: u32,
    debugger_pid: u32,
    expected_start_time: Option<u64>,
    before_start_time: Option<u64>,
    tracer: Option<u32>,
    after_start_time: Option<u64>,
    operation: &str,
) -> Result<u64, String> {
    let before = before_start_time
        .ok_or_else(|| format!("Cannot establish the identity of process {pid}"))?;
    let tracer = tracer.ok_or_else(|| format!("/proc/{pid}/status did not expose TracerPid"))?;
    if tracer != debugger_pid {
        return Err(format!(
            "PID {pid} is not a local inferior traced by this GDB process (expected tracer {debugger_pid}, found {tracer})"
        ));
    }
    let after = after_start_time
        .ok_or_else(|| format!("Process {pid} disappeared while its {operation}"))?;
    if before != after || expected_start_time.is_some_and(|expected| expected != before) {
        return Err(format!("Process {pid} changed while its {operation}"));
    }
    Ok(before)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_a_stable_identity_owned_by_the_expected_tracer() {
        assert_eq!(
            validate_identity_observation(7, 42, Some(99), Some(99), Some(42), Some(99), "read"),
            Ok(99)
        );
    }

    #[test]
    fn rejects_changed_process_generations() {
        assert!(
            validate_identity_observation(7, 42, Some(98), Some(99), Some(42), Some(99), "read")
                .unwrap_err()
                .contains("changed")
        );
        assert!(
            validate_identity_observation(7, 42, Some(99), Some(99), Some(42), Some(100), "read")
                .unwrap_err()
                .contains("changed")
        );
    }

    #[test]
    fn rejects_tracer_changes() {
        assert!(
            validate_identity_observation(7, 42, Some(99), Some(99), Some(41), Some(99), "read")
                .unwrap_err()
                .contains("found 41")
        );
    }

    #[test]
    fn reports_disappearing_targets_without_panicking() {
        assert!(
            validate_identity_observation(7, 42, Some(99), Some(99), Some(42), None, "read")
                .unwrap_err()
                .contains("disappeared")
        );
    }
}
