use super::*;

impl Ui {
    pub fn show_frames(&self, frames: &[StackFrame]) {
        clear_box(&self.call_stack_list);
        self.frame_buttons.borrow_mut().clear();
        if frames.is_empty() {
            self.call_stack_list
                .append(&empty_label("No stack frames available"));
            return;
        }

        for frame in frames {
            let location_text = frame.line.map_or_else(
                || frame.address.clone(),
                |line| {
                    format!(
                        "{}:{line}",
                        frame.source_path().unwrap_or(frame.address.as_str())
                    )
                },
            );
            let row = gtk::Box::new(gtk::Orientation::Vertical, 0);
            let displayed_function = compact_function_name(&frame.function);
            let function =
                gtk::Label::new(Some(&format!("#{}  {displayed_function}", frame.level)));
            function.set_halign(gtk::Align::Start);
            function.set_ellipsize(pango::EllipsizeMode::End);
            function.set_tooltip_text(Some(&frame.function));
            let location = gtk::Label::new(Some(&location_text));
            location.add_css_class("muted");
            location.set_halign(gtk::Align::Start);
            location.set_ellipsize(pango::EllipsizeMode::Middle);
            location.set_tooltip_text(Some(&location_text));
            row.append(&function);
            row.append(&location);
            let button = gtk::Button::builder().child(&row).build();
            button.add_css_class("stack-frame");
            if frame.level == self.selected_frame_level.get() {
                button.add_css_class("current-debug-item");
            }
            let level = frame.level;
            let handler = Rc::clone(&self.frame_selection_handler);
            let frame_buttons = Rc::clone(&self.frame_buttons);
            let selected_frame_level = Rc::clone(&self.selected_frame_level);
            button.connect_clicked(move |_| {
                selected_frame_level.set(level);
                update_selected_frame_buttons(&frame_buttons.borrow(), level);
                if let Some(handler) = handler.borrow().as_ref() {
                    handler(level);
                }
            });
            self.frame_buttons
                .borrow_mut()
                .push((level, button.clone()));
            self.call_stack_list.append(&button);
        }
    }

    pub fn show_locals(&self, variables: &[Variable]) {
        let selected_name = variable_at(&self.locals_selection, self.locals_selection.selected())
            .map(|variable| variable.name);
        replace_boxed_store(
            &self.locals_store,
            variables.iter().cloned().map(VariableNode::new),
        );
        self.locals_selection
            .set_selected(gtk::INVALID_LIST_POSITION);
        if variables.is_empty() {
            self.locals_empty.set_visible(true);
            self.locals_edit_button.set_sensitive(false);
        } else {
            self.locals_empty.set_visible(false);
            let selected = selected_name
                .as_deref()
                .and_then(|name| variables.iter().position(|variable| variable.name == name))
                .and_then(|position| u32::try_from(position).ok())
                .unwrap_or(0);
            self.locals_selection.set_selected(selected);
            self.locals_edit_button.set_sensitive(true);
        }
    }

    pub fn show_locals_for_refresh(&self, generation: u64, variables: &[Variable]) {
        if self.is_stop_refresh_current(generation) {
            self.show_locals(variables);
        }
    }

    pub fn show_variable_children_page(
        &self,
        parent: &Variable,
        from: usize,
        variables: &[Variable],
        has_more: bool,
    ) -> bool {
        let Some(parent_name) = parent.varobj.as_deref() else {
            return false;
        };
        let Some(node) = self.find_variable_node(parent_name) else {
            return false;
        };
        if from != 0 {
            remove_load_more_rows(&node.children);
        }
        let mut additions = variables
            .iter()
            .cloned()
            .map(VariableNode::new)
            .map(glib::BoxedAnyObject::new)
            .collect::<Vec<_>>();
        if has_more {
            additions.push(glib::BoxedAnyObject::new(VariableNode::load_more(
                parent.clone(),
                from.saturating_add(variables.len()),
            )));
        }
        if from == 0 {
            node.children.splice(0, node.children.n_items(), &additions);
        } else {
            node.children.extend_from_slice(&additions);
        }
        node.children_loading.set(false);
        node.children_loaded.set(true);
        true
    }

    pub fn show_variable_children(&self, parent: &str, variables: &[Variable]) -> bool {
        let Some(node) = self.find_variable_node(parent) else {
            return false;
        };
        let parent = node.variable.clone();
        self.show_variable_children_page(&parent, 0, variables, false)
    }

    pub fn has_variable_object(&self, varobj: &str) -> bool {
        self.find_variable_node(varobj).is_some()
    }

    pub fn show_variable_children_error(&self, parent: &str, error: &str) {
        let Some(node) = self.find_variable_node(parent) else {
            return;
        };
        node.children.splice(
            0,
            node.children.n_items(),
            &[glib::BoxedAnyObject::new(VariableNode::placeholder(
                "unavailable",
                error,
            ))],
        );
        node.children_loading.set(false);
        node.children_loaded.set(true);
    }

    pub fn local_variable_object_names(&self) -> Vec<String> {
        let mut names = Vec::new();
        collect_variable_object_roots(&self.locals_store, None, &mut names);
        names
    }

    fn find_variable_node(&self, varobj: &str) -> Option<VariableNode> {
        find_variable_node(&self.locals_store, varobj)
            .or_else(|| find_variable_node(&self.expression_watches_store, varobj))
    }

    pub(super) fn connect_local_activation(&self) {
        let window = self.window.clone();
        let selection = self.locals_selection.clone();
        let handler = Rc::clone(&self.variable_assignment_handler);
        let float_handler = Rc::clone(&self.float_assignment_handler);
        let editor_handler = Rc::clone(&self.variable_editor_handler);
        let string_handler = Rc::clone(&self.string_assignment_handler);
        let children_handler = Rc::clone(&self.variable_children_handler);
        let target_pointer_bits = Rc::clone(&self.target_pointer_bits);
        let current_source_is_rust = Rc::clone(&self.current_source_is_rust);
        self.locals_view.connect_activate(move |_, position| {
            let Some((row, node)) = variable_node_at(&selection, position) else {
                return;
            };
            if node.load_more.is_some() {
                request_next_variable_page_if_needed(&node, &children_handler);
            } else if !node.placeholder {
                if row.is_expandable() {
                    row.set_expanded(!row.is_expanded());
                } else {
                    let variable = node.variable;
                    if let Some(editor_handler) = editor_handler.borrow().as_ref() {
                        editor_handler(variable);
                    } else {
                        open_variable_editor(
                            &window,
                            variable,
                            target_pointer_bits.get(),
                            current_source_is_rust.get(),
                            None,
                            ValueEditorHandlers {
                                assignment: Rc::clone(&handler),
                                float: Rc::clone(&float_handler),
                                string: Rc::clone(&string_handler),
                            },
                        );
                    }
                }
            }
        });

        let window = self.window.clone();
        let selection = self.locals_selection.clone();
        let handler = Rc::clone(&self.variable_assignment_handler);
        let float_handler = Rc::clone(&self.float_assignment_handler);
        let editor_handler = Rc::clone(&self.variable_editor_handler);
        let string_handler = Rc::clone(&self.string_assignment_handler);
        let target_pointer_bits = Rc::clone(&self.target_pointer_bits);
        let current_source_is_rust = Rc::clone(&self.current_source_is_rust);
        self.locals_edit_button.connect_clicked(move |_| {
            if let Some(variable) = variable_at(&selection, selection.selected()) {
                if let Some(editor_handler) = editor_handler.borrow().as_ref() {
                    editor_handler(variable);
                } else {
                    open_variable_editor(
                        &window,
                        variable,
                        target_pointer_bits.get(),
                        current_source_is_rust.get(),
                        None,
                        ValueEditorHandlers {
                            assignment: Rc::clone(&handler),
                            float: Rc::clone(&float_handler),
                            string: Rc::clone(&string_handler),
                        },
                    );
                }
            }
        });

        let edit_button = self.locals_edit_button.clone();
        let ready = Rc::clone(&self.debugger_ready);
        let started = Rc::clone(&self.inferior_started);
        let running = Rc::clone(&self.inferior_running);
        let pending = Rc::clone(&self.command_pending);
        self.locals_selection
            .connect_selected_notify(move |selection| {
                edit_button.set_sensitive(
                    ready.get()
                        && started.get()
                        && !running.get()
                        && !pending.get()
                        && variable_at(selection, selection.selected()).is_some(),
                );
            });
    }

    pub(super) fn connect_register_activation(&self) {
        for group in &self.register_groups {
            let parent = self.window.clone();
            let store = group.store.clone();
            let handler = Rc::clone(&self.variable_assignment_handler);
            let float_handler = Rc::clone(&self.float_assignment_handler);
            let string_handler = Rc::clone(&self.string_assignment_handler);
            let vector_handler = Rc::clone(&self.vector_assignment_handler);
            let target_pointer_bits = Rc::clone(&self.target_pointer_bits);
            let current_source_is_rust = Rc::clone(&self.current_source_is_rust);
            group.view.connect_activate(move |_, position| {
                let Some(item) = store
                    .item(position)
                    .and_then(|item| item.downcast::<glib::BoxedAnyObject>().ok())
                else {
                    return;
                };
                let register = item.borrow::<RegisterRowData>().register.clone();
                if matches!(register.name.as_str(), "eflags" | "rflags") {
                    open_flag_editor(&parent, register, Rc::clone(&handler));
                } else if vector_register_bytes(&register.name).is_some() {
                    open_vector_editor(&parent, register, Rc::clone(&vector_handler));
                } else {
                    open_variable_editor(
                        &parent,
                        Variable {
                            name: format!("${}", register.name),
                            value: register.value,
                            type_name: None,
                            varobj: None,
                            num_children: 0,
                            has_more: false,
                        },
                        target_pointer_bits.get(),
                        current_source_is_rust.get(),
                        None,
                        ValueEditorHandlers {
                            assignment: Rc::clone(&handler),
                            float: Rc::clone(&float_handler),
                            string: Rc::clone(&string_handler),
                        },
                    );
                }
            });
        }
    }

    pub fn show_threads(&self, threads: &[ThreadInfo]) {
        clear_box(&self.threads_list);
        if threads.is_empty() {
            self.threads_list
                .append(&empty_label("No threads available"));
            return;
        }
        let stop_reason = self.thread_stop_reason.borrow().clone();
        for thread in threads {
            let marker = if thread.current { "*" } else { " " };
            let tid = thread_os_id(&thread.target_id).unwrap_or_else(|| String::from("?"));
            let name = thread.name.as_deref().unwrap_or("<unnamed>");
            let reason = thread
                .current
                .then(|| stop_reason.as_deref().unwrap_or("STOPPED"));
            let detail = thread_detail(thread, reason);
            let row = gtk::Box::new(gtk::Orientation::Vertical, 0);
            let heading = gtk::Label::new(Some(&format!(
                "[{marker}Thread Id:{}, tid:{tid}]",
                thread.id
            )));
            heading.add_css_class("thread-heading");
            heading.set_halign(gtk::Align::Start);
            heading.set_ellipsize(pango::EllipsizeMode::End);
            let name = gtk::Label::new(Some(&format!("Name: \"{name}\"")));
            name.add_css_class("thread-name");
            name.set_halign(gtk::Align::Start);
            name.set_ellipsize(pango::EllipsizeMode::End);
            let detail_widget = thread_detail_widget(thread, reason);
            let full_symbol = thread.frame.as_ref().map(|frame| frame.function.as_str());
            detail_widget.set_tooltip_text(Some(&match full_symbol {
                Some(symbol) => format!(
                    "{detail}\nFull symbol: {symbol}\nGDB target: {}",
                    thread.target_id
                ),
                None => format!("{detail}\nGDB target: {}", thread.target_id),
            }));
            row.append(&heading);
            row.append(&name);
            row.append(&detail_widget);
            let button = gtk::Button::builder().child(&row).build();
            button.add_css_class("stack-frame");
            if thread.current {
                button.add_css_class("current-debug-item");
            }
            let id = thread.id.clone();
            let handler = Rc::clone(&self.thread_selection_handler);
            button.connect_clicked(move |_| {
                if let Some(handler) = handler.borrow().as_ref() {
                    handler(id.clone());
                }
            });
            self.threads_list.append(&button);
        }
    }

    pub fn show_modules(&self, modules: &[SharedLibrary]) {
        if self.latest_modules.borrow().as_slice() == modules {
            return;
        }
        self.latest_modules.replace(modules.to_vec());
        clear_box(&self.modules_list);
        if modules.is_empty() {
            self.modules_list
                .append(&empty_label("No shared libraries loaded"));
            return;
        }

        for module in modules {
            let row = gtk::Box::new(gtk::Orientation::Vertical, 0);
            row.add_css_class("module-row");
            let heading = gtk::Box::new(gtk::Orientation::Horizontal, 4);
            let name = Path::new(&module.target_name)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(&module.target_name);
            let name = gtk::Label::new(Some(name));
            name.add_css_class("module-name");
            name.set_halign(gtk::Align::Start);
            name.set_hexpand(true);
            name.set_ellipsize(pango::EllipsizeMode::End);
            let symbol_state = gtk::Label::new(Some(if module.symbols_loaded {
                "SYMBOLS"
            } else {
                "NO SYMBOLS"
            }));
            symbol_state.add_css_class("module-symbol-state");
            symbol_state.add_css_class(if module.symbols_loaded {
                "module-symbols-loaded"
            } else {
                "module-symbols-missing"
            });
            heading.append(&name);
            heading.append(&symbol_state);

            let range = match (&module.from, &module.to) {
                (Some(from), Some(to)) => format!("{from}–{to}"),
                _ => String::from("address range unavailable"),
            };
            let range = gtk::Label::new(Some(&range));
            range.add_css_class("module-range");
            range.set_halign(gtk::Align::Start);
            enable_stable_text_selection(&range);
            let path = module.host_name.as_deref().unwrap_or(&module.target_name);
            let path_label = gtk::Label::new(Some(path));
            path_label.add_css_class("module-path");
            path_label.set_halign(gtk::Align::Start);
            path_label.set_ellipsize(pango::EllipsizeMode::Middle);
            enable_stable_text_selection(&path_label);
            path_label.set_tooltip_text(Some(&format!(
                "Target: {}\nHost: {}",
                module.target_name, path
            )));
            row.append(&heading);
            row.append(&range);
            row.append(&path_label);
            self.modules_list.append(&row);
        }
    }

    pub fn start_thread_refresh(&self) -> u64 {
        let generation = self.thread_refresh_generation.get().wrapping_add(1);
        self.thread_refresh_generation.set(generation);
        generation
    }

    pub fn show_threads_for_refresh(&self, generation: u64, threads: &[ThreadInfo]) {
        if self.is_thread_refresh_current(generation) {
            self.show_threads(threads);
        }
    }

    pub fn is_thread_refresh_current(&self, generation: u64) -> bool {
        self.thread_refresh_generation.get() == generation
    }

    pub fn show_instructions(
        &self,
        instructions: &[Instruction],
        pc: &str,
        architecture: Option<&str>,
    ) {
        self.instructions_selection
            .set_selected(gtk::INVALID_LIST_POSITION);
        let title = architecture.map_or_else(
            || String::from("INSTRUCTIONS"),
            |architecture| format!("INSTRUCTIONS · {architecture} · GDB NATIVE"),
        );
        self.instructions_title.set_text(&title);
        self.instructions_title.set_tooltip_text(Some(&title));
        if instructions.is_empty() {
            self.instructions_empty.set_visible(true);
            self.current_instruction.replace(None);
            self.current_instruction_memory_expression.replace(None);
            self.instruction_flow
                .set_text("Flow information appears at a branch or call");
            self.instruction_flow.set_visible(true);
            self.instruction_arguments.set_visible(false);
            self.instruction_memory.set_visible(false);
            self.update_control_sensitivity();
            return;
        }
        self.instructions_empty.set_visible(false);
        let current = instructions
            .iter()
            .position(|instruction| addresses_equal(&instruction.address, pc))
            .unwrap_or(0);
        self.current_instruction
            .replace(instructions.get(current).cloned());
        let start = current.saturating_sub(3);
        let rows = instructions
            .iter()
            .skip(start)
            .take(9)
            .map(|instruction| InstructionRowData {
                instruction: instruction.clone(),
                current: addresses_equal(&instruction.address, pc),
            })
            .collect::<Vec<_>>();
        let selected = rows
            .iter()
            .position(|row| row.current)
            .map(|index| index as u32);
        replace_boxed_store(&self.instructions_store, rows);
        if let Some(selected) = selected {
            self.instructions_selection.set_selected(selected);
        }
        self.update_instruction_insight();
        self.update_control_sensitivity();
    }

    fn update_instruction_insight(&self) {
        let Some(instruction) = self.current_instruction.borrow().clone() else {
            return;
        };
        let registers = self.latest_registers.borrow();
        let branch_taken = conditional_branch_taken(&instruction, &registers);
        let flow = instruction_flow_description(&instruction, &registers);
        self.instruction_flow.set_text(&flow);
        self.instruction_flow.set_tooltip_text(Some(&flow));
        self.instruction_flow.set_visible(true);
        self.instruction_flow.remove_css_class("branch-taken");
        self.instruction_flow.remove_css_class("branch-not-taken");
        if let Some(taken) = branch_taken {
            self.instruction_flow.add_css_class(if taken {
                "branch-taken"
            } else {
                "branch-not-taken"
            });
        }

        let arguments = instruction_arguments_description(&instruction, &registers);
        self.instruction_arguments
            .set_visible(!arguments.is_empty());
        self.instruction_arguments.set_text(&arguments);
        self.instruction_arguments
            .set_tooltip_text((!arguments.is_empty()).then_some(arguments.as_str()));

        let expression = instruction_memory_expression(&instruction, &registers);
        drop(registers);
        let mut current = self.current_instruction_memory_expression.borrow_mut();
        if current.as_ref() == expression.as_ref() {
            return;
        }
        current.clone_from(&expression);
        drop(current);
        let Some(expression) = expression else {
            self.instruction_memory.set_visible(false);
            return;
        };
        self.instruction_memory
            .set_text(&format!("MEM  {expression} · reading…"));
        self.instruction_memory.set_visible(true);
        if let Some(handler) = self.instruction_memory_handler.borrow().as_ref() {
            handler(expression);
        }
    }

    pub fn show_instruction_memory(&self, expression: &str, result: Result<&MemoryBlock, &str>) {
        if self
            .current_instruction_memory_expression
            .borrow()
            .as_deref()
            != Some(expression)
        {
            return;
        }
        let text = match result {
            Ok(memory) => format!(
                "MEM  {expression} = 0x{:016x}  {}",
                memory.begin,
                compact_memory_preview(&memory.bytes)
            ),
            Err(error) => format!("MEM  {expression} · {error}"),
        };
        self.instruction_memory.set_text(&text);
        self.instruction_memory.set_tooltip_text(Some(&text));
        self.instruction_memory.set_visible(true);
    }

    pub(super) fn connect_instruction_activation(&self) {
        let store = self.instructions_store.clone();
        let handler = Rc::clone(&self.instruction_handler);
        self.instructions_view.connect_activate(move |_, position| {
            let Some(item) = store
                .item(position)
                .and_then(|item| item.downcast::<glib::BoxedAnyObject>().ok())
            else {
                return;
            };
            let address = item
                .borrow::<InstructionRowData>()
                .instruction
                .address
                .clone();
            if let Some(handler) = handler.borrow().as_ref() {
                handler(address);
            }
        });
    }

    pub fn show_signal(&self, name: Option<&str>, meaning: Option<&str>) {
        let text = match (name, meaning) {
            (Some(name), Some(meaning)) => format!("{name} · {meaning}"),
            (Some(name), None) => name.to_owned(),
            (None, _) => String::from("No signal at the current stop"),
        };
        self.signal_detail.set_text(&text);
        if name.is_some() {
            self.signal_detail.add_css_class("signal-active");
        } else {
            self.signal_detail.remove_css_class("signal-active");
        }
    }

    pub fn show_registers(&self, registers: &[Register]) {
        for group in &self.register_groups {
            group.panel.set_visible(false);
            if registers.is_empty() {
                group.store.remove_all();
            }
        }
        if registers.is_empty() {
            self.registers_empty.set_visible(true);
        } else {
            self.registers_empty.set_visible(false);
            let previous = self.previous_registers.borrow();
            let by_name = registers
                .iter()
                .map(|register| (register.name.as_str(), register))
                .collect::<HashMap<_, _>>();
            let ring = by_name
                .get("cs")
                .and_then(|register| hex_value(&register.value))
                .map(|value| value & 0x3);
            for group in self.register_groups.iter() {
                let grouped = registers.iter().filter(|register| {
                    register_in_group(group.kind, &register.name)
                        && (group.kind != RegisterGroupKind::Other
                            || !self.register_groups.iter().any(|candidate| {
                                candidate.kind != RegisterGroupKind::Other
                                    && register_in_group(candidate.kind, &register.name)
                            }))
                });
                populate_register_group(group, grouped, &previous, ring);
            }
        }
        let values_changed = {
            let latest = self.latest_registers.borrow();
            !same_register_values(&latest, registers)
        };
        if values_changed {
            self.latest_registers.replace(registers.to_vec());
        }
        self.update_instruction_insight();
    }

    pub fn start_stop_refresh(&self) -> u64 {
        let latest = self.latest_registers.borrow();
        let mut previous = self.previous_registers.borrow_mut();
        previous.clear();
        previous.reserve(latest.len());
        previous.extend(
            latest
                .iter()
                .map(|register| (register.name.clone(), register.value.clone())),
        );
        drop(previous);
        drop(latest);
        let generation = self.stop_refresh_generation.get().wrapping_add(1);
        self.stop_refresh_generation.set(generation);
        generation
    }

    pub fn is_stop_refresh_current(&self, generation: u64) -> bool {
        self.stop_refresh_generation.get() == generation
    }

    pub fn current_stop_refresh_generation(&self) -> u64 {
        self.stop_refresh_generation.get()
    }

    pub fn cached_register_names(&self) -> Option<Rc<Vec<String>>> {
        self.cached_register_names.borrow().clone()
    }

    pub fn cache_register_names(&self, names: Rc<Vec<String>>) {
        if !names.is_empty() {
            self.cached_register_names.replace(Some(names));
        }
    }

    pub fn show_registers_for_refresh(&self, generation: u64, registers: &[Register]) {
        if self.is_stop_refresh_current(generation) {
            self.show_registers(registers);
        }
    }

    pub fn show_stack(&self, entries: &[StackEntry]) {
        replace_boxed_store(&self.stack_store, entries.iter().cloned());
        if entries.is_empty() {
            self.stack_empty.set_visible(true);
            return;
        }
        self.stack_empty.set_visible(false);
    }

    pub fn show_stack_for_refresh(&self, generation: u64, entries: &[StackEntry]) {
        if self.is_stop_refresh_current(generation) {
            self.show_stack(entries);
        }
    }

    pub fn show_memory_regions_for_refresh(&self, generation: u64, regions: &[MemoryRegion]) {
        if !self.is_stop_refresh_current(generation) {
            return;
        }
        replace_boxed_store(&self.memory_region_store, regions.iter().cloned());
        self.memory_regions.replace(regions.to_vec());
        self.memory_regions_empty.set_visible(regions.is_empty());
    }

    pub(super) fn connect_memory_controls(&self) {
        let list = self.memory_watch_list.clone();
        let empty = self.memory_watches_empty.clone();
        let watches = Rc::clone(&self.memory_watches);
        let handler = Rc::clone(&self.memory_watch_handler);
        let expression = self.memory_address_entry.clone();
        let size = self.memory_size.clone();
        let format = self.memory_format.clone();
        self.memory_add_button.connect_clicked(move |_| {
            let expression_text = expression.text().trim().to_owned();
            if expression_text.is_empty() {
                return;
            }
            let byte_count = usize::try_from(size.value_as_int()).unwrap_or(128);
            let format = match format.selected() {
                1 => MemoryWatchFormat::Words,
                2 => MemoryWatchFormat::Pointers,
                _ => MemoryWatchFormat::Bytes,
            };
            add_memory_watch(
                &list,
                &empty,
                &watches,
                &handler,
                expression_text,
                byte_count,
                format,
            );
            expression.set_text("");
            expression.grab_focus();
        });
        let button = self.memory_add_button.clone();
        self.memory_address_entry.connect_activate(move |_| {
            if button.is_sensitive() {
                button.emit_clicked();
            }
        });

        let button = self.memory_add_button.clone();
        let ready = Rc::clone(&self.debugger_ready);
        let started = Rc::clone(&self.inferior_started);
        let running = Rc::clone(&self.inferior_running);
        let pending = Rc::clone(&self.command_pending);
        self.memory_address_entry.connect_changed(move |entry| {
            button.set_sensitive(
                ready.get()
                    && started.get()
                    && !running.get()
                    && !pending.get()
                    && !entry.text().trim().is_empty(),
            );
        });
    }

    pub(super) fn connect_watchpoint_controls(&self) {
        let expression = self.watchpoint_expression.clone();
        let access = self.watchpoint_access.clone();
        let handler = Rc::clone(&self.watchpoint_insert_handler);
        self.watchpoint_add_button.connect_clicked(move |_| {
            let expression = expression.text().trim().to_owned();
            if expression.is_empty() {
                return;
            }
            let access = match access.selected() {
                1 => WatchpointAccess::Read,
                2 => WatchpointAccess::Access,
                _ => WatchpointAccess::Write,
            };
            if let Some(handler) = handler.borrow().as_ref() {
                handler(expression, access);
            }
        });
        let button = self.watchpoint_add_button.clone();
        self.watchpoint_expression
            .connect_activate(move |_| button.emit_clicked());
    }

    pub(super) fn connect_breakpoint_bulk_controls(&self) {
        let breakpoints = Rc::clone(&self.breakpoints);
        let handler = Rc::clone(&self.breakpoint_bulk_delete_handler);
        self.delete_all_breakpoints_button
            .connect_clicked(move |_| {
                let numbers = breakpoint_command_numbers(&breakpoints.borrow(), false);
                if !numbers.is_empty()
                    && let Some(handler) = handler.borrow().as_ref()
                {
                    handler(numbers);
                }
            });
        let breakpoints = Rc::clone(&self.breakpoints);
        let handler = Rc::clone(&self.breakpoint_bulk_delete_handler);
        self.delete_all_watchpoints_button
            .connect_clicked(move |_| {
                let numbers = breakpoint_command_numbers(&breakpoints.borrow(), true);
                if !numbers.is_empty()
                    && let Some(handler) = handler.borrow().as_ref()
                {
                    handler(numbers);
                }
            });
        let breakpoints = Rc::clone(&self.breakpoints);
        let handler = Rc::clone(&self.breakpoint_bulk_delete_handler);
        self.delete_all_catchpoints_button
            .connect_clicked(move |_| {
                let numbers = event_catchpoint_command_numbers(&breakpoints.borrow());
                if !numbers.is_empty()
                    && let Some(handler) = handler.borrow().as_ref()
                {
                    handler(numbers);
                }
            });
        let breakpoints = Rc::clone(&self.breakpoints);
        let handler = Rc::clone(&self.breakpoint_bulk_delete_handler);
        self.delete_all_signal_catchpoints_button
            .connect_clicked(move |_| {
                let numbers = signal_catchpoint_command_numbers(&breakpoints.borrow());
                if !numbers.is_empty()
                    && let Some(handler) = handler.borrow().as_ref()
                {
                    handler(numbers);
                }
            });
    }

    pub(super) fn connect_event_catchpoint_controls(&self) {
        for (button, event) in &self.event_catchpoint_buttons {
            let event = *event;
            let breakpoints = Rc::clone(&self.breakpoints);
            let handler = Rc::clone(&self.event_catchpoint_handler);
            button.connect_clicked(move |_| {
                let existing = event_catchpoint_command_number(&breakpoints.borrow(), event);
                if let Some(handler) = handler.borrow().as_ref() {
                    handler(event, existing);
                }
            });
        }
    }

    pub fn refresh_memory_watches(&self) {
        let Some(handler) = self.memory_watch_handler.borrow().clone() else {
            return;
        };
        for watch in self.memory_watches.borrow().iter() {
            watch.status.remove_css_class("memory-watch-error");
            watch.status.set_text("reading…");
            handler(watch.id, watch.expression.clone(), watch.byte_count);
        }
    }

    pub fn show_memory_watch(&self, id: u64, result: Result<&MemoryBlock, &str>) {
        let watches = self.memory_watches.borrow();
        let Some(watch) = watches.iter().find(|watch| watch.id == id) else {
            return;
        };
        match result {
            Ok(memory) => {
                watch.status.remove_css_class("memory-watch-error");
                let region = self
                    .memory_regions
                    .borrow()
                    .iter()
                    .find(|region| region.contains(memory.begin))
                    .map(MemoryRegion::description)
                    .unwrap_or_else(|| String::from("unmapped"));
                watch
                    .status
                    .set_text(&format!("0x{:016x} · {region}", memory.begin));
                let output = format_memory_watch(memory.begin, &memory.bytes, watch.format);
                watch.output_addresses.set_text(&output.addresses);
                watch.output_values.set_text(&output.values);
                watch.output_decoded.set_text(&output.decoded);
            }
            Err(error) => {
                watch.status.add_css_class("memory-watch-error");
                watch.status.set_text(error);
                watch.output_addresses.set_text("");
                watch.output_values.set_text("");
                watch.output_decoded.set_text("");
            }
        }
    }

    pub fn show_breakpoints(&self, breakpoints: Vec<Breakpoint>) {
        if self.breakpoints.borrow().as_slice() == breakpoints {
            return;
        }
        self.breakpoints.replace(breakpoints);
        clear_box(&self.breakpoints_list);
        let breakpoints = self.breakpoints.borrow();
        if breakpoints.is_empty() {
            self.breakpoints_list.append(&empty_label(
                "No breakpoints, catchpoints, or watchpoints set",
            ));
        } else {
            for breakpoint in breakpoints.iter() {
                let name = if breakpoint.is_watchpoint() {
                    breakpoint
                        .original_location
                        .as_deref()
                        .or(breakpoint.function.as_deref())
                        .or(breakpoint.address.as_deref())
                        .unwrap_or("unresolved expression")
                } else if breakpoint.is_catchpoint() {
                    breakpoint
                        .original_location
                        .as_deref()
                        .or(breakpoint.catch_type.as_deref())
                        .unwrap_or("event")
                } else {
                    breakpoint
                        .function
                        .as_deref()
                        .or(breakpoint.original_location.as_deref())
                        .or(breakpoint.address.as_deref())
                        .unwrap_or("unresolved")
                };
                let location = match (breakpoint.source_path(), breakpoint.line) {
                    (Some(file), Some(line)) => format!("{file}:{line}"),
                    _ if breakpoint.is_watchpoint() => breakpoint.kind.clone(),
                    _ if breakpoint.is_catchpoint() => {
                        breakpoint.catch_type.as_deref().map_or_else(
                            || String::from("event catchpoint"),
                            |kind| format!("{kind} catchpoint"),
                        )
                    }
                    _ => breakpoint
                        .address
                        .clone()
                        .unwrap_or_else(|| String::from("pending")),
                };
                let row = gtk::Box::new(gtk::Orientation::Vertical, 0);
                row.add_css_class("stack-row");
                row.add_css_class("breakpoint-row");
                if breakpoint.is_watchpoint() {
                    row.add_css_class("watchpoint-row");
                }
                if !breakpoint.enabled {
                    row.add_css_class("breakpoint-row-disabled");
                }
                let heading_row = gtk::Box::new(gtk::Orientation::Horizontal, 3);
                let kind = if breakpoint.is_watchpoint() || breakpoint.is_catchpoint() {
                    breakpoint.kind.to_ascii_uppercase()
                } else {
                    String::from("BREAKPOINT")
                };
                let badge = gtk::Button::with_label(&format!("#{}", breakpoint.number));
                badge.add_css_class("breakpoint-badge");
                badge.set_focus_on_click(false);
                badge.add_css_class(if breakpoint.enabled {
                    "breakpoint-badge-enabled"
                } else {
                    "breakpoint-badge-disabled"
                });
                badge.set_tooltip_text(Some(if breakpoint.enabled {
                    "Disable this stop point"
                } else {
                    "Enable this stop point"
                }));
                let heading_text = format!("{kind}  {}", compact_function_name(name));
                let heading = gtk::Label::new(Some(&heading_text));
                heading.set_halign(gtk::Align::Start);
                heading.set_ellipsize(pango::EllipsizeMode::End);
                heading.set_hexpand(true);
                heading.set_tooltip_text(Some(&format!("{kind}  {name}")));
                let condition_button = gtk::Button::with_label(if breakpoint.condition.is_some() {
                    "Edit condition"
                } else {
                    "Condition"
                });
                condition_button.add_css_class("inline-action");
                condition_button.set_tooltip_text(Some("Add, edit, or clear a GDB condition"));
                let delete_button = gtk::Button::with_label("Delete");
                delete_button.add_css_class("inline-action");
                delete_button.add_css_class("danger-action");
                delete_button.set_tooltip_text(Some("Delete this breakpoint"));
                heading_row.append(&badge);
                heading_row.append(&heading);
                heading_row.append(&condition_button);
                heading_row.append(&delete_button);
                let location_text = location;
                let location = gtk::Label::new(Some(&location_text));
                location.add_css_class("muted");
                location.set_halign(gtk::Align::Start);
                location.set_ellipsize(pango::EllipsizeMode::Middle);
                enable_stable_text_selection(&location);
                location.set_tooltip_text(Some(&location_text));
                row.append(&heading_row);
                row.append(&location);
                let mut metadata = Vec::new();
                if breakpoint.hit_count > 0 {
                    metadata.push(format!(
                        "{} HIT{}",
                        breakpoint.hit_count,
                        if breakpoint.hit_count == 1 { "" } else { "S" }
                    ));
                }
                if let Some(thread) = breakpoint.thread.as_deref() {
                    metadata.push(format!("THREAD {thread}"));
                }
                if breakpoint.disposition.as_deref() == Some("del") {
                    metadata.push(String::from("TEMPORARY"));
                }
                if !metadata.is_empty() {
                    let metadata = gtk::Label::new(Some(&metadata.join("  ·  ")));
                    metadata.add_css_class("breakpoint-metadata");
                    metadata.set_halign(gtk::Align::Start);
                    enable_stable_text_selection(&metadata);
                    row.append(&metadata);
                }
                if let Some(condition) = breakpoint.condition.as_deref() {
                    let condition = gtk::Label::new(Some(&format!("WHEN  {condition}")));
                    condition.add_css_class("breakpoint-condition");
                    condition.set_halign(gtk::Align::Start);
                    condition.set_ellipsize(pango::EllipsizeMode::End);
                    condition.set_tooltip_text(Some(condition.text().as_str()));
                    row.append(&condition);
                }

                let parent = self.window.clone();
                let breakpoint_for_condition = breakpoint.clone();
                let condition_handler = Rc::clone(&self.breakpoint_condition_handler);
                condition_button.connect_clicked(move |_| {
                    open_breakpoint_condition_editor(
                        &parent,
                        breakpoint_for_condition.clone(),
                        Rc::clone(&condition_handler),
                    );
                });
                let number = breakpoint.command_number().to_owned();
                let enable = !breakpoint.enabled;
                let enabled_handler = Rc::clone(&self.breakpoint_enabled_handler);
                badge.connect_clicked(move |_| {
                    if let Some(handler) = enabled_handler.borrow().as_ref() {
                        handler(number.clone(), enable);
                    }
                });
                let number = breakpoint.command_number().to_owned();
                let delete_handler = Rc::clone(&self.breakpoint_delete_handler);
                delete_button.connect_clicked(move |_| {
                    if let Some(handler) = delete_handler.borrow().as_ref() {
                        handler(number.clone());
                    }
                });
                self.breakpoints_list.append(&row);
            }
        }
        for (button, signal, description) in &self.signal_buttons {
            if let Some(number) = signal_catchpoint_command_number(&breakpoints, signal) {
                button.add_css_class("signal-caught");
                button.set_tooltip_text(Some(&format!(
                    "{description}\nCatchpoint #{number} is active; click to remove it"
                )));
            } else {
                button.remove_css_class("signal-caught");
                button.set_tooltip_text(Some(&format!(
                    "{description}\nClick to add a GDB signal catchpoint"
                )));
            }
        }
        for (button, event) in &self.event_catchpoint_buttons {
            if let Some(number) = event_catchpoint_command_number(&breakpoints, *event) {
                button.add_css_class("signal-caught");
                button.set_tooltip_text(Some(&format!(
                    "{} catchpoint #{number} is active; click to remove it",
                    event.label()
                )));
            } else {
                button.remove_css_class("signal-caught");
                let description = EventCatchpoint::ALL
                    .iter()
                    .find(|(candidate, _, _)| candidate == event)
                    .map(|(_, _, description)| *description)
                    .unwrap_or("Click to add this catchpoint");
                button.set_tooltip_text(Some(description));
            }
        }
        for document in self.source_documents.borrow().iter() {
            document.breakpoint_renderer.queue_draw();
        }
        drop(breakpoints);
        self.update_control_sensitivity();
    }

    pub fn start_breakpoint_refresh(&self) -> u64 {
        let generation = self.breakpoint_refresh_generation.get().wrapping_add(1);
        self.breakpoint_refresh_generation.set(generation);
        generation
    }

    pub fn begin_breakpoint_refresh(&self) -> Option<u64> {
        self.breakpoint_refresh_gate
            .begin()
            .then(|| self.start_breakpoint_refresh())
    }

    pub fn finish_breakpoint_refresh(&self) -> bool {
        self.breakpoint_refresh_gate.finish()
    }

    pub fn begin_module_refresh(&self) -> bool {
        self.module_refresh_gate.begin()
    }

    pub fn finish_module_refresh(&self) -> bool {
        self.module_refresh_gate.finish()
    }

    pub fn mark_modules_dirty(&self) {
        self.modules_dirty.set(true);
    }

    pub fn take_modules_dirty(&self) -> bool {
        self.modules_dirty.replace(false)
    }

    pub fn show_breakpoints_for_refresh(&self, generation: u64, breakpoints: Vec<Breakpoint>) {
        if self.breakpoint_refresh_generation.get() == generation {
            self.show_breakpoints(breakpoints);
        }
    }

    pub fn set_breakpoint_enabled_pending(&self, number: &str, enabled: bool) -> bool {
        let mut breakpoints = self.breakpoints.borrow().clone();
        let changed = set_breakpoint_enabled(&mut breakpoints, number, enabled);
        if changed {
            self.start_breakpoint_refresh();
            self.breakpoint_refresh_gate.invalidate();
            self.show_breakpoints(breakpoints);
        }
        changed
    }

    pub fn breakpoint_number_at_address(&self, address: &str) -> Option<String> {
        breakpoint_command_number_at_address(&self.breakpoints.borrow(), address)
    }
}
