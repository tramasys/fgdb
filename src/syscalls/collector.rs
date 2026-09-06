use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
};

use super::Key;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Target {
    pub(crate) pid: u32,
    pub(crate) debugger_pid: u32,
}

pub(crate) enum Phase {
    Collecting,
    Stopped(String),
    Unavailable(String),
}

#[derive(Default)]
pub(crate) struct Counts {
    pub(crate) entries: Vec<(Key, u64)>,
    pub(crate) lost: u64,
}

pub(crate) struct Snapshot {
    pub(crate) counts: Option<Counts>,
    pub(crate) phase: Phase,
    pub(crate) revision: u64,
}

#[derive(Default)]
struct Shared {
    stop: AtomicBool,
    revision: AtomicU64,
    latest: Mutex<Option<Snapshot>>,
}

#[derive(Default)]
#[cfg(any(syscall_bpf, test))]
struct Baseline {
    entries: std::collections::HashMap<Key, u64>,
    lost: u64,
    revision: u64,
}

#[cfg(any(syscall_bpf, test))]
impl Baseline {
    fn project(&mut self, counts: &mut Counts, revision: u64) {
        if revision != self.revision {
            self.entries.clear();
            self.entries.extend(counts.entries.iter().copied());
            self.lost = counts.lost;
            self.revision = revision;
        }

        for (key, count) in &mut counts.entries {
            *count = count.saturating_sub(self.entries.get(key).copied().unwrap_or_default());
        }

        counts.lost = counts.lost.saturating_sub(self.lost);
    }
}

impl Shared {
    fn publish(&self, snapshot: Snapshot) {
        *self
            .latest
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(snapshot);
    }
}

pub(crate) struct Collector {
    shared: Arc<Shared>,
    worker: JoinHandle<()>,
}

impl Collector {
    pub(crate) fn start(target: Target) -> Result<Self, String> {
        let shared = Arc::new(Shared::default());
        let state = Arc::clone(&shared);

        let worker = thread::Builder::new()
            .name("fgdb-syscalls".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(|| collect(target, &state))
                    .unwrap_or_else(|_| Err("The syscall collector stopped unexpectedly".into()));

                if let Err(error) = result {
                    state.publish(Snapshot {
                        counts: None,
                        phase: Phase::Unavailable(error),
                        revision: state.revision.load(Ordering::Acquire),
                    });
                }
            })
            .map_err(|error| format!("Cannot start the syscall collector: {error}"))?;

        Ok(Self { shared, worker })
    }

    pub(crate) fn take_snapshot(&self) -> Option<Snapshot> {
        match self.shared.latest.try_lock() {
            Ok(mut latest) => latest.take(),
            Err(std::sync::TryLockError::Poisoned(error)) => error.into_inner().take(),
            Err(std::sync::TryLockError::WouldBlock) => None,
        }
    }

    pub(crate) fn reset(&self) -> u64 {
        let revision = self.shared.revision.fetch_add(1, Ordering::AcqRel) + 1;
        self.worker.thread().unpark();
        revision
    }

    pub(crate) fn stop(&self) {
        self.shared.stop.store(true, Ordering::Release);
        self.worker.thread().unpark();
    }

    pub(crate) fn is_finished(&self) -> bool {
        self.worker.is_finished()
    }
}

impl Drop for Collector {
    fn drop(&mut self) {
        // Never join a loading BPF program on GTK's main thread. The worker owns
        // every kernel handle and drops them before terminating.
        self.stop();
    }
}

#[cfg(not(syscall_bpf))]
fn collect(_target: Target, _shared: &Shared) -> Result<(), String> {
    Err("Syscall collection currently supports local x86_64 and aarch64 Linux hosts".into())
}

#[cfg(syscall_bpf)]
fn collect(target: Target, shared: &Shared) -> Result<(), String> {
    use std::{os::unix::fs::MetadataExt, time::Duration};

    use libbpf_rs::{MapCore, MapFlags, ObjectBuilder};

    use super::Abi;

    // Reject time namespaces with offsets rather than comparing different clock
    // domains for the kernel and /proc process identities.
    let offsets = match crate::bounded::read_string(
        std::path::Path::new("/proc/self/timens_offsets"),
        4096,
    ) {
        Ok(offsets) => offsets,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => {
            return Err(format!(
                "Cannot verify the process clock namespace: {error}"
            ));
        }
    };

    for line in offsets.lines() {
        let mut fields = line.split_whitespace();

        if fields.next() == Some("boottime")
            && (fields.next() != Some("0") || fields.next() != Some("0"))
        {
            return Err("Syscall collection is unavailable in an offset time namespace".into());
        }
    }

    let (started, namespace, namespace_pid) =
        crate::kernel::read_verified_local_proc(target.pid, target.debugger_pid, |process| {
            let namespace = std::fs::metadata(process.root().join("ns/pid"))
                .map_err(|error| format!("Cannot inspect the process PID namespace: {error}"))?;

            let status =
                crate::bounded::read_string(&process.root().join("status"), 1024 * 1024)
                    .map_err(|error| format!("Cannot inspect the process identity: {error}"))?;

            let namespace_pid = status
                .lines()
                .find_map(|line| line.strip_prefix("NSpid:"))
                .and_then(|line| line.split_whitespace().last())
                .and_then(|value| value.parse::<u64>().ok())
                .ok_or("The kernel did not expose the process namespace identity")?;

            Ok((process.start_time(), namespace, namespace_pid))
        })?;

    let cpus = libbpf_rs::num_possible_cpus().map_err(|error| error.to_string())?;
    let bytes_per_cpu = (3 * 1024 + 1 + 128) * 8;

    if cpus.saturating_mul(bytes_per_cpu) > 16 * 1024 * 1024 {
        return Err("The syscall counters would exceed their 16 MiB per-CPU data budget".into());
    }

    let ticks = rustix::param::clock_ticks_per_second();

    if ticks == 0 || 1_000_000_000 % ticks != 0 {
        return Err(
            "The process clock resolution is not supported by the syscall collector".into(),
        );
    }

    let config: Vec<u8> = [
        namespace.dev(),
        namespace.ino(),
        started,
        1_000_000_000 / ticks,
        namespace_pid,
    ]
    .into_iter()
    .flat_map(u64::to_ne_bytes)
    .collect();

    let object = ObjectBuilder::default()
        .open_memory(include_bytes!(concat!(env!("OUT_DIR"), "/syscalls.bpf.o")))
        .and_then(|open| open.load())
        .map_err(|error| format!(
            "Cannot load eBPF syscall counters: {error:#}. The kernel must expose BTF and allow BPF tracing. Permission is normally controlled by CAP_BPF and CAP_PERFMON or administrator policy. fgdb will not elevate privileges or use GDB catchpoints"
        ))?;

    if shared.stop.load(Ordering::Acquire) {
        shared.publish(Snapshot {
            counts: None,
            phase: Phase::Stopped("Collection cancelled".into()),
            revision: 0,
        });

        return Ok(());
    }

    let config_map = object
        .maps()
        .find(|map| map.name() == "config")
        .ok_or("Missing syscall configuration map")?;

    config_map
        .update(&0_u32.to_ne_bytes(), &config, MapFlags::ANY)
        .map_err(|error| error.to_string())?;

    libbpf_rs::MapHandle::try_from(&config_map)
        .and_then(|map| map.freeze())
        .map_err(|error| error.to_string())?;

    let link = object
        .progs_mut()
        .find(|program| program.name() == "count_entry")
        .ok_or("Missing syscall trace program")?
        .attach_raw_tracepoint("sys_enter")
        .map_err(|error| format!("Cannot attach the syscall entry tracepoint: {error:#}"))?;

    let dense = object
        .maps()
        .find(|map| map.name() == "counts")
        .ok_or("Missing syscall counters")?;

    let unusual = object
        .maps()
        .find(|map| map.name() == "unusual")
        .ok_or("Missing unknown syscall counters")?;

    let abis = Abi::host_abis();
    let mut baseline = Baseline::default();
    let mut link = Some(link);

    loop {
        let reason = if shared.stop.load(Ordering::Acquire) {
            Some("Collection stopped".to_owned())
        } else {
            crate::kernel::read_verified_local_proc(target.pid, target.debugger_pid, |process| {
                if process.start_time() == started {
                    Ok(())
                } else {
                    Err("The process was replaced".into())
                }
            })
            .err()
            .map(|error| format!("Collection ended: {error}"))
        };

        // Detach before the last read, including on process exit or cancellation.
        if reason.is_some() {
            drop(link.take());
        }

        let values = dense
            .lookup_percpu(&0_u32.to_ne_bytes(), MapFlags::ANY)
            .map_err(|error| format!("Cannot read syscall counters: {error:#}"))?
            .ok_or("The syscall counters disappeared")?;

        let mut sums = [0_u64; 3 * 1024 + 1];

        for cpu in values {
            for (sum, bytes) in sums.iter_mut().zip(cpu.as_chunks::<8>().0) {
                *sum = sum.saturating_add(u64::from_ne_bytes(*bytes));
            }
        }

        let mut counts = Counts {
            entries: Vec::new(),
            lost: sums[3 * 1024],
        };

        for (bank, abi) in abis.iter().copied().enumerate() {
            for (number, count) in sums[bank * 1024..(bank + 1) * 1024]
                .iter()
                .copied()
                .enumerate()
            {
                if count != 0 {
                    let number = number as u64 | if abi == Abi::X32 { 0x40000000 } else { 0 };
                    counts.entries.push((Key { abi, number }, count));
                }
            }
        }

        for key in unusual.keys().take(128) {
            let number = u64::from_ne_bytes(key[..8].try_into().unwrap());
            let bank = u64::from_ne_bytes(key[8..].try_into().unwrap()) as usize;
            let abi = *abis.get(bank).ok_or("Invalid syscall ABI counter")?;

            let values = unusual
                .lookup_percpu(&key, MapFlags::ANY)
                .map_err(|error| format!("Cannot read unknown syscall counters: {error:#}"))?;

            let count = values.unwrap_or_default().iter().fold(0_u64, |sum, bytes| {
                sum.saturating_add(u64::from_ne_bytes(bytes.as_slice().try_into().unwrap()))
            });

            counts.entries.push((Key { abi, number }, count));
        }

        let requested_revision = shared.revision.load(Ordering::Acquire);

        baseline.project(&mut counts, requested_revision);

        shared.publish(Snapshot {
            counts: Some(counts),
            phase: reason.clone().map_or(Phase::Collecting, Phase::Stopped),
            revision: requested_revision,
        });

        if reason.is_some() {
            return Ok(());
        }

        thread::park_timeout(Duration::from_secs(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syscalls::Abi;

    #[test]
    fn reset_is_abi_scoped_and_does_not_delete_live_counters() {
        let native = Key {
            abi: Abi::X86_64,
            number: 0,
        };

        let compat = Key {
            abi: Abi::I386,
            number: 0,
        };

        let unknown = Key {
            abi: Abi::X86_64,
            number: u64::MAX,
        };

        let mut baseline = Baseline::default();
        let mut counts = Counts {
            entries: vec![(native, 10), (compat, 20)],
            lost: 5,
        };

        baseline.project(&mut counts, 1);
        assert_eq!(counts.entries, vec![(native, 0), (compat, 0)]);
        assert_eq!(counts.lost, 0);

        let mut counts = Counts {
            entries: vec![(native, 13), (compat, 25), (unknown, 2)],
            lost: 7,
        };

        baseline.project(&mut counts, 1);
        assert_eq!(counts.entries, vec![(native, 3), (compat, 5), (unknown, 2)]);
        assert_eq!(counts.lost, 2);

        let mut counts = Counts {
            entries: vec![(native, u64::MAX)],
            lost: u64::MAX,
        };

        baseline.project(&mut counts, 3);
        assert_eq!(counts.entries, vec![(native, 0)]);
        assert_eq!(counts.lost, 0);
    }
}
