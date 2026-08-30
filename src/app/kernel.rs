use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc::{self, TryRecvError},
    },
    time::{Duration, Instant},
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
    let cached_pid = current_ui.inferior_pid();
    let debugger_pid = current_ui.debugger_pid();
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
        let debugger_pid = current_ui.debugger_pid();
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
            current_ui.set_inferior_pid(Some(pid));
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
    let (sender, receiver) = mpsc::channel();
    let worker = std::thread::Builder::new()
        .name(String::from("fgdb-procfs"))
        .spawn(move || {
            let _guard = KernelWorkerGuard;
            let _ = sender.send(crate::kernel::read_snapshot(
                pid,
                debugger_pid,
                include_tls_metadata,
            ));
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
    let started = Instant::now();
    gtk::glib::timeout_add_local(Duration::from_millis(20), move || {
        match receiver.try_recv() {
            Ok(Ok(snapshot)) => {
                if let Some(ui) = ui.upgrade() {
                    ui.show_kernel_snapshot(generation, snapshot);
                }
                gtk::glib::ControlFlow::Break
            }
            Ok(Err(error)) => {
                show_kernel_error(&ui, generation, &error);
                gtk::glib::ControlFlow::Break
            }
            Err(TryRecvError::Empty)
                if ui.strong_count() > 0 && started.elapsed() < SNAPSHOT_TIMEOUT =>
            {
                gtk::glib::ControlFlow::Continue
            }
            Err(TryRecvError::Empty) if ui.strong_count() > 0 => {
                show_kernel_error(
                    &ui,
                    generation,
                    "The procfs snapshot exceeded the 15-second collection limit",
                );
                gtk::glib::ControlFlow::Break
            }
            Err(TryRecvError::Disconnected) => {
                show_kernel_error(
                    &ui,
                    generation,
                    "The background procfs reader stopped before returning a snapshot",
                );
                gtk::glib::ControlFlow::Break
            }
            Err(TryRecvError::Empty) => gtk::glib::ControlFlow::Break,
        }
    });
}

fn show_kernel_error(ui: &Weak<Ui>, generation: u64, error: &str) {
    if let Some(ui) = ui.upgrade() {
        ui.show_kernel_error(generation, error);
    }
}
