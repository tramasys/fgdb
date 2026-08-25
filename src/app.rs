use std::{
    cell::RefCell,
    path::PathBuf,
    rc::{Rc, Weak},
};

use gtk::prelude::*;

use crate::{
    config::LaunchConfig,
    debugger::{
        MemoryKind, MiClient, MiEvent, Register, SessionEvent, StackEntry, StackFrame, Variable,
        context::{
            MemoryRegion, build_stack_entries, is_pointer_register, looks_like_string_word,
            pointer_address, read_memory_regions,
        },
        launch_gdb,
    },
    theme::Theme,
    ui::{EventCatchpoint, Ui, WatchpointAccess},
};

const MAX_POINTER_CHAIN_DEPTH: usize = 3;
const AUTOMATIC_PRINT_ELEMENTS: usize = 128;
const VARIABLE_CHILD_PAGE_SIZE: usize = 128;
const STACK_WORD_COUNT: usize = 32;
const POINTER_STRING_PREVIEW_ELEMENTS: usize = 4096;

struct RegisterRefresh {
    ui: Weak<Ui>,
    generation: u64,
    registers: Vec<Register>,
    remaining: usize,
}

struct StackRefresh {
    ui: Weak<Ui>,
    generation: u64,
    entries: Vec<StackEntry>,
    remaining: usize,
}

struct StackInputs {
    ui: Weak<Ui>,
    generation: u64,
    frames: Option<Vec<StackFrame>>,
    registers: Option<Vec<Register>>,
}

struct VariableRefresh {
    ui: Weak<Ui>,
    generation: u64,
    variables: Vec<Variable>,
    next_index: usize,
}

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
    let weak_ui = Rc::downgrade(&ui);
    let weak_client = Rc::downgrade(&mi_client);
    ui.set_frame_selection_handler(move |level| {
        let (Some(client), weak_ui) = (weak_client.upgrade(), weak_ui.clone()) else {
            return;
        };
        let _ = client.request(
            &format!("-stack-select-frame {level}"),
            move |client, record| {
                if record.is_done() {
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
            if record.is_done() {
                refresh_stopped_state(&weak_ui, client);
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
        let (Some(client), weak_ui) = (weak_client.upgrade(), weak_ui.clone()) else {
            return;
        };
        let command = format!(
            "-data-read-memory-bytes {} 32",
            crate::debugger::quote(&expression)
        );
        let expression_for_response = expression.clone();
        let weak_ui_for_response = weak_ui.clone();
        if client
            .request(&command, move |_, record| {
                let Some(ui) = weak_ui_for_response.upgrade() else {
                    return;
                };
                if let Some(memory) = crate::debugger::memory_block(&record) {
                    ui.show_instruction_memory(&expression_for_response, Ok(&memory));
                } else {
                    ui.show_instruction_memory(
                        &expression_for_response,
                        Err(record.error_message().unwrap_or("memory is not readable")),
                    );
                }
            })
            .is_err()
            && let Some(ui) = weak_ui.upgrade()
        {
            ui.show_instruction_memory(&expression, Err("MI channel is unavailable"));
        }
    });
    let weak_ui = Rc::downgrade(&ui);
    let weak_client = Rc::downgrade(&mi_client);
    ui.set_memory_watch_handler(move |id, expression, byte_count| {
        let (Some(client), weak_ui) = (weak_client.upgrade(), weak_ui.clone()) else {
            return;
        };
        let command = format!(
            "-data-read-memory-bytes {} {byte_count}",
            crate::debugger::quote(&expression)
        );
        let weak_ui_for_response = weak_ui.clone();
        if client
            .request(&command, move |_, record| {
                let Some(ui) = weak_ui_for_response.upgrade() else {
                    return;
                };
                if let Some(memory) = crate::debugger::memory_block(&record) {
                    ui.show_memory_watch(id, Ok(&memory));
                } else {
                    ui.show_memory_watch(
                        id,
                        Err(record.error_message().unwrap_or("memory is not readable")),
                    );
                }
            })
            .is_err()
            && let Some(ui) = weak_ui.upgrade()
        {
            ui.show_memory_watch(id, Err("MI channel is unavailable"));
        }
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

fn assignment_expression(name: &str, value: &str) -> String {
    format!("{name} = ({value})")
}

fn handle_mi_event(weak_ui: &Weak<Ui>, client: &MiClient, event: MiEvent) {
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

fn request_initial_source(ui: &Weak<Ui>, client: &MiClient) {
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

fn refresh_stopped_state(ui: &Weak<Ui>, client: &MiClient) {
    let Some(current_ui) = ui.upgrade() else {
        return;
    };
    current_ui.clear_execution_location();
    let generation = current_ui.start_stop_refresh();
    for varobj in current_ui.variable_object_names() {
        delete_variable_object(client, &varobj);
    }
    drop(current_ui);

    refresh_modules(ui, client, generation);

    let stack_inputs = Rc::new(RefCell::new(StackInputs {
        ui: ui.clone(),
        generation,
        frames: None,
        registers: None,
    }));

    let weak_ui = ui.clone();
    let stack_inputs_for_frames = Rc::clone(&stack_inputs);
    if client
        .request("-stack-list-frames 0 24", move |client, record| {
            if !stop_refresh_is_current(&weak_ui, generation) {
                return;
            }
            let frames = if record.is_done() {
                crate::debugger::stack_frames(&record)
            } else {
                Vec::new()
            };
            if record.is_done()
                && let Some(ui) = weak_ui.upgrade()
            {
                ui.show_frames(&frames);
            }
            stack_inputs_for_frames.borrow_mut().frames = Some(frames);
            start_stack_refresh_if_ready(&stack_inputs_for_frames, client);
        })
        .is_err()
    {
        if let Some(ui) = stack_inputs.borrow().ui.upgrade() {
            ui.show_frames(&[]);
        }
        stack_inputs.borrow_mut().frames = Some(Vec::new());
        start_stack_refresh_if_ready(&stack_inputs, client);
    }

    let weak_ui = ui.clone();
    let _ = client.request("-stack-info-frame", move |client, record| {
        if !stop_refresh_is_current(&weak_ui, generation) {
            return;
        }
        if record.is_done()
            && let (Some(ui), Some(frame)) =
                (weak_ui.upgrade(), crate::debugger::current_frame(&record))
        {
            ui.show_execution_location(&frame);
            let pc = frame.address.clone();
            let architecture = frame.architecture.clone();
            let weak_ui = Rc::downgrade(&ui);
            let _ = client.request(
                "-data-disassemble -a $pc --opcodes bytes -- 0",
                move |_, record| {
                    if stop_refresh_is_current(&weak_ui, generation)
                        && record.is_done()
                        && let Some(ui) = weak_ui.upgrade()
                    {
                        ui.show_instructions(
                            &crate::debugger::instructions(&record),
                            &pc,
                            architecture.as_deref(),
                        );
                    }
                },
            );
        }
    });

    let weak_ui = ui.clone();
    let weak_ui_for_guard = ui.clone();
    let _ = client.request_with_print_limit_when(
        "-stack-list-variables --simple-values",
        AUTOMATIC_PRINT_ELEMENTS,
        move || stop_refresh_is_current(&weak_ui_for_guard, generation),
        move |client, record| {
            if record.is_done() {
                refresh_variable_objects(
                    weak_ui.clone(),
                    client,
                    generation,
                    crate::debugger::variables(&record),
                );
            }
        },
    );

    refresh_registers(ui, client, generation, stack_inputs);

    refresh_breakpoints(ui, client);
    refresh_threads(ui, client);
}

fn refresh_registers(
    ui: &Weak<Ui>,
    client: &MiClient,
    generation: u64,
    stack_inputs: Rc<RefCell<StackInputs>>,
) {
    let weak_ui = ui.clone();
    let stack_inputs_for_names = Rc::clone(&stack_inputs);
    if client
        .request("-data-list-register-names", move |client, record| {
            if !stop_refresh_is_current(&weak_ui, generation) {
                return;
            }
            if !record.is_done() {
                finish_empty_register_refresh(
                    &weak_ui,
                    client,
                    generation,
                    &stack_inputs_for_names,
                );
                return;
            }
            let names = crate::debugger::register_names(&record);
            let numbers = crate::debugger::compact_register_numbers(&names);
            if numbers.is_empty() {
                finish_empty_register_refresh(
                    &weak_ui,
                    client,
                    generation,
                    &stack_inputs_for_names,
                );
                return;
            }
            let command = format!(
                "-data-list-register-values x {}",
                numbers
                    .iter()
                    .map(usize::to_string)
                    .collect::<Vec<_>>()
                    .join(" ")
            );
            let weak_ui = weak_ui.clone();
            let weak_ui_for_values = weak_ui.clone();
            let stack_inputs_for_values = Rc::clone(&stack_inputs_for_names);
            if client
                .request(&command, move |client, record| {
                    if !stop_refresh_is_current(&weak_ui_for_values, generation) {
                        return;
                    }
                    if !record.is_done() {
                        finish_empty_register_refresh(
                            &weak_ui_for_values,
                            client,
                            generation,
                            &stack_inputs_for_values,
                        );
                        return;
                    }
                    let registers = crate::debugger::registers(&record, &names);
                    if let Some(ui) = weak_ui_for_values.upgrade() {
                        ui.show_registers_for_refresh(generation, &registers);
                    }
                    stack_inputs_for_values.borrow_mut().registers = Some(registers.clone());
                    start_stack_refresh_if_ready(&stack_inputs_for_values, client);
                    enrich_registers(weak_ui_for_values, client, generation, registers);
                })
                .is_err()
            {
                finish_empty_register_refresh(
                    &weak_ui,
                    client,
                    generation,
                    &stack_inputs_for_names,
                );
            }
        })
        .is_err()
    {
        finish_empty_register_refresh(ui, client, generation, &stack_inputs);
    }
}

fn refresh_variable_objects(
    ui: Weak<Ui>,
    client: &MiClient,
    generation: u64,
    variables: Vec<Variable>,
) {
    if let Some(ui) = ui.upgrade() {
        ui.show_locals_for_refresh(generation, &variables);
    }
    if variables.is_empty() {
        return;
    }
    if !variables.iter().any(Variable::needs_variable_object) {
        return;
    }
    let state = Rc::new(RefCell::new(VariableRefresh {
        ui,
        generation,
        variables,
        next_index: 0,
    }));
    request_next_variable_object(client, state);
}

fn request_next_variable_object(client: &MiClient, state: Rc<RefCell<VariableRefresh>>) {
    let (ui, generation) = {
        let state = state.borrow();
        (state.ui.clone(), state.generation)
    };
    if !stop_refresh_is_current(&ui, generation) {
        discard_variable_refresh(client, &state);
        return;
    }
    let next = {
        let mut state = state.borrow_mut();
        while state.next_index < state.variables.len()
            && !state.variables[state.next_index].needs_variable_object()
        {
            state.next_index += 1;
        }
        (state.next_index < state.variables.len()).then(|| {
            let index = state.next_index;
            state.next_index += 1;
            (index, state.variables[index].name.clone())
        })
    };
    let Some((index, display_name)) = next else {
        let (ui, generation, variables) = {
            let mut state = state.borrow_mut();
            (
                state.ui.clone(),
                state.generation,
                std::mem::take(&mut state.variables),
            )
        };
        if let Some(ui) = ui.upgrade() {
            ui.show_locals_for_refresh(generation, &variables);
        }
        return;
    };
    let command = format!("-var-create - * {}", crate::debugger::quote(&display_name));
    let state_for_response = Rc::clone(&state);
    let state_for_guard = Rc::clone(&state);
    if client
        .request_with_print_limit_when(
            &command,
            AUTOMATIC_PRINT_ELEMENTS,
            move || {
                let state = state_for_guard.borrow();
                stop_refresh_is_current(&state.ui, state.generation)
            },
            move |client, record| {
                let variable = record
                    .is_done()
                    .then(|| crate::debugger::variable_object(&record, &display_name))
                    .flatten();
                let (ui, generation) = {
                    let state = state_for_response.borrow();
                    (state.ui.clone(), state.generation)
                };
                if !stop_refresh_is_current(&ui, generation) {
                    if let Some(varobj) = variable
                        .as_ref()
                        .and_then(|variable| variable.varobj.as_deref())
                    {
                        delete_variable_object(client, varobj);
                    }
                    discard_variable_refresh(client, &state_for_response);
                    return;
                }
                if let Some(variable) = variable {
                    state_for_response.borrow_mut().variables[index] = variable;
                }
                request_next_variable_object(client, state_for_response);
            },
        )
        .is_err()
    {
        request_next_variable_object(client, state);
    }
}

fn stop_refresh_is_current(ui: &Weak<Ui>, generation: u64) -> bool {
    ui.upgrade()
        .is_some_and(|ui| ui.is_stop_refresh_current(generation))
}

fn delete_variable_object(client: &MiClient, varobj: &str) {
    let command = format!("-var-delete {}", crate::debugger::quote(varobj));
    // Parent deletion can legitimately remove child objects also present in
    // the expanded UI tree, so consume any resulting errors locally.
    let _ = client.request(&command, |_, _| {});
}

fn discard_variable_refresh(client: &MiClient, state: &Rc<RefCell<VariableRefresh>>) {
    let state = state.borrow();
    for varobj in state
        .variables
        .iter()
        .filter_map(|variable| variable.varobj.as_deref())
    {
        delete_variable_object(client, varobj);
    }
}

fn request_variable_children(ui: Weak<Ui>, client: Rc<MiClient>, variable: Variable, from: usize) {
    let Some(varobj) = variable.varobj.clone() else {
        return;
    };
    if variable.num_children > 0 {
        let to = from.saturating_add(VARIABLE_CHILD_PAGE_SIZE);
        let command = format!(
            "-var-list-children --all-values {} {from} {to}",
            crate::debugger::quote(&varobj),
        );
        let ui_for_response = ui.clone();
        let ui_for_guard = ui.clone();
        let varobj_for_response = varobj.clone();
        let varobj_for_guard = varobj.clone();
        let variable_for_response = variable.clone();
        if let Err(error) = client.request_with_print_limit_when(
            &command,
            AUTOMATIC_PRINT_ELEMENTS,
            move || {
                ui_for_guard
                    .upgrade()
                    .is_some_and(|ui| ui.has_variable_object(&varobj_for_guard))
            },
            move |_, record| {
                if let Some(ui) = ui_for_response.upgrade() {
                    if record.is_done() {
                        let children = crate::debugger::variable_children(&record);
                        let next = from.saturating_add(children.len());
                        let has_more = !children.is_empty()
                            && (crate::debugger::variable_children_have_more(&record)
                                || next < variable_for_response.num_children);
                        ui.show_variable_children_page(
                            &variable_for_response,
                            from,
                            &children,
                            has_more,
                        );
                    } else {
                        ui.show_variable_children_error(
                            &varobj_for_response,
                            record
                                .error_message()
                                .unwrap_or("GDB could not expand this value"),
                        );
                    }
                }
            },
        ) && let Some(ui) = ui.upgrade()
        {
            ui.show_variable_children_error(&varobj, &error.to_string());
        }
        return;
    }
    if !variable.is_pointer() {
        if let Some(ui) = ui.upgrade() {
            ui.show_variable_children(&varobj, &[]);
        }
        return;
    }
    let command = format!(
        "-var-info-path-expression {}",
        crate::debugger::quote(&varobj)
    );
    let ui_for_path = ui.clone();
    let client_for_path = Rc::clone(&client);
    let varobj_for_path = varobj.clone();
    let display_name = variable.name;
    if client
        .request(&command, move |_, record| {
            let Some(path) = crate::debugger::variable_path_expression(&record) else {
                if let Some(ui) = ui_for_path.upgrade() {
                    ui.show_variable_children_error(
                        &varobj_for_path,
                        record
                            .error_message()
                            .unwrap_or("GDB cannot dereference this pointer type"),
                    );
                }
                return;
            };
            let command = format!(
                "-var-create - * {}",
                crate::debugger::quote(&format!("*({path})"))
            );
            if !ui_for_path
                .upgrade()
                .is_some_and(|ui| ui.has_variable_object(&varobj_for_path))
            {
                return;
            }
            let ui_for_dereference = ui_for_path.clone();
            let ui_for_guard = ui_for_path.clone();
            let varobj_for_dereference = varobj_for_path.clone();
            let varobj_for_guard = varobj_for_path.clone();
            let ui_for_request_error = ui_for_path.clone();
            let varobj_for_request_error = varobj_for_path.clone();
            if client_for_path
                .request_with_print_limit_when(
                    &command,
                    AUTOMATIC_PRINT_ELEMENTS,
                    move || {
                        ui_for_guard
                            .upgrade()
                            .is_some_and(|ui| ui.has_variable_object(&varobj_for_guard))
                    },
                    move |client, record| {
                        let child = record
                            .is_done()
                            .then(|| {
                                crate::debugger::variable_object(
                                    &record,
                                    &format!("*{display_name}"),
                                )
                            })
                            .flatten();
                        if let Some(child) = child {
                            let attached = ui_for_dereference.upgrade().is_some_and(|ui| {
                                ui.show_variable_children(
                                    &varobj_for_dereference,
                                    std::slice::from_ref(&child),
                                )
                            });
                            if !attached && let Some(varobj) = child.varobj.as_deref() {
                                delete_variable_object(client, varobj);
                            }
                        } else if let Some(ui) = ui_for_dereference.upgrade() {
                            ui.show_variable_children_error(
                                &varobj_for_dereference,
                                record
                                    .error_message()
                                    .unwrap_or("GDB cannot dereference this pointer"),
                            );
                        }
                    },
                )
                .is_err()
                && let Some(ui) = ui_for_request_error.upgrade()
            {
                ui.show_variable_children_error(
                    &varobj_for_request_error,
                    "The MI channel is unavailable",
                );
            }
        })
        .is_err()
        && let Some(ui) = ui.upgrade()
    {
        ui.show_variable_children_error(&varobj, "The MI channel is unavailable");
    }
}

fn finish_empty_register_refresh(
    ui: &Weak<Ui>,
    client: &MiClient,
    generation: u64,
    stack_inputs: &Rc<RefCell<StackInputs>>,
) {
    if let Some(ui) = ui.upgrade() {
        ui.show_registers_for_refresh(generation, &[]);
    }
    stack_inputs.borrow_mut().registers = Some(Vec::new());
    start_stack_refresh_if_ready(stack_inputs, client);
}

fn enrich_registers(ui: Weak<Ui>, client: &MiClient, generation: u64, registers: Vec<Register>) {
    if !stop_refresh_is_current(&ui, generation) {
        return;
    }
    let indices = registers
        .iter()
        .enumerate()
        .filter(|(_, register)| {
            is_pointer_register(&register.name)
                && pointer_address(&register.value).is_some_and(|address| address != 0)
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    if indices.is_empty() {
        return;
    }

    let refresh = Rc::new(RefCell::new(RegisterRefresh {
        ui,
        generation,
        registers,
        remaining: indices.len(),
    }));
    for index in indices {
        request_register_chain(client, Rc::clone(&refresh), index, 0);
    }
}

fn request_register_chain(
    client: &MiClient,
    refresh: Rc<RefCell<RegisterRefresh>>,
    register_index: usize,
    depth: usize,
) {
    let (ui, generation) = {
        let state = refresh.borrow();
        (state.ui.clone(), state.generation)
    };
    if !stop_refresh_is_current(&ui, generation) {
        return;
    }
    let name = refresh.borrow().registers[register_index].name.clone();
    let expression = pointer_expression(&name, depth);
    let command = format!(
        "-data-evaluate-expression {}",
        crate::debugger::quote(&expression)
    );
    let refresh_for_handler = Rc::clone(&refresh);
    if client
        .request(&command, move |client, record| {
            let (ui, generation) = {
                let state = refresh_for_handler.borrow();
                (state.ui.clone(), state.generation)
            };
            if !stop_refresh_is_current(&ui, generation) {
                return;
            }
            let value = record
                .is_done()
                .then(|| crate::debugger::evaluated_value(&record))
                .flatten();
            let mut continue_chain = false;
            let mut string_address = None;
            if let Some(value) = value
                && let Some(address) = pointer_address(&value)
            {
                let mut state = refresh_for_handler.borrow_mut();
                let register = &mut state.registers[register_index];
                let chain = &mut register.pointer_chain;
                if chain
                    .iter()
                    .filter_map(|previous| pointer_address(previous))
                    .any(|previous| previous == address)
                {
                    chain.push(String::from("[loop detected]"));
                } else {
                    chain.push(value);
                    string_address = register_string_address(register, address, depth);
                    continue_chain =
                        string_address.is_none() && address != 0 && depth < MAX_POINTER_CHAIN_DEPTH;
                }
            }

            if let Some(address) = string_address {
                request_register_string(
                    client,
                    Rc::clone(&refresh_for_handler),
                    register_index,
                    address,
                );
            } else if continue_chain {
                request_register_chain(
                    client,
                    Rc::clone(&refresh_for_handler),
                    register_index,
                    depth + 1,
                );
            } else {
                complete_register_sequence(&refresh_for_handler);
            }
        })
        .is_err()
    {
        complete_register_sequence(&refresh);
    }
}

fn register_string_address(register: &Register, decoded_word: u64, depth: usize) -> Option<u64> {
    if depth == 0
        || matches!(register.name.as_str(), "rip" | "eip")
        || !looks_like_string_word(decoded_word)
    {
        return None;
    }
    register
        .pointer_chain
        .len()
        .checked_sub(2)
        .and_then(|index| register.pointer_chain.get(index))
        .filter(|value| !value.contains('<'))
        .and_then(|value| pointer_address(value))
}

fn request_register_string(
    client: &MiClient,
    refresh: Rc<RefCell<RegisterRefresh>>,
    register_index: usize,
    address: u64,
) {
    let (ui, generation) = {
        let state = refresh.borrow();
        (state.ui.clone(), state.generation)
    };
    if !stop_refresh_is_current(&ui, generation) {
        return;
    }
    let expression = format!("(char*)0x{address:x}");
    let command = format!(
        "-data-evaluate-expression {}",
        crate::debugger::quote(&expression)
    );
    let refresh_for_handler = Rc::clone(&refresh);
    let refresh_for_guard = Rc::clone(&refresh);
    if client
        .request_with_print_limit_when(
            &command,
            POINTER_STRING_PREVIEW_ELEMENTS,
            move || {
                let state = refresh_for_guard.borrow();
                stop_refresh_is_current(&state.ui, state.generation)
            },
            move |_, record| {
                let (ui, generation) = {
                    let state = refresh_for_handler.borrow();
                    (state.ui.clone(), state.generation)
                };
                if !stop_refresh_is_current(&ui, generation) {
                    return;
                }
                if let Some(value) = record
                    .is_done()
                    .then(|| crate::debugger::evaluated_value(&record))
                    .flatten()
                    .filter(|value| value.contains('"'))
                {
                    let mut state = refresh_for_handler.borrow_mut();
                    let chain = &mut state.registers[register_index].pointer_chain;
                    chain.pop();
                    chain.push(value);
                }
                complete_register_sequence(&refresh_for_handler);
            },
        )
        .is_err()
    {
        complete_register_sequence(&refresh);
    }
}

fn complete_register_sequence(refresh: &Rc<RefCell<RegisterRefresh>>) {
    let completed = {
        let mut state = refresh.borrow_mut();
        state.remaining = state.remaining.saturating_sub(1);
        if state.remaining == 0 {
            let ui = state.ui.clone();
            let generation = state.generation;
            Some((ui, generation, std::mem::take(&mut state.registers)))
        } else {
            None
        }
    };
    if let Some((ui, generation, registers)) = completed
        && let Some(ui) = ui.upgrade()
    {
        ui.show_registers_for_refresh(generation, &registers);
    }
}

fn pointer_expression(register: &str, depth: usize) -> String {
    let mut expression = format!("${register}");
    if depth == 0 {
        return format!("(void*)({expression})");
    }
    for _ in 0..depth {
        expression = format!("*(void**)({expression})");
    }
    expression
}

fn start_stack_refresh_if_ready(refresh: &Rc<RefCell<StackInputs>>, client: &MiClient) {
    let inputs = {
        let mut refresh = refresh.borrow_mut();
        match (refresh.frames.take(), refresh.registers.take()) {
            (Some(frames), Some(registers)) => {
                Some((refresh.ui.clone(), refresh.generation, registers, frames))
            }
            (frames, registers) => {
                refresh.frames = frames;
                refresh.registers = registers;
                None
            }
        }
    };
    let Some((ui, generation, registers, frames)) = inputs else {
        return;
    };
    if !stop_refresh_is_current(&ui, generation) {
        return;
    }
    let ui_for_request = ui.clone();
    if client
        .request("-list-thread-groups", move |client, record| {
            if !stop_refresh_is_current(&ui, generation) {
                return;
            }
            let regions = crate::debugger::inferior_pid(&record)
                .map(read_memory_regions)
                .unwrap_or_default();
            if let Some(current_ui) = ui.upgrade() {
                current_ui.show_memory_regions_for_refresh(generation, &regions);
                current_ui.refresh_memory_watches();
            }
            request_stack_memory(ui, client, generation, registers, frames, regions);
        })
        .is_err()
        && let Some(ui) = ui_for_request.upgrade()
    {
        ui.show_stack_for_refresh(generation, &[]);
        ui.show_memory_regions_for_refresh(generation, &[]);
        ui.refresh_memory_watches();
    }
}

fn request_stack_memory(
    ui: Weak<Ui>,
    client: &MiClient,
    generation: u64,
    registers: Vec<Register>,
    frames: Vec<StackFrame>,
    regions: Vec<MemoryRegion>,
) {
    if !stop_refresh_is_current(&ui, generation) {
        return;
    }
    let is_64_bit = registers.iter().any(|register| register.name == "rsp");
    let stack_register = if is_64_bit { "rsp" } else { "esp" };
    let word_size = if is_64_bit { 8 } else { 4 };
    let command = format!(
        "-data-read-memory-bytes ${stack_register} {}",
        word_size * STACK_WORD_COUNT
    );
    let ui_for_request = ui.clone();
    if client
        .request(&command, move |client, record| {
            if !stop_refresh_is_current(&ui, generation) {
                return;
            }
            let Some(memory) = crate::debugger::memory_block(&record) else {
                if let Some(ui) = ui.upgrade() {
                    ui.show_stack_for_refresh(generation, &[]);
                }
                return;
            };
            let entries = build_stack_entries(&memory, word_size, &registers, &frames, &regions);
            if let Some(ui) = ui.upgrade() {
                ui.show_stack_for_refresh(generation, &entries);
            }
            enrich_stack(ui, client, generation, entries, stack_register);
        })
        .is_err()
        && let Some(ui) = ui_for_request.upgrade()
    {
        ui.show_stack_for_refresh(generation, &[]);
    }
}

fn enrich_stack(
    ui: Weak<Ui>,
    client: &MiClient,
    generation: u64,
    entries: Vec<StackEntry>,
    stack_register: &'static str,
) {
    if !stop_refresh_is_current(&ui, generation) {
        return;
    }
    let indices = entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| {
            pointer_address(&entry.value).is_some_and(|value| value != 0)
                && (entry.region.is_some()
                    || !entry.value_registers.is_empty()
                    || entry.return_frame.is_some())
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    if indices.is_empty() {
        return;
    }
    let refresh = Rc::new(RefCell::new(StackRefresh {
        ui,
        generation,
        entries,
        remaining: indices.len(),
    }));
    for index in indices {
        request_stack_chain(client, Rc::clone(&refresh), index, stack_register, 0);
    }
}

fn request_stack_chain(
    client: &MiClient,
    refresh: Rc<RefCell<StackRefresh>>,
    entry_index: usize,
    stack_register: &'static str,
    depth: usize,
) {
    let (ui, generation) = {
        let state = refresh.borrow();
        (state.ui.clone(), state.generation)
    };
    if !stop_refresh_is_current(&ui, generation) {
        return;
    }
    let offset = refresh.borrow().entries[entry_index].offset;
    let expression = stack_pointer_expression(stack_register, offset, depth);
    let command = format!(
        "-data-evaluate-expression {}",
        crate::debugger::quote(&expression)
    );
    let refresh_for_handler = Rc::clone(&refresh);
    if client
        .request(&command, move |client, record| {
            let (ui, generation) = {
                let state = refresh_for_handler.borrow();
                (state.ui.clone(), state.generation)
            };
            if !stop_refresh_is_current(&ui, generation) {
                return;
            }
            let value = record
                .is_done()
                .then(|| crate::debugger::evaluated_value(&record))
                .flatten();
            let mut continue_chain = false;
            let mut string_address = None;
            if let Some(value) = value
                && let Some(address) = pointer_address(&value)
            {
                let mut state = refresh_for_handler.borrow_mut();
                let entry = &mut state.entries[entry_index];
                let chain = &mut entry.pointer_chain;
                if chain
                    .iter()
                    .filter_map(|previous| pointer_address(previous))
                    .any(|previous| previous == address)
                {
                    chain.push(String::from("[loop detected]"));
                } else {
                    chain.push(value);
                    string_address = stack_string_address(entry, address, depth);
                    continue_chain =
                        string_address.is_none() && address != 0 && depth < MAX_POINTER_CHAIN_DEPTH;
                }
            }
            if let Some(address) = string_address {
                request_stack_string(
                    client,
                    Rc::clone(&refresh_for_handler),
                    entry_index,
                    address,
                );
            } else if continue_chain {
                request_stack_chain(
                    client,
                    Rc::clone(&refresh_for_handler),
                    entry_index,
                    stack_register,
                    depth + 1,
                );
            } else {
                complete_stack_sequence(&refresh_for_handler);
            }
        })
        .is_err()
    {
        complete_stack_sequence(&refresh);
    }
}

fn stack_string_address(entry: &StackEntry, decoded_word: u64, depth: usize) -> Option<u64> {
    if depth == 0
        || !looks_like_string_word(decoded_word)
        || matches!(entry.memory_kind, MemoryKind::Code | MemoryKind::Rwx)
    {
        return None;
    }
    entry
        .pointer_chain
        .len()
        .checked_sub(2)
        .and_then(|index| entry.pointer_chain.get(index))
        .and_then(|value| pointer_address(value))
}

fn request_stack_string(
    client: &MiClient,
    refresh: Rc<RefCell<StackRefresh>>,
    entry_index: usize,
    address: u64,
) {
    let (ui, generation) = {
        let state = refresh.borrow();
        (state.ui.clone(), state.generation)
    };
    if !stop_refresh_is_current(&ui, generation) {
        return;
    }
    let expression = format!("(char*)0x{address:x}");
    let command = format!(
        "-data-evaluate-expression {}",
        crate::debugger::quote(&expression)
    );
    let refresh_for_handler = Rc::clone(&refresh);
    let refresh_for_guard = Rc::clone(&refresh);
    if client
        .request_with_print_limit_when(
            &command,
            POINTER_STRING_PREVIEW_ELEMENTS,
            move || {
                let state = refresh_for_guard.borrow();
                stop_refresh_is_current(&state.ui, state.generation)
            },
            move |_, record| {
                let (ui, generation) = {
                    let state = refresh_for_handler.borrow();
                    (state.ui.clone(), state.generation)
                };
                if !stop_refresh_is_current(&ui, generation) {
                    return;
                }
                if let Some(value) = record
                    .is_done()
                    .then(|| crate::debugger::evaluated_value(&record))
                    .flatten()
                    .filter(|value| value.contains('"'))
                {
                    let mut state = refresh_for_handler.borrow_mut();
                    let entry = &mut state.entries[entry_index];
                    entry.pointer_chain.pop();
                    entry.pointer_chain.push(value);
                    entry.memory_kind = MemoryKind::String;
                }
                complete_stack_sequence(&refresh_for_handler);
            },
        )
        .is_err()
    {
        complete_stack_sequence(&refresh);
    }
}

fn complete_stack_sequence(refresh: &Rc<RefCell<StackRefresh>>) {
    let completed = {
        let mut state = refresh.borrow_mut();
        state.remaining = state.remaining.saturating_sub(1);
        if state.remaining == 0 {
            for entry in &mut state.entries {
                if entry
                    .pointer_chain
                    .iter()
                    .skip(1)
                    .filter_map(|value| pointer_address(value))
                    .any(looks_like_string_word)
                {
                    entry.memory_kind = MemoryKind::String;
                }
            }
            let ui = state.ui.clone();
            let generation = state.generation;
            Some((ui, generation, std::mem::take(&mut state.entries)))
        } else {
            None
        }
    };
    if let Some((ui, generation, entries)) = completed
        && let Some(ui) = ui.upgrade()
    {
        ui.show_stack_for_refresh(generation, &entries);
    }
}

fn stack_pointer_expression(register: &str, offset: usize, depth: usize) -> String {
    let mut expression = format!("*(void**)(${register}+0x{offset:x})");
    for _ in 0..depth {
        expression = format!("*(void**)({expression})");
    }
    expression
}

fn insert_source_breakpoint(ui: Weak<Ui>, client: &MiClient, path: PathBuf, line: u32) {
    if !client.is_ready() {
        if let Some(ui) = ui.upgrade() {
            ui.set_status(
                "Breakpoint unavailable",
                "Wait for the GDB/MI channel to become ready.",
                Some("status-error"),
            );
        }
        return;
    }
    let location = format!("{}:{line}", path.display());
    if let Some(ui) = ui.upgrade() {
        ui.set_command_pending(true);
        ui.set_status(
            "Checking source line",
            &format!("Looking for executable code at {location}"),
            None,
        );
    }
    let command = format!(
        "-symbol-list-lines {}",
        crate::debugger::quote(&path.to_string_lossy())
    );
    let ui_for_response = ui.clone();
    let path_for_response = path.clone();
    if let Err(error) = client.request(&command, move |client, record| {
        if !record.is_done() {
            if let Some(ui) = ui_for_response.upgrade() {
                ui.set_command_pending(false);
                ui.set_status(
                    "Breakpoint unavailable",
                    record
                        .error_message()
                        .unwrap_or("GDB could not inspect executable lines for this source file"),
                    Some("status-error"),
                );
            }
            return;
        }
        if !crate::debugger::executable_source_lines(&record).contains(&line) {
            if let Some(ui) = ui_for_response.upgrade() {
                ui.set_command_pending(false);
                ui.set_status(
                    "No breakpoint added",
                    &format!("{location} contains no executable code"),
                    None,
                );
            }
            return;
        }
        request_exact_source_breakpoint(
            ui_for_response.clone(),
            client,
            path_for_response.clone(),
            line,
        );
    }) && let Some(ui) = ui.upgrade()
    {
        ui.set_command_pending(false);
        ui.set_status(
            "Breakpoint unavailable",
            &error.to_string(),
            Some("status-error"),
        );
    }
}

fn request_exact_source_breakpoint(ui: Weak<Ui>, client: &MiClient, path: PathBuf, line: u32) {
    let location = format!("{}:{line}", path.display());
    let command = format!("-break-insert {}", crate::debugger::quote(&location));
    let ui_for_response = ui.clone();
    let path_for_response = path.clone();
    if let Err(error) = client.request(&command, move |client, record| {
        if !record.is_done() {
            if let Some(ui) = ui_for_response.upgrade() {
                ui.set_command_pending(false);
                ui.set_status(
                    "Breakpoint failed",
                    record
                        .error_message()
                        .unwrap_or("GDB rejected the source breakpoint"),
                    Some("status-error"),
                );
            }
            refresh_breakpoints(&ui_for_response, client);
            return;
        }

        let inserted = crate::debugger::inserted_breakpoints(&record);
        let exact = inserted.iter().any(|breakpoint| {
            breakpoint.line == Some(line)
                && breakpoint.source_path().is_some_and(|reported| {
                    crate::source::paths_match(&path_for_response, reported)
                })
        });
        if exact {
            if let Some(ui) = ui_for_response.upgrade() {
                ui.set_command_pending(false);
                ui.set_status(
                    "Paused",
                    &format!("Added breakpoint at {location}"),
                    Some("status-ready"),
                );
            }
            refresh_breakpoints(&ui_for_response, client);
        } else {
            remove_relocated_source_breakpoint(ui_for_response.clone(), client, inserted, location);
        }
    }) && let Some(ui) = ui.upgrade()
    {
        ui.set_command_pending(false);
        ui.set_status(
            "Breakpoint failed",
            &error.to_string(),
            Some("status-error"),
        );
    }
}

fn remove_relocated_source_breakpoint(
    ui: Weak<Ui>,
    client: &MiClient,
    inserted: Vec<crate::debugger::Breakpoint>,
    requested_location: String,
) {
    let mut numbers = inserted
        .iter()
        .map(|breakpoint| breakpoint.command_number().to_owned())
        .collect::<Vec<_>>();
    numbers.sort();
    numbers.dedup();
    if numbers.is_empty() {
        if let Some(ui) = ui.upgrade() {
            ui.set_command_pending(false);
            ui.set_status(
                "Breakpoint rejected",
                "GDB did not return an exact source location for the breakpoint",
                Some("status-error"),
            );
        }
        refresh_breakpoints(&ui, client);
        return;
    }

    let command = format!("-break-delete {}", numbers.join(" "));
    let ui_for_response = ui.clone();
    if let Err(error) = client.request(&command, move |client, record| {
        if let Some(ui) = ui_for_response.upgrade() {
            ui.set_command_pending(false);
            if record.is_done() {
                ui.set_status(
                    "No breakpoint added",
                    &format!(
                        "{requested_location} did not resolve exactly; GDB's relocated breakpoint was removed"
                    ),
                    None,
                );
            } else {
                ui.set_status(
                    "Breakpoint cleanup failed",
                    record
                        .error_message()
                        .unwrap_or("Could not remove GDB's relocated breakpoint"),
                    Some("status-error"),
                );
            }
        }
        refresh_breakpoints(&ui_for_response, client);
    }) && let Some(ui) = ui.upgrade()
    {
        ui.set_command_pending(false);
        ui.set_status(
            "Breakpoint cleanup failed",
            &error.to_string(),
            Some("status-error"),
        );
    }
}

fn mutate_breakpoint(ui: Weak<Ui>, client: &MiClient, command: String, success: String) {
    if !client.is_ready() {
        if let Some(ui) = ui.upgrade() {
            ui.set_status(
                "Stop-point command unavailable",
                "Wait for the GDB/MI channel to become ready.",
                Some("status-error"),
            );
        }
        return;
    }
    if let Some(ui) = ui.upgrade() {
        ui.set_command_pending(true);
    }
    let ui_for_response = ui.clone();
    if let Err(error) = client.request(&command, move |client, record| {
        if let Some(ui) = ui_for_response.upgrade() {
            ui.set_command_pending(false);
            if record.is_done() {
                ui.set_status("Paused", &success, Some("status-ready"));
            } else {
                ui.set_status(
                    "Stop-point command failed",
                    record
                        .error_message()
                        .unwrap_or("GDB rejected the breakpoint or watchpoint command"),
                    Some("status-error"),
                );
            }
        }
        refresh_breakpoints(&ui_for_response, client);
    }) && let Some(ui) = ui.upgrade()
    {
        ui.set_command_pending(false);
        ui.set_status(
            "Stop-point command failed",
            &error.to_string(),
            Some("status-error"),
        );
    }
}

fn refresh_breakpoints(ui: &Weak<Ui>, client: &MiClient) {
    let Some(current_ui) = ui.upgrade() else {
        return;
    };
    let generation = current_ui.start_breakpoint_refresh();
    drop(current_ui);
    let weak_ui = ui.clone();
    let _ = client.request("-break-list", move |_, record| {
        if record.is_done()
            && let Some(ui) = weak_ui.upgrade()
        {
            ui.show_breakpoints_for_refresh(generation, crate::debugger::breakpoints(&record));
        }
    });
}

fn refresh_threads(ui: &Weak<Ui>, client: &MiClient) {
    let Some(current_ui) = ui.upgrade() else {
        return;
    };
    let generation = current_ui.start_thread_refresh();
    drop(current_ui);
    let weak_ui = ui.clone();
    let _ = client.request("-thread-info", move |client, record| {
        if !record.is_done() {
            return;
        }
        let threads = crate::debugger::threads(&record);
        let Some(ui) = weak_ui.upgrade() else {
            return;
        };
        if !ui.is_thread_refresh_current(generation) {
            return;
        }
        if !threads.is_empty() {
            ui.set_inferior_started(true);
        }
        ui.show_threads_for_refresh(generation, &threads);
        drop(ui);
        if !threads.iter().any(|thread| thread.current) {
            return;
        }
        let weak_ui = weak_ui.clone();
        let _ = client.request(
            &format!(
                "-data-evaluate-expression {}",
                crate::debugger::quote("(void*)$pc")
            ),
            move |_, record| {
                if !record.is_done() {
                    return;
                }
                let Some(value) = crate::debugger::evaluated_value(&record) else {
                    return;
                };
                let Some(symbol) = symbol_annotation(&value).map(str::to_owned) else {
                    return;
                };
                let mut threads = threads;
                if let Some(thread) = threads.iter_mut().find(|thread| thread.current) {
                    thread.pc_symbol = Some(symbol);
                }
                if let Some(ui) = weak_ui.upgrade() {
                    ui.show_threads_for_refresh(generation, &threads);
                }
            },
        );
    });
}

fn refresh_modules(ui: &Weak<Ui>, client: &MiClient, generation: u64) {
    let weak_ui = ui.clone();
    let _ = client.request("-file-list-shared-libraries", move |_, record| {
        if stop_refresh_is_current(&weak_ui, generation)
            && let Some(ui) = weak_ui.upgrade()
        {
            let modules = if record.is_done() {
                crate::debugger::shared_libraries(&record)
            } else {
                Vec::new()
            };
            ui.show_modules(&modules);
        }
    });
}

fn infer_initial_stop_reason(ui: &Weak<Ui>, client: &MiClient) {
    let weak_ui = ui.clone();
    let _ = client.request(
        &format!(
            "-data-evaluate-expression {}",
            crate::debugger::quote("$_hit_bpnum")
        ),
        move |client, record| {
            if !record.is_done() {
                return;
            }
            let hit_breakpoint = crate::debugger::evaluated_value(&record)
                .as_deref()
                .and_then(parse_gdb_integer)
                .is_some_and(|number| number != 0);
            if hit_breakpoint {
                if let Some(ui) = weak_ui.upgrade() {
                    ui.set_thread_stop_reason(Some("breakpoint-hit"));
                }
                refresh_threads(&weak_ui, client);
            }
        },
    );
}

fn request_source_symbol(
    ui: Weak<Ui>,
    client: Rc<MiClient>,
    symbol: String,
    load_libraries_on_miss: bool,
) {
    let pattern = source_symbol_pattern(&symbol);
    let command = format!(
        "-symbol-info-functions --name {} --max-results 256",
        crate::debugger::quote(&pattern)
    );
    if let Some(ui) = ui.upgrade() {
        ui.set_status(
            "Resolving source",
            &format!("Looking up {symbol} through GDB…"),
            None,
        );
    }
    let ui_for_response = ui.clone();
    let client_for_response = Rc::clone(&client);
    let symbol_for_response = symbol.clone();
    if let Err(error) = client.request(&command, move |_, record| {
        let locations = record
            .is_done()
            .then(|| crate::debugger::source_locations(&record));
        match locations {
            Some(locations) if !locations.is_empty() => {
                if let Some(ui) = ui_for_response.upgrade() {
                    ui.show_source_locations(&symbol_for_response, &locations);
                }
            }
            Some(_) if load_libraries_on_miss => load_library_symbols_for_source(
                ui_for_response.clone(),
                Rc::clone(&client_for_response),
                symbol_for_response.clone(),
            ),
            Some(_) => {
                if let Some(ui) = ui_for_response.upgrade() {
                    ui.show_source_locations(&symbol_for_response, &[]);
                }
            }
            None => {
                if let Some(ui) = ui_for_response.upgrade() {
                    ui.set_status(
                        "Symbol lookup failed",
                        record
                            .error_message()
                            .unwrap_or("GDB could not resolve that source symbol"),
                        Some("status-error"),
                    );
                }
            }
        }
    }) && let Some(ui) = ui.upgrade()
    {
        ui.set_status(
            "Symbol lookup failed",
            &error.to_string(),
            Some("status-error"),
        );
    }
}

fn load_library_symbols_for_source(ui: Weak<Ui>, client: Rc<MiClient>, symbol: String) {
    if let Some(ui) = ui.upgrade() {
        ui.set_status(
            "Loading library symbols",
            &format!("No definition for {symbol} was loaded; asking GDB to load shared libraries…"),
            None,
        );
    }
    let command = format!(
        "-interpreter-exec console {}",
        crate::debugger::quote("sharedlibrary")
    );
    let ui_for_response = ui.clone();
    let client_for_response = Rc::clone(&client);
    let symbol_for_response = symbol.clone();
    if let Err(error) = client.request(&command, move |_, record| {
        if record.is_done() {
            request_source_symbol(
                ui_for_response.clone(),
                Rc::clone(&client_for_response),
                symbol_for_response.clone(),
                false,
            );
        } else if let Some(ui) = ui_for_response.upgrade() {
            ui.set_status(
                "Library symbols unavailable",
                record.error_message().unwrap_or(
                    "GDB could not load shared-library symbols; pause the target and try again",
                ),
                Some("status-error"),
            );
        }
    }) && let Some(ui) = ui.upgrade()
    {
        ui.set_status(
            "Library symbols unavailable",
            &error.to_string(),
            Some("status-error"),
        );
    }
}

fn source_symbol_pattern(symbol: &str) -> String {
    symbol
        .split("::")
        .filter(|component| !component.is_empty())
        .map(|component| {
            component
                .chars()
                .fold(String::new(), |mut escaped, character| {
                    if matches!(
                        character,
                        '\\' | '.'
                            | '+'
                            | '*'
                            | '?'
                            | '('
                            | ')'
                            | '['
                            | ']'
                            | '{'
                            | '}'
                            | '^'
                            | '$'
                            | '|'
                    ) {
                        escaped.push('\\');
                    }
                    escaped.push(character);
                    escaped
                })
        })
        .collect::<Vec<_>>()
        .join(".*::")
}

fn vector_assignment_expression(
    register: &str,
    field: &str,
    changes: &[(usize, String)],
) -> Option<String> {
    (!changes.is_empty()).then(|| {
        format!(
            "({})",
            changes
                .iter()
                .map(|(lane, value)| format!("${register}.{field}[{lane}] = ({value})"))
                .collect::<Vec<_>>()
                .join(", ")
        )
    })
}

fn parse_gdb_integer(value: &str) -> Option<u64> {
    let value = value.trim();
    value.strip_prefix("0x").map_or_else(
        || value.parse().ok(),
        |value| u64::from_str_radix(value, 16).ok(),
    )
}

fn symbol_annotation(value: &str) -> Option<&str> {
    let start = value.find('<')?;
    let end = value[start..].find('>')? + start;
    value.get(start..=end)
}

fn handle_session_event(ui: &Weak<Ui>, event: SessionEvent) {
    let Some(ui) = ui.upgrade() else {
        return;
    };

    match event {
        SessionEvent::Spawned => ui.set_status(
            "Connecting",
            "GDB started; waiting for its secondary MI interface.",
            None,
        ),
        SessionEvent::Failed(message) => ui.set_status(
            "GDB failed",
            &format!("Could not start the configured debugger: {message}"),
            Some("status-error"),
        ),
        SessionEvent::Exited(status) => {
            ui.set_status(
                "GDB exited",
                &format!("The debugger process exited with status {status}."),
                Some("status-error"),
            );
            ui.set_controls_ready(false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        assignment_expression, parse_gdb_integer, pointer_expression, register_string_address,
        source_symbol_pattern, stack_pointer_expression, stack_string_address, symbol_annotation,
        vector_assignment_expression,
    };
    use crate::debugger::{MemoryKind, Register, StackEntry};

    #[test]
    fn builds_pointer_chain_expressions() {
        assert_eq!(pointer_expression("rsp", 0), "(void*)($rsp)");
        assert_eq!(pointer_expression("rsp", 1), "*(void**)($rsp)");
        assert_eq!(pointer_expression("rsp", 2), "*(void**)(*(void**)($rsp))");
        assert_eq!(
            stack_pointer_expression("rsp", 8, 1),
            "*(void**)(*(void**)($rsp+0x8))"
        );
    }

    #[test]
    fn finds_the_pointer_behind_an_inline_stack_string_preview() {
        let mut entry = StackEntry {
            address: 0x7fff_0000,
            offset: 0,
            index: 0,
            value: String::from("0x7fff1000"),
            pointer_chain: vec![
                String::from("0x7fff1000"),
                String::from("0x415242494c5f444c"),
            ],
            address_registers: vec![String::from("rsp")],
            value_registers: Vec::new(),
            return_frame: None,
            memory_kind: MemoryKind::Stack,
            region: Some(String::from("rw-p · [stack]")),
        };
        let word = u64::from_le_bytes(*b"LD_LIBRA");
        assert_eq!(stack_string_address(&entry, word, 1), Some(0x7fff_1000));
        entry.memory_kind = MemoryKind::Code;
        assert_eq!(stack_string_address(&entry, word, 1), None);
    }

    #[test]
    fn finds_the_pointer_behind_an_inline_register_string_preview() {
        let word = u64::from_le_bytes(*b"LD_LIBRA");
        let mut register = Register {
            name: String::from("rsp"),
            value: String::from("0x7fffffffc5f0"),
            pointer_chain: vec![
                String::from("0x7fffffffc5f0"),
                String::from("0x7ffff7feedf6"),
                format!("0x{word:x}"),
            ],
        };
        assert_eq!(
            register_string_address(&register, word, 2),
            Some(0x7fff_f7fe_edf6)
        );
        register.name = String::from("rip");
        assert_eq!(register_string_address(&register, word, 2), None);
    }

    #[test]
    fn builds_variable_assignment_expressions() {
        assert_eq!(assignment_expression("count", "42"), "count = (42)");
        assert_eq!(
            assignment_expression("message", "\"hello world\""),
            "message = (\"hello world\")"
        );
    }

    #[test]
    fn extracts_addresses_from_symbolic_values() {
        assert_eq!(
            symbol_annotation("0x55555555516f <main+15>"),
            Some("<main+15>")
        );
        assert_eq!(parse_gdb_integer("0x2"), Some(2));
        assert_eq!(parse_gdb_integer("17"), Some(17));
    }

    #[test]
    fn builds_gdb_symbol_search_patterns() {
        assert_eq!(source_symbol_pattern("mmap"), "mmap");
        assert_eq!(source_symbol_pattern("Vec::new"), "Vec.*::new");
        assert_eq!(source_symbol_pattern("foo.bar+1"), r"foo\.bar\+1");
    }

    #[test]
    fn builds_typed_vector_lane_assignments() {
        assert_eq!(
            vector_assignment_expression(
                "ymm0",
                "v8_float",
                &[(0, String::from("1.5")), (7, String::from("-2.0"))],
            )
            .as_deref(),
            Some("($ymm0.v8_float[0] = (1.5), $ymm0.v8_float[7] = (-2.0))")
        );
        assert_eq!(vector_assignment_expression("xmm0", "v2_int64", &[]), None);
    }
}
