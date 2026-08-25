use super::*;

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

pub(super) fn load_library_symbols_for_source(ui: Weak<Ui>, client: Rc<MiClient>, symbol: String) {
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
