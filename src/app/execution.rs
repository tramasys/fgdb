//! Execution dispatch, admission and transport recovery for every UI action.

use crate::{
    config::DebugSession,
    debugger::MiClient,
    model::{
        TargetConnection,
        actions::{ExecutionAction, ExecutionTarget},
        configured_target_can_start,
    },
    ui::Ui,
};
use std::{rc::Rc, time::Duration};

pub(super) fn handle(ui: &Rc<Ui>, client: &MiClient, action: ExecutionAction) {
    match action {
        ExecutionAction::Run => run(ui, client),
        ExecutionAction::Pause => pause(ui, client),
        action => {
            if !ui.model.movement_commands_available() {
                return;
            }

            let (command, detail) = match action {
                ExecutionAction::Next => ("-exec-next", "Stepping over the current source line…"),
                ExecutionAction::Step => ("-exec-step", "Stepping into the current source line…"),
                ExecutionAction::NextInstruction => (
                    "-exec-next-instruction",
                    "Stepping over one machine instruction…",
                ),
                ExecutionAction::StepInstruction => (
                    "-exec-step-instruction",
                    "Stepping into one machine instruction…",
                ),
                ExecutionAction::Finish => (
                    "-exec-finish",
                    "Running until the current function returns…",
                ),
                ExecutionAction::Run | ExecutionAction::Pause => unreachable!(),
            };

            match ui.model.directional_command(command) {
                Ok(command) => {
                    issue_execution_command(
                        ui,
                        client,
                        &command,
                        ExecutionTarget::SelectedThread,
                        false,
                        detail,
                    );
                }
                Err(message) => {
                    ui.set_status("Execution unavailable", message, Some("status-error"))
                }
            }
        }
    }
}

fn run(ui: &Rc<Ui>, client: &MiClient) {
    let debugger_state = ui.model.execution().state;

    if debugger_state.inferior_running()
        || ui.model.execution().command_pending
        || debugger_state.transition_pending()
        || debugger_state.stopped_context_is_stale()
        || ui.model.execution().session_pending
        || ui.model.execution().native_until_active
        || !ui.model.execution().ready
    {
        return;
    }

    let (command, detail) = {
        let session = ui.model.session();

        if !debugger_state.inferior_started()
            && matches!(session.as_ref(), Some(DebugSession::RrReplay { .. }))
            && debugger_state.target_connection() == TargetConnection::Remote
        {
            drop(session);
            ui.restart_rr_replay();
            return;
        }

        if debugger_state.inferior_started() {
            if session
                .as_ref()
                .is_some_and(|session| !session.supports_execution())
            {
                return;
            }

            let command = if let Some(id) = ui.model.selected_inferior_id() {
                let Some(id) = crate::debugger::thread_group_argument(&id) else {
                    ui.set_status(
                        "Continue unavailable",
                        "GDB reported an unsupported inferior identifier",
                        Some("status-error"),
                    );

                    return;
                };

                format!("-exec-continue --thread-group {id}")
            } else {
                String::from("-exec-continue")
            };

            (command, "Continuing the selected inferior…")
        } else if configured_target_can_start(session.as_ref(), debugger_state.target_connection())
        {
            (String::from("-exec-run"), "Starting the inferior…")
        } else {
            return;
        }
    };

    match ui.model.directional_command(&command) {
        Ok(command) => {
            let inferior = ui.model.selected_inferior_id();
            let target = inferior
                .as_deref()
                .map_or(ExecutionTarget::SelectedInferior, ExecutionTarget::Inferior);

            issue_execution_command(ui, client, &command, target, false, detail);
        }
        Err(message) => ui.set_status("Execution unavailable", message, Some("status-error")),
    }
}

fn pause(ui: &Rc<Ui>, client: &MiClient) {
    if ui.model.native_until_active() {
        ui.cancel_native_until();
    } else if ui.model.execution().ready
        && ui.model.execution().state.inferior_started()
        && ui.model.execution().state.inferior_running()
        && !ui.model.execution().command_pending
        && !ui.model.execution().state.transition_pending()
        && !ui.model.execution().session_pending
    {
        let command = if let Some(id) = ui.model.selected_inferior_id() {
            let Some(id) = crate::debugger::thread_group_argument(&id) else {
                ui.set_status(
                    "Pause unavailable",
                    "GDB reported an unsupported inferior identifier",
                    Some("status-error"),
                );

                return;
            };

            format!("-exec-interrupt --thread-group {id}")
        } else {
            String::from("-exec-interrupt")
        };

        issue_execution_command(
            ui,
            client,
            &command,
            ExecutionTarget::SelectedInferior,
            true,
            "Interrupting the selected inferior…",
        );
    }
}

pub(super) fn issue_execution_command(
    ui: &Rc<Ui>,
    client: &MiClient,
    command: &str,
    target: ExecutionTarget<'_>,
    interrupt: bool,
    detail: &str,
) -> bool {
    // Thread analysis is speculative inspection work. Let explicit execution
    // preempt it rather than forcing Run/Continue to wait behind a large set
    // of stack requests. Its generation guards cancel queued MI work safely.
    ui.reset_thread_analysis();
    let origin = ui.model.prepare_execution(target, interrupt);

    match client.send(command) {
        Ok(_) => {
            let generation = ui.model.accept_execution();
            ui.refresh_execution_controls();
            let weak_ui = Rc::downgrade(ui);
            let weak_client = client.weak();

            gtk::glib::timeout_add_local_once(Duration::from_secs(15), move || {
                let Some(ui) = weak_ui.upgrade() else {
                    return;
                };

                if ui.model.execution_transition_is_pending(generation) {
                    let message = "GDB accepted an execution command but did not report a running or stopped transition within 15 seconds. Restart GDB from the Session menu.";

                    if let Some(client) = weak_client.upgrade() {
                        client.quarantine(message);
                    } else {
                        ui.require_gdb_recovery("GDB recovery required", message);
                    }
                }
            });

            ui.set_execution_status("Executing", detail);

            true
        }
        Err(error) => {
            ui.model.reject_execution(origin);
            ui.refresh_execution_controls();
            ui.set_status("Command failed", &error.to_string(), Some("status-error"));

            false
        }
    }
}
