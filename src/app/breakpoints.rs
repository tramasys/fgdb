use super::*;

pub(super) fn insert_source_breakpoint(ui: Weak<Ui>, client: &MiClient, path: PathBuf, line: u32) {
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

pub(super) fn request_exact_source_breakpoint(
    ui: Weak<Ui>,
    client: &MiClient,
    path: PathBuf,
    line: u32,
) {
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

pub(super) fn remove_relocated_source_breakpoint(
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

pub(super) fn mutate_breakpoint(ui: Weak<Ui>, client: &MiClient, command: String, success: String) {
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

pub(super) fn refresh_breakpoints(ui: &Weak<Ui>, client: &MiClient) {
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

pub(super) fn refresh_threads(ui: &Weak<Ui>, client: &MiClient) {
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

pub(super) fn refresh_modules(ui: &Weak<Ui>, client: &MiClient, generation: u64) {
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

pub(super) fn infer_initial_stop_reason(ui: &Weak<Ui>, client: &MiClient) {
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
