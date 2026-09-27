use super::*;
use crate::model::lifecycle::selected_thread_execution_may_be_orphaned;
use crate::model::lifecycle::{EventAdmission, admit_event};

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
            ui.model.observe_backend_ready(capabilities.clone());
            ui.reset_runtime_pretty_printer_scripts();
            ui.render_gdb_capabilities(Some(&capabilities));
            ui.render_backend_state();
            ui.clear_gef_capabilities();
            ui.invalidate_allocator_probe_cache();
            ui.invalidate_target_caches();

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
            ui.model.set_gdb_capabilities(capabilities.clone());
            ui.render_gdb_capabilities(Some(&capabilities));
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
            ui.model.observe_inferior_started(&id, pid);
            // A terminal user can load and run a different executable in the
            // same GDB process. Register-number caches are target-specific and
            // must not leak across that boundary. The stopped-state refresh
            // will establish the new ABI from GDB and the traced ELF.
            ui.invalidate_target_caches();
            ui.invalidate_allocator_probe_cache();
            refresh_inferiors(weak_ui, client);
            refresh_thread_policy(weak_ui, client);
        }
        MiEvent::InferiorExited { id, exit_code: _ } => {
            let selected_exited = ui.model.observe_inferior_exit(&id);
            ui.render_inferior_exit(selected_exited);

            if selected_exited {
                if ui.model.native_until_active() {
                    ui.abort_native_until();
                }

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
            let selected_affected = ui.model.observe_running(thread_id.as_deref());
            ui.schedule_running_context_render();
            ui.refresh_execution_controls();

            if !selected_affected || ui.model.native_until_active() {
                return;
            }

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
            details,
            reason,
            return_value,
            signal_name,
            signal_meaning,
            address,
            thread_id,
            group_id,
            frame_level,
            fork_pid,
            all_stopped,
        } => {
            ui.model.observe_stop(
                thread_id.as_deref(),
                group_id.as_deref(),
                frame_level.unwrap_or(0),
                fork_pid,
                all_stopped,
                return_value,
            );
            ui.render_observed_stop();

            if ui.handle_native_until_stop(
                reason.as_deref(),
                address.as_deref(),
                thread_id.as_deref(),
            ) {
                return;
            }

            ui.model
                .stop_info
                .borrow_mut()
                .record(crate::model::stop_info::StopEntry {
                    sequence: ui.model.observed_stop_sequence(),
                    epoch: client.transport_epoch(),
                    reason: reason
                        .as_deref()
                        .map(crate::debugger::stop_info::bounded)
                        .unwrap_or_else(|| "stopped".into()),
                    inferior: ui.model.selected_inferior_id(),
                    thread: thread_id,
                    address,
                    signal: signal_name.clone(),
                    meaning: signal_meaning.clone(),
                    details: *details,
                });

            ui.render_stop_info();
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
            let effect = ui.model.observe_thread_exit(&id);

            if effect.abort_until {
                ui.abort_native_until();
            }

            ui.refresh_execution_controls();

            if effect.selected_non_stop {
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
            let effect =
                ui.model
                    .observe_selection(thread_id.as_deref(), group_id.as_deref(), frame_level);
            ui.render_gdb_selection(effect.inferior_changed);

            if let Some(level) = frame_level {
                ui.select_frame_in_view(level);
            }

            ui.refresh_execution_controls();
            refresh_inferiors(weak_ui, client);

            if effect.inspectable {
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
            let until_active = ui.model.native_until_active();
            ui.model.execution_failed();

            if until_active {
                ui.abort_native_until();
            }

            ui.refresh_execution_controls();
            ui.set_status("Command failed", &message, Some("status-error"));
        }
        MiEvent::DebuggerUnusable(message) => {
            enter_gdb_recovery(&ui, "GDB recovery required", &message);
        }
        MiEvent::Disconnected => {
            let until_active = ui.model.native_until_active();
            ui.model.observe_backend_disconnected();

            if until_active {
                ui.abort_native_until();
            }

            ui.clear_gef_capabilities();
            ui.render_gdb_capabilities(None);
            ui.render_thread_control_policy();
            ui.invalidate_target_caches();
            ui.clear_debugger_state();

            ui.set_status(
                "Disconnected",
                "The GDB/MI channel was closed. Restart GDB from the Session menu.",
                Some("status-error"),
            );

            ui.render_backend_state();
        }
    }
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

    let reason = reason.unwrap_or_else(|| String::from("stopped"));
    let exited = ui.model.complete_stop(&reason);
    ui.refresh_execution_controls();

    if exited {
        ui.clear_debugger_state();
        refresh_inferiors(weak_ui, client);
        refresh_breakpoints(weak_ui, client);
    } else {
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
                ui.model.set_target_architecture(architecture);

                if let Some(bits) = description.and_then(
                    crate::debugger::TargetArchitecture::pointer_width_from_gdb_description,
                ) {
                    ui.model.set_target_pointer_width(bits);
                }

                if let Some(endian) = description
                    .and_then(crate::debugger::TargetEndian::from_architecture_description)
                {
                    ui.model.set_target_endian(Some(endian));
                }
            }

            detect_target_pointer_width(&weak_ui, client);
        })
        .is_err()
    {
        if let Some(ui) = ui.upgrade() {
            ui.model
                .set_target_architecture(TargetArchitecture::Unknown);
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
                    ui.model.set_target_pointer_bits(bytes.saturating_mul(8));
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
                && (endian.is_some() || ui.model.target_endian().is_none())
            {
                ui.model.set_target_endian(endian);
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
                        if client.transport_epoch() != epoch || record.is_superseded() {
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
            (Language::D, "fixture.d", "D main"),
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
