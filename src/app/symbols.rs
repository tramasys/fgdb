use super::*;

struct SourceSymbolSearch {
    ui: Weak<Ui>,
    query: String,
    generation: u64,
    pending: Cell<u8>,
    locations: RefCell<Vec<crate::debugger::SourceLocation>>,
}

pub(super) fn request_source_discovery(
    ui: Weak<Ui>,
    client: Rc<MiClient>,
    request: SourceDiscoveryRequest,
) {
    match request {
        SourceDiscoveryRequest::LoadedFiles(generation) => {
            let ui_for_response = ui.clone();
            let ui_for_guard = ui.clone();

            if client
                .request_when(
                    "-file-list-exec-source-files",
                    move || {
                        ui_for_guard
                            .upgrade()
                            .is_some_and(|ui| ui.loaded_source_files_request_is_current(generation))
                    },
                    move |_, record| {
                        if let Some(ui) = ui_for_response.upgrade() {
                            if record.is_done() {
                                ui.show_loaded_source_files(
                                    generation,
                                    crate::debugger::source_files(&record),
                                );
                            } else {
                                ui.fail_loaded_source_files_request(
                                    generation,
                                    record
                                        .error_message()
                                        .unwrap_or("GDB could not enumerate loaded source files")
                                        .to_owned(),
                                );
                            }
                        }
                    },
                )
                .is_err()
                && let Some(ui) = ui.upgrade()
            {
                ui.fail_loaded_source_files_request(
                    generation,
                    String::from("the MI request could not be queued"),
                );
            }
        }
        SourceDiscoveryRequest::Symbols { query, generation } => {
            request_source_symbol_results(ui, &client, query, generation);
        }
    }
}

fn request_source_symbol_results(ui: Weak<Ui>, client: &MiClient, query: String, generation: u64) {
    let pattern = source_symbol_pattern(&query);

    let search = Rc::new(SourceSymbolSearch {
        ui,
        query,
        generation,
        pending: Cell::new(2),
        locations: RefCell::new(Vec::new()),
    });

    for command in [
        format!(
            "-symbol-info-functions --name {} --max-results 256",
            crate::debugger::quote(&pattern)
        ),
        format!(
            "-symbol-info-variables --name {} --max-results 256",
            crate::debugger::quote(&pattern)
        ),
    ] {
        let search_for_response = Rc::clone(&search);
        let ui_for_guard = search.ui.clone();
        let generation = search.generation;

        if client
            .request_when(
                &command,
                move || {
                    ui_for_guard
                        .upgrade()
                        .is_some_and(|ui| ui.source_symbol_request_is_current(generation))
                },
                move |_, record| {
                    if record.is_done() {
                        search_for_response
                            .locations
                            .borrow_mut()
                            .extend(crate::debugger::source_locations(&record));
                    }

                    finish_source_symbol_request(&search_for_response);
                },
            )
            .is_err()
        {
            finish_source_symbol_request(&search);
        }
    }
}

fn finish_source_symbol_request(search: &SourceSymbolSearch) {
    let remaining = search.pending.get().saturating_sub(1);
    search.pending.set(remaining);

    if remaining != 0 {
        return;
    }

    let mut locations = search.locations.borrow().clone();

    locations.sort_unstable_by(|left, right| {
        left.function
            .cmp(&right.function)
            .then_with(|| left.source_path().cmp(right.source_path()))
            .then_with(|| left.line.cmp(&right.line))
    });

    locations.dedup();

    if let Some(ui) = search.ui.upgrade() {
        ui.show_source_symbol_results(search.generation, &search.query, locations);
    }
}

pub(super) fn request_source_symbol(
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
    let symbol_for_response = symbol;

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

pub(super) fn load_library_symbols_for_source(ui: Weak<Ui>, client: Rc<MiClient>, symbol: String) {
    if let Some(ui) = ui.upgrade() {
        ui.set_status(
            "Loading library symbols",
            &format!("No definition for {symbol} was loaded. Asking GDB to load shared libraries…"),
            None,
        );
    }

    let command = crate::debugger::console_command("sharedlibrary");
    let ui_for_response = ui.clone();
    let client_for_response = Rc::clone(&client);
    let symbol_for_response = symbol;

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
                    "GDB could not load shared-library symbols. Pause the target and try again",
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

pub(super) fn source_symbol_pattern(symbol: &str) -> String {
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

pub(super) fn vector_assignment_expression(
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

pub(super) fn parse_gdb_integer(value: &str) -> Option<u64> {
    let value = value.trim();

    value.strip_prefix("0x").map_or_else(
        || value.parse().ok(),
        |value| u64::from_str_radix(value, 16).ok(),
    )
}

pub(super) fn symbol_annotation(value: &str) -> Option<&str> {
    let start = value.find('<')?;
    let end = value[start..].find('>')? + start;

    value.get(start..=end)
}

pub(super) fn handle_session_event(ui: &Weak<Ui>, event: SessionEvent) {
    let Some(ui) = ui.upgrade() else {
        return;
    };

    match event {
        SessionEvent::Spawned(pid) => {
            ui.model.set_debugger_pid(Some(pid));

            ui.set_status(
                "Connecting",
                "GDB started. Waiting for its secondary MI interface.",
                None,
            );
        }
        SessionEvent::Failed(message) => {
            ui.model.set_debugger_pid(None);
            ui.set_controls_ready(false);

            ui.set_status(
                "GDB failed",
                &format!("Could not start the configured debugger: {message}"),
                Some("status-error"),
            );
        }
        SessionEvent::Exited(status) => {
            ui.model.set_debugger_pid(None);
            ui.set_command_pending(false);
            ui.set_debug_state_stale(true);
            ui.clear_gef_capabilities();
            ui.set_inferior_started(false);
            ui.reset_target_abi();
            ui.clear_debugger_state();

            ui.set_status(
                "GDB exited",
                &format!("The debugger process exited with status {status}."),
                Some("status-error"),
            );

            ui.set_controls_ready(false);
        }
    }
}
