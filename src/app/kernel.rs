use std::{
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

use super::*;

const MAX_KERNEL_WORKERS: usize = 2;
static ACTIVE_KERNEL_WORKERS: AtomicUsize = AtomicUsize::new(0);
struct KernelWorkerGuard;

impl Drop for KernelWorkerGuard {
    fn drop(&mut self) {
        ACTIVE_KERNEL_WORKERS.fetch_sub(1, Ordering::Relaxed);
    }
}

pub(super) fn request_kernel_refresh(ui: Weak<Ui>, client: Rc<MiClient>) {
    let Some(current_ui) = ui.upgrade() else {
        return;
    };

    let Some(generation) = current_ui.begin_kernel_refresh() else {
        return;
    };

    let cached_pid = current_ui.model.inferior_pid();
    let debugger_pid = current_ui.model.debugger_pid();
    let include_tls_metadata = current_ui.kernel_tls_requested();
    drop(current_ui);

    if let (Some(pid), Some(debugger_pid)) = (cached_pid, debugger_pid) {
        read_kernel_snapshot(ui, generation, pid, debugger_pid, include_tls_metadata);
        return;
    }

    let ui_for_response = ui.clone();

    if let Err(error) = client.request("-list-thread-groups", move |_, record| {
        let Some(current_ui) = ui_for_response.upgrade() else {
            return;
        };

        if !current_ui.kernel_refresh_is_current(generation) {
            current_ui.finish_stale_kernel_refresh();
            return;
        }

        let debugger_pid = current_ui.model.debugger_pid();
        let include_tls_metadata = current_ui.kernel_tls_requested();
        drop(current_ui);

        let Some(pid) = crate::debugger::inferior_pid(&record) else {
            show_kernel_error(
                &ui_for_response,
                generation,
                record
                    .error_message()
                    .unwrap_or("GDB did not report a live inferior process ID"),
            );

            return;
        };

        if let Some(current_ui) = ui_for_response.upgrade() {
            current_ui.model.set_inferior_pid(Some(pid));
        }

        let Some(debugger_pid) = debugger_pid else {
            show_kernel_error(
                &ui_for_response,
                generation,
                "The local GDB process identity is unavailable",
            );

            return;
        };

        read_kernel_snapshot(
            ui_for_response,
            generation,
            pid,
            debugger_pid,
            include_tls_metadata,
        );
    }) {
        show_kernel_error(&ui, generation, &error.to_string());
    }
}

fn read_kernel_snapshot(
    ui: Weak<Ui>,
    generation: u64,
    pid: u32,
    debugger_pid: u32,
    include_tls_metadata: bool,
) {
    const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(15);

    if ACTIVE_KERNEL_WORKERS.fetch_add(1, Ordering::Relaxed) >= MAX_KERNEL_WORKERS {
        ACTIVE_KERNEL_WORKERS.fetch_sub(1, Ordering::Relaxed);

        show_kernel_error(
            &ui,
            generation,
            "Previous procfs readers are still finishing. Try the refresh again shortly",
        );

        return;
    }

    let (sender, receiver) = futures_channel::oneshot::channel();
    let work = crate::kernel::WorkDeadline::new(SNAPSHOT_TIMEOUT);
    let worker_work = work.clone();

    let worker = std::thread::Builder::new()
        .name(String::from("fgdb-procfs"))
        .spawn(move || {
            let result = {
                let _guard = KernelWorkerGuard;
                crate::kernel::read_snapshot(pid, debugger_pid, include_tls_metadata, &worker_work)
            };

            let _ = sender.send(result);
        });

    if let Err(error) = worker {
        ACTIVE_KERNEL_WORKERS.fetch_sub(1, Ordering::Relaxed);

        show_kernel_error(
            &ui,
            generation,
            &format!("Cannot start procfs reader: {error}"),
        );

        return;
    }

    gtk::glib::MainContext::default().spawn_local(async move {
        let result = crate::background::receive_current(receiver, SNAPSHOT_TIMEOUT, || {
            ui.upgrade()
                .is_some_and(|ui| ui.kernel_refresh_is_current(generation))
        })
        .await;

        work.cancel();

        match result {
            Ok(Ok(snapshot)) => {
                if let Some(ui) = ui.upgrade() {
                    ui.show_kernel_snapshot(generation, snapshot);
                }
            }
            Ok(Err(error)) => show_kernel_error(&ui, generation, &error),
            Err(crate::background::CompletionError::Superseded) => {
                if let Some(ui) = ui.upgrade() {
                    ui.finish_stale_kernel_refresh();
                }
            }
            Err(crate::background::CompletionError::TimedOut) => show_kernel_error(
                &ui,
                generation,
                "The procfs snapshot exceeded the 15-second collection limit",
            ),
            Err(crate::background::CompletionError::Disconnected) => show_kernel_error(
                &ui,
                generation,
                "The background procfs reader stopped before returning a snapshot",
            ),
        }
    });
}

fn show_kernel_error(ui: &Weak<Ui>, generation: u64, error: &str) {
    if let Some(ui) = ui.upgrade() {
        ui.show_kernel_error(generation, error);
    }
}
