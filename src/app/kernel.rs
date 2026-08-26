use std::{
    sync::mpsc::{self, TryRecvError},
    time::Duration,
};

use super::*;

pub(super) fn request_kernel_refresh(ui: Weak<Ui>, client: Rc<MiClient>) {
    let Some(generation) = ui.upgrade().and_then(|ui| ui.begin_kernel_refresh()) else {
        return;
    };
    let ui_for_response = ui.clone();
    if let Err(error) = client.request("-list-thread-groups", move |_, record| {
        let Some(current_ui) = ui_for_response.upgrade() else {
            return;
        };
        if !current_ui.kernel_refresh_is_current(generation) {
            current_ui.finish_stale_kernel_refresh();
            return;
        }
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
        read_kernel_snapshot(ui_for_response, generation, pid);
    }) {
        show_kernel_error(&ui, generation, &error.to_string());
    }
}

fn read_kernel_snapshot(ui: Weak<Ui>, generation: u64, pid: u32) {
    let (sender, receiver) = mpsc::channel();
    let worker = std::thread::Builder::new()
        .name(String::from("fgdb-procfs"))
        .spawn(move || {
            let _ = sender.send(crate::kernel::read_snapshot(pid));
        });
    if let Err(error) = worker {
        show_kernel_error(
            &ui,
            generation,
            &format!("Cannot start procfs reader: {error}"),
        );
        return;
    }
    gtk::glib::timeout_add_local(Duration::from_millis(20), move || {
        match receiver.try_recv() {
            Ok(Ok(snapshot)) => {
                if let Some(ui) = ui.upgrade() {
                    ui.show_kernel_snapshot(generation, &snapshot);
                }
                gtk::glib::ControlFlow::Break
            }
            Ok(Err(error)) => {
                show_kernel_error(&ui, generation, &error);
                gtk::glib::ControlFlow::Break
            }
            Err(TryRecvError::Empty) if ui.strong_count() > 0 => gtk::glib::ControlFlow::Continue,
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
