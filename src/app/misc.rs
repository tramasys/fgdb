use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, TryRecvError},
    },
    time::{Duration, Instant},
};

use super::*;

static MISC_READER_ACTIVE: AtomicBool = AtomicBool::new(false);

struct MiscWorkerGuard;

impl Drop for MiscWorkerGuard {
    fn drop(&mut self) {
        MISC_READER_ACTIVE.store(false, Ordering::Release);
    }
}

pub(super) fn request_misc_refresh(ui: Weak<Ui>, client: Rc<MiClient>) {
    let Some(generation) = ui.upgrade().and_then(|ui| ui.begin_misc_refresh()) else {
        return;
    };
    let session = ui.upgrade().and_then(|ui| ui.current_session());
    if let Some(DebugSession::CoreDump { core_dump, .. }) = session {
        read_core_dump(ui, generation, core_dump);
        return;
    }
    let ui_for_response = ui.clone();
    if let Err(error) = client.request("-list-thread-groups", move |_, record| {
        let Some(current_ui) = ui_for_response.upgrade() else {
            return;
        };
        if !current_ui.misc_refresh_is_current(generation) {
            current_ui.finish_stale_misc_refresh();
            return;
        }
        let debugger_pid = current_ui.debugger_pid();
        let include_locks = current_ui.misc_locks_requested();
        drop(current_ui);
        let Some(pid) = crate::debugger::inferior_pid(&record) else {
            show_misc_error(
                &ui_for_response,
                generation,
                record
                    .error_message()
                    .unwrap_or("GDB did not report a live inferior process ID"),
            );
            return;
        };
        let Some(debugger_pid) = debugger_pid else {
            show_misc_error(
                &ui_for_response,
                generation,
                "The local GDB process identity is unavailable",
            );
            return;
        };
        read_live_misc(
            ui_for_response,
            generation,
            pid,
            debugger_pid,
            include_locks,
        );
    }) {
        show_misc_error(&ui, generation, &error.to_string());
    }
}

fn read_live_misc(ui: Weak<Ui>, generation: u64, pid: u32, debugger_pid: u32, include_locks: bool) {
    const READ_TIMEOUT: Duration = Duration::from_secs(5);
    if MISC_READER_ACTIVE.swap(true, Ordering::AcqRel) {
        show_misc_error(
            &ui,
            generation,
            "A previous Misc data reader is still finishing",
        );
        return;
    }
    let (sender, receiver) = mpsc::channel();
    let worker = std::thread::Builder::new()
        .name(String::from("fgdb-misc-live"))
        .spawn(move || {
            let _guard = MiscWorkerGuard;
            let _ = sender.send(crate::misc::read_live_misc(
                pid,
                debugger_pid,
                include_locks,
            ));
        });
    if let Err(error) = worker {
        MISC_READER_ACTIVE.store(false, Ordering::Release);
        show_misc_error(
            &ui,
            generation,
            &format!("Cannot start the Misc data reader: {error}"),
        );
        return;
    }
    let started = Instant::now();
    gtk::glib::timeout_add_local(Duration::from_millis(20), move || {
        match receiver.try_recv() {
            Ok(Ok(snapshot)) => {
                if let Some(ui) = ui.upgrade() {
                    ui.show_misc_snapshot(generation, snapshot);
                }
                gtk::glib::ControlFlow::Break
            }
            Ok(Err(error)) => {
                show_misc_error(&ui, generation, &error);
                gtk::glib::ControlFlow::Break
            }
            Err(TryRecvError::Empty)
                if ui.strong_count() > 0 && started.elapsed() < READ_TIMEOUT =>
            {
                gtk::glib::ControlFlow::Continue
            }
            Err(TryRecvError::Empty) if ui.strong_count() > 0 => {
                show_misc_error(
                    &ui,
                    generation,
                    "Reading bounded Misc process data exceeded five seconds",
                );
                gtk::glib::ControlFlow::Break
            }
            Err(TryRecvError::Disconnected) => {
                show_misc_error(
                    &ui,
                    generation,
                    "The Misc data reader stopped before returning data",
                );
                gtk::glib::ControlFlow::Break
            }
            Err(TryRecvError::Empty) => gtk::glib::ControlFlow::Break,
        }
    });
}

fn read_core_dump(ui: Weak<Ui>, generation: u64, path: std::path::PathBuf) {
    const READ_TIMEOUT: Duration = Duration::from_secs(5);
    if MISC_READER_ACTIVE.swap(true, Ordering::AcqRel) {
        show_misc_error(
            &ui,
            generation,
            "A previous Misc data reader is still finishing",
        );
        return;
    }
    let (sender, receiver) = mpsc::channel();
    let worker = std::thread::Builder::new()
        .name(String::from("fgdb-misc-core"))
        .spawn(move || {
            let _guard = MiscWorkerGuard;
            let _ = sender.send(crate::misc::read_core_dump(&path));
        });
    if let Err(error) = worker {
        MISC_READER_ACTIVE.store(false, Ordering::Release);
        show_misc_error(
            &ui,
            generation,
            &format!("Cannot start the core-note reader: {error}"),
        );
        return;
    }
    let started = Instant::now();
    gtk::glib::timeout_add_local(Duration::from_millis(20), move || {
        match receiver.try_recv() {
            Ok(Ok(snapshot)) => {
                if let Some(ui) = ui.upgrade() {
                    ui.show_misc_core_snapshot(generation, snapshot);
                }
                gtk::glib::ControlFlow::Break
            }
            Ok(Err(error)) => {
                show_misc_error(&ui, generation, &error);
                gtk::glib::ControlFlow::Break
            }
            Err(TryRecvError::Empty)
                if ui.strong_count() > 0 && started.elapsed() < READ_TIMEOUT =>
            {
                gtk::glib::ControlFlow::Continue
            }
            Err(TryRecvError::Empty) if ui.strong_count() > 0 => {
                show_misc_error(
                    &ui,
                    generation,
                    "Reading bounded core metadata exceeded five seconds",
                );
                gtk::glib::ControlFlow::Break
            }
            Err(TryRecvError::Disconnected) => {
                show_misc_error(
                    &ui,
                    generation,
                    "The core-note reader stopped before returning data",
                );
                gtk::glib::ControlFlow::Break
            }
            Err(TryRecvError::Empty) => gtk::glib::ControlFlow::Break,
        }
    });
}

fn show_misc_error(ui: &Weak<Ui>, generation: u64, error: &str) {
    if let Some(ui) = ui.upgrade() {
        ui.show_misc_error(generation, error);
    }
}
