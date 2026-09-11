use std::{
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

use crate::{bounded::FileIdentity, performance::BoundedLruCache};

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

        target_abi_cache().resolve(identity.clone(), || {
            let bytes = crate::bounded::read_prefix(&executable, 40)
                .map_err(|error| format!("Cannot inspect /proc/{pid}/exe: {error}"))?;

            let abi = crate::debugger::TargetArchitecture::from_elf_ident(&bytes)
                .ok_or_else(|| format!("Cannot identify the executable ABI for process {pid}"))?;

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
    target_abi_cache().clear();
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct TargetAbiIdentity {
    pid: u32,
    start_time: u64,
    executable: FileIdentity,
}

impl TargetAbiIdentity {
    fn read(pid: u32, start_time: u64, executable: &Path) -> std::io::Result<Self> {
        Ok(Self {
            pid,
            start_time,
            executable: FileIdentity::read(executable)?,
        })
    }
}

struct TargetAbiCache {
    state: Mutex<TargetAbiCacheState>,
}

struct TargetAbiCacheState {
    entries: BoundedLruCache<TargetAbiIdentity, TargetAbi>,
    generation: u64,
}

impl TargetAbiCache {
    fn new(capacity: usize) -> Self {
        Self {
            state: Mutex::new(TargetAbiCacheState {
                entries: BoundedLruCache::new(capacity),
                generation: 0,
            }),
        }
    }

    fn resolve(
        &self,
        identity: TargetAbiIdentity,
        load: impl FnOnce() -> Result<TargetAbi, String>,
    ) -> Result<TargetAbi, String> {
        let generation = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);

            if let Some(abi) = state.entries.get_cloned(&identity) {
                return Ok(abi);
            }

            state.generation
        };

        // Target reset clears this cache from GTK. Never hold its mutex during
        // filesystem work, or publish a read that crossed that invalidation.
        let abi = load()?;

        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        if state.generation == generation {
            state.entries.insert(identity, abi);
        }

        Ok(abi)
    }

    fn clear(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        state.generation = state.generation.wrapping_add(1);
        state.entries.clear();
    }
}

fn target_abi_cache() -> &'static TargetAbiCache {
    static CACHE: OnceLock<TargetAbiCache> = OnceLock::new();

    CACHE.get_or_init(|| TargetAbiCache::new(MAX_TARGET_ABI_CACHE_ENTRIES))
}

pub(crate) fn read_local_parent_pid(pid: u32, debugger_pid: u32) -> Option<u32> {
    read_verified_local_proc(pid, debugger_pid, |target| {
        let status = crate::bounded::read_bytes(&target.root().join("status"), 1024 * 1024)
            .map_err(|error| format!("Cannot inspect /proc/{pid}/status: {error}"))?;

        String::from_utf8_lossy(&status)
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

    let status = crate::bounded::read_bytes(&root.join("status"), 1024 * 1024)
        .map_err(|error| format!("Cannot inspect /proc/{pid}/status: {error}"))?;

    let tracer = tracer_pid(&String::from_utf8_lossy(&status));
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
    fn non_utf8_names_do_not_hide_the_tracer_or_process_identity() {
        let root =
            gtk::glib::mkdtemp(std::env::temp_dir().join("fgdb-proc-identity-XXXXXX")).unwrap();

        std::fs::write(
            root.join("stat"),
            b"42 (bad\xffname) t 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 99 0",
        )
        .unwrap();

        std::fs::write(
            root.join("status"),
            b"Name:\tbad\xffname\nTracerPid:\t17\nUid:\t1000\n",
        )
        .unwrap();

        assert_eq!(
            observe_identity(&root, 42, 17, Some(99), "checked").unwrap(),
            99
        );

        let values = super::super::process::read_key_values(&root.join("status")).unwrap();
        assert_eq!(values.get("TracerPid").map(String::as_str), Some("17"));
        std::fs::remove_dir_all(root).unwrap();
    }

    fn abi_identity(pid: u32, start_time: u64, executable_inode: u64) -> TargetAbiIdentity {
        TargetAbiIdentity {
            pid,
            start_time,
            executable: FileIdentity {
                path: PathBuf::from("/target"),
                size: 4096,
                modified: std::time::SystemTime::UNIX_EPOCH,
                device: 1,
                inode: executable_inode,
                changed: (0, 0),
            },
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
        let cache = TargetAbiCache::new(8);

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
        let cache = TargetAbiCache::new(8);

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
        let cache = TargetAbiCache::new(2);

        for inode in 1..=3 {
            assert_eq!(
                cache.resolve(abi_identity(inode, 1, u64::from(inode)), || Ok(test_abi())),
                Ok(test_abi())
            );
        }

        let state = cache.state.lock().unwrap();
        assert_eq!(state.entries.keys().count(), 2);
        assert!(state.entries.keys().all(|identity| identity.pid != 1));
    }

    #[test]
    fn abi_loading_does_not_lock_out_reset_or_repopulate_an_invalidated_cache() {
        let cache = TargetAbiCache::new(2);
        let identity = abi_identity(7, 99, 100);

        assert_eq!(
            cache.resolve(identity.clone(), || {
                assert!(cache.state.try_lock().is_ok());
                cache.clear();
                Ok(test_abi())
            }),
            Ok(test_abi())
        );

        assert_eq!(cache.state.lock().unwrap().entries.keys().count(), 0);
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
}
