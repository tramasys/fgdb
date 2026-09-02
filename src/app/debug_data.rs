use super::*;

use crate::ui::DebugDataAction;

struct DebugDataQuery {
    ui: Weak<Ui>,
    client: Rc<MiClient>,
    generation: u64,
    debuginfod_status: String,
    debuginfod_urls: String,
    source_directories: Vec<String>,
    substitutions: Vec<(String, String)>,
    errors: Vec<String>,
}

pub(super) fn handle_debug_data_action(
    ui: Weak<Ui>,
    client: Rc<MiClient>,
    action: DebugDataAction,
) {
    match action {
        DebugDataAction::Refresh => refresh_debug_data(ui, client),
        DebugDataAction::SetDebuginfodEnabled(enabled) => {
            let value = if enabled { "on" } else { "off" };
            run_console_setting(
                ui,
                client,
                format!("set debuginfod enabled {value}"),
                format!("Debuginfod {value}"),
            );
        }
        DebugDataAction::SetDebuginfodUrls(urls) => run_console_setting(
            ui,
            client,
            format!("set debuginfod urls {urls}"),
            String::from("Updated debuginfod URLs"),
        ),
        DebugDataAction::SetPrettyPrinting(enabled) => {
            set_pretty_printing(ui, client, enabled);
        }
        DebugDataAction::ShowSourceFiles => {
            if let Some(ui) = ui.upgrade()
                && ui.show_debug_data_source_files()
            {
                ui.add_debug_data_progress("Loading source files from GDB");
                ui.request_loaded_source_files();
            }
        }
        DebugDataAction::ReloadSourceFiles => {
            if let Some(ui) = ui.upgrade() {
                ui.add_debug_data_progress("Reloading source files from GDB");
                ui.request_loaded_source_files();
            }
        }
        DebugDataAction::ShowMoreModules => {
            if let Some(ui) = ui.upgrade() {
                ui.show_more_debug_data_modules();
            }
        }
        DebugDataAction::ShowMoreSources => {
            if let Some(ui) = ui.upgrade() {
                ui.show_more_debug_data_sources();
            }
        }
        DebugDataAction::ShowMorePrettyPrinters => {
            if let Some(ui) = ui.upgrade() {
                ui.show_more_debug_data_pretty_printers();
            }
        }
        DebugDataAction::LoadPrettyPrinters => request_pretty_printers(ui, client),
        DebugDataAction::AddSourceDirectory(path) => add_source_directory(ui, client, path),
        DebugDataAction::RemoveSourceDirectory(path) => {
            remove_source_directory(ui, client, path);
        }
        DebugDataAction::AddSubstitution { from, to } => {
            set_substitution(ui, client, from, to);
        }
        DebugDataAction::RemoveSubstitution(from) => remove_substitution(ui, client, from),
        DebugDataAction::RetrySymbols(module) => retry_symbols(ui, client, module),
    }
}

fn refresh_debug_data(ui: Weak<Ui>, client: Rc<MiClient>) {
    let Some(current_ui) = ui.upgrade() else {
        return;
    };
    let generation = current_ui.begin_debug_data_refresh();
    let refresh_printers = current_ui.debug_data_pretty_printers_were_requested();
    if !current_ui.gdb_capabilities().pretty_printing {
        current_ui.add_debug_data_warning(
            "Pretty printing is disabled; raw values remain available in Locals and Arguments",
        );
    }
    drop(current_ui);
    if refresh_printers {
        request_pretty_printers(ui.clone(), Rc::clone(&client));
    }
    refresh_modules(&ui, &client);
    request_debuginfod_status(Rc::new(RefCell::new(DebugDataQuery {
        ui,
        client,
        generation,
        debuginfod_status: String::new(),
        debuginfod_urls: String::new(),
        source_directories: Vec::new(),
        substitutions: Vec::new(),
        errors: Vec::new(),
    })));
}

fn request_debuginfod_status(query: Rc<RefCell<DebugDataQuery>>) {
    let client = Rc::clone(&query.borrow().client);
    let query_for_response = Rc::clone(&query);
    if let Err(error) = client.request_console_when(
        "show debuginfod enabled",
        debug_data_query_guard(&query),
        move |_, record, output| {
            let mut query = query_for_response.borrow_mut();
            if record.is_done() {
                query.debuginfod_status = parse_debuginfod_status(&output);
            } else {
                query.errors.push(format!(
                    "Debuginfod status: {}",
                    console_error(&record, &output)
                ));
            }
            drop(query);
            request_debuginfod_urls(query_for_response);
        },
    ) {
        query
            .borrow_mut()
            .errors
            .push(format!("Debuginfod status: {error}"));
        request_debuginfod_urls(query);
    }
}

fn request_debuginfod_urls(query: Rc<RefCell<DebugDataQuery>>) {
    let client = Rc::clone(&query.borrow().client);
    let query_for_response = Rc::clone(&query);
    if let Err(error) = client.request_console_when(
        "show debuginfod urls",
        debug_data_query_guard(&query),
        move |_, record, output| {
            let mut query = query_for_response.borrow_mut();
            if record.is_done() {
                query.debuginfod_urls = parse_setting_tail(&output);
            } else {
                query.errors.push(format!(
                    "Debuginfod URLs: {}",
                    console_error(&record, &output)
                ));
            }
            drop(query);
            request_source_directories(query_for_response);
        },
    ) {
        query
            .borrow_mut()
            .errors
            .push(format!("Debuginfod URLs: {error}"));
        request_source_directories(query);
    }
}

fn request_source_directories(query: Rc<RefCell<DebugDataQuery>>) {
    let client = Rc::clone(&query.borrow().client);
    let query_for_response = Rc::clone(&query);
    if let Err(error) = client.request_console_when(
        "show directories",
        debug_data_query_guard(&query),
        move |_, record, output| {
            let mut query = query_for_response.borrow_mut();
            if record.is_done() {
                query.source_directories = parse_source_directories(&output);
            } else {
                query.errors.push(format!(
                    "Source directories: {}",
                    console_error(&record, &output)
                ));
            }
            drop(query);
            request_substitutions(query_for_response);
        },
    ) {
        query
            .borrow_mut()
            .errors
            .push(format!("Source directories: {error}"));
        request_substitutions(query);
    }
}

fn request_substitutions(query: Rc<RefCell<DebugDataQuery>>) {
    let client = Rc::clone(&query.borrow().client);
    let query_for_response = Rc::clone(&query);
    if let Err(error) = client.request_console_when(
        "show substitute-path",
        debug_data_query_guard(&query),
        move |_, record, output| {
            let mut query = query_for_response.borrow_mut();
            if record.is_done() {
                query.substitutions = parse_substitutions(&output);
            } else {
                query.errors.push(format!(
                    "Substitute paths: {}",
                    console_error(&record, &output)
                ));
            }
            drop(query);
            finish_debug_data_query(query_for_response);
        },
    ) {
        query
            .borrow_mut()
            .errors
            .push(format!("Substitute paths: {error}"));
        finish_debug_data_query(query);
    }
}

fn request_pretty_printers(ui: Weak<Ui>, client: Rc<MiClient>) {
    let Some(current_ui) = ui.upgrade() else {
        return;
    };
    let Some(generation) = current_ui.begin_debug_data_pretty_printer_refresh() else {
        return;
    };
    drop(current_ui);
    let ui_for_guard = ui.clone();
    let ui_for_response = ui.clone();
    if let Err(error) = client.request_console_when(
        "info pretty-printer",
        move || {
            ui_for_guard
                .upgrade()
                .is_some_and(|ui| ui.debug_data_pretty_printer_refresh_is_current(generation))
        },
        move |_, record, output| {
            let Some(ui) = ui_for_response.upgrade() else {
                return;
            };
            if !ui.debug_data_pretty_printer_refresh_is_current(generation) {
                return;
            }
            let result = if record.is_done() {
                Ok(parse_pretty_printers(&output))
            } else {
                Err(format!(
                    "Pretty-printers: {}",
                    console_error(&record, &output)
                ))
            };
            if let Err(error) = &result {
                ui.add_debug_data_error(error.clone());
            }
            ui.finish_debug_data_pretty_printer_refresh(generation, result);
        },
    ) {
        let message = format!("Pretty-printers: {error}");
        if let Some(ui) = ui.upgrade() {
            ui.add_debug_data_error(message.clone());
            ui.finish_debug_data_pretty_printer_refresh(generation, Err(message));
        }
    }
}

fn debug_data_query_guard(query: &Rc<RefCell<DebugDataQuery>>) -> impl Fn() -> bool + 'static {
    let query = query.borrow();
    let ui = query.ui.clone();
    let generation = query.generation;
    move || {
        ui.upgrade()
            .is_some_and(|ui| ui.debug_data_refresh_is_current(generation))
    }
}

fn finish_debug_data_query(query: Rc<RefCell<DebugDataQuery>>) {
    let query = query.borrow();
    let Some(ui) = query.ui.upgrade() else {
        return;
    };
    if !ui.debug_data_refresh_is_current(query.generation) {
        return;
    }
    ui.set_debug_data_debuginfod(
        query.generation,
        query.debuginfod_status.clone(),
        query.debuginfod_urls.clone(),
    );
    ui.set_debug_data_sources(
        query.generation,
        query.source_directories.clone(),
        query.substitutions.clone(),
    );
    for error in &query.errors {
        ui.add_debug_data_error(error.clone());
    }
    ui.finish_debug_data_refresh(query.generation);
}

fn run_console_setting(ui: Weak<Ui>, client: Rc<MiClient>, command: String, success: String) {
    let ui_for_response = ui.clone();
    if let Some(ui) = ui.upgrade() {
        ui.add_debug_data_progress(format!("Applying: {command}"));
    }
    let client_for_refresh = Rc::clone(&client);
    if let Err(error) = client.request_console(&command, move |_, record, output| {
        let Some(ui) = ui_for_response.upgrade() else {
            return;
        };
        if record.is_done() {
            ui.add_debug_data_success(success);
        } else {
            ui.add_debug_data_error(format!(
                "Setting failed: {}",
                console_error(&record, &output)
            ));
        }
        refresh_debug_data(ui_for_response, client_for_refresh);
    }) && let Some(ui) = ui.upgrade()
    {
        ui.add_debug_data_error(format!("Could not queue setting: {error}"));
        refresh_debug_data(Rc::downgrade(&ui), client);
    }
}

fn set_pretty_printing(ui: Weak<Ui>, client: Rc<MiClient>, enabled: bool) {
    let ui_for_response = ui.clone();
    let client_for_refresh = Rc::clone(&client);
    if client
        .set_pretty_printing(enabled, move |_, record| {
            if let Some(ui) = ui_for_response.upgrade() {
                if record.is_done() {
                    ui.add_debug_data_success(if enabled {
                        "Dynamic pretty printing enabled"
                    } else {
                        "Dynamic pretty printing disabled"
                    });
                } else {
                    ui.add_debug_data_error(format!(
                        "Could not change pretty printing: {}",
                        record.error_message().unwrap_or("GDB rejected the command")
                    ));
                }
            }
            refresh_debug_data(ui_for_response, client_for_refresh);
        })
        .is_err()
        && let Some(ui) = ui.upgrade()
    {
        ui.add_debug_data_error("Could not queue the pretty-printing command");
        refresh_debug_data(Rc::downgrade(&ui), client);
    }
}

fn add_source_directory(ui: Weak<Ui>, client: Rc<MiClient>, path: PathBuf) {
    let Some(path_text) = path.to_str() else {
        if let Some(ui) = ui.upgrade() {
            ui.add_debug_data_error("Source path is not valid UTF-8 for this GDB session");
        }
        return;
    };
    let Ok(path_argument) = crate::debugger::gdb_cli_string(path_text) else {
        return;
    };
    let command = format!("directory {path_argument}");
    let ui_for_response = ui.clone();
    let client_for_refresh = Rc::clone(&client);
    if let Err(error) = client.request_console(&command, move |_, record, output| {
        if let Some(ui) = ui_for_response.upgrade() {
            if record.is_done() {
                ui.add_runtime_source_directory(path.clone());
                ui.add_debug_data_success(format!("Added source directory {}", path.display()));
            } else {
                ui.add_debug_data_error(format!(
                    "Could not add source directory: {}",
                    console_error(&record, &output)
                ));
            }
        }
        refresh_debug_data(ui_for_response, client_for_refresh);
    }) && let Some(ui) = ui.upgrade()
    {
        ui.add_debug_data_error(format!("Could not queue source-directory change: {error}"));
        refresh_debug_data(Rc::downgrade(&ui), client);
    }
}

fn remove_source_directory(ui: Weak<Ui>, client: Rc<MiClient>, path: String) {
    let Some(current_ui) = ui.upgrade() else {
        return;
    };
    let directories = current_ui
        .source_directories_for_debug_data()
        .into_iter()
        .filter(|directory| directory != &path)
        .collect::<Vec<_>>();
    let command = format!("set directories {}", directories.join(":"));
    drop(current_ui);
    let ui_for_response = ui.clone();
    let client_for_refresh = Rc::clone(&client);
    if let Err(error) = client.request_console(&command, move |_, record, output| {
        if let Some(ui) = ui_for_response.upgrade() {
            if record.is_done() {
                ui.remove_runtime_source_directory(&path);
                ui.add_debug_data_success(format!("Removed source directory {path}"));
            } else {
                ui.add_debug_data_error(format!(
                    "Could not remove source directory: {}",
                    console_error(&record, &output)
                ));
            }
        }
        refresh_debug_data(ui_for_response, client_for_refresh);
    }) && let Some(ui) = ui.upgrade()
    {
        ui.add_debug_data_error(format!("Could not queue source-directory change: {error}"));
        refresh_debug_data(Rc::downgrade(&ui), client);
    }
}

fn set_substitution(ui: Weak<Ui>, client: Rc<MiClient>, from: String, to: String) {
    let (Ok(from_arg), Ok(to_arg)) = (
        crate::debugger::gdb_cli_string(&from),
        crate::debugger::gdb_cli_string(&to),
    ) else {
        return;
    };
    run_console_setting(
        ui,
        client,
        format!("set substitute-path {from_arg} {to_arg}"),
        format!("Mapped {from} to {to}"),
    );
}

fn remove_substitution(ui: Weak<Ui>, client: Rc<MiClient>, from: String) {
    let Ok(from_arg) = crate::debugger::gdb_cli_string(&from) else {
        return;
    };
    run_console_setting(
        ui,
        client,
        format!("unset substitute-path {from_arg}"),
        format!("Removed source mapping for {from}"),
    );
}

fn retry_symbols(ui: Weak<Ui>, client: Rc<MiClient>, module: Option<String>) {
    if let Some(current_ui) = ui.upgrade()
        && current_ui.debuginfod_status_for_debug_data() == "ask"
    {
        current_ui.add_debug_data_warning(
            "Symbol retry was not started: choose whether to enable or disable debuginfod first",
        );
        return;
    }
    let command = module.as_deref().map_or_else(
        || String::from("sharedlibrary"),
        |module| {
            let pattern = exact_gdb_regex(module);
            crate::debugger::gdb_cli_string(&pattern).map_or_else(
                |_| String::from("sharedlibrary"),
                |pattern| format!("sharedlibrary {pattern}"),
            )
        },
    );
    if let Some(ui) = ui.upgrade() {
        ui.add_debug_data_progress(module.as_deref().map_or_else(
            || String::from("Retrying symbols for all shared libraries…"),
            |module| format!("Retrying symbols for {module}…"),
        ));
    }
    let ui_for_response = ui.clone();
    let client_for_refresh = Rc::clone(&client);
    if let Err(error) = client.request_console(&command, move |_, record, output| {
        if let Some(ui) = ui_for_response.upgrade() {
            let detail = output.trim();
            if record.is_done() {
                if detail.is_empty() {
                    ui.add_debug_data_success("Symbol loading completed");
                } else if detail.contains("No loaded shared libraries match") {
                    ui.add_debug_data_warning(detail);
                } else {
                    ui.add_debug_data_success(detail);
                }
            } else {
                ui.add_debug_data_error(format!(
                    "Symbol loading failed: {}",
                    console_error(&record, &output)
                ));
            }
        }
        refresh_debug_data(ui_for_response, client_for_refresh);
    }) && let Some(ui) = ui.upgrade()
    {
        ui.add_debug_data_error(format!("Could not queue symbol loading: {error}"));
        refresh_debug_data(Rc::downgrade(&ui), client);
    }
}

fn parse_debuginfod_status(output: &str) -> String {
    ["on", "off", "ask"]
        .into_iter()
        .find(|state| output.contains(&format!("\"{state}\"")))
        .map_or_else(|| output.trim().to_owned(), |state| state.to_owned())
}

fn parse_setting_tail(output: &str) -> String {
    output
        .split_once(':')
        .map_or(output, |(_, tail)| tail)
        .trim()
        .to_owned()
}

fn parse_source_directories(output: &str) -> Vec<String> {
    parse_setting_tail(output)
        .split(':')
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(str::to_owned)
        .collect()
}

fn parse_substitutions(output: &str) -> Vec<(String, String)> {
    output
        .lines()
        .filter_map(|line| {
            let (from, rest) = quoted_gdb_value(line)?;
            let (_, rest) = rest.split_once("->")?;
            let (to, _) = quoted_gdb_value(rest)?;
            Some((from.to_owned(), to.to_owned()))
        })
        .collect()
}

fn quoted_gdb_value(input: &str) -> Option<(&str, &str)> {
    let (_, value) = input.split_once('`')?;
    value.split_once('\'')
}

fn parse_pretty_printers(output: &str) -> Vec<String> {
    output
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.trim().is_empty())
        .map(str::to_owned)
        .collect()
}

fn console_error<'a>(record: &'a crate::debugger::MiRecord, output: &'a str) -> &'a str {
    record
        .error_message()
        .or_else(|| (!output.trim().is_empty()).then_some(output.trim()))
        .unwrap_or("GDB rejected the command")
}

fn exact_gdb_regex(value: &str) -> String {
    let mut regex = String::with_capacity(value.len().saturating_add(2));
    regex.push('^');
    for character in value.chars() {
        if matches!(
            character,
            '.' | '^' | '$' | '*' | '+' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '|' | '\\'
        ) {
            regex.push('\\');
        }
        regex.push(character);
    }
    regex.push('$');
    regex
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_gdb_debug_data_reports() {
        assert_eq!(
            parse_debuginfod_status("Debuginfod functionality is currently set to \"ask\"."),
            "ask"
        );
        assert_eq!(
            parse_source_directories("Source directories searched: /src:$cdir:$cwd\n"),
            ["/src", "$cdir", "$cwd"]
        );
        assert_eq!(
            parse_substitutions("List:\n  `/rustc/hash' -> `/local/rust'.\n"),
            [(String::from("/rustc/hash"), String::from("/local/rust"))]
        );
    }

    #[test]
    fn quotes_module_names_as_exact_gdb_regexes() {
        assert_eq!(
            exact_gdb_regex("/usr/lib/libc.so.6"),
            r"^/usr/lib/libc\.so\.6$"
        );
    }
}
