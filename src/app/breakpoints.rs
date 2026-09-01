use super::*;

pub(super) fn watchpoint_command(
    request: &WatchpointRequest,
) -> Result<(String, String), &'static str> {
    match request {
        WatchpointRequest::Standard { expression, access } => {
            let expression = expression.trim();
            if expression.is_empty() {
                return Err("Enter a variable or address expression");
            }
            let option = access.mi_option();
            let command = if option.is_empty() {
                format!("-break-watch {}", crate::debugger::quote(expression))
            } else {
                format!(
                    "-break-watch {option} {}",
                    crate::debugger::quote(expression)
                )
            };
            let kind = match access {
                WatchpointAccess::Write => "write",
                WatchpointAccess::Read => "read",
                WatchpointAccess::Access => "access",
            };
            Ok((command, format!("Added {kind} watchpoint for {expression}")))
        }
        WatchpointRequest::Masked { expression, mask } => {
            let expression = expression.trim();
            if expression.is_empty() {
                return Err("Enter a variable or address expression");
            }
            let mask = normalized_watchpoint_mask(mask)
                .ok_or("The mask must be a decimal or hexadecimal integer")?;
            let tail = format!("{expression} mask {mask}");
            let command = crate::debugger::CliCommandBuilder::new("watch")
                .verbatim_tail(&tail)?
                .finish();
            Ok((
                command,
                format!("Added masked watchpoint for {expression} with mask {mask}"),
            ))
        }
    }
}

fn normalized_watchpoint_mask(mask: &str) -> Option<&str> {
    let mask = mask.trim();
    let valid = mask
        .strip_prefix("0x")
        .or_else(|| mask.strip_prefix("0X"))
        .map_or_else(
            || !mask.is_empty() && mask.bytes().all(|byte| byte.is_ascii_digit()),
            |digits| !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_hexdigit()),
        );
    valid.then_some(mask)
}

pub(super) fn filtered_catchpoint_command(
    request: &FilteredCatchpointRequest,
) -> Result<String, &'static str> {
    let filter = request.filter.trim();
    if filter.is_empty() {
        return Err("Enter at least one syscall or a shared-library pattern");
    }
    let filter = match request.kind {
        FilteredCatchpointKind::Syscall => normalize_syscall_filter(filter)?,
        FilteredCatchpointKind::LibraryLoad | FilteredCatchpointKind::LibraryUnload => {
            filter.to_owned()
        }
    };
    let builder = crate::debugger::CliCommandBuilder::new("catch").keyword(match request.kind {
        FilteredCatchpointKind::Syscall => "syscall",
        FilteredCatchpointKind::LibraryLoad => "load",
        FilteredCatchpointKind::LibraryUnload => "unload",
    });
    builder
        .verbatim_tail(&filter)
        .map(crate::debugger::CliCommandBuilder::finish)
}

fn normalize_syscall_filter(filter: &str) -> Result<String, &'static str> {
    let filters = filter
        .split(|character: char| character == ',' || character.is_whitespace())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    if filters.is_empty() || filters.len() > 64 {
        return Err("Enter between 1 and 64 syscall names or numbers");
    }
    if filters.iter().any(|filter| {
        let mut bytes = filter.bytes();
        let Some(first) = bytes.next() else {
            return true;
        };
        if first.is_ascii_digit() {
            !bytes.all(|byte| byte.is_ascii_digit())
        } else {
            !(first.is_ascii_alphabetic() || first == b'_')
                || !bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        }
    }) {
        return Err("Syscalls must be names such as openat or decimal syscall numbers");
    }
    Ok(filters.join(" "))
}

pub(super) fn insert_source_breakpoint(ui: Weak<Ui>, client: &MiClient, path: PathBuf, line: u32) {
    resolve_executable_source_line(
        ui,
        client,
        path,
        line,
        SourceLineOperation::Breakpoint,
        |ui, client, source| {
            request_exact_source_breakpoint(ui, client, source.path, source.line);
        },
    );
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
    let path_for_response = path;
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
                        "{requested_location} did not resolve exactly. GDB's relocated breakpoint was removed"
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

pub(super) fn edit_breakpoint(ui: Weak<Ui>, client: &MiClient, request: BreakpointEditRequest) {
    if !client.is_ready() {
        breakpoint_failure(
            &ui,
            "Breakpoint unavailable",
            "Wait for the GDB/MI channel to become ready.",
        );
        return;
    }
    if let Some(current_ui) = ui.upgrade() {
        current_ui.set_command_pending(true);
    }
    if request.spec.regex {
        create_regex_breakpoints(ui, client, request.spec);
        return;
    }

    let recreate = request
        .original
        .as_ref()
        .is_some_and(|original| breakpoint_needs_recreation(original, &request.spec));
    if request.original.is_none() || recreate {
        create_standard_breakpoint(ui, client, request);
    } else if let Some(original) = request.original {
        let number = original.command_number().to_owned();
        let commands = mutable_breakpoint_commands(&number, &original, &request.spec);
        if commands.is_empty() {
            if let Some(current_ui) = ui.upgrade() {
                current_ui.set_command_pending(false);
                current_ui.set_status(
                    "Paused",
                    &format!("Breakpoint #{number} is unchanged"),
                    Some("status-ready"),
                );
            }
            return;
        }
        run_breakpoint_commands(
            ui,
            client,
            commands.into(),
            format!("Updated breakpoint #{number}"),
        );
    }
}

fn create_standard_breakpoint(ui: Weak<Ui>, client: &MiClient, request: BreakpointEditRequest) {
    let command = breakpoint_insert_command(&request.spec);
    let ui_for_response = ui.clone();
    let old_number = request
        .original
        .as_ref()
        .map(|breakpoint| breakpoint.command_number().to_owned());
    let commands = request.spec.effective_commands();
    if let Err(error) = client.request(&command, move |client, record| {
        if !record.is_done() {
            breakpoint_failure(
                &ui_for_response,
                "Breakpoint creation failed",
                record
                    .error_message()
                    .unwrap_or("GDB rejected the breakpoint location or options"),
            );
            refresh_breakpoints(&ui_for_response, client);
            return;
        }
        let mut numbers = crate::debugger::inserted_breakpoints(&record)
            .into_iter()
            .map(|breakpoint| breakpoint.command_number().to_owned())
            .collect::<Vec<_>>();
        numbers.sort();
        numbers.dedup();
        let Some(number) = numbers.into_iter().next() else {
            breakpoint_failure(
                &ui_for_response,
                "Breakpoint creation failed",
                "GDB did not report the newly created breakpoint number",
            );
            refresh_breakpoints(&ui_for_response, client);
            return;
        };
        if let (Some(current_ui), Some(old_number)) =
            (ui_for_response.upgrade(), old_number.as_deref())
        {
            current_ui.move_stop_point_metadata(old_number, &number);
        }
        let mut follow_up = VecDeque::new();
        if !commands.is_empty() {
            follow_up.push_back(breakpoint_commands_command(&number, &commands));
        }
        if let Some(old_number) = old_number.as_deref() {
            follow_up.push_back(format!("-break-delete {old_number}"));
        }
        run_breakpoint_commands(
            ui_for_response,
            client,
            follow_up,
            old_number.map_or_else(
                || format!("Added breakpoint #{number}"),
                |_| format!("Replaced breakpoint with #{number}"),
            ),
        );
    }) {
        breakpoint_failure(&ui, "Breakpoint creation failed", &error.to_string());
    }
}

fn create_regex_breakpoints(ui: Weak<Ui>, client: &MiClient, spec: BreakpointSpec) {
    let ui_for_list = ui.clone();
    if let Err(error) = client.request("-break-list", move |client, record| {
        if !record.is_done() {
            breakpoint_failure(
                &ui_for_list,
                "Regex breakpoint failed",
                record
                    .error_message()
                    .unwrap_or("Could not read existing breakpoints"),
            );
            return;
        }
        let before = crate::debugger::breakpoints(&record)
            .into_iter()
            .filter(|breakpoint| !breakpoint.is_location())
            .map(|breakpoint| breakpoint.number)
            .collect::<HashSet<_>>();
        let Ok(command) = crate::debugger::CliCommandBuilder::new("rbreak")
            .verbatim_tail(&spec.location)
            .map(crate::debugger::CliCommandBuilder::finish)
        else {
            breakpoint_failure(
                &ui_for_list,
                "Regex breakpoint failed",
                "Function regular expressions cannot contain NUL or line breaks",
            );
            return;
        };
        let ui_for_regex = ui_for_list.clone();
        if let Err(error) = client.request(&command, move |client, record| {
            if !record.is_done() {
                breakpoint_failure(
                    &ui_for_regex,
                    "Regex breakpoint failed",
                    record
                        .error_message()
                        .unwrap_or("GDB rejected the function regex"),
                );
                refresh_breakpoints(&ui_for_regex, client);
                return;
            }
            configure_regex_breakpoints(ui_for_regex, client, before, spec);
        }) {
            breakpoint_failure(&ui_for_list, "Regex breakpoint failed", &error.to_string());
        }
    }) {
        breakpoint_failure(&ui, "Regex breakpoint failed", &error.to_string());
    }
}

fn configure_regex_breakpoints(
    ui: Weak<Ui>,
    client: &MiClient,
    before: HashSet<String>,
    spec: BreakpointSpec,
) {
    let ui_for_response = ui.clone();
    if let Err(error) = client.request("-break-list", move |client, record| {
        if !record.is_done() {
            breakpoint_failure(
                &ui_for_response,
                "Regex breakpoint failed",
                record
                    .error_message()
                    .unwrap_or("Could not inspect regex matches"),
            );
            return;
        }
        let mut numbers = crate::debugger::breakpoints(&record)
            .into_iter()
            .filter(|breakpoint| !breakpoint.is_location() && !before.contains(&breakpoint.number))
            .map(|breakpoint| breakpoint.number)
            .collect::<Vec<_>>();
        numbers.sort_by_key(|number| number.parse::<u64>().map_or((1, 0), |number| (0, number)));
        if numbers.is_empty() {
            breakpoint_failure(
                &ui_for_response,
                "No breakpoint added",
                "No currently loaded function matched that regular expression",
            );
            refresh_breakpoints(&ui_for_response, client);
            return;
        }
        let mut commands = VecDeque::new();
        if spec.temporary {
            let console = format!("enable delete {}", numbers.join(" "));
            commands.push_back(crate::debugger::console_command(&console));
        }
        let effective_commands = spec.effective_commands();
        for number in &numbers {
            if let Some(condition) = spec.condition.as_deref() {
                commands.push_back(format!(
                    "-break-condition {number} {}",
                    crate::debugger::quote(condition)
                ));
            }
            let ignore = spec.stop_after.saturating_sub(1);
            if ignore > 0 {
                commands.push_back(format!("-break-after {number} {ignore}"));
            }
            if !effective_commands.is_empty() {
                commands.push_back(breakpoint_commands_command(number, &effective_commands));
            }
        }
        if !spec.enabled {
            commands.push_back(format!("-break-disable {}", numbers.join(" ")));
        }
        let count = numbers.len();
        run_breakpoint_commands(
            ui_for_response,
            client,
            commands,
            format!(
                "Added {count} regex breakpoint{}",
                if count == 1 { "" } else { "s" }
            ),
        );
    }) {
        breakpoint_failure(&ui, "Regex breakpoint failed", &error.to_string());
    }
}

fn run_breakpoint_commands(
    ui: Weak<Ui>,
    client: &MiClient,
    mut commands: VecDeque<String>,
    success: String,
) {
    let Some(command) = commands.pop_front() else {
        if let Some(current_ui) = ui.upgrade() {
            current_ui.set_command_pending(false);
            current_ui.set_status("Paused", &success, Some("status-ready"));
        }
        refresh_breakpoints(&ui, client);
        return;
    };
    let ui_for_response = ui.clone();
    if let Err(error) = client.request(&command, move |client, record| {
        if record.is_done() {
            run_breakpoint_commands(ui_for_response, client, commands, success);
        } else {
            breakpoint_failure(
                &ui_for_response,
                "Breakpoint update failed",
                record
                    .error_message()
                    .unwrap_or("GDB rejected a breakpoint setting"),
            );
            refresh_breakpoints(&ui_for_response, client);
        }
    }) {
        breakpoint_failure(&ui, "Breakpoint update failed", &error.to_string());
        refresh_breakpoints(&ui, client);
    }
}

fn breakpoint_failure(ui: &Weak<Ui>, title: &str, detail: &str) {
    if let Some(current_ui) = ui.upgrade() {
        current_ui.set_command_pending(false);
        current_ui.set_status(title, detail, Some("status-error"));
    }
}

fn breakpoint_insert_command(spec: &BreakpointSpec) -> String {
    let mut command = String::from("-break-insert");
    if spec.hardware {
        command.push_str(" -h");
    }
    if spec.temporary {
        command.push_str(" -t");
    }
    if spec.allow_pending {
        command.push_str(" -f");
    }
    if !spec.enabled {
        command.push_str(" -d");
    }
    if let Some(condition) = spec.condition.as_deref() {
        let _ = write!(command, " -c {}", crate::debugger::quote(condition));
    }
    let ignore = spec.stop_after.saturating_sub(1);
    if ignore > 0 {
        let _ = write!(command, " -i {ignore}");
    }
    if let Some(thread) = spec.thread.as_deref() {
        let _ = write!(command, " -p {}", crate::debugger::quote(thread));
    }
    if let Some(inferior) = spec.inferior.as_deref() {
        let inferior = if inferior.starts_with('i') {
            inferior.to_owned()
        } else {
            format!("i{inferior}")
        };
        let _ = write!(command, " -g {}", crate::debugger::quote(&inferior));
    }
    let location = canonical_breakpoint_location(&spec.location);
    let _ = write!(command, " {}", crate::debugger::quote(&location));
    command
}

fn canonical_breakpoint_location(location: &str) -> String {
    let location = location.trim();
    if location.starts_with('*') {
        return location.to_owned();
    }
    let digits = location
        .strip_prefix("0x")
        .or_else(|| location.strip_prefix("0X"));
    if digits.is_some_and(|digits| {
        !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_hexdigit())
    }) {
        format!("*{location}")
    } else {
        location.to_owned()
    }
}

fn breakpoint_needs_recreation(
    original: &crate::debugger::Breakpoint,
    spec: &BreakpointSpec,
) -> bool {
    let current_location = original
        .original_location
        .as_deref()
        .or(original.address.as_deref())
        .unwrap_or_default();
    canonical_breakpoint_location(current_location) != canonical_breakpoint_location(&spec.location)
        || (original.disposition.as_deref() == Some("del")) != spec.temporary
        || original.is_hardware_breakpoint() != spec.hardware
        || original.pending.is_some() != spec.allow_pending
        || original.thread != spec.thread
        || original.inferior != spec.inferior
}

fn mutable_breakpoint_commands(
    number: &str,
    original: &crate::debugger::Breakpoint,
    spec: &BreakpointSpec,
) -> Vec<String> {
    let mut commands = Vec::new();
    if original.enabled != spec.enabled {
        commands.push(format!(
            "-break-{} {number}",
            if spec.enabled { "enable" } else { "disable" }
        ));
    }
    if original.condition != spec.condition {
        commands.push(spec.condition.as_deref().map_or_else(
            || format!("-break-condition {number}"),
            |condition| {
                format!(
                    "-break-condition {number} {}",
                    crate::debugger::quote(condition)
                )
            },
        ));
    }
    let ignore = spec.stop_after.saturating_sub(1);
    if original.ignore_count != ignore {
        commands.push(format!("-break-after {number} {ignore}"));
    }
    let effective_commands = spec.effective_commands();
    if original.commands != effective_commands {
        commands.push(breakpoint_commands_command(number, &effective_commands));
    }
    commands
}

fn breakpoint_commands_command(number: &str, commands: &[String]) -> String {
    let mut command = format!("-break-commands {number}");
    for breakpoint_command in commands {
        let breakpoint_command = breakpoint_command.trim();
        if breakpoint_command.is_empty() {
            continue;
        }
        command.push(' ');
        command.push_str(&crate::debugger::quote(breakpoint_command));
    }
    command
}

pub(super) fn refresh_breakpoints(ui: &Weak<Ui>, client: &MiClient) {
    let Some(current_ui) = ui.upgrade() else {
        return;
    };
    let Some(generation) = current_ui.begin_breakpoint_refresh() else {
        return;
    };
    drop(current_ui);
    let weak_ui = ui.clone();
    let weak_ui_for_error = ui.clone();
    if client
        .request("-break-list", move |client, record| {
            let Some(ui) = weak_ui.upgrade() else {
                return;
            };
            if record.is_done() {
                ui.show_breakpoints_for_refresh(generation, crate::debugger::breakpoints(&record));
            }
            let refresh_again = ui.finish_breakpoint_refresh();
            drop(ui);
            if refresh_again {
                refresh_breakpoints(&weak_ui, client);
            }
        })
        .is_err()
        && let Some(ui) = weak_ui_for_error.upgrade()
    {
        ui.finish_breakpoint_refresh();
    }
}

pub(super) fn refresh_modules(ui: &Weak<Ui>, client: &MiClient) {
    let Some(current_ui) = ui.upgrade() else {
        return;
    };
    if !current_ui.begin_module_refresh() {
        return;
    }
    let inferior_id = current_ui.selected_inferior_id();
    let command = match inferior_id.as_deref() {
        Some(id) => {
            let Some(group) = crate::debugger::thread_group_argument(id) else {
                current_ui.finish_module_refresh();
                current_ui.set_status(
                    "Module refresh failed",
                    "GDB reported an unsupported inferior identifier",
                    Some("status-error"),
                );
                return;
            };
            format!("-file-list-shared-libraries --thread-group {group}")
        }
        None => String::from("-file-list-shared-libraries"),
    };
    drop(current_ui);
    let weak_ui = ui.clone();
    let weak_ui_for_error = ui.clone();
    if client
        .request(&command, move |client, record| {
            let Some(ui) = weak_ui.upgrade() else {
                return;
            };
            if record.is_done() && ui.selected_inferior_id() == inferior_id {
                ui.show_modules(&crate::debugger::shared_libraries(&record));
            }
            let refresh_again = ui.finish_module_refresh();
            drop(ui);
            if refresh_again {
                refresh_modules(&weak_ui, client);
            }
        })
        .is_err()
        && let Some(ui) = weak_ui_for_error.upgrade()
    {
        ui.finish_module_refresh();
    }
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
        let selection_changed = ui.reconcile_stop_owner_from_threads(&threads);
        let threads = ui.threads_for_selected_inferior(threads);
        let recovered_stopped_context = ui.debug_state_is_stale()
            && threads
                .iter()
                .any(|thread| thread.current && thread.state == "stopped");
        if !threads.is_empty() {
            ui.set_inferior_started(true);
        }
        ui.show_threads_for_refresh(generation, &threads);
        if recovered_stopped_context {
            ui.set_controls_running(false);
            ui.set_debug_state_stale(false);
        }
        drop(ui);
        if recovered_stopped_context {
            refresh_stopped_state(&weak_ui, client);
        }
        if selection_changed {
            refresh_modules(&weak_ui, client);
            detect_target_abi(&weak_ui, client);
        }
        if !threads.iter().any(|thread| {
            thread.current
                && thread
                    .frame
                    .as_ref()
                    .is_some_and(|frame| frame.function == "??")
        }) {
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

pub(super) fn infer_initial_stop_reason(ui: &Weak<Ui>, client: &MiClient) {
    let weak_ui = ui.clone();
    let _ = client.request(
        &format!(
            "-data-evaluate-expression {}",
            crate::debugger::quote("$_hit_bpnum")
        ),
        move |client, record| {
            let hit_breakpoint = record.is_done()
                && crate::debugger::evaluated_value(&record)
                    .as_deref()
                    .and_then(parse_gdb_integer)
                    .is_some_and(|number| number != 0);
            if hit_breakpoint {
                if let Some(ui) = weak_ui.upgrade() {
                    ui.set_thread_stop_reason(Some("breakpoint-hit"));
                    ui.set_inferior_started(true);
                }
                // Startup commands can stop before fgdb's MI channel is
                // attached, in which case no async *stopped event reaches us.
                refresh_stopped_state(&weak_ui, client);
            } else {
                infer_existing_stopped_thread(&weak_ui, client);
            }
        },
    );
}

fn infer_existing_stopped_thread(ui: &Weak<Ui>, client: &MiClient) {
    let weak_ui = ui.clone();
    let _ = client.request("-thread-info", move |client, record| {
        if !record.is_done()
            || !crate::debugger::threads(&record)
                .iter()
                .any(|thread| thread.state == "stopped")
        {
            return;
        }
        if let Some(ui) = weak_ui.upgrade() {
            ui.set_thread_stop_reason(Some("stopped"));
            ui.set_inferior_started(true);
        }
        refresh_stopped_state(&weak_ui, client);
    });
}

#[cfg(test)]
mod tests {
    use super::{
        breakpoint_commands_command, breakpoint_insert_command, canonical_breakpoint_location,
        filtered_catchpoint_command, watchpoint_command,
    };
    use crate::ui::{
        BreakpointSpec, FilteredCatchpointKind, FilteredCatchpointRequest, WatchpointAccess,
        WatchpointRequest,
    };

    fn breakpoint_spec(location: &str) -> BreakpointSpec {
        BreakpointSpec {
            location: location.to_owned(),
            regex: false,
            hardware: false,
            enabled: true,
            temporary: false,
            allow_pending: false,
            condition: None,
            stop_after: 1,
            thread: None,
            inferior: None,
            commands: Vec::new(),
            logpoint: false,
        }
    }

    #[test]
    fn builds_complete_breakpoint_insert_commands() {
        let mut spec = breakpoint_spec("0x401120");
        spec.enabled = false;
        spec.hardware = true;
        spec.temporary = true;
        spec.allow_pending = true;
        spec.condition = Some(String::from("count == 4"));
        spec.stop_after = 5;
        spec.thread = Some(String::from("2"));
        spec.inferior = Some(String::from("3"));
        assert_eq!(
            breakpoint_insert_command(&spec),
            "-break-insert -h -t -f -d -c \"count == 4\" -i 4 -p \"2\" -g \"i3\" \"*0x401120\""
        );
        assert_eq!(canonical_breakpoint_location("main"), "main");
        assert_eq!(canonical_breakpoint_location("*0x10"), "*0x10");
    }

    #[test]
    fn builds_command_lists_and_logpoint_wrappers() {
        let mut spec = breakpoint_spec("main");
        spec.logpoint = true;
        spec.commands = vec![String::from("printf \"count=%d\\n\", count")];
        let commands = spec.effective_commands();
        assert_eq!(commands.first().map(String::as_str), Some("silent"));
        assert_eq!(commands.last().map(String::as_str), Some("continue"));
        assert_eq!(
            breakpoint_commands_command("7", &commands),
            "-break-commands 7 \"silent\" \"printf \\\"count=%d\\\\n\\\", count\" \"continue\""
        );
        assert_eq!(breakpoint_commands_command("7", &[]), "-break-commands 7");
    }

    #[test]
    fn builds_standard_and_masked_watchpoint_commands() {
        assert_eq!(
            watchpoint_command(&WatchpointRequest::Standard {
                expression: String::from("counter"),
                access: WatchpointAccess::Read,
            })
            .unwrap()
            .0,
            "-break-watch -r \"counter\""
        );
        assert_eq!(
            watchpoint_command(&WatchpointRequest::Masked {
                expression: String::from("*0x4000"),
                mask: String::from("0xffffff00"),
            })
            .unwrap()
            .0,
            "-interpreter-exec console \"watch *0x4000 mask 0xffffff00\""
        );
        assert!(
            watchpoint_command(&WatchpointRequest::Masked {
                expression: String::from("counter"),
                mask: String::from("0xff; quit"),
            })
            .is_err()
        );
    }

    #[test]
    fn validates_and_builds_filtered_catchpoints() {
        assert_eq!(
            filtered_catchpoint_command(&FilteredCatchpointRequest {
                kind: FilteredCatchpointKind::Syscall,
                filter: String::from("openat, read 257"),
            })
            .unwrap(),
            "-interpreter-exec console \"catch syscall openat read 257\""
        );
        assert_eq!(
            filtered_catchpoint_command(&FilteredCatchpointRequest {
                kind: FilteredCatchpointKind::LibraryLoad,
                filter: String::from("lib(ssl|crypto)\\.so"),
            })
            .unwrap(),
            "-interpreter-exec console \"catch load lib(ssl|crypto)\\\\.so\""
        );
        assert!(
            filtered_catchpoint_command(&FilteredCatchpointRequest {
                kind: FilteredCatchpointKind::Syscall,
                filter: String::from("openat; quit"),
            })
            .is_err()
        );
    }
}
