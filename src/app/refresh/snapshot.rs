//! Prefetch local process metadata alongside GDB's stopped-state queries.

use super::*;

pub(super) struct ProcessSnapshotRequest {
    target: (u32, u32),
    receiver: futures_channel::oneshot::Receiver<StopProcessSnapshot>,
}

impl ProcessSnapshotRequest {
    pub(super) fn start(pid: u32, debugger_pid: u32) -> Option<Self> {
        let receiver =
            crate::background::submit_result(crate::background::Priority::Critical, move || {
                StopProcessSnapshot {
                    abi: crate::kernel::read_local_target_abi(pid, debugger_pid),
                    regions: read_memory_regions(pid, debugger_pid),
                }
            })
            .ok()?;

        Some(Self {
            target: (pid, debugger_pid),
            receiver,
        })
    }
}

pub(super) fn finish_process_snapshot(
    inputs: (Weak<Ui>, StopRequests, Vec<Register>, Vec<StackFrame>),
    client: &MiClient,
    target: (u32, u32),
    prefetched: Option<ProcessSnapshotRequest>,
) {
    let (ui, requests, registers, frames) = inputs;
    let pending = prefetched
        .filter(|pending| pending.target == target)
        .or_else(|| ProcessSnapshotRequest::start(target.0, target.1));
    let Some(mut pending) = pending else {
        finish_stop_process_snapshot(
            ui,
            client,
            requests,
            registers,
            frames,
            StopProcessSnapshot::default(),
        );
        return;
    };

    // Usually the worker finished while GDB was collecting registers. Avoid
    // even an extra main-loop turn on this fast path.
    match pending.receiver.try_recv() {
        Ok(Some(snapshot)) => {
            finish_stop_process_snapshot(ui, client, requests, registers, frames, snapshot);
            return;
        }
        Err(_) => {
            finish_stop_process_snapshot(
                ui,
                client,
                requests,
                registers,
                frames,
                StopProcessSnapshot::default(),
            );
            return;
        }
        Ok(None) => {}
    }

    let client = client.weak();
    gtk::glib::MainContext::default().spawn_local(async move {
        let snapshot =
            gtk::glib::future_with_timeout(std::time::Duration::from_secs(5), pending.receiver)
                .await
                .ok()
                .and_then(Result::ok)
                .unwrap_or_default();

        if requests.is_current()
            && let Some(client) = client.upgrade()
        {
            finish_stop_process_snapshot(ui, &client, requests, registers, frames, snapshot);
        }
    });
}
