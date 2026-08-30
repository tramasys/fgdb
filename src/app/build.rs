use super::*;

pub fn build(application: &gtk::Application, launch_config: LaunchConfig) {
    let theme = Theme::graphite();
    theme.install();

    let ui = Rc::new(Ui::build(application, &launch_config, &theme));
    ui.set_controls_ready(false);

    let weak_ui = Rc::downgrade(&ui);
    let mi_client = match MiClient::open(move |client, event| {
        handle_mi_event(&weak_ui, client, event);
    }) {
        Ok(client) => client,
        Err(error) => {
            ui.set_status(
                "MI unavailable",
                &format!("Could not allocate the MI pseudo-terminal: {error}"),
                Some("status-error"),
            );
            ui.window.present();
            return;
        }
    };
    ui.connect_debug_controls(&mi_client);
    ui.connect_source_actions();
    ui.connect_session_actions();
    let disassembly_controller =
        DisassemblyController::new(Rc::downgrade(&ui), Rc::clone(&mi_client));
    let controller = Rc::clone(&disassembly_controller);
    ui.set_disassembly_handler(move |request| controller.handle(request));
    let until_controller = NativeUntilController::new(Rc::downgrade(&ui), Rc::clone(&mi_client));
    let controller = Rc::clone(&until_controller);
    ui.set_until_action_handler(move |action| controller.start(action));
    let controller = Rc::clone(&until_controller);
    ui.set_until_cancel_handler(move || controller.cancel());
    let controller = Rc::clone(&until_controller);
    ui.set_until_stop_handler(move |reason, address| controller.on_stopped(reason, address));
    let session_controller = SessionController::new(Rc::downgrade(&ui), Rc::clone(&mi_client));
    let controller = Rc::clone(&session_controller);
    ui.set_session_handler(move |session| controller.configure(session));
    let controller = Rc::clone(&session_controller);
    ui.set_session_action_handler(move |action| controller.action(action));
    let weak_ui = Rc::downgrade(&ui);
    let weak_client = Rc::downgrade(&mi_client);
    ui.set_frame_selection_handler(move |level| {
        let (Some(client), weak_ui) = (weak_client.upgrade(), weak_ui.clone()) else {
            return;
        };
        let _ = client.request(
            &format!("-stack-select-frame {level}"),
            move |client, record| {
                if record.is_done()
                    && weak_ui
                        .upgrade()
                        .is_some_and(|ui| !ui.inferior_is_running())
                {
                    refresh_stopped_state(&weak_ui, client);
                }
            },
        );
    });
    let weak_ui = Rc::downgrade(&ui);
    let weak_client = Rc::downgrade(&mi_client);
    ui.set_thread_selection_handler(move |id| {
        let (Some(client), weak_ui) = (weak_client.upgrade(), weak_ui.clone()) else {
            return;
        };
        let _ = client.request(&format!("-thread-select {id}"), move |client, record| {
            if record.is_done()
                && weak_ui
                    .upgrade()
                    .is_some_and(|ui| !ui.inferior_is_running())
            {
                refresh_stopped_state(&weak_ui, client);
            } else if let Some(ui) = weak_ui.upgrade() {
                ui.set_status(
                    "Thread selection failed",
                    record
                        .error_message()
                        .unwrap_or("GDB rejected the selected thread"),
                    Some("status-error"),
                );
            }
        });
    });
    let weak_ui = Rc::downgrade(&ui);
    let weak_client = Rc::downgrade(&mi_client);
    ui.set_instruction_handler(move |address| {
        let (Some(client), Some(ui)) = (weak_client.upgrade(), weak_ui.upgrade()) else {
            return;
        };
        let (command, detail) = ui.breakpoint_number_at_address(&address).map_or_else(
            || {
                (
                    format!("-break-insert *{address}"),
                    format!("Added instruction breakpoint at {address}"),
                )
            },
            |number| {
                (
                    format!("-break-delete {number}"),
                    format!("Deleted instruction breakpoint #{number}"),
                )
            },
        );
        drop(ui);
        mutate_breakpoint(weak_ui.clone(), &client, command, detail);
    });
    let weak_ui = Rc::downgrade(&ui);
    let weak_client = Rc::downgrade(&mi_client);
    ui.set_instruction_memory_handler(move |expression| {
        let (Some(client), Some(current_ui)) = (weak_client.upgrade(), weak_ui.upgrade()) else {
            return;
        };
        let generation = current_ui.current_stop_refresh_generation();
        drop(current_ui);
        let weak_ui = weak_ui.clone();
        let command = format!(
            "-data-read-memory-bytes {} 32",
            crate::debugger::quote(&expression)
        );
        let expression_for_response = expression.clone();
        let weak_ui_for_response = weak_ui.clone();
        let weak_ui_for_guard = weak_ui.clone();
        if client
            .request_when(
                &command,
                move || {
                    weak_ui_for_guard
                        .upgrade()
                        .is_some_and(|ui| ui.is_stop_refresh_current(generation))
                },
                move |_, record| {
                    let Some(ui) = weak_ui_for_response.upgrade() else {
                        return;
                    };
                    if !ui.is_stop_refresh_current(generation) {
                        return;
                    }
                    if let Some(memory) = crate::debugger::memory_block(&record) {
                        ui.show_instruction_memory(&expression_for_response, Ok(&memory));
                    } else {
                        ui.show_instruction_memory(
                            &expression_for_response,
                            Err(record.error_message().unwrap_or("memory is not readable")),
                        );
                    }
                },
            )
            .is_err()
            && let Some(ui) = weak_ui.upgrade()
            && ui.is_stop_refresh_current(generation)
        {
            ui.show_instruction_memory(&expression, Err("MI channel is unavailable"));
        }
    });
    let weak_ui = Rc::downgrade(&ui);
    let weak_client = Rc::downgrade(&mi_client);
    ui.set_memory_watch_handler(move |id, expression, byte_count| {
        let (Some(client), Some(current_ui)) = (weak_client.upgrade(), weak_ui.upgrade()) else {
            return;
        };
        let generation = current_ui.current_stop_refresh_generation();
        drop(current_ui);
        let weak_ui = weak_ui.clone();
        let command = format!(
            "-data-read-memory-bytes {} {byte_count}",
            crate::debugger::quote(&expression)
        );
        let weak_ui_for_response = weak_ui.clone();
        let weak_ui_for_guard = weak_ui.clone();
        if client
            .request_when(
                &command,
                move || {
                    weak_ui_for_guard
                        .upgrade()
                        .is_some_and(|ui| ui.is_stop_refresh_current(generation))
                },
                move |_, record| {
                    let Some(ui) = weak_ui_for_response.upgrade() else {
                        return;
                    };
                    if !ui.is_stop_refresh_current(generation) {
                        return;
                    }
                    if let Some(memory) = crate::debugger::memory_block(&record) {
                        ui.show_memory_watch(id, Ok(memory));
                    } else {
                        ui.show_memory_watch(
                            id,
                            Err(record.error_message().unwrap_or("memory is not readable")),
                        );
                    }
                },
            )
            .is_err()
            && let Some(ui) = weak_ui.upgrade()
            && ui.is_stop_refresh_current(generation)
        {
            ui.show_memory_watch(id, Err("MI channel is unavailable"));
        }
    });
    let weak_ui = Rc::downgrade(&ui);
    let weak_client = Rc::downgrade(&mi_client);
    ui.set_kernel_refresh_handler(move || {
        let (Some(client), weak_ui) = (weak_client.upgrade(), weak_ui.clone()) else {
            return;
        };
        request_kernel_refresh(weak_ui, client);
    });
    let weak_ui = Rc::downgrade(&ui);
    let weak_client = Rc::downgrade(&mi_client);
    ui.set_misc_refresh_handler(move || {
        let (Some(client), weak_ui) = (weak_client.upgrade(), weak_ui.clone()) else {
            return;
        };
        request_misc_refresh(weak_ui, client);
    });
    let weak_ui = Rc::downgrade(&ui);
    let weak_client = Rc::downgrade(&mi_client);
    ui.set_heap_inspection_handler(move |request| {
        let (Some(client), weak_ui) = (weak_client.upgrade(), weak_ui.clone()) else {
            return;
        };
        request_heap_inspection(weak_ui, client, request);
    });
    let weak_ui = Rc::downgrade(&ui);
    let weak_client = Rc::downgrade(&mi_client);
    ui.set_breakpoint_insert_handler(move |path, line| {
        let Some(client) = weak_client.upgrade() else {
            return;
        };
        insert_source_breakpoint(weak_ui.clone(), &client, path, line);
    });
    let weak_ui = Rc::downgrade(&ui);
    let weak_client = Rc::downgrade(&mi_client);
    ui.set_source_jump_handler(move |path, line| {
        let Some(client) = weak_client.upgrade() else {
            return;
        };
        run_to_source_line(weak_ui.clone(), &client, path, line);
    });
    let weak_ui = Rc::downgrade(&ui);
    let weak_client = Rc::downgrade(&mi_client);
    ui.set_breakpoint_delete_handler(move |number| {
        let Some(client) = weak_client.upgrade() else {
            return;
        };
        mutate_breakpoint(
            weak_ui.clone(),
            &client,
            format!("-break-delete {number}"),
            format!("Deleted breakpoint #{number}"),
        );
    });
    let weak_ui = Rc::downgrade(&ui);
    let weak_client = Rc::downgrade(&mi_client);
    ui.set_breakpoint_enabled_handler(move |number, enabled| {
        let Some(client) = weak_client.upgrade() else {
            return;
        };
        let Some(ui) = weak_ui.upgrade() else {
            return;
        };
        if !ui.set_breakpoint_enabled_pending(&number, enabled) {
            return;
        }
        drop(ui);
        let action = if enabled { "enable" } else { "disable" };
        mutate_breakpoint(
            weak_ui.clone(),
            &client,
            format!("-break-{action} {number}"),
            format!(
                "{} breakpoint or watchpoint #{number}",
                if enabled { "Enabled" } else { "Disabled" }
            ),
        );
    });
    let weak_ui = Rc::downgrade(&ui);
    let weak_client = Rc::downgrade(&mi_client);
    ui.set_breakpoint_bulk_delete_handler(move |numbers| {
        let Some(client) = weak_client.upgrade() else {
            return;
        };
        let count = numbers.len();
        mutate_breakpoint(
            weak_ui.clone(),
            &client,
            format!("-break-delete {}", numbers.join(" ")),
            format!(
                "Deleted {count} stop point{}",
                if count == 1 { "" } else { "s" }
            ),
        );
    });
    let weak_ui = Rc::downgrade(&ui);
    let weak_client = Rc::downgrade(&mi_client);
    ui.set_signal_catchpoint_handler(move |signal, existing| {
        let Some(client) = weak_client.upgrade() else {
            return;
        };
        let (command, detail) = existing.map_or_else(
            || {
                let console_command = format!("catch signal {signal}");
                (
                    format!(
                        "-interpreter-exec console {}",
                        crate::debugger::quote(&console_command)
                    ),
                    format!("Added a catchpoint for {signal}"),
                )
            },
            |number| {
                (
                    format!("-break-delete {number}"),
                    format!("Removed catchpoint #{number} for {signal}"),
                )
            },
        );
        mutate_breakpoint(weak_ui.clone(), &client, command, detail);
    });
    let weak_ui = Rc::downgrade(&ui);
    let weak_client = Rc::downgrade(&mi_client);
    ui.set_event_catchpoint_handler(move |event, existing| {
        let Some(client) = weak_client.upgrade() else {
            return;
        };
        let (command, detail) = existing.map_or_else(
            || {
                let command = if event == EventCatchpoint::RustPanic {
                    String::from("-break-insert -f rust_panic")
                } else {
                    format!(
                        "-interpreter-exec console {}",
                        crate::debugger::quote(event.command())
                    )
                };
                (command, format!("Added the {} stop point", event.label()))
            },
            |number| {
                (
                    format!("-break-delete {number}"),
                    format!("Removed stop point #{number}"),
                )
            },
        );
        mutate_breakpoint(weak_ui.clone(), &client, command, detail);
    });
    let weak_ui = Rc::downgrade(&ui);
    let weak_client = Rc::downgrade(&mi_client);
    ui.set_breakpoint_condition_handler(move |number, condition| {
        let Some(client) = weak_client.upgrade() else {
            return;
        };
        let (command, detail) = condition.map_or_else(
            || {
                (
                    format!("-break-condition {number}"),
                    format!("Cleared condition on breakpoint #{number}"),
                )
            },
            |condition| {
                (
                    format!(
                        "-break-condition {number} {}",
                        crate::debugger::quote(&condition)
                    ),
                    format!("Breakpoint #{number} now stops if {condition}"),
                )
            },
        );
        mutate_breakpoint(weak_ui.clone(), &client, command, detail);
    });
    let weak_ui = Rc::downgrade(&ui);
    let weak_client = Rc::downgrade(&mi_client);
    ui.set_breakpoint_editor_handler(move |request| {
        let Some(client) = weak_client.upgrade() else {
            return;
        };
        edit_breakpoint(weak_ui.clone(), &client, request);
    });
    let weak_ui = Rc::downgrade(&ui);
    let weak_client = Rc::downgrade(&mi_client);
    ui.set_watchpoint_insert_handler(move |expression, access| {
        let Some(client) = weak_client.upgrade() else {
            return;
        };
        let option = access.mi_option();
        let command = if option.is_empty() {
            format!("-break-watch {}", crate::debugger::quote(&expression))
        } else {
            format!(
                "-break-watch {option} {}",
                crate::debugger::quote(&expression)
            )
        };
        let kind = match access {
            WatchpointAccess::Write => "write",
            WatchpointAccess::Read => "read",
            WatchpointAccess::Access => "access",
        };
        mutate_breakpoint(
            weak_ui.clone(),
            &client,
            command,
            format!("Added {kind} watchpoint for {expression}"),
        );
    });
    let weak_ui = Rc::downgrade(&ui);
    let weak_client = Rc::downgrade(&mi_client);
    ui.set_source_symbol_handler(move |symbol| {
        let (Some(client), weak_ui) = (weak_client.upgrade(), weak_ui.clone()) else {
            return;
        };
        request_source_symbol(weak_ui, client, symbol, true);
    });
    let weak_ui = Rc::downgrade(&ui);
    let weak_client = Rc::downgrade(&mi_client);
    ui.set_variable_editor_handler(move |variable| {
        let (Some(client), weak_ui) = (weak_client.upgrade(), weak_ui.clone()) else {
            return;
        };
        request_value_type_metadata(weak_ui, client, variable);
    });
    let weak_ui = Rc::downgrade(&ui);
    let weak_client = Rc::downgrade(&mi_client);
    ui.set_variable_object_assignment_handler(move |variable, value| {
        let (Some(client), weak_ui) = (weak_client.upgrade(), weak_ui.clone()) else {
            return;
        };
        let command = variable.varobj.as_deref().map_or_else(
            || {
                let expression = assignment_expression(&variable.name, &value);
                format!(
                    "-data-evaluate-expression {}",
                    crate::debugger::quote(&expression)
                )
            },
            |varobj| {
                format!(
                    "-var-assign {} {}",
                    crate::debugger::quote(varobj),
                    crate::debugger::quote(&value)
                )
            },
        );
        let name = variable.name;
        let weak_ui_for_response = weak_ui.clone();
        if let Err(error) = client.request(&command, move |client, record| {
            let Some(ui) = weak_ui_for_response.upgrade() else {
                return;
            };
            if record.is_done() {
                ui.set_status(
                    "Paused",
                    &format!("Updated {name} to {value}"),
                    Some("status-ready"),
                );
                refresh_stopped_state(&weak_ui_for_response, client);
            } else {
                ui.set_status(
                    "Assignment failed",
                    record
                        .error_message()
                        .unwrap_or("GDB rejected the new value"),
                    Some("status-error"),
                );
            }
        }) && let Some(ui) = weak_ui.upgrade()
        {
            ui.set_status(
                "Assignment failed",
                &error.to_string(),
                Some("status-error"),
            );
        }
    });
    let weak_ui = Rc::downgrade(&ui);
    let weak_client = Rc::downgrade(&mi_client);
    ui.set_string_assignment_handler(move |variable, bytes, kind| {
        let (Some(client), weak_ui) = (weak_client.upgrade(), weak_ui.clone()) else {
            return;
        };
        assign_string(weak_ui, client, variable, bytes, kind);
    });
    let weak_ui = Rc::downgrade(&ui);
    let weak_client = Rc::downgrade(&mi_client);
    ui.set_float_assignment_handler(move |variable, raw_bytes| {
        let (Some(client), weak_ui) = (weak_client.upgrade(), weak_ui.clone()) else {
            return;
        };
        assign_float_bytes(weak_ui, client, variable, raw_bytes);
    });
    let weak_ui = Rc::downgrade(&ui);
    let weak_client = Rc::downgrade(&mi_client);
    ui.set_vector_assignment_handler(move |register, field, changes| {
        let (Some(client), weak_ui) = (weak_client.upgrade(), weak_ui.clone()) else {
            return;
        };
        let Some(expression) = vector_assignment_expression(&register, &field, &changes) else {
            return;
        };
        let command = format!(
            "-data-evaluate-expression {}",
            crate::debugger::quote(&expression)
        );
        let register_for_response = register.clone();
        let weak_ui_for_response = weak_ui.clone();
        if let Err(error) = client.request(&command, move |client, record| {
            let Some(ui) = weak_ui_for_response.upgrade() else {
                return;
            };
            if record.is_done() {
                ui.set_status(
                    "Paused",
                    &format!(
                        "Updated {} lane{} in ${register_for_response}",
                        changes.len(),
                        if changes.len() == 1 { "" } else { "s" }
                    ),
                    Some("status-ready"),
                );
                refresh_stopped_state(&weak_ui_for_response, client);
            } else {
                ui.set_status(
                    "Register assignment failed",
                    record
                        .error_message()
                        .unwrap_or("GDB rejected one of the lane values"),
                    Some("status-error"),
                );
            }
        }) && let Some(ui) = weak_ui.upgrade()
        {
            ui.set_status(
                "Register assignment failed",
                &error.to_string(),
                Some("status-error"),
            );
        }
    });
    let weak_ui = Rc::downgrade(&ui);
    let weak_client = Rc::downgrade(&mi_client);
    ui.set_variable_children_handler(move |variable, from| {
        let (Some(client), weak_ui) = (weak_client.upgrade(), weak_ui.clone()) else {
            return;
        };
        request_variable_children(weak_ui, client, variable, from);
    });
    let weak_ui = Rc::downgrade(&ui);
    let weak_client = Rc::downgrade(&mi_client);
    ui.set_expression_watch_refresh_handler(move || {
        let (Some(client), Some(ui)) = (weak_client.upgrade(), weak_ui.upgrade()) else {
            return;
        };
        let generation = ui.current_stop_refresh_generation();
        drop(ui);
        refresh_expression_watches(weak_ui.clone(), &client, generation);
    });

    let weak_ui = Rc::downgrade(&ui);
    launch_gdb(&ui.terminal, &launch_config, &mi_client, move |event| {
        handle_session_event(&weak_ui, event)
    });

    ui.window.present();

    // GTK owns the widget tree, while the event handlers intentionally keep
    // only weak references to the aggregated UI state. Retain that state until
    // application shutdown without introducing a widget/signal reference cycle.
    let retained_ui = Rc::new(RefCell::new(Some(ui)));
    application.connect_shutdown(move |_| {
        if let Some(ui) = retained_ui.borrow_mut().take() {
            ui.save_layout();
        }
    });
}

pub(super) fn assignment_expression(name: &str, value: &str) -> String {
    format!("{name} = ({value})")
}
