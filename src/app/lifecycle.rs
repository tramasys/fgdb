use super::*;

pub(super) fn handle_mi_event(weak_ui: &Weak<Ui>, client: &MiClient, event: MiEvent) {
    let Some(ui) = weak_ui.upgrade() else {
        return;
    };

    match event {
        MiEvent::Ready => {
            ui.set_command_pending(false);
            ui.set_debug_state_stale(false);
            ui.set_status(
                "Ready",
                "The native controls and terminal share one GDB process.",
                Some("status-ready"),
            );
            ui.set_controls_ready(true);
            request_initial_source(weak_ui, client);
            refresh_stopped_state(weak_ui, client);
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
            ui.show_signal(signal_name.as_deref(), signal_meaning.as_deref());
            ui.set_status(
                "Paused",
                &format!("GDB reported: {}", reason.replace('-', " ")),
                Some("status-ready"),
            );
            ui.set_controls_running(false);
        }
        MiEvent::BreakpointsChanged => refresh_breakpoints(weak_ui, client),
        MiEvent::ThreadsChanged => refresh_threads(weak_ui, client),
        MiEvent::SelectionChanged => refresh_stopped_state(weak_ui, client),
        MiEvent::Error(message) => {
            ui.set_command_pending(false);
            ui.set_status("Command failed", &message, Some("status-error"));
        }
        MiEvent::Disconnected => {
            ui.set_command_pending(false);
            ui.set_debug_state_stale(true);
            ui.set_status(
                "Disconnected",
                "The GDB/MI channel was closed.",
                Some("status-error"),
            );
            ui.set_controls_ready(false);
        }
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
