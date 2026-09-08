use super::lifecycle_reducer::{EventAdmission, admit_event, reduce_stop_transition};
use super::*;

mod initial_stop;

pub(super) fn handle_mi_event(weak_ui: &Weak<Ui>, client: &MiClient, event: MiEvent) {
    let Some(ui) = weak_ui.upgrade() else {
        return;
    };

    // Once protocol or target corruption has quarantined the backend, late
    // records from the old command must not make the UI look usable again.
    // Ready is allowed to establish a freshly reconnected backend and
    // Disconnected still performs final transport cleanup.
    if admit_event(ui.model.gdb_recovery_required(), &event)
        == EventAdmission::IgnoreFromQuarantinedBackend
    {
        return;
    }

    match event {
        MiEvent::Ready(capabilities) => {
            ui.model.reset_replay();
            ui.model
                .replay
                .borrow_mut()
                .set_backend_direction(crate::model::replay::ExecutionDirection::Forward);

            ui.reset_runtime_pretty_printer_scripts();
            ui.finish_execution_transition();
            ui.set_command_pending(false);
            ui.model.set_active_thread_execution(None);
            ui.model.set_thread_execution_exit_candidate(None);
            ui.set_debug_state_stale(false);
            ui.set_gdb_recovery_available(false);
            ui.set_gdb_capabilities(capabilities.clone());
            ui.clear_gef_capabilities();
            ui.invalidate_allocator_probe_cache();
            ui.reset_target_abi();

            let detail = if !capabilities.mi_async {
                "GDB is ready in compatibility mode. It did not accept asynchronous MI mode."
            } else if !capabilities.pretty_printing {
                "GDB is ready. Dynamic C++ and Rust pretty printing is unavailable in this build."
            } else if !capabilities.features_known {
                "GDB is ready. It did not expose an MI feature list, so optional commands use compatibility defaults."
            } else {
                "The native controls and terminal share one GDB process."
            };

            ui.set_status("Ready", detail, Some("status-ready"));
            ui.set_controls_ready(true);
            detect_terminal_prompt(weak_ui, client);
            detect_target_abi(weak_ui, client);
            detect_gef(weak_ui, client);
            request_initial_source(weak_ui, client);
            refresh_breakpoints(weak_ui, client);
            refresh_inferiors(weak_ui, client);
            refresh_fork_policy(weak_ui, client);
            refresh_thread_policy(weak_ui, client);
            ui.take_modules_dirty();
            refresh_modules(weak_ui, client);
        }
        MiEvent::CapabilitiesChanged(capabilities) => {
            ui.set_gdb_capabilities(capabilities);
        }
        MiEvent::RecordingChanged {
            group_id,
            started,
            method,
            format,
        } => {
            use crate::model::replay::RecordingMethod;
            let selected = ui.model.selected_inferior_id().as_deref() == Some(&group_id);
            let mut state = ui.model.replay.borrow_mut();
            let method = match method.as_deref() {
                Some("full") => Some(RecordingMethod::Full),
                Some("btrace") => Some(match format.as_deref() {
                    Some("pt") => RecordingMethod::ProcessorTrace,
                    Some("bts") => RecordingMethod::BranchStore,
                    _ => RecordingMethod::BranchTrace,
                }),
                _ => None,
            };

            if !started {
                state.recording_stopped(&group_id, selected);
            } else if let Some(method) = method {
                state.recordings.insert(group_id.clone(), method);
            }

            if selected {
                state.reverse_supported = started;
                state.reverse_owner = Some(group_id);
                state.invalidate_query();
            }

            drop(state);
            ui.render_replay_controls();
            replay::refresh(weak_ui, client, false);
        }
        MiEvent::InferiorsChanged => {
            refresh_inferiors(weak_ui, client);
        }
        MiEvent::InferiorStarted { id, pid } => {
            ui.model.symbols.forget_inferior(&id);
            // A terminal user can load and run a different executable in the
            // same GDB process. Register-number caches are target-specific and
            // must not leak across that boundary. The stopped-state refresh
            // will establish the new ABI from GDB and the traced ELF.
            ui.reset_target_abi();
            ui.invalidate_allocator_probe_cache();
            ui.model.record_inferior_started(&id, pid);
            refresh_inferiors(weak_ui, client);
            refresh_thread_policy(weak_ui, client);
        }
        MiEvent::InferiorExited { id, exit_code: _ } => {
            ui.model.symbols.forget_inferior(&id);
            let selected_exited = ui.model.inferior_exit_owns_selected_context(&id);
            let pending_exited =
                ui.model.pending_execution_inferior().as_deref() == Some(id.as_str());

            let active_execution_exited = selected_exited
                || ui
                    .model
                    .active_thread_execution()
                    .as_deref()
                    .and_then(|thread| ui.model.inferior_for_thread(thread))
                    .as_deref()
                    == Some(id.as_str());

            let execution_transition_exited = pending_exited
                || (active_execution_exited
                    && ui.model.execution_transition_matches_thread(None, true));

            if active_execution_exited {
                ui.model.set_active_thread_execution(None);
                ui.model.set_thread_execution_exit_candidate(None);
            }

            ui.record_inferior_exited(&id);

            if execution_transition_exited {
                ui.finish_execution_transition();
                ui.set_command_pending(false);
            }

            if pending_exited {
                ui.model.set_pending_execution_inferior(None);
                ui.finish_inferior_execution_action();
            }

            if selected_exited {
                if ui.model.native_until_active() {
                    ui.abort_native_until();
                }

                ui.finish_thread_execution_action();
                ui.model.set_current_thread_id(None);
                ui.set_thread_stop_reason(None);

                // The executable/remote target remains reusable, but this
                // process no longer exists. Do not leave the global execution
                // interlock tied to a selector snapshot that may have arrived
                // after the exit notification.
                ui.set_controls_running(false);
                ui.set_inferior_started(false);
                ui.set_debug_state_stale(false);
                ui.clear_debugger_state();

                let detail = if matches!(
                    ui.model.session().as_ref(),
                    Some(DebugSession::RrReplay { .. })
                ) {
                    String::from(
                        "The replay reached program exit. Select Run to replay the trace from the beginning.",
                    )
                } else if ui.model.configured_session_can_start() {
                    format!(
                        "{id} exited. The configured target remains loaded. Select Run to start it again."
                    )
                } else {
                    format!("{id} no longer has a live process.")
                };

                ui.set_status("Inferior exited", &detail, Some("status-ready"));
            }

            refresh_inferiors(weak_ui, client);
        }
        MiEvent::Running { thread_id } => {
            {
                let mut state = ui.model.replay.borrow_mut();

                if state.querying {
                    state.invalidate_query();
                }
            }

            let transition_targets_group = ui.model.pending_execution_inferior().is_some();

            let thread_transition_affected = ui
                .model
                .execution_transition_matches_thread(thread_id.as_deref(), false);

            let thread_action_affected = ui
                .model
                .thread_execution_transition_matches(thread_id.as_deref(), false);

            let (selected_affected, inferior_transition_affected) =
                ui.mark_inferior_running(thread_id.as_deref());

            ui.schedule_running_context_render();

            // A response queued for the previous stop must not overwrite the
            // authoritative running model while its paint is being deferred.
            ui.model.start_inferior_refresh();

            if selected_affected {
                // Set the durable running interlock before completing the
                // short command transition, so controls never pass through a
                // briefly enabled state between the two.
                ui.set_controls_running(true);
            }

            if inferior_transition_affected {
                ui.finish_inferior_execution_action();
            }

            if thread_action_affected {
                ui.finish_thread_execution_action();
            }

            let execution_transition_affected = if transition_targets_group {
                inferior_transition_affected
            } else {
                thread_transition_affected
            };

            if execution_transition_affected {
                ui.finish_execution_transition();
                ui.set_command_pending(false);
            }

            if !selected_affected {
                return;
            }

            if ui.model.native_until_active() {
                return;
            }

            ui.set_debug_state_stale(true);
            ui.set_inferior_started(true);
            ui.set_thread_stop_reason(None);

            // Any queued stop-state responses now describe the previous stop.
            // Invalidating them also prevents recursive pointer enrichment from
            // issuing more MI work while the inferior is running.
            let generation = ui.start_stop_refresh();
            client.cancel_stale_stop_requests(generation);
            ui.model.start_thread_refresh();
            ui.invalidate_kernel_refresh();
            ui.invalidate_misc_refresh();

            // Keep the last source tab identity stable while a short execution
            // command is in flight. The stale line marker is removed, while a
            // subsequent stop atomically moves the active-source decoration.
            ui.suspend_execution_location();

            ui.set_execution_status(
                "Running",
                "The inferior is running. Pause it to inspect state.",
            );
        }

        MiEvent::Stopped {
            reason,
            signal_name,
            signal_meaning,
            address,
            thread_id,
            group_id,
            frame_level,
            fork_pid,
            all_stopped,
        } => {
            // In all-stop mode a selected thread can exit while completing a
            // step, after which GDB reports the replacement stop on a
            // different thread. Some GDB versions omit stopped-threads="all"
            // on that replacement record. The preceding exit candidate makes
            // this stop unambiguous and prevents a false 15-second hang.
            let stop_transition = reduce_stop_transition(
                ui.model.non_stop_mode(),
                ui.model.thread_execution_exit_candidate().is_some(),
                ui.model.active_thread_execution().as_deref(),
                thread_id.as_deref(),
                all_stopped,
            );

            let terminal_all_stopped = stop_transition.terminal_all_stopped;
            let transition_targets_group = ui.model.pending_execution_inferior().is_some();

            let thread_transition_affected = ui
                .model
                .execution_transition_matches_thread(thread_id.as_deref(), terminal_all_stopped);

            let thread_action_affected = ui
                .model
                .thread_execution_transition_matches(thread_id.as_deref(), terminal_all_stopped);

            let active_execution_stopped = stop_transition.active_execution_stopped;
            let was_until_active = ui.model.native_until_active();

            if active_execution_stopped || was_until_active {
                ui.model.set_active_thread_execution(None);
                ui.model.set_thread_execution_exit_candidate(None);
            }

            ui.model
                .record_thread_group(thread_id.as_deref(), group_id.as_deref());
            ui.model.set_current_thread_id(thread_id.as_deref());
            ui.select_frame_in_view(frame_level.unwrap_or(0));

            ui.model.record_pending_fork(thread_id.as_deref(), fork_pid);

            // Reconcile every stop before Until can inspect or complete it.
            // Completion may be synchronous or follow an MI reply. Applying
            // this later either revokes the freshly bound stop context or
            // leaves the process model running after asynchronous completion.
            let inferior_transition_affected =
                ui.mark_inferior_stopped(thread_id.as_deref(), terminal_all_stopped);

            ui.set_controls_running(false);

            if inferior_transition_affected {
                ui.finish_inferior_execution_action();
            }

            if thread_action_affected {
                ui.finish_thread_execution_action();
            }

            let execution_transition_affected = if transition_targets_group {
                inferior_transition_affected
            } else {
                thread_transition_affected
            };

            if execution_transition_affected {
                ui.finish_execution_transition();
                ui.set_command_pending(false);
            }

            if ui.handle_native_until_stop(
                reason.as_deref(),
                address.as_deref(),
                thread_id.as_deref(),
            ) {
                return;
            }

            drop(ui);
            finish_stopped_state(weak_ui, client, reason, signal_name, signal_meaning, None);
        }
        MiEvent::BreakpointsChanged => refresh_breakpoints(weak_ui, client),
        MiEvent::ThreadsChanged { id, group_id } => {
            ui.model
                .record_thread_group(id.as_deref(), group_id.as_deref());

            if !ui.model.inferior_is_running() && !ui.model.native_until_active() {
                refresh_inferiors(weak_ui, client);

                if group_id.is_none() || group_id == ui.model.selected_inferior_id() {
                    refresh_threads(weak_ui, client);
                }
            }
        }
        MiEvent::ThreadExited { id, group_id } => {
            let current_thread_exited =
                ui.model.current_thread_id().as_deref() == Some(id.as_str());

            let execution_transition_affected = ui
                .model
                .execution_transition_matches_thread(Some(&id), false);

            let thread_action_affected = ui
                .model
                .thread_execution_transition_matches(Some(&id), false);
            let active_thread_exited =
                ui.model.active_thread_execution().as_deref() == Some(id.as_str());
            let until_thread_exited = ui.model.native_until_active() && active_thread_exited;
            ui.model.forget_thread_group(&id);

            let watch_for_orphaned_step = selected_thread_execution_may_be_orphaned(
                ui.model.active_thread_execution().as_deref(),
                ui.model.current_thread_id().as_deref(),
                &id,
                ui.model.inferior_is_running(),
                ui.model.non_stop_mode(),
            );

            if watch_for_orphaned_step {
                ui.model
                    .set_thread_execution_exit_candidate(Some(id.clone()));
            }

            if active_thread_exited && ui.model.non_stop_mode() == Some(true) {
                if until_thread_exited {
                    ui.abort_native_until();
                }

                if execution_transition_affected {
                    ui.finish_execution_transition();
                    ui.set_command_pending(false);
                }

                if thread_action_affected {
                    ui.finish_thread_execution_action();
                }

                ui.model.set_active_thread_execution(None);
                ui.model.set_thread_execution_exit_candidate(None);
            }

            if current_thread_exited && ui.model.non_stop_mode() == Some(true) {
                ui.model.set_current_thread_id(None);
                ui.set_controls_running(false);
                ui.set_debug_state_stale(true);
                ui.clear_debugger_state();

                ui.set_status(
                    "Thread exited",
                    &format!(
                        "Thread {id} exited. fgdb is selecting another stopped thread when one is available."
                    ),
                    Some("status-ready"),
                );

                refresh_inferiors(weak_ui, client);
                refresh_threads(weak_ui, client);
                return;
            }

            if !ui.model.inferior_is_running() && !ui.model.native_until_active() {
                refresh_inferiors(weak_ui, client);

                if group_id.is_none() || group_id == ui.model.selected_inferior_id() {
                    refresh_threads(weak_ui, client);
                }
            }
        }
        MiEvent::ThreadExitPrompt => {
            let candidate = ui.model.thread_execution_exit_candidate();

            if let Some(id) = candidate.as_deref()
                && selected_thread_execution_may_be_orphaned(
                    ui.model.active_thread_execution().as_deref(),
                    ui.model.current_thread_id().as_deref(),
                    id,
                    ui.model.inferior_is_running(),
                    ui.model.non_stop_mode(),
                )
            {
                recover_from_orphaned_thread_execution(client, id);
            } else {
                ui.model.set_thread_execution_exit_candidate(None);
            }
        }
        MiEvent::LibrariesChanged { group_id } => {
            ui.invalidate_allocator_probe_cache();
            ui.mark_modules_dirty();

            if !ui.model.inferior_is_running()
                && !ui.model.native_until_active()
                && (group_id.is_none() || group_id == ui.model.selected_inferior_id())
                && ui.take_modules_dirty()
            {
                refresh_modules(weak_ui, client);
            }
        }

        MiEvent::SelectionChanged {
            thread_id,
            group_id,
            frame_level,
        } => {
            ui.apply_gdb_selection(thread_id.as_deref(), group_id.as_deref());

            if let Some(level) = frame_level {
                ui.select_frame_in_view(level);
            }

            let inspectable =
                ui.model.selected_inferior_context_stopped() && !ui.model.native_until_active();
            refresh_inferiors(weak_ui, client);

            if inspectable {
                ui.set_controls_running(false);
                ui.set_debug_state_stale(false);
                refresh_stopped_state(weak_ui, client);
            }
        }
        MiEvent::CommandParameterChanged { parameter, value } => {
            // GDB emits these while processing init files too. Ready performs
            // the initial synchronization. Reacting before that boundary can
            // interleave application requests with MI bootstrap commands.
            if !client.is_ready() {
                return;
            }

            match parameter.as_str() {
                "prompt" => {
                    if let Some(prompt) = value.as_deref() {
                        ui.set_terminal_prompt(prompt);
                    }
                }
                "scheduler-locking" | "non-stop" => refresh_thread_policy(weak_ui, client),
                "exec-direction" => {
                    use crate::model::replay::ExecutionDirection;

                    let direction = match value.as_deref() {
                        Some("forward") => ExecutionDirection::Forward,
                        Some("reverse") => ExecutionDirection::Reverse,
                        _ => return,
                    };

                    {
                        let mut state = ui.model.replay.borrow_mut();
                        state.set_backend_direction(direction);
                        state.invalidate_query();
                    }

                    ui.render_replay_controls();
                }
                "follow-fork-mode" | "detach-on-fork" => refresh_fork_policy(weak_ui, client),
                "architecture" | "endian" => {
                    ui.reset_target_abi();
                    detect_target_abi(weak_ui, client);
                }
                "directories" | "substitute-path" => {
                    ui.invalidate_source_discovery();
                    request_initial_source(weak_ui, client);
                }
                _ => {}
            }
        }
        MiEvent::Performance(notice) => {
            ui.record_performance_notice(notice);
        }
        MiEvent::Error(message) => {
            if ui.model.native_until_active() {
                ui.abort_native_until();
            }

            ui.finish_execution_transition();
            ui.set_command_pending(false);
            ui.model.set_active_thread_execution(None);
            ui.model.set_thread_execution_exit_candidate(None);
            ui.model.set_pending_execution_inferior(None);
            ui.clear_inferior_action_pending();
            ui.clear_thread_action_pending();
            ui.set_status("Command failed", &message, Some("status-error"));
        }
        MiEvent::DebuggerUnusable(message) => {
            enter_gdb_recovery(&ui, "GDB recovery required", &message);
        }
        MiEvent::Disconnected => {
            ui.model.reset_replay();
            if ui.model.native_until_active() {
                ui.abort_native_until();
            }

            ui.finish_execution_transition();
            ui.set_command_pending(false);
            ui.model.set_active_thread_execution(None);
            ui.model.set_thread_execution_exit_candidate(None);
            ui.model.set_pending_execution_inferior(None);
            ui.clear_inferior_action_pending();
            ui.clear_thread_action_pending();
            ui.finish_full_resynchronization();
            ui.set_debug_state_stale(true);
            ui.clear_gef_capabilities();
            ui.clear_gdb_capabilities();
            ui.set_thread_control_policy(None, None);
            ui.clear_inferiors();
            ui.set_inferior_started(false);
            ui.reset_target_abi();
            ui.clear_debugger_state();

            ui.set_status(
                "Disconnected",
                "The GDB/MI channel was closed. Restart GDB from the Session menu.",
                Some("status-error"),
            );

            ui.set_controls_ready(false);
            ui.set_gdb_recovery_available(true);
        }
    }
}

fn selected_thread_execution_may_be_orphaned(
    active_thread: Option<&str>,
    current_thread: Option<&str>,
    exited_thread: &str,
    running: bool,
    non_stop: Option<bool>,
) -> bool {
    running
        && non_stop != Some(true)
        && active_thread == Some(exited_thread)
        && current_thread == Some(exited_thread)
}

fn recover_from_orphaned_thread_execution(client: &MiClient, thread_id: &str) {
    client.quarantine(format!(
        "Thread {thread_id} exited while GDB was completing its step and no replacement stop was reported. Restart GDB from the Session menu."
    ));
}

fn enter_gdb_recovery(ui: &Ui, title: &str, detail: &str) {
    ui.require_gdb_recovery(title, detail);
}

pub(super) fn finish_stopped_state(
    weak_ui: &Weak<Ui>,
    client: &MiClient,
    reason: Option<String>,
    signal_name: Option<String>,
    signal_meaning: Option<String>,
    status_detail: Option<String>,
) {
    let Some(ui) = weak_ui.upgrade() else {
        return;
    };

    ui.set_debug_state_stale(false);
    ui.set_controls_running(false);
    let reason = reason.unwrap_or_else(|| String::from("stopped"));
    let exited = reason.starts_with("exited");
    ui.set_thread_stop_reason(Some(&reason));

    if exited {
        ui.clear_debugger_state();
        refresh_inferiors(weak_ui, client);
        refresh_breakpoints(weak_ui, client);
    } else {
        ui.set_inferior_started(true);
        refresh_inferiors(weak_ui, client);
        refresh_stopped_state(weak_ui, client);
    }

    if ui.take_modules_dirty() {
        refresh_modules(weak_ui, client);
    }

    ui.show_signal(signal_name.as_deref(), signal_meaning.as_deref());

    let routine_step = signal_name.is_none()
        && signal_meaning.is_none()
        && status_detail.is_none()
        && matches!(
            reason.as_str(),
            "end-stepping-range" | "function-finished" | "location-reached"
        );

    let detail = status_detail.unwrap_or_else(|| {
        if exited && matches!(ui.model.session().as_ref(), Some(DebugSession::RrReplay { .. })) {
            return String::from("The replay reached program exit. Select Run to replay the trace from the beginning.");
        }
        if reason == "no-history" {
            return String::from("Reached the recorded history boundary. Choose Forward to replay or continue recording.");
        }

        let reason = reason.replace('-', " ");

        ui.stop_owner_summary().map_or_else(
            || format!("GDB reported: {reason}"),
            |owner| format!("{owner} stopped: {reason}"),
        )
    });

    if routine_step {
        ui.set_transient_status("Paused", &detail, Some("status-ready"));
    } else {
        ui.set_status(
            if exited { "Inferior exited" } else { "Paused" },
            &detail,
            Some("status-ready"),
        );
    }
}

pub(super) fn detect_target_abi(ui: &Weak<Ui>, client: &MiClient) {
    let weak_ui = ui.clone();

    if client
        .request("-gdb-show architecture", move |client, record| {
            let description = record
                .is_done()
                .then(|| record.field("value"))
                .flatten()
                .and_then(|value| value.as_const());

            let architecture = description
                .map(crate::debugger::TargetArchitecture::from_gdb_description)
                .unwrap_or_default();

            if let Some(ui) = weak_ui.upgrade() {
                ui.set_target_architecture(architecture);

                if let Some(bits) = description.and_then(
                    crate::debugger::TargetArchitecture::pointer_bits_from_gdb_description,
                ) {
                    ui.set_target_pointer_bits(bits);
                }

                if let Some(endian) = description
                    .and_then(crate::debugger::TargetEndian::from_architecture_description)
                {
                    ui.set_target_endian(Some(endian));
                }
            }

            detect_target_pointer_width(&weak_ui, client);
        })
        .is_err()
    {
        if let Some(ui) = ui.upgrade() {
            ui.set_target_architecture(TargetArchitecture::Unknown);
        }

        detect_target_pointer_width(ui, client);
    }
}

fn detect_target_pointer_width(ui: &Weak<Ui>, client: &MiClient) {
    let weak_ui = ui.clone();

    if client
        .request(
            "-data-evaluate-expression sizeof(void*)",
            move |client, record| {
                let bytes = record
                    .is_done()
                    .then(|| crate::debugger::evaluated_value(&record))
                    .flatten()
                    .and_then(|value| parse_pointer_size(&value));

                if let (Some(ui), Some(bytes)) = (weak_ui.upgrade(), bytes) {
                    ui.set_target_pointer_bits(bytes.saturating_mul(8));
                }

                detect_target_endian(&weak_ui, client);
            },
        )
        .is_err()
    {
        detect_target_endian(ui, client);
    }
}

fn parse_pointer_size(value: &str) -> Option<u32> {
    let value = value.split_whitespace().next()?.trim();

    value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
        .map_or_else(
            || value.parse::<u32>().ok(),
            |digits| u32::from_str_radix(digits, 16).ok(),
        )
        .filter(|bytes| matches!(bytes, 4 | 8))
}

fn detect_terminal_prompt(ui: &Weak<Ui>, client: &MiClient) {
    let weak_ui = ui.clone();

    let _ = client.request("-gdb-show prompt", move |_, record| {
        if record.is_done()
            && let Some(prompt) = record.field("value").and_then(|value| value.as_const())
            && let Some(ui) = weak_ui.upgrade()
        {
            ui.set_terminal_prompt(prompt);
        }
    });
}

fn detect_target_endian(ui: &Weak<Ui>, client: &MiClient) {
    let weak_ui = ui.clone();

    if client
        .request("-gdb-show endian", move |client, record| {
            let endian = record
                .is_done()
                .then(|| record.field("value"))
                .flatten()
                .and_then(|value| value.as_const())
                .and_then(crate::debugger::TargetEndian::from_gdb_description);

            if let Some(ui) = weak_ui.upgrade()
                && (endian.is_some() || ui.target_endian().is_none())
            {
                ui.set_target_endian(endian);
            }

            refresh_after_target_abi_detection(&weak_ui, client);
        })
        .is_err()
    {
        refresh_after_target_abi_detection(ui, client);
    }
}

fn refresh_after_target_abi_detection(ui: &Weak<Ui>, client: &MiClient) {
    let Some(current_ui) = ui.upgrade() else {
        return;
    };

    let started = current_ui.model.inferior_has_started();
    let running = current_ui.model.inferior_is_running();
    let resynchronized = current_ui.finish_full_resynchronization();
    drop(current_ui);

    if started && !running {
        refresh_stopped_state(ui, client);
    } else if !started {
        initial_stop::refresh(ui, client);
    }

    if resynchronized
        && !running
        && let Some(ui) = ui.upgrade()
    {
        ui.set_status(
            "Paused",
            "Debugger state was re-read from GDB.",
            Some("status-ready"),
        );
    }
}

fn detect_gef(ui: &Weak<Ui>, client: &MiClient) {
    let weak_ui = ui.clone();

    if client
        .request("-complete gef", move |client, record| {
            if crate::debugger::has_exact_command_completion(&record, "gef") {
                detect_gef_capabilities(weak_ui, client);
            } else if let Some(ui) = weak_ui.upgrade() {
                ui.clear_gef_capabilities();
            }
        })
        .is_err()
        && let Some(ui) = ui.upgrade()
    {
        ui.clear_gef_capabilities();
    }
}

struct GefCapabilityDiscovery {
    ui: Weak<Ui>,
    next: usize,
    available: HashSet<&'static str>,
}

fn detect_gef_capabilities(ui: Weak<Ui>, client: &MiClient) {
    let capabilities = crate::ui::GEF_COMMAND_CAPABILITIES;

    if capabilities.is_empty() {
        if let Some(ui) = ui.upgrade() {
            ui.show_gef_capabilities(&HashSet::new());
        }

        return;
    }

    let discovery = Rc::new(RefCell::new(GefCapabilityDiscovery {
        ui,
        next: 0,
        available: HashSet::with_capacity(capabilities.len()),
    }));

    probe_next_gef_capability(client, discovery);
}

fn probe_next_gef_capability(client: &MiClient, discovery: Rc<RefCell<GefCapabilityDiscovery>>) {
    loop {
        let capability = {
            let mut discovery = discovery.borrow_mut();

            if discovery.ui.upgrade().is_none() {
                return;
            }

            let Some(&capability) = crate::ui::GEF_COMMAND_CAPABILITIES.get(discovery.next) else {
                let Some(ui) = discovery.ui.upgrade() else {
                    return;
                };

                ui.show_gef_capabilities(&discovery.available);
                configure_gef_context(&discovery.ui, client);
                return;
            };

            discovery.next += 1;

            capability
        };

        let command = format!("-complete {}", crate::debugger::quote(capability));
        let discovery_for_response = Rc::clone(&discovery);

        if client
            .request(&command, move |client, record| {
                if crate::debugger::has_exact_command_completion(&record, capability) {
                    discovery_for_response
                        .borrow_mut()
                        .available
                        .insert(capability);
                }

                probe_next_gef_capability(client, discovery_for_response);
            })
            .is_ok()
        {
            return;
        }

        // A saturated or disconnected MI client rejected this probe. Skip it
        // without recursively walking the remaining capability list.
    }
}

fn configure_gef_context(ui: &Weak<Ui>, client: &MiClient) {
    let Some(current_ui) = ui.upgrade() else {
        return;
    };

    let visible = current_ui.gef_context_visible();
    let control = current_ui.detected_gef_context_control();
    current_ui.set_gef_context_hidden_by_fgdb(false);

    let Some(command) = gef_context_configuration_command(control, visible) else {
        return;
    };

    let command = crate::debugger::console_command(command);
    let weak_ui = ui.clone();

    if client
        .request(&command, move |_, record| {
            if let Some(ui) = weak_ui.upgrade() {
                ui.set_gef_context_hidden_by_fgdb(!visible && record.is_success());
            }
        })
        .is_err()
        && let Some(ui) = ui.upgrade()
    {
        ui.set_gef_context_hidden_by_fgdb(false);
    }
}

fn gef_context_configuration_command(
    control: GefContextControl,
    visible: bool,
) -> Option<&'static str> {
    match (control, visible) {
        (GefContextControl::ContextCommand, false) => Some("context off"),
        (GefContextControl::ContextCommand, true) => Some("context on"),
        (GefContextControl::OriginalGef, false) => Some("gef config context.enable false"),
        (GefContextControl::OriginalGef, true) => Some("gef config context.enable true"),
        (GefContextControl::None, _) => None,
    }
}

pub(super) fn request_initial_source(ui: &Weak<Ui>, client: &MiClient) {
    let Some(current_ui) = ui.upgrade() else {
        return;
    };

    let Some(generation) = current_ui.initial_source_generation() else {
        return;
    };

    let session = current_ui.model.current_session();
    let epoch = client.transport_epoch();
    let weak_ui = ui.clone();

    let current = Rc::new(move || {
        weak_ui.upgrade().filter(|ui| {
            ui.initial_source_generation() == Some(generation)
                && ui.model.session().as_ref() == session.as_ref()
                && ui.model.execution().ready
                && !ui.model.execution().session_pending
                && !ui.model.inferior_is_running()
        })
    });

    let guard = Rc::clone(&current);

    let _ = client.request_when(
        "-file-list-exec-source-file",
        move || guard().is_some(),
        move |client, record| {
            if !record.is_done() || client.transport_epoch() != epoch {
                return;
            }

            let (Some(ui), Some(source_file)) =
                (current(), crate::debugger::current_source(&record))
            else {
                return;
            };

            let language =
                crate::language::Language::from_path(Path::new(source_file.source_path()));

            let Some(pattern) = language
                .entrypoint_pattern()
                .filter(|_| !ui.model.inferior_has_started())
            else {
                ui.show_initial_source(&source_file);
                return;
            };

            // Query only the qualified entry symbol, not the loaded-source list.
            // Two matches are enough to reject an ambiguous entry point.
            let command = format!(
                "-symbol-info-functions --name {} --max-results 2",
                crate::debugger::quote(pattern)
            );

            let guard = Rc::clone(&current);
            let fallback = source_file.clone();

            if client
                .request_when(
                    &command,
                    move || guard().is_some_and(|ui| !ui.model.inferior_has_started()),
                    move |client, record| {
                        if client.transport_epoch() != epoch || record.class == "superseded" {
                            return;
                        }

                        let Some(ui) = current().filter(|ui| !ui.model.inferior_has_started())
                        else {
                            return;
                        };

                        let source = entrypoint_source(&record, language).unwrap_or(fallback);
                        ui.show_initial_source(&source);
                    },
                )
                .is_err()
            {
                ui.show_initial_source(&source_file);
            }
        },
    );
}

fn entrypoint_source(
    record: &MiRecord,
    language: crate::language::Language,
) -> Option<crate::debugger::SourceFile> {
    if !record.is_done() {
        return None;
    }

    let mut locations = crate::debugger::source_locations(record).into_iter();
    let location = locations.next()?;

    if locations.next().is_some()
        || location.line == 0
        || crate::language::Language::from_path(Path::new(location.source_path())) != language
    {
        return None;
    }

    Some(crate::debugger::SourceFile {
        file: location.file,
        fullname: location.fullname,
        line: location.line,
    })
}

pub(super) fn resynchronize_debugger_state(ui: &Weak<Ui>, client: &MiClient) {
    let Some(current_ui) = ui.upgrade() else {
        return;
    };

    if !current_ui.model.debugger_synchronization_available() {
        current_ui.set_status(
            "Refresh unavailable",
            "Wait for the current debugger action to finish or pause the target before refreshing debugger state.",
            Some("status-error"),
        );

        return;
    }

    current_ui.prepare_full_resynchronization();

    current_ui.set_status(
        "Refreshing debugger state",
        "Re-reading target ABI, frames, registers, variables, stop points, modules, memory, and inspectors…",
        None,
    );

    drop(current_ui);
    request_initial_source(ui, client);
    refresh_breakpoints(ui, client);
    refresh_inferiors(ui, client);
    refresh_fork_policy(ui, client);
    refresh_thread_policy(ui, client);
    refresh_modules(ui, client);
    client.refresh_pretty_printer_capabilities();
    detect_gef(ui, client);
    detect_target_abi(ui, client);
}

#[cfg(test)]
mod tests {
    use super::{
        GefContextControl, entrypoint_source, gef_context_configuration_command,
        parse_pointer_size, selected_thread_execution_may_be_orphaned,
    };

    #[test]
    fn initial_entrypoint_requires_one_source_in_the_expected_language() {
        use crate::{debugger::parse_record, language::Language};

        for (language, file, name) in [
            (Language::Zig, "fixture.zig", "fixture.main"),
            (Language::Odin, "fixture.odin", "fixture::main"),
        ] {
            let symbol = format!(r#"{{line="6",name="{name}"}}"#);

            let record = parse_record(&format!(
                r#"1^done,symbols={{debug=[{{filename="{file}",fullname="/src/{file}",symbols=[{symbol}]}}]}}"#
            ))
            .unwrap();

            let source = entrypoint_source(&record, language).unwrap();
            assert_eq!(source.source_path(), format!("/src/{file}"));
            assert_eq!(source.line, 6);
            assert!(entrypoint_source(&record, Language::C).is_none());

            let ambiguous = parse_record(&format!(
                r#"1^done,symbols={{debug=[{{filename="{file}",symbols=[{symbol},{symbol}]}}]}}"#
            ))
            .unwrap();

            assert!(entrypoint_source(&ambiguous, language).is_none());
        }

        for reply in [
            "1^done,symbols={}",
            r#"1^error,msg="Undefined MI command""#,
            r#"1^done,symbols={debug=[{filename="x.zig",symbols=[{line="0",name="x.main"}]}]}"#,
        ] {
            let record = parse_record(reply).unwrap();
            assert!(entrypoint_source(&record, Language::Zig).is_none());
        }
    }

    #[test]
    fn accepts_decimal_and_gdb_hex_pointer_sizes() {
        assert_eq!(parse_pointer_size("4"), Some(4));
        assert_eq!(parse_pointer_size("0x8"), Some(8));
        assert_eq!(parse_pointer_size("16"), None);
        assert_eq!(parse_pointer_size("not-a-size"), None);
    }

    #[test]
    fn configures_context_for_both_supported_gef_families() {
        assert_eq!(
            gef_context_configuration_command(GefContextControl::ContextCommand, false),
            Some("context off")
        );

        assert_eq!(
            gef_context_configuration_command(GefContextControl::ContextCommand, true),
            Some("context on")
        );

        assert_eq!(
            gef_context_configuration_command(GefContextControl::OriginalGef, false),
            Some("gef config context.enable false")
        );

        assert_eq!(
            gef_context_configuration_command(GefContextControl::OriginalGef, true),
            Some("gef config context.enable true")
        );

        assert_eq!(
            gef_context_configuration_command(GefContextControl::None, false),
            None
        );
    }

    #[test]
    fn only_flags_an_exited_selected_thread_during_all_stop_stepping() {
        assert!(selected_thread_execution_may_be_orphaned(
            Some("2"),
            Some("2"),
            "2",
            true,
            Some(false),
        ));

        assert!(!selected_thread_execution_may_be_orphaned(
            Some("2"),
            Some("1"),
            "2",
            true,
            Some(false),
        ));

        assert!(!selected_thread_execution_may_be_orphaned(
            Some("2"),
            Some("2"),
            "2",
            false,
            Some(false),
        ));

        assert!(!selected_thread_execution_may_be_orphaned(
            Some("2"),
            Some("2"),
            "2",
            true,
            Some(true),
        ));

        assert!(!selected_thread_execution_may_be_orphaned(
            None,
            Some("2"),
            "2",
            true,
            Some(false),
        ));
    }
}
