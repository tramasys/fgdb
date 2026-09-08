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

pub(super) fn connect_source_symbol_navigation(
    ui: &Rc<Ui>,
    client: &Rc<MiClient>,
    resolver: &Rc<crate::symbols::SymbolResolver>,
) {
    let navigation = Rc::new(SourceSymbolNavigation {
        ui: Rc::downgrade(ui),
        client: Rc::downgrade(client),
        resolver: Rc::downgrade(resolver),
        generation: Cell::new(0),
    });

    ui.set_source_symbol_handler(move |symbol| {
        let (Some(ui), Some(client)) = (navigation.ui.upgrade(), navigation.client.upgrade())
        else {
            return;
        };

        let generation = navigation.generation.get().wrapping_add(1);
        navigation.generation.set(generation);

        let lookup = Rc::new(SourceSymbolLookup {
            navigation: Rc::clone(&navigation),
            generation,
            lifetime: ui.model.symbols.lifetime(),
            epoch: client.transport_epoch(),
            inferior: ui.model.selected_inferior_id(),
            symbol,
        });

        ui.set_status(
            "Resolving source",
            &format!("Looking up {} through GDB…", lookup.symbol),
            None,
        );
        lookup.request(true);
    });
}

struct SourceSymbolNavigation {
    ui: Weak<Ui>,
    client: Weak<MiClient>,
    resolver: Weak<crate::symbols::SymbolResolver>,
    generation: Cell<u64>,
}

/// A source lookup and its optional symbol-loading retry share one identity.
/// Neither a newer lookup nor a different session may inherit the old action.
struct SourceSymbolLookup {
    navigation: Rc<SourceSymbolNavigation>,
    generation: u64,
    lifetime: u64,
    epoch: u64,
    inferior: Option<String>,
    symbol: String,
}

impl SourceSymbolLookup {
    fn current(&self) -> bool {
        self.navigation.generation.get() == self.generation
            && self
                .navigation
                .client
                .upgrade()
                .is_some_and(|client| client.is_ready() && client.transport_epoch() == self.epoch)
            && self.navigation.ui.upgrade().is_some_and(|ui| {
                ui.model.symbols.lifetime() == self.lifetime
                    && ui.model.selected_inferior_id() == self.inferior
            })
    }

    fn fail(&self, message: &str) {
        if self.current()
            && let Some(ui) = self.navigation.ui.upgrade()
        {
            ui.set_status("Symbol lookup failed", message, Some("status-error"));
        }
    }

    fn request(self: &Rc<Self>, load_on_miss: bool) {
        if !self.current() {
            return;
        }

        let Some(client) = self.navigation.client.upgrade() else {
            return;
        };

        let command = format!(
            "-symbol-info-functions --name {} --max-results 256",
            crate::debugger::quote(&source_symbol_pattern(&self.symbol)),
        );

        let lookup = Rc::clone(self);
        let guard = Rc::clone(self);

        if let Err(error) = client.request_when(
            &command,
            move || guard.current(),
            move |_, record| {
                if !lookup.current() {
                    return;
                }

                if !record.is_done() {
                    lookup.fail(
                        record
                            .error_message()
                            .unwrap_or("GDB could not resolve that source symbol"),
                    );
                    return;
                }

                let locations = crate::debugger::source_locations(&record);

                if locations.is_empty() && load_on_miss {
                    lookup.load_symbols();
                } else if let Some(ui) = lookup.navigation.ui.upgrade() {
                    ui.show_source_locations(&lookup.symbol, &locations);
                }
            },
        ) {
            self.fail(&error.to_string());
        }
    }

    fn load_symbols(self: &Rc<Self>) {
        let (Some(ui), Some(resolver)) = (
            self.navigation.ui.upgrade(),
            self.navigation.resolver.upgrade(),
        ) else {
            return;
        };

        if !self.current() {
            return;
        }

        let keys = ui.symbol_module_keys();

        if keys.is_empty() {
            ui.show_source_locations(&self.symbol, &[]);
            return;
        }

        let lookup = Rc::clone(self);

        if let Err(error) = resolver.resolve_all_then(keys, move |summary| {
            if lookup.current() {
                if summary.cancelled == 0 {
                    lookup.request(false);
                } else {
                    lookup.fail("Library symbol loading was cancelled");
                }
            }
        }) {
            self.fail(&error);
        } else {
            ui.set_status(
                "Loading library symbols",
                &format!(
                    "Resolving libraries for {} using the configured symbol policy…",
                    self.symbol
                ),
                None,
            );
        }
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
