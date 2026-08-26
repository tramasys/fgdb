use super::*;

pub(super) fn handle_mi_event(weak_ui: &Weak<Ui>, client: &MiClient, event: MiEvent) {
    let Some(ui) = weak_ui.upgrade() else {
        return;
    };

    match event {
        MiEvent::Ready => {
            ui.set_command_pending(false);
            ui.set_debug_state_stale(false);
            ui.set_gef_available(false);
            ui.set_status(
                "Ready",
                "The native controls and terminal share one GDB process.",
                Some("status-ready"),
            );
            ui.set_controls_ready(true);
            detect_gef(weak_ui, client);
            request_initial_source(weak_ui, client);
            refresh_stopped_state(weak_ui, client);
            refresh_breakpoints(weak_ui, client);
            ui.take_modules_dirty();
            refresh_modules(weak_ui, client);
            infer_initial_stop_reason(weak_ui, client);
        }
        MiEvent::Running => {
            ui.set_command_pending(false);
            ui.set_debug_state_stale(true);
            ui.set_inferior_started(true);
            ui.set_thread_stop_reason(None);
            // Any queued stop-state responses now describe the previous stop.
            // Invalidating them also prevents recursive pointer enrichment from
            // issuing more MI work while the inferior is running.
            ui.start_stop_refresh();
            ui.start_thread_refresh();
            ui.invalidate_kernel_refresh();
            ui.clear_execution_location();
            ui.set_status(
                "Running",
                "The inferior is running. Pause it to inspect state.",
                Some("status-running"),
            );
            ui.set_controls_running(true);
        }
        MiEvent::Stopped {
            reason,
            signal_name,
            signal_meaning,
        } => {
            ui.set_command_pending(false);
            ui.set_debug_state_stale(false);
            let reason = reason.unwrap_or_else(|| String::from("stopped"));
            ui.set_thread_stop_reason(Some(&reason));
            if reason.starts_with("exited") {
                ui.set_inferior_started(false);
                ui.clear_debugger_state();
                refresh_breakpoints(weak_ui, client);
            } else {
                ui.set_inferior_started(true);
                refresh_stopped_state(weak_ui, client);
            }
            if ui.take_modules_dirty() {
                refresh_modules(weak_ui, client);
            }
            ui.show_signal(signal_name.as_deref(), signal_meaning.as_deref());
            ui.set_status(
                "Paused",
                &format!("GDB reported: {}", reason.replace('-', " ")),
                Some("status-ready"),
            );
            ui.set_controls_running(false);
            ui.refresh_kernel_after_stop();
        }
        MiEvent::BreakpointsChanged => refresh_breakpoints(weak_ui, client),
        MiEvent::ThreadsChanged => {
            if !ui.inferior_is_running() {
                refresh_threads(weak_ui, client);
            }
        }
        MiEvent::LibrariesChanged => {
            ui.mark_modules_dirty();
            if !ui.inferior_is_running() && ui.take_modules_dirty() {
                refresh_modules(weak_ui, client);
            }
        }
        MiEvent::SelectionChanged => refresh_stopped_state(weak_ui, client),
        MiEvent::Error(message) => {
            ui.set_command_pending(false);
            ui.set_status("Command failed", &message, Some("status-error"));
        }
        MiEvent::Disconnected => {
            ui.set_command_pending(false);
            ui.set_debug_state_stale(true);
            ui.set_gef_available(false);
            ui.set_status(
                "Disconnected",
                "The GDB/MI channel was closed.",
                Some("status-error"),
            );
            ui.set_controls_ready(false);
        }
    }
}

fn detect_gef(ui: &Weak<Ui>, client: &MiClient) {
    let weak_ui = ui.clone();
    if client
        .request("-complete gef", move |_, record| {
            if let Some(ui) = weak_ui.upgrade() {
                let available = record.is_done()
                    && record
                        .field("completion")
                        .and_then(|value| value.as_const())
                        == Some("gef");
                ui.set_gef_available(available);
            }
        })
        .is_err()
        && let Some(ui) = ui.upgrade()
    {
        ui.set_gef_available(false);
    }
}

pub(super) fn request_initial_source(ui: &Weak<Ui>, client: &MiClient) {
    let weak_ui = ui.clone();
    let _ = client.request("-file-list-exec-source-file", move |_, record| {
        if record.is_done()
            && let (Some(ui), Some(source_file)) =
                (weak_ui.upgrade(), crate::debugger::current_source(&record))
        {
            ui.show_initial_source(&source_file);
        }
    });
}
