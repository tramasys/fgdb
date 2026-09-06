use super::*;
use crate::config::ReplayConfig;
use crate::model::{TargetConnection, replay::*};

enum ReplayCommand {
    Configure(String),
    SetForward,
    StopRecording,
    Restore(String),
}

impl ReplayCommand {
    fn text(&self) -> &str {
        match self {
            Self::Configure(text) | Self::Restore(text) => text,
            Self::SetForward => "set exec-direction forward",
            Self::StopRecording => "record stop",
        }
    }

    fn may_resume(&self) -> bool {
        matches!(self, Self::Restore(_))
    }

    fn apply(&self, state: &mut ReplayState, group: &str, selected: bool) {
        match self {
            Self::SetForward => state.set_backend_direction(ExecutionDirection::Forward),
            Self::StopRecording => state.recording_stopped(group, selected),
            Self::Configure(_) | Self::Restore(_) => {}
        }
    }
}

pub(super) fn refresh(ui: &Weak<Ui>, client: &MiClient, force: bool) {
    let Some(current) = ui.upgrade() else { return };

    if !current.model.debugger_synchronization_available() {
        return;
    }

    let group = current.model.selected_inferior_id().unwrap_or_default();
    let epoch = client.transport_epoch();
    let generation = {
        let mut state = current.model.replay.borrow_mut();

        if state.querying {
            state.refresh_requested |= force;
            return;
        }

        if !force
            && state
                .queried_target
                .as_ref()
                .is_some_and(|(saved_epoch, saved_group)| {
                    *saved_epoch == epoch && *saved_group == group
                })
        {
            return;
        }

        state.query_generation = state.query_generation.wrapping_add(1);
        state.querying = true;
        state.query_generation
    };

    // MI's -gdb-show reports the setting buffer even when GDB rejected a
    // direction change. The CLI show command reports the actual direction.
    let mut commands = VecDeque::from(["-list-target-features", "show exec-direction"]);

    let capabilities = current.model.replay.borrow().capabilities;

    for (command, capability) in [
        ("help record full", capabilities.full_recording),
        ("help record btrace", capabilities.branch_tracing),
        ("help checkpoint", capabilities.checkpoints),
    ] {
        if capability.is_none() {
            commands.push_back(command);
        }
    }

    if current.model.inferior_has_started() {
        commands.push_back("info record");

        if force && current.model.checkpoint_target_available() {
            commands.push_back("info checkpoints");
        }
    }

    current.render_replay_controls();
    drop(current);
    query_next(Rc::new(ReplayQuery {
        ui: ui.clone(),
        client: client.weak(),
        generation,
        epoch,
        group,
        commands: RefCell::new(commands),
    }));
}

struct ReplayQuery {
    ui: Weak<Ui>,
    client: Weak<MiClient>,
    generation: u64,
    epoch: u64,
    group: String,
    commands: RefCell<VecDeque<&'static str>>,
}

impl ReplayQuery {
    fn current(&self) -> bool {
        self.client
            .upgrade()
            .is_some_and(|client| client.transport_epoch() == self.epoch)
            && self.ui.upgrade().is_some_and(|ui| {
                ui.model.replay.borrow().query_generation == self.generation
                    && ui.model.selected_inferior_id().unwrap_or_default() == self.group
                    && ui.model.debugger_synchronization_available()
            })
    }

    fn finish(&self, complete: bool) {
        if let Some(ui) = self.ui.upgrade() {
            let mut state = ui.model.replay.borrow_mut();

            if state.query_generation != self.generation {
                return;
            }

            state.querying = false;
            state.queried_target = complete.then(|| (self.epoch, self.group.clone()));
            let refresh_requested = std::mem::take(&mut state.refresh_requested);
            drop(state);
            ui.render_replay_controls();
            let target_changed = ui.model.selected_inferior_id().unwrap_or_default() != self.group;

            if (refresh_requested || target_changed)
                && let Some(client) = self.client.upgrade()
            {
                refresh(&self.ui, &client, refresh_requested);
            }
        }
    }
}

fn query_next(query: Rc<ReplayQuery>) {
    if !query.current() {
        query.finish(false);
        return;
    }

    let Some(command) = query.commands.borrow_mut().pop_front() else {
        query.finish(true);
        return;
    };

    let Some(client) = query.client.upgrade() else {
        return;
    };

    let guard = Rc::clone(&query);
    let response = Rc::clone(&query);

    let result = if command.starts_with('-') {
        client.request_when(
            command,
            move || guard.current(),
            move |_, record| {
                receive_query(response, command, record, String::new());
            },
        )
    } else {
        client.request_console_when(
            command,
            move || guard.current(),
            move |_, record, output| {
                receive_query(response, command, record, output);
            },
        )
    };

    if result.is_err() {
        query.finish(false);
    }
}

fn receive_query(query: Rc<ReplayQuery>, command: &str, record: MiRecord, output: String) {
    if !query.current() || matches!(record.class.as_str(), "superseded" | "timeout") {
        query.finish(false);
        return;
    }

    let Some(ui) = query.ui.upgrade() else { return };
    let mut state = ui.model.replay.borrow_mut();

    match command {
        "-list-target-features" if record.is_done() => {
            state.reverse_supported = record.has_feature("reverse");
            state.reverse_owner = Some(query.group.clone());
        }
        "show exec-direction" if record.is_done() => {
            if let Some(direction) = ExecutionDirection::from_gdb_output(&output) {
                state.backend_direction = direction;

                if state.backend_direction == ExecutionDirection::Reverse {
                    state.direction = ExecutionDirection::Reverse;
                }
            }
        }
        "help record full" => state.capabilities.full_recording = Some(record.is_done()),
        "help record btrace" => state.capabilities.branch_tracing = Some(record.is_done()),
        "help checkpoint" => state.capabilities.checkpoints = Some(record.is_done()),
        "info record" => {
            if record.is_done() && !record.output_was_truncated() {
                state.details = output.trim().to_owned();

                let method = if output.contains("record-full") {
                    Some(RecordingMethod::Full)
                } else if output.contains("record-btrace") {
                    Some(
                        state
                            .recordings
                            .get(&query.group)
                            .copied()
                            .filter(|method| *method != RecordingMethod::Full)
                            .unwrap_or(RecordingMethod::BranchTrace),
                    )
                } else {
                    None
                };

                if let Some(method) = method {
                    state.recordings.insert(query.group.clone(), method);
                } else if output.contains("No recording is currently active")
                    && state.recordings.contains_key(&query.group)
                {
                    state.recording_stopped(&query.group, true);
                }
            }
        }
        "info checkpoints" => {
            state.checkpoint_query_complete = record.is_done() && !record.output_was_truncated();

            if state.checkpoint_query_complete {
                let (rows, capped) = parse_checkpoints(&output);
                state.checkpoints = rows;
                state.checkpoint_owner = Some(query.group.clone());
                state.checkpoint_list_capped = capped;
            }
        }
        _ => {}
    }

    drop(state);
    drop(ui);
    query_next(query);
}

pub(super) fn handle(ui: Weak<Ui>, client: Rc<MiClient>, action: ReplayAction) {
    let Some(current) = ui.upgrade() else { return };

    if matches!(action, ReplayAction::Refresh) {
        refresh(&ui, &client, true);
        return;
    }

    if !current.model.debugger_synchronization_available() {
        return;
    }

    if let ReplayAction::Direction(direction) = action {
        let normalize_backend = direction == ExecutionDirection::Forward
            && current.model.replay.borrow().backend_direction == ExecutionDirection::Reverse;

        if !normalize_backend {
            if direction == ExecutionDirection::Forward || current.model.reverse_available() {
                current.model.replay.borrow_mut().direction = direction;
                current.render_replay_controls();
            }

            return;
        }
    }

    if !matches!(
        action,
        ReplayAction::RestartReplay | ReplayAction::Direction(_)
    ) && !current.model.stopped_inspection_available()
    {
        return;
    }

    let group = current.model.selected_inferior_id().unwrap_or_default();
    let rr = matches!(
        current.model.current_session(),
        Some(DebugSession::RrReplay { .. })
    );

    let mut commands = VecDeque::new();
    let restore = matches!(
        action,
        ReplayAction::RestoreCheckpoint(_) | ReplayAction::RestartReplay
    );

    let title = match &action {
        ReplayAction::Direction(ExecutionDirection::Forward) => {
            commands.push_back(ReplayCommand::SetForward);
            "Execution direction changed"
        }
        ReplayAction::RestartReplay => {
            if !rr || current.model.target_connection() != TargetConnection::Remote {
                return;
            }

            if current.model.replay.borrow().backend_direction == ExecutionDirection::Reverse {
                commands.push_back(ReplayCommand::SetForward);
            }

            commands.push_back(ReplayCommand::Restore(String::from("run 0")));
            "Replay restarted"
        }
        ReplayAction::Start(method, limit, buffer_kib) => {
            let supported = current
                .model
                .replay
                .borrow()
                .capabilities
                .supports_recording(*method);

            if !supported
                || rr
                || current.model.recording_method().is_some()
                || current.model.non_stop_mode() != Some(false)
                || !ReplayConfig::FULL_INSTRUCTION_LIMIT.contains(limit)
                || !ReplayConfig::BTRACE_BUFFER_KIB.contains(buffer_kib)
                || !matches!(
                    current.model.target_connection(),
                    TargetConnection::Local | TargetConnection::Remote
                )
            {
                return;
            }

            if *method == RecordingMethod::Full {
                commands.push_back(ReplayCommand::Configure(format!(
                    "set record full insn-number-max {limit}"
                )));

                commands.push_back(ReplayCommand::Configure(String::from(
                    "set record full stop-at-limit off",
                )));
            } else {
                for format in match method {
                    RecordingMethod::ProcessorTrace => &["pt"][..],
                    RecordingMethod::BranchStore => &["bts"][..],
                    _ => &["pt", "bts"][..],
                } {
                    commands.push_back(ReplayCommand::Configure(format!(
                        "set record btrace {format} buffer-size {}",
                        u64::from(*buffer_kib) * 1024
                    )));
                }

                commands.push_back(ReplayCommand::Configure(String::from(
                    "set record btrace replay-memory-access read-only",
                )));
            }

            commands.push_back(ReplayCommand::Configure(method.command().to_owned()));
            "Recording started"
        }
        ReplayAction::Stop => {
            if current.model.recording_method().is_none() || rr {
                return;
            }

            // GDB can reject direction changes after the recording target is
            // removed. Reset its global direction while that target exists.
            if current.model.replay.borrow().backend_direction == ExecutionDirection::Reverse {
                commands.push_back(ReplayCommand::SetForward);
            }

            commands.push_back(ReplayCommand::StopRecording);
            "Recording stopped"
        }
        ReplayAction::CreateCheckpoint
        | ReplayAction::RestoreCheckpoint(_)
        | ReplayAction::DeleteCheckpoint(_) => {
            let state = current.model.replay.borrow();

            if !current.model.checkpoint_target_available()
                || state.capabilities.checkpoints != Some(true)
                || !state.checkpoint_query_complete
                || state.checkpoint_owner.as_deref() != Some(&group)
            {
                return;
            }

            match &action {
                ReplayAction::CreateCheckpoint => {
                    if state.checkpoint_list_capped || state.checkpoints.len() >= MAX_CHECKPOINTS {
                        return;
                    }

                    commands.push_back(ReplayCommand::Configure(String::from("checkpoint")));
                }
                ReplayAction::RestoreCheckpoint(id) | ReplayAction::DeleteCheckpoint(id) => {
                    if !checkpoint_id_valid(id)
                        || !state
                            .checkpoints
                            .iter()
                            .any(|row| row.id == *id && !row.active)
                    {
                        return;
                    }

                    commands.push_back(if restore {
                        ReplayCommand::Restore(format!("restart {id}"))
                    } else {
                        ReplayCommand::Configure(format!("delete checkpoint {id}"))
                    });
                }
                _ => unreachable!(),
            }

            "Checkpoint updated"
        }
        ReplayAction::Refresh | ReplayAction::Direction(ExecutionDirection::Reverse) => return,
    };

    current.set_session_pending(true);
    current.set_debug_state_stale(true);
    let generation = current.start_stop_refresh();
    client.cancel_stale_stop_requests(generation);
    current.invalidate_kernel_refresh();
    current.invalidate_misc_refresh();
    let operation_generation = {
        let mut state = current.model.replay.borrow_mut();
        state.invalidate_query();
        state.operation_generation = state.operation_generation.wrapping_add(1);
        state.operation_generation
    };

    let operation = Rc::new(ReplayOperation {
        ui: ui.clone(),
        epoch: client.transport_epoch(),
        client,
        group,
        commands: RefCell::new(commands),
        restore,
        title,
        generation,
        operation_generation,
    });

    current.set_status("Updating execution history", "Waiting for GDB…", None);
    current.render_replay_controls();
    drop(current);
    run_next(operation);
}

struct ReplayOperation {
    ui: Weak<Ui>,
    client: Rc<MiClient>,
    epoch: u64,
    group: String,
    commands: RefCell<VecDeque<ReplayCommand>>,
    restore: bool,
    title: &'static str,
    generation: u64,
    operation_generation: u64,
}

impl ReplayOperation {
    fn current(&self) -> bool {
        self.client.transport_epoch() == self.epoch
            && self.client.is_ready()
            && self.ui.upgrade().is_some_and(|ui| {
                ui.model.execution().session_pending
                    && ui.model.replay.borrow().operation_generation == self.operation_generation
            })
    }
}

fn run_next(operation: Rc<ReplayOperation>) {
    if !operation.current() {
        return;
    }

    let Some(ui) = operation.ui.upgrade() else {
        return;
    };

    let Some(command) = operation.commands.borrow_mut().pop_front() else {
        complete_operation(&operation, None);
        return;
    };

    if ui.model.inferior_is_running()
        || ui.model.selected_inferior_id().unwrap_or_default() != operation.group
    {
        complete_operation(
            &operation,
            Some(
                "The target changed during the operation. Refresh execution history before retrying",
            ),
        );
        return;
    }

    drop(ui);
    let response = Rc::clone(&operation);
    let guard = Rc::clone(&operation);
    let may_resume = command.may_resume();

    if let Err(error) = operation.client.request_for_session(
        &crate::debugger::console_command(command.text()),
        operation.generation,
        move || {
            guard.current()
                && (may_resume
                    || guard.ui.upgrade().is_some_and(|ui| {
                        !ui.model.inferior_is_running()
                            && ui.model.selected_inferior_id().unwrap_or_default() == guard.group
                    }))
        },
        move |_, record| {
            if !response.current() {
                return;
            }

            if record.class == "timeout" {
                // The transport quarantines timed-out commands that reached GDB.
                // A request canceled before dispatch must not kill a healthy backend.
                // Do not queue inspection work before that decision is applied.
                if let Some(ui) = response.ui.upgrade() {
                    ui.model.replay.borrow_mut().invalidate_query();
                    ui.set_session_pending(false);
                    ui.set_status(
                        "Execution history timed out",
                        "GDB did not answer the command. Refresh debugger state before retrying",
                        Some("status-error"),
                    );
                }
            } else if record.class == "superseded" {
                complete_operation(
                    &response,
                    Some("The execution-history command was interrupted. Refresh before retrying"),
                );
            } else if record.is_success() {
                if let Some(ui) = response.ui.upgrade() {
                    let selected =
                        ui.model.selected_inferior_id().as_deref() == Some(&response.group);

                    let mut state = ui.model.replay.borrow_mut();

                    command.apply(&mut state, &response.group, selected);
                }

                run_next(response);
            } else {
                complete_operation(
                    &response,
                    Some(
                        record
                            .error_message()
                            .unwrap_or("GDB rejected the execution-history command"),
                    ),
                );
            }
        },
    ) {
        complete_operation(&operation, Some(&error.to_string()));
    }
}

fn complete_operation(operation: &ReplayOperation, error: Option<&str>) {
    if !operation.current() {
        return;
    }

    let Some(ui) = operation.ui.upgrade() else {
        return;
    };

    ui.set_session_pending(false);

    {
        let mut state = ui.model.replay.borrow_mut();
        state.invalidate_query();
    }

    if !ui.model.inferior_is_running() {
        ui.set_debug_state_stale(false);

        if operation.restore {
            resynchronize_debugger_state(&operation.ui, &operation.client);
        } else {
            refresh_stopped_state(&operation.ui, &operation.client);
        }

        refresh(&operation.ui, &operation.client, true);
    }

    if let Some(error) = error {
        ui.set_status("Execution history failed", error, Some("status-error"));
    } else {
        ui.set_status(
            operation.title,
            "Execution history updated",
            Some("status-ready"),
        );
    }

    ui.render_replay_controls();
}
