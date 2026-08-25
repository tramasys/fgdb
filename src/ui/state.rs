use super::*;

impl Ui {
    pub fn build(application: &gtk::Application, config: &LaunchConfig, theme: &Theme) -> Self {
        let window = gtk::ApplicationWindow::builder()
            .application(application)
            .title("fgdb")
            .default_width(1380)
            .default_height(820)
            .build();
        window.add_css_class("fgdb-window");

        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("debugger-root");
        let terminal = build_terminal(theme);
        let topbar = build_topbar(config, &window, &terminal);
        window.set_titlebar(Some(&topbar.root));

        let source_style_scheme = theme.source_style_scheme();
        let source_notebook = build_source_notebook(source_style_scheme.as_ref());
        let source_documents = Rc::new(RefCell::new(Vec::new()));
        let breakpoints = Rc::new(RefCell::new(Vec::new()));
        let variable_children_handler = Rc::new(RefCell::new(None));
        let target_pointer_bits = Rc::new(Cell::new(usize::BITS));

        let workspace = build_workspace(
            config,
            theme,
            &source_notebook,
            &terminal,
            &variable_children_handler,
            &target_pointer_bits,
        );
        root.append(&workspace.root);
        root.append(&workspace.status_detail);
        let terminal_panel = workspace.terminal_panel.clone();
        topbar
            .terminal_toggle_button
            .connect_toggled(move |button| terminal_panel.set_visible(button.is_active()));
        window.set_child(Some(&root));
        let layout = layout::Persistence::install(&window, workspace.layout_panes.clone());

        let ui = Self {
            window,
            terminal,
            open_source_button: topbar.open_source_button,
            load_symbols_button: topbar.load_symbols_button,
            run_button: topbar.run_button,
            pause_button: topbar.pause_button,
            next_button: topbar.next_button,
            step_button: topbar.step_button,
            next_instruction_button: topbar.next_instruction_button,
            step_instruction_button: topbar.step_instruction_button,
            finish_button: topbar.finish_button,
            until_button: topbar.until_button,
            until_popover: topbar.until_popover,
            gef_tools_button: topbar.gef_tools_button,
            status_label: topbar.status_label,
            status_detail: workspace.status_detail,
            debug_state_panels: workspace.debug_state_panels,
            source_notebook,
            source_documents,
            source_theme: theme.clone(),
            source_style_scheme,
            resolved_source_paths: Rc::new(RefCell::new(HashMap::new())),
            call_stack_list: workspace.call_stack_list,
            frame_buttons: Rc::new(RefCell::new(Vec::new())),
            selected_frame_level: Rc::new(Cell::new(0)),
            threads_list: workspace.threads_list,
            modules_list: workspace.modules_list,
            locals_store: workspace.locals_store,
            locals_selection: workspace.locals_selection,
            locals_view: workspace.locals_view,
            locals_empty: workspace.locals_empty,
            locals_edit_button: workspace.locals_edit_button,
            target_pointer_bits,
            instructions_title: workspace.instructions_title,
            instructions_store: workspace.instructions_store,
            instructions_selection: workspace.instructions_selection,
            instructions_view: workspace.instructions_view,
            instructions_empty: workspace.instructions_empty,
            instruction_flow: workspace.instruction_flow,
            instruction_arguments: workspace.instruction_arguments,
            instruction_memory: workspace.instruction_memory,
            current_instruction: Rc::new(RefCell::new(None)),
            current_instruction_memory_expression: Rc::new(RefCell::new(None)),
            latest_registers: Rc::new(RefCell::new(Vec::new())),
            instruction_memory_handler: Rc::new(RefCell::new(None)),
            register_groups: workspace.register_groups,
            registers_empty: workspace.registers_empty,
            stack_store: workspace.stack_store,
            stack_empty: workspace.stack_empty,
            breakpoints_list: workspace.breakpoints_list,
            delete_all_breakpoints_button: workspace.delete_all_breakpoints_button,
            delete_all_watchpoints_button: workspace.delete_all_watchpoints_button,
            delete_all_catchpoints_button: workspace.delete_all_catchpoints_button,
            event_catchpoint_buttons: workspace.event_catchpoint_buttons,
            watchpoint_expression: workspace.watchpoint_expression,
            watchpoint_access: workspace.watchpoint_access,
            watchpoint_add_button: workspace.watchpoint_add_button,
            signal_detail: workspace.signal_detail,
            signal_buttons: workspace.signal_buttons,
            signal_entry: workspace.signal_entry,
            signal_add_button: workspace.signal_add_button,
            delete_all_signal_catchpoints_button: workspace.delete_all_signal_catchpoints_button,
            until_actions: topbar.until_actions,
            until_condition_entry: topbar.until_condition_entry,
            until_condition_button: topbar.until_condition_button,
            memory_region_store: workspace.memory_region_store,
            memory_regions_empty: workspace.memory_regions_empty,
            memory_regions: Rc::new(RefCell::new(Vec::new())),
            memory_watches: Rc::new(RefCell::new(Vec::new())),
            memory_watch_list: workspace.memory_watch_list,
            memory_watches_empty: workspace.memory_watches_empty,
            memory_address_entry: workspace.memory_address_entry,
            memory_size: workspace.memory_size,
            memory_format: workspace.memory_format,
            memory_add_button: workspace.memory_add_button,
            memory_watch_handler: Rc::new(RefCell::new(None)),
            layout,
            breakpoints,
            previous_registers: Rc::new(RefCell::new(HashMap::new())),
            stop_refresh_generation: Rc::new(Cell::new(0)),
            thread_refresh_generation: Rc::new(Cell::new(0)),
            breakpoint_refresh_generation: Rc::new(Cell::new(0)),
            command_pending: Rc::new(Cell::new(false)),
            source_roots: Rc::new(source::roots(config)),
            frame_selection_handler: Rc::new(RefCell::new(None)),
            thread_selection_handler: Rc::new(RefCell::new(None)),
            instruction_handler: Rc::new(RefCell::new(None)),
            variable_assignment_handler: Rc::new(RefCell::new(None)),
            variable_children_handler,
            vector_assignment_handler: Rc::new(RefCell::new(None)),
            breakpoint_insert_handler: Rc::new(RefCell::new(None)),
            breakpoint_delete_handler: Rc::new(RefCell::new(None)),
            breakpoint_condition_handler: Rc::new(RefCell::new(None)),
            breakpoint_enabled_handler: Rc::new(RefCell::new(None)),
            breakpoint_bulk_delete_handler: Rc::new(RefCell::new(None)),
            signal_catchpoint_handler: Rc::new(RefCell::new(None)),
            event_catchpoint_handler: Rc::new(RefCell::new(None)),
            watchpoint_insert_handler: Rc::new(RefCell::new(None)),
            source_symbol_handler: Rc::new(RefCell::new(None)),
            thread_stop_reason: Rc::new(RefCell::new(None)),
            debugger_ready: Rc::new(Cell::new(false)),
            inferior_running: Rc::new(Cell::new(false)),
            inferior_started: Rc::new(Cell::new(false)),
        };
        ui.connect_instruction_activation();
        ui.connect_local_activation();
        ui.connect_register_activation();
        ui.connect_memory_controls();
        ui.connect_watchpoint_controls();
        ui.connect_breakpoint_bulk_controls();
        ui.connect_event_catchpoint_controls();
        ui.connect_keyboard_shortcuts();
        ui
    }

    pub fn save_layout(&self) {
        self.layout.save();
    }

    pub fn connect_debug_controls(self: &Rc<Self>, client: &Rc<MiClient>) {
        let client_for_run = Rc::clone(client);
        let weak_ui = Rc::downgrade(self);
        self.run_button.connect_clicked(move |_| {
            let Some(ui) = weak_ui.upgrade() else {
                return;
            };
            let (command, detail) = if ui.inferior_started.get() {
                ("-exec-continue", "Continuing the inferior…")
            } else {
                ("-exec-run", "Starting the inferior…")
            };
            issue_execution_command(&ui, &client_for_run, command, detail);
        });
        connect_execution_button(
            &self.pause_button,
            self,
            client,
            "-exec-interrupt",
            "Interrupting the inferior…",
        );
        connect_execution_button(
            &self.next_button,
            self,
            client,
            "-exec-next",
            "Stepping over the current source line…",
        );
        connect_execution_button(
            &self.step_button,
            self,
            client,
            "-exec-step",
            "Stepping into the current source line…",
        );
        connect_execution_button(
            &self.next_instruction_button,
            self,
            client,
            "-exec-next-instruction",
            "Stepping over one machine instruction…",
        );
        connect_execution_button(
            &self.step_instruction_button,
            self,
            client,
            "-exec-step-instruction",
            "Stepping into one machine instruction…",
        );
        connect_execution_button(
            &self.finish_button,
            self,
            client,
            "-exec-finish",
            "Running until the current function returns…",
        );
        for (button, command) in &self.until_actions {
            let client = Rc::clone(client);
            let command = *command;
            let until_popover = self.until_popover.clone();
            let weak_ui = Rc::downgrade(self);
            button.connect_clicked(move |_| {
                until_popover.popdown();
                let Some(ui) = weak_ui.upgrade() else {
                    return;
                };
                let command = if command.starts_with('-') {
                    command.to_owned()
                } else {
                    format!(
                        "-interpreter-exec console {}",
                        crate::debugger::quote(command)
                    )
                };
                issue_execution_command(&ui, &client, &command, "Running to the selected event…");
            });
        }
        let condition_client = Rc::clone(client);
        let condition_entry = self.until_condition_entry.clone();
        let until_popover = self.until_popover.clone();
        let weak_ui = Rc::downgrade(self);
        self.until_condition_button.connect_clicked(move |_| {
            let condition = condition_entry.text().trim().to_owned();
            if condition.is_empty() {
                return;
            }
            until_popover.popdown();
            let command = format!("exec-until cond {condition}");
            let command = format!(
                "-interpreter-exec console {}",
                crate::debugger::quote(&command)
            );
            if let Some(ui) = weak_ui.upgrade() {
                issue_execution_command(
                    &ui,
                    &condition_client,
                    &command,
                    "Running until the expression becomes true…",
                );
            }
        });
        let symbol_client = Rc::clone(client);
        let weak_ui = Rc::downgrade(self);
        self.load_symbols_button.connect_clicked(move |_| {
            let command = format!(
                "-interpreter-exec console {}",
                crate::debugger::quote("sharedlibrary")
            );
            let weak_ui_for_response = weak_ui.clone();
            if let Some(ui) = weak_ui.upgrade() {
                ui.set_status("Loading symbols", "Loading shared-library symbols…", None);
            }
            if symbol_client
                .request(&command, move |_, record| {
                    if let Some(ui) = weak_ui_for_response.upgrade() {
                        if record.is_done() {
                            ui.set_status(
                                "Paused",
                                "Shared-library symbols are loaded",
                                Some("status-ready"),
                            );
                        } else {
                            ui.set_status(
                                "Symbol load failed",
                                record
                                    .error_message()
                                    .unwrap_or("GDB rejected sharedlibrary"),
                                Some("status-error"),
                            );
                        }
                    }
                })
                .is_err()
                && let Some(ui) = weak_ui.upgrade()
            {
                ui.set_status(
                    "Symbol load failed",
                    "The MI channel is unavailable",
                    Some("status-error"),
                );
            }
        });
        for (button, signal, _) in &self.signal_buttons {
            let signal = (*signal).to_owned();
            let weak_ui = Rc::downgrade(self);
            button.connect_clicked(move |_| {
                if let Some(ui) = weak_ui.upgrade() {
                    request_signal_catchpoint_toggle(&ui, &signal);
                }
            });
        }
        let signal_button = self.signal_add_button.clone();
        self.signal_entry.connect_activate(move |_| {
            if signal_button.is_sensitive() {
                signal_button.emit_clicked();
            }
        });
        let signal_button = self.signal_add_button.clone();
        let ready = Rc::clone(&self.debugger_ready);
        let running = Rc::clone(&self.inferior_running);
        let pending = Rc::clone(&self.command_pending);
        self.signal_entry.connect_changed(move |entry| {
            signal_button.set_sensitive(
                ready.get()
                    && !running.get()
                    && !pending.get()
                    && normalized_signal_name(&entry.text()).is_some(),
            );
        });
        let signal_entry = self.signal_entry.clone();
        let weak_ui = Rc::downgrade(self);
        self.signal_add_button.connect_clicked(move |_| {
            if let Some(ui) = weak_ui.upgrade() {
                request_signal_catchpoint_toggle(&ui, &signal_entry.text());
            }
        });
    }

    pub fn connect_source_actions(&self) {
        self.connect_open_source();
    }

    fn connect_keyboard_shortcuts(&self) {
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let run = self.run_button.clone();
        let pause = self.pause_button.clone();
        let next = self.next_button.clone();
        let step = self.step_button.clone();
        let next_instruction = self.next_instruction_button.clone();
        let step_instruction = self.step_instruction_button.clone();
        let finish = self.finish_button.clone();
        keys.connect_key_pressed(move |_, key, _, state| {
            let control = state.contains(gtk::gdk::ModifierType::CONTROL_MASK);
            let shift = state.contains(gtk::gdk::ModifierType::SHIFT_MASK);
            let blocked = state
                .intersects(gtk::gdk::ModifierType::ALT_MASK | gtk::gdk::ModifierType::SUPER_MASK);
            if blocked {
                return gtk::glib::Propagation::Proceed;
            }
            let button = match (key, control, shift) {
                (gtk::gdk::Key::F5, false, false) => Some(&run),
                (gtk::gdk::Key::F6, false, false) => Some(&pause),
                (gtk::gdk::Key::F10, false, false) => Some(&next),
                (gtk::gdk::Key::F10, true, false) => Some(&next_instruction),
                (gtk::gdk::Key::F11, false, false) => Some(&step),
                (gtk::gdk::Key::F11, true, false) => Some(&step_instruction),
                (gtk::gdk::Key::F11, false, true) => Some(&finish),
                _ => None,
            };
            let Some(button) = button.filter(|button| button.is_sensitive()) else {
                return gtk::glib::Propagation::Proceed;
            };
            button.emit_clicked();
            gtk::glib::Propagation::Stop
        });
        self.window.add_controller(keys);
    }

    pub fn set_status(&self, text: &str, detail: &str, class: Option<&str>) {
        set_status_widgets(&self.status_label, &self.status_detail, text, detail, class);
    }

    pub fn set_controls_ready(&self, ready: bool) {
        self.debugger_ready.set(ready);
        if !ready {
            self.inferior_running.set(false);
            self.command_pending.set(false);
        }
        self.update_control_sensitivity();
    }

    pub fn set_controls_running(&self, running: bool) {
        self.inferior_running.set(running);
        self.update_control_sensitivity();
    }

    pub fn set_command_pending(&self, pending: bool) {
        self.command_pending.set(pending);
        self.update_control_sensitivity();
    }

    pub fn set_debug_state_stale(&self, stale: bool) {
        for panel in &self.debug_state_panels {
            if stale {
                panel.add_css_class("debug-state-stale");
            } else {
                panel.remove_css_class("debug-state-stale");
            }
        }
    }

    pub fn set_inferior_started(&self, started: bool) {
        self.inferior_started.set(started);
        self.update_control_sensitivity();
        self.run_button
            .set_label(if started { "Continue" } else { "Run" });
    }

    pub(super) fn update_control_sensitivity(&self) {
        let ready = self.debugger_ready.get();
        let started = self.inferior_started.get();
        let running = self.inferior_running.get();
        let pending = self.command_pending.get();
        let can_move = ready && started && !running && !pending;

        self.run_button.set_sensitive(ready && !running && !pending);
        self.pause_button
            .set_sensitive(ready && started && running && !pending);
        self.next_button.set_sensitive(can_move);
        self.step_button.set_sensitive(can_move);
        self.next_instruction_button.set_sensitive(can_move);
        self.step_instruction_button.set_sensitive(can_move);
        self.finish_button.set_sensitive(can_move);
        self.until_button.set_sensitive(can_move);
        self.gef_tools_button
            .set_sensitive(ready && !running && !pending);
        self.locals_view.set_sensitive(can_move);
        self.locals_edit_button.set_sensitive(
            can_move
                && variable_at(&self.locals_selection, self.locals_selection.selected()).is_some(),
        );
        for group in &self.register_groups {
            group.view.set_sensitive(can_move);
        }
        self.memory_add_button
            .set_sensitive(can_move && !self.memory_address_entry.text().trim().is_empty());
        self.watchpoint_add_button.set_sensitive(can_move);
        self.load_symbols_button.set_sensitive(can_move);
        let breakpoints = self.breakpoints.borrow();
        let can_edit_stop_points = ready && !running && !pending;
        for (button, _, _) in &self.signal_buttons {
            button.set_sensitive(can_edit_stop_points);
        }
        for (button, _) in &self.event_catchpoint_buttons {
            button.set_sensitive(can_edit_stop_points);
        }
        self.signal_entry.set_sensitive(can_edit_stop_points);
        self.signal_add_button.set_sensitive(
            can_edit_stop_points && normalized_signal_name(&self.signal_entry.text()).is_some(),
        );
        self.delete_all_signal_catchpoints_button.set_sensitive(
            can_edit_stop_points && breakpoints.iter().any(Breakpoint::is_signal_catchpoint),
        );
        self.delete_all_catchpoints_button.set_sensitive(
            can_edit_stop_points
                && breakpoints.iter().any(|breakpoint| {
                    EventCatchpoint::ALL
                        .iter()
                        .any(|(event, _, _)| event.matches(breakpoint))
                }),
        );
        self.delete_all_breakpoints_button.set_sensitive(
            can_edit_stop_points
                && breakpoints
                    .iter()
                    .any(|breakpoint| !breakpoint.is_watchpoint() && !breakpoint.is_catchpoint()),
        );
        self.delete_all_watchpoints_button.set_sensitive(
            can_edit_stop_points && breakpoints.iter().any(Breakpoint::is_watchpoint),
        );
    }

    pub fn set_frame_selection_handler(&self, handler: impl Fn(u32) + 'static) {
        self.frame_selection_handler.replace(Some(Rc::new(handler)));
    }

    pub fn set_thread_selection_handler(&self, handler: impl Fn(String) + 'static) {
        self.thread_selection_handler
            .replace(Some(Rc::new(handler)));
    }

    pub fn set_instruction_handler(&self, handler: impl Fn(String) + 'static) {
        self.instruction_handler.replace(Some(Rc::new(handler)));
    }

    pub fn set_instruction_memory_handler(&self, handler: impl Fn(String) + 'static) {
        self.instruction_memory_handler
            .replace(Some(Rc::new(handler)));
    }

    pub fn set_memory_watch_handler(&self, handler: impl Fn(u64, String, usize) + 'static) {
        self.memory_watch_handler.replace(Some(Rc::new(handler)));
    }

    pub fn set_variable_object_assignment_handler(
        &self,
        handler: impl Fn(Variable, String) + 'static,
    ) {
        self.variable_assignment_handler
            .replace(Some(Rc::new(handler)));
    }

    pub fn set_variable_children_handler(&self, handler: impl Fn(Variable, usize) + 'static) {
        self.variable_children_handler
            .replace(Some(Rc::new(handler)));
    }

    pub fn set_vector_assignment_handler(
        &self,
        handler: impl Fn(String, String, Vec<(usize, String)>) + 'static,
    ) {
        self.vector_assignment_handler
            .replace(Some(Rc::new(handler)));
    }

    pub fn set_breakpoint_insert_handler(&self, handler: impl Fn(PathBuf, u32) + 'static) {
        self.breakpoint_insert_handler
            .replace(Some(Rc::new(handler)));
    }

    pub fn set_breakpoint_delete_handler(&self, handler: impl Fn(String) + 'static) {
        self.breakpoint_delete_handler
            .replace(Some(Rc::new(handler)));
    }

    pub fn set_breakpoint_condition_handler(
        &self,
        handler: impl Fn(String, Option<String>) + 'static,
    ) {
        self.breakpoint_condition_handler
            .replace(Some(Rc::new(handler)));
    }

    pub fn set_breakpoint_enabled_handler(&self, handler: impl Fn(String, bool) + 'static) {
        self.breakpoint_enabled_handler
            .replace(Some(Rc::new(handler)));
    }

    pub fn set_breakpoint_bulk_delete_handler(&self, handler: impl Fn(Vec<String>) + 'static) {
        self.breakpoint_bulk_delete_handler
            .replace(Some(Rc::new(handler)));
    }

    pub fn set_event_catchpoint_handler(
        &self,
        handler: impl Fn(EventCatchpoint, Option<String>) + 'static,
    ) {
        self.event_catchpoint_handler
            .replace(Some(Rc::new(handler)));
    }

    pub fn set_signal_catchpoint_handler(
        &self,
        handler: impl Fn(String, Option<String>) + 'static,
    ) {
        self.signal_catchpoint_handler
            .replace(Some(Rc::new(handler)));
    }

    pub fn set_watchpoint_insert_handler(
        &self,
        handler: impl Fn(String, WatchpointAccess) + 'static,
    ) {
        self.watchpoint_insert_handler
            .replace(Some(Rc::new(handler)));
    }

    pub fn set_source_symbol_handler(&self, handler: impl Fn(String) + 'static) {
        self.source_symbol_handler.replace(Some(Rc::new(handler)));
    }

    pub fn set_thread_stop_reason(&self, reason: Option<&str>) {
        self.thread_stop_reason
            .replace(reason.map(stop_reason_label));
    }
}
