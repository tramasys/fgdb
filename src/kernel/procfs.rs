use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::SystemTime,
};

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

const MAX_TARGET_ABI_CACHE_ENTRIES: usize = 32;

type TargetAbi = (
    crate::debugger::TargetArchitecture,
    crate::debugger::TargetEndian,
    u32,
);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct VerifiedProcTarget {
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

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn start_time(&self) -> u64 {
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

pub(crate) fn read_verified_local_proc<T>(
    pid: u32,
    debugger_pid: u32,
    read: impl FnOnce(&VerifiedProcTarget) -> Result<T, String>,
) -> Result<T, String> {
    let target = VerifiedProcTarget::establish(pid, debugger_pid)?;

    finish_verified_read(|| read(&target), || target.revalidate())
}

fn finish_verified_read<T>(
    read: impl FnOnce() -> Result<T, String>,
    revalidate: impl FnOnce() -> Result<(), String>,
) -> Result<T, String> {
    let value = read()?;
    revalidate()?;

    Ok(value)
}

/// Returns the ABI encoded by the traced process' executable.
///
/// GDB often reports its architecture and byte order as `auto`, especially
/// before the inferior has stopped. Reading the verified `/proc` entry gives
/// local sessions an authoritative fallback without trusting an arbitrary PID.
pub(crate) fn read_local_target_abi(pid: u32, debugger_pid: u32) -> Option<TargetAbi> {
    read_verified_local_proc(pid, debugger_pid, |target| {
        let executable = target.root().join("exe");

        let identity = TargetAbiIdentity::read(pid, target.start_time(), &executable)
            .map_err(|error| format!("Cannot inspect /proc/{pid}/exe: {error}"))?;

        target_abi_cache()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .resolve(identity.clone(), || {
                let bytes = crate::bounded::read_prefix(&executable, 40)
                    .map_err(|error| format!("Cannot inspect /proc/{pid}/exe: {error}"))?;

                let abi = crate::debugger::TargetArchitecture::from_elf_ident(&bytes).ok_or_else(
                    || format!("Cannot identify the executable ABI for process {pid}"),
                )?;

                let after = TargetAbiIdentity::read(pid, target.start_time(), &executable)
                    .map_err(|error| format!("Cannot recheck /proc/{pid}/exe: {error}"))?;

                if after != identity {
                    return Err(format!(
                        "Process {pid} changed executable while its ABI was being read"
                    ));
                }

                Ok(abi)
            })
    })
    .ok()
}

pub(crate) fn invalidate_local_target_abi_cache() {
    target_abi_cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clear();
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TargetAbiIdentity {
    pid: u32,
    start_time: u64,
    executable_size: u64,
    executable_modified: SystemTime,
    #[cfg(unix)]
    executable_device: u64,
    #[cfg(unix)]
    executable_inode: u64,
}

impl TargetAbiIdentity {
    fn read(pid: u32, start_time: u64, executable: &Path) -> std::io::Result<Self> {
        let metadata = std::fs::metadata(executable)?;

        Ok(Self {
            pid,
            start_time,
            executable_size: metadata.len(),
            executable_modified: metadata.modified()?,
            #[cfg(unix)]
            executable_device: metadata.dev(),
            #[cfg(unix)]
            executable_inode: metadata.ino(),
        })
    }
}

struct TargetAbiCache {
    entries: VecDeque<(TargetAbiIdentity, TargetAbi)>,
    capacity: usize,
}

impl TargetAbiCache {
    fn new(capacity: usize) -> Self {
        Self {
            entries: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    fn resolve(
        &mut self,
        identity: TargetAbiIdentity,
        load: impl FnOnce() -> Result<TargetAbi, String>,
    ) -> Result<TargetAbi, String> {
        if let Some(index) = self
            .entries
            .iter()
            .position(|(cached, _)| cached == &identity)
        {
            let entry = self.entries.remove(index).expect("cache index is valid");
            let abi = entry.1;
            self.entries.push_back(entry);
            return Ok(abi);
        }

        let abi = load()?;

        if self.capacity > 0 {
            self.entries.push_back((identity, abi));

            while self.entries.len() > self.capacity {
                self.entries.pop_front();
            }
        }

        Ok(abi)
    }

    fn clear(&mut self) {
        self.entries.clear();
    }
}

fn target_abi_cache() -> &'static Mutex<TargetAbiCache> {
    static CACHE: OnceLock<Mutex<TargetAbiCache>> = OnceLock::new();

    CACHE.get_or_init(|| Mutex::new(TargetAbiCache::new(MAX_TARGET_ABI_CACHE_ENTRIES)))
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

    fn abi_identity(pid: u32, start_time: u64, executable_inode: u64) -> TargetAbiIdentity {
        TargetAbiIdentity {
            pid,
            start_time,
            executable_size: 4096,
            executable_modified: SystemTime::UNIX_EPOCH,
            #[cfg(unix)]
            executable_device: 1,
            #[cfg(unix)]
            executable_inode,
        }
    }

    fn test_abi() -> TargetAbi {
        (
            crate::debugger::TargetArchitecture::X86_64,
            crate::debugger::TargetEndian::Little,
            64,
        )
    }

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

    #[test]
    fn discards_a_read_when_post_read_identity_validation_fails() {
        let result = finish_verified_read(
            || Ok(String::from("untrusted maps contents")),
            || {
                validate_identity_observation(
                    7,
                    42,
                    Some(99),
                    Some(100),
                    Some(42),
                    Some(100),
                    "data was being read",
                )
                .map(drop)
            },
        );

        assert!(result.unwrap_err().contains("changed"));
    }

    #[test]
    fn target_abi_cache_uses_process_and_executable_identity() {
        let loads = std::cell::Cell::new(0);
        let mut cache = TargetAbiCache::new(8);

        let load = || {
            loads.set(loads.get() + 1);

            Ok(test_abi())
        };

        let original = abi_identity(7, 99, 100);
        assert_eq!(cache.resolve(original.clone(), load), Ok(test_abi()));
        assert_eq!(cache.resolve(original, load), Ok(test_abi()));
        assert_eq!(loads.get(), 1);

        assert_eq!(
            cache.resolve(abi_identity(7, 100, 100), load),
            Ok(test_abi())
        );

        assert_eq!(
            cache.resolve(abi_identity(8, 100, 100), load),
            Ok(test_abi())
        );

        assert_eq!(
            cache.resolve(abi_identity(8, 100, 101), load),
            Ok(test_abi())
        );

        assert_eq!(loads.get(), 4, "start, PID, and exec changes must miss");
    }

    #[test]
    fn failed_abi_reads_do_not_poison_the_cache_and_invalidation_forces_a_miss() {
        let identity = abi_identity(7, 99, 100);
        let mut cache = TargetAbiCache::new(8);

        assert!(
            cache
                .resolve(identity.clone(), || Err(String::from("temporary failure")))
                .is_err()
        );

        assert_eq!(
            cache.resolve(identity.clone(), || Ok(test_abi())),
            Ok(test_abi())
        );

        cache.clear();
        let loads = std::cell::Cell::new(0);

        assert_eq!(
            cache.resolve(identity, || {
                loads.set(loads.get() + 1);

                Ok(test_abi())
            }),
            Ok(test_abi())
        );

        assert_eq!(loads.get(), 1);
    }

    #[test]
    fn target_abi_cache_is_bounded() {
        let mut cache = TargetAbiCache::new(2);

        for inode in 1..=3 {
            assert_eq!(
                cache.resolve(abi_identity(inode, 1, u64::from(inode)), || Ok(test_abi())),
                Ok(test_abi())
            );
        }

        assert_eq!(cache.entries.len(), 2);
        assert!(cache.entries.iter().all(|(identity, _)| identity.pid != 1));
    }
}
