use std::{
    path::Path,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use super::{KernelSnapshot, WorkDeadline, elf, memory, process, procfs, resources};

pub(crate) fn read_snapshot(
    pid: u32,
    debugger_pid: u32,
    include_tls_metadata: bool,
    work: &WorkDeadline,
) -> Result<KernelSnapshot, String> {
    work.check()?;

    procfs::read_verified_local_proc(pid, debugger_pid, |target| {
        work.check()?;

        read_snapshot_from(
            target.root(),
            pid,
            target.start_time(),
            include_tls_metadata,
            work,
        )
    })
}

fn read_snapshot_from(
    root: &Path,
    pid: u32,
    expected_start_time: u64,
    include_tls_metadata: bool,
    work: &WorkDeadline,
) -> Result<KernelSnapshot, String> {
    let started = Instant::now();

    let status = process::read_key_values(&root.join("status"))
        .map_err(|error| format!("Cannot inspect /proc/{pid}/status: {error}"))?;

    let stat = process::read_proc_stat(&root.join("stat"))
        .ok_or_else(|| format!("Cannot establish the identity of process {pid}"))?;

    ensure_snapshot_identity(pid, expected_start_time, Some(stat.start_time))?;
    work.check()?;

    let mut snapshot = KernelSnapshot {
        pid,
        identity: Some(expected_start_time),
        ..KernelSnapshot::default()
    };

    process::populate_process(&mut snapshot, root, &status);
    work.check()?;
    memory::populate_mappings(&mut snapshot, root, work);
    work.check()?;
    memory::populate_memory(&mut snapshot, root, &status, work);
    work.check()?;
    memory::populate_numa(&mut snapshot, root, work);
    work.check()?;
    memory::populate_page_samples(&mut snapshot, root, work);
    work.check()?;

    if include_tls_metadata {
        elf::populate_tls_metadata(&mut snapshot, root, work);
        work.check()?;
        snapshot.tls_metadata_scanned = true;
    }

    process::populate_scheduler(&mut snapshot, root, &status, Some(&stat));
    work.check()?;
    process::populate_security(&mut snapshot, root, &status);
    work.check()?;
    process::populate_io(&mut snapshot, root);
    work.check()?;
    process::populate_isolation(&mut snapshot, root, &status);
    work.check()?;
    process::populate_runtime(&mut snapshot, root);
    work.check()?;
    process::populate_threads_and_signals(&mut snapshot, root, &status, work);
    work.check()?;
    process::populate_hierarchy(&mut snapshot, root, &status, work);
    work.check()?;
    resources::populate_constraints(&mut snapshot, root);
    work.check()?;
    resources::populate_descriptors(&mut snapshot, root, work);
    work.check()?;
    resources::populate_limits(&mut snapshot, root);
    work.check()?;
    resources::populate_kernel_policy(&mut snapshot, root);
    work.check()?;
    super::populate_diagnostics(&mut snapshot);
    snapshot.metrics.minor_faults = stat.minor_faults;
    snapshot.metrics.major_faults = stat.major_faults;
    snapshot.metrics.user_ticks = stat.user_ticks;
    snapshot.metrics.system_ticks = stat.system_ticks;
    snapshot.metrics.mappings = snapshot.mappings.len() as u64;
    snapshot.metrics.descriptors = snapshot.file_descriptors.len() as u64;
    let after = process::read_proc_stat(&root.join("stat")).map(|stat| stat.start_time);
    ensure_snapshot_identity(pid, expected_start_time, after)?;
    work.check()?;

    snapshot.captured_at_millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        });

    snapshot.capture_duration_micros =
        u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);

    Ok(snapshot)
}

fn ensure_snapshot_identity(
    pid: u32,
    expected_start_time: u64,
    observed_start_time: Option<u64>,
) -> Result<(), String> {
    let observed = observed_start_time
        .ok_or_else(|| format!("Process {pid} disappeared while its snapshot was being read"))?;

    if observed != expected_start_time {
        return Err(String::from(
            "The inferior changed while its procfs snapshot was being read",
        ));
    }

    Ok(())
}

#[cfg(test)]
pub(super) fn read_snapshot_for_test(
    root: &Path,
    pid: u32,
    include_tls_metadata: bool,
) -> Result<KernelSnapshot, String> {
    let identity = process::read_proc_stat(&root.join("stat"))
        .ok_or_else(|| format!("Cannot establish the identity of process {pid}"))?
        .start_time;

    read_snapshot_from(
        root,
        pid,
        identity,
        include_tls_metadata,
        &WorkDeadline::new(std::time::Duration::from_secs(60)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_identity_must_match_the_verified_target() {
        assert!(ensure_snapshot_identity(7, 99, Some(99)).is_ok());

        assert!(
            ensure_snapshot_identity(7, 99, Some(100))
                .unwrap_err()
                .contains("changed")
        );

        assert!(
            ensure_snapshot_identity(7, 99, None)
                .unwrap_err()
                .contains("disappeared")
        );
    }
}
