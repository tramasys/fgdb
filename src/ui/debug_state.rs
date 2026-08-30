use super::*;

impl Ui {
    pub fn show_frames(&self, frames: &[StackFrame]) {
        if self.latest_frames.borrow().as_slice() == frames {
            return;
        }
        let can_update_in_place = {
            let latest = self.latest_frames.borrow();
            let buttons = self.frame_buttons.borrow();
            latest.len() == frames.len()
                && buttons.len() == frames.len()
                && latest
                    .iter()
                    .zip(frames)
                    .all(|(previous, current)| previous.level == current.level)
        };
        if can_update_in_place {
            let latest = self.latest_frames.borrow();
            for (((level, button), previous), frame) in self
                .frame_buttons
                .borrow()
                .iter()
                .zip(latest.iter())
                .zip(frames)
            {
                debug_assert_eq!(*level, frame.level);
                if previous != frame {
                    update_frame_button(button, frame);
                }
            }
            drop(latest);
            self.latest_frames.replace(frames.to_vec());
            update_selected_frame_buttons(
                &self.frame_buttons.borrow(),
                self.selected_frame_level.get(),
            );
            return;
        }
        self.latest_frames.replace(frames.to_vec());
        clear_box(&self.call_stack_list);
        self.frame_buttons.borrow_mut().clear();
        if frames.is_empty() {
            self.call_stack_list
                .append(&empty_label("No stack frames available"));
            return;
        }

        for frame in frames {
            let row = gtk::Box::new(gtk::Orientation::Vertical, 0);
            let displayed_function = compact_function_name(&frame.function);
            let function =
                gtk::Label::new(Some(&format!("#{}  {displayed_function}", frame.level)));
            function.set_halign(gtk::Align::Start);
            function.set_ellipsize(pango::EllipsizeMode::End);
            function.set_tooltip_text(Some(&frame.function));
            let location_text = frame_location_text(frame);
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
        let changed = replace_variable_roots_if_changed(&self.locals_store, variables);
        if !changed {
            self.locals_empty.set_visible(variables.is_empty());
            self.locals_edit_button.set_sensitive(!variables.is_empty());
            return;
        }
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

    pub fn local_variable_objects(&self) -> Vec<Variable> {
        root_variables(&self.locals_store)
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
        let target_architecture = Rc::clone(&self.target_architecture);
        let current_source_is_rust = Rc::clone(&self.current_source_is_rust);
        let debugger_ready = Rc::clone(&self.debugger_ready);
        let inferior_started = Rc::clone(&self.inferior_started);
        let inferior_running = Rc::clone(&self.inferior_running);
        let command_pending = Rc::clone(&self.command_pending);
        let session_pending = Rc::clone(&self.session_pending);
        self.locals_view.connect_activate(move |_, position| {
            if !debugger_ready.get()
                || !inferior_started.get()
                || inferior_running.get()
                || command_pending.get()
                || session_pending.get()
            {
                return;
            }
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
                            target_architecture.get(),
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
        let target_architecture = Rc::clone(&self.target_architecture);
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
                        target_architecture.get(),
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
            let target_architecture = Rc::clone(&self.target_architecture);
            let current_source_is_rust = Rc::clone(&self.current_source_is_rust);
            let debugger_ready = Rc::clone(&self.debugger_ready);
            let inferior_started = Rc::clone(&self.inferior_started);
            let inferior_running = Rc::clone(&self.inferior_running);
            let command_pending = Rc::clone(&self.command_pending);
            let session_pending = Rc::clone(&self.session_pending);
            group.view.connect_activate(move |_, position| {
                if !debugger_ready.get()
                    || !inferior_started.get()
                    || inferior_running.get()
                    || command_pending.get()
                    || session_pending.get()
                {
                    return;
                }
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
                        target_architecture.get(),
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
        let executable_name = self
            .current_session
            .borrow()
            .as_ref()
            .and_then(DebugSession::executable)
            .and_then(std::path::Path::file_name)
            .and_then(std::ffi::OsStr::to_str)
            .map(str::to_owned);
        let stop_reason = self.thread_stop_reason.borrow().clone();
        if self.latest_threads.borrow().as_ref().is_some_and(|state| {
            state.threads == threads
                && state.stop_reason == stop_reason
                && state.executable_name == executable_name
        }) {
            return;
        }
        self.kernel_view
            .set_tls_thread(threads, executable_name.as_deref());
        let can_update_in_place = self
            .latest_threads
            .borrow()
            .as_ref()
            .is_some_and(|previous| {
                previous.threads.len() == threads.len()
                    && self.thread_buttons.borrow().len() == threads.len()
                    && previous
                        .threads
                        .iter()
                        .zip(threads)
                        .all(|(previous, current)| previous.id == current.id)
            });
        if can_update_in_place {
            let latest = self.latest_threads.borrow();
            let previous = latest
                .as_ref()
                .expect("in-place thread update requires prior state");
            for (((_, button), old_thread), thread) in self
                .thread_buttons
                .borrow()
                .iter()
                .zip(previous.threads.iter())
                .zip(threads)
            {
                let reason = thread
                    .current
                    .then(|| stop_reason.as_deref().unwrap_or("STOPPED"));
                let old_reason = old_thread
                    .current
                    .then(|| previous.stop_reason.as_deref().unwrap_or("STOPPED"));
                if old_thread != thread || old_reason != reason {
                    update_thread_button(button, thread, reason);
                }
            }
            drop(latest);
            self.latest_threads.replace(Some(ThreadRenderState {
                threads: threads.to_vec(),
                stop_reason,
                executable_name,
            }));
            return;
        }
        self.latest_threads.replace(Some(ThreadRenderState {
            threads: threads.to_vec(),
            stop_reason: stop_reason.clone(),
            executable_name,
        }));
        clear_box(&self.threads_list);
        self.thread_buttons.borrow_mut().clear();
        if threads.is_empty() {
            self.threads_list
                .append(&empty_label("No threads available"));
            return;
        }
        for thread in threads {
            let reason = thread
                .current
                .then(|| stop_reason.as_deref().unwrap_or("STOPPED"));
            let row = thread_button_content(thread, reason);
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
            self.thread_buttons
                .borrow_mut()
                .push((thread.id.clone(), button.clone()));
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
                (Some(from), Some(to)) => format!("{from}-{to}"),
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
        instructions: Vec<Instruction>,
        pc: &str,
        focus: &str,
        architecture: Option<&str>,
        mixed: bool,
    ) {
        if let Some(description) = architecture {
            let detected = TargetArchitecture::from_gdb_description(description);
            if detected != TargetArchitecture::Unknown {
                self.set_target_architecture(detected);
            }
            if let Some(bits) =
                TargetArchitecture::explicit_pointer_bits_from_gdb_description(description)
            {
                self.set_target_pointer_bits(bits);
            }
            if let Some(endian) = TargetEndian::from_architecture_description(description) {
                self.set_target_endian(Some(endian));
            }
        }
        self.disassembly_controls.source_column.set_visible(mixed);
        let syntax_applicable = matches!(
            self.target_architecture(),
            TargetArchitecture::X86 | TargetArchitecture::X86_64
        );
        self.disassembly_controls
            .syntax_applicable
            .set(syntax_applicable);
        self.disassembly_controls
            .syntax
            .set_sensitive(syntax_applicable);
        let title = architecture.map_or_else(
            || String::from("INSTRUCTIONS"),
            |architecture| format!("INSTRUCTIONS · {architecture} · GDB NATIVE"),
        );
        self.instructions_title.set_text(&title);
        self.instructions_title.set_tooltip_text(Some(&title));
        if instructions.is_empty() {
            self.instructions_empty.set_visible(true);
            self.instructions_store.remove_all();
            self.instructions_selection
                .set_selected(gtk::INVALID_LIST_POSITION);
            self.disassembly_controls.range.set_text("");
            self.disassembly_controls
                .previous_function
                .set_sensitive(false);
            self.disassembly_controls.next_function.set_sensitive(false);
            self.disassembly_controls.follow.set_sensitive(false);
            self.disassembly_controls.open_memory.set_sensitive(false);
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
            .position(|instruction| addresses_equal(&instruction.address, pc));
        let selected = instructions
            .iter()
            .position(|instruction| addresses_equal(&instruction.address, focus))
            .or(current)
            .unwrap_or(0);
        let selected_address = instructions[selected].address.clone();
        self.current_instruction
            .replace(current.and_then(|position| instructions.get(position).cloned()));
        if let Some(position) = current {
            self.call_abi_instruction
                .replace(Some(CallAbiInstructionContext {
                    current: instructions[position].clone(),
                    previous: position
                        .checked_sub(1)
                        .and_then(|previous| instructions.get(previous).cloned()),
                    target_resolution: None,
                    pending_target: None,
                }));
            self.call_abi_instruction_generation
                .set(Some(self.current_stop_refresh_generation()));
            self.refresh_call_abi_transfer();
        }
        let rows = instructions
            .into_iter()
            .map(|instruction| InstructionRowData {
                current: addresses_equal(&instruction.address, pc),
                pointer_bits: self.target_pointer_bits(),
                source_text: self.disassembly_source_text(&instruction),
                instruction,
            })
            .collect::<Vec<_>>();
        let rows_changed = replace_boxed_store_if_changed(&self.instructions_store, rows);
        let selected = u32::try_from(selected).unwrap_or(0);
        if self.instructions_selection.selected() != selected {
            self.instructions_selection.set_selected(selected);
        }
        if rows_changed {
            self.instructions_view
                .scroll_to(selected, None, gtk::ListScrollFlags::FOCUS, None);
        }
        if let (Some(first), Some(last)) = (
            self.instructions_store
                .item(0)
                .and_downcast::<glib::BoxedAnyObject>(),
            self.instructions_store
                .item(self.instructions_store.n_items().saturating_sub(1))
                .and_downcast::<glib::BoxedAnyObject>(),
        ) {
            let first = first.borrow::<InstructionRowData>();
            let last = last.borrow::<InstructionRowData>();
            let function = if first.instruction.function == "??" {
                "unknown function"
            } else {
                first.instruction.function.as_str()
            };
            let range = format!(
                "{function} · {}-{} · {} instructions",
                full_address(&first.instruction.address, self.target_pointer_bits()),
                full_address(&last.instruction.address, self.target_pointer_bits()),
                self.instructions_store.n_items()
            );
            self.disassembly_controls.range.set_text(&range);
            self.disassembly_controls
                .range
                .set_tooltip_text(Some(&range));
            self.disassembly_controls
                .location
                .set_text(&selected_address);
        }
        self.disassembly_controls
            .previous_function
            .set_sensitive(true);
        self.disassembly_controls.next_function.set_sensitive(true);
        self.update_disassembly_selection();
        self.update_instruction_insight();
        self.update_control_sensitivity();
    }

    fn disassembly_source_text(&self, instruction: &Instruction) -> Option<Rc<str>> {
        const MAX_SOURCE_FILES: usize = 8;
        const MAX_SOURCE_BYTES: usize = 2 * 1024 * 1024;

        let source = instruction.source.as_ref()?;
        let path = self.resolve_source_path(source.source_path())?;
        let lines = if let Some(lines) = self.disassembly_source_cache.borrow().get(&path).cloned()
        {
            lines
        } else {
            let contents = crate::bounded::read_string(&path, MAX_SOURCE_BYTES).ok()?;
            let lines = Rc::new(contents.lines().map(Rc::<str>::from).collect::<Vec<_>>());
            let mut cache = self.disassembly_source_cache.borrow_mut();
            if cache.len() >= MAX_SOURCE_FILES {
                cache.clear();
            }
            cache.insert(path, Rc::clone(&lines));
            lines
        };
        let index = usize::try_from(source.line).ok()?.checked_sub(1)?;
        lines.get(index).cloned()
    }

    fn update_instruction_insight(&self) {
        let Some(instruction) = self.current_instruction.borrow().clone() else {
            self.instruction_flow
                .set_text("Flow information appears at the current branch or call");
            self.instruction_flow.set_tooltip_text(None);
            self.instruction_flow.remove_css_class("branch-taken");
            self.instruction_flow.remove_css_class("branch-not-taken");
            self.instruction_arguments.set_visible(false);
            self.instruction_memory.set_visible(false);
            self.current_instruction_memory_expression.replace(None);
            return;
        };
        let registers = self.latest_registers.borrow();
        let architecture = self.target_architecture();
        let branch_taken = conditional_branch_taken(&instruction, &registers, architecture);
        let flow = instruction_flow_description(&instruction, &registers, architecture);
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

        let arguments = instruction_arguments_description(&instruction, &registers, architecture);
        self.instruction_arguments
            .set_visible(!arguments.is_empty());
        self.instruction_arguments.set_text(&arguments);
        self.instruction_arguments
            .set_tooltip_text((!arguments.is_empty()).then_some(arguments.as_str()));

        let expression = instruction_memory_expression(&instruction, &registers, architecture);
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
            Ok(memory) => {
                let width = usize::try_from(self.target_pointer_bits() / 4)
                    .unwrap_or(16)
                    .clamp(8, 16);
                format!(
                    "MEM  {expression} = 0x{:0width$x}  {}",
                    memory.begin,
                    compact_memory_preview(&memory.bytes)
                )
            }
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
        let ui = self.clone();
        self.instructions_selection
            .connect_selected_notify(move |_| ui.update_disassembly_selection());
    }

    pub(super) fn connect_disassembly_controls(&self) {
        let handler = Rc::clone(&self.disassembly_handler);
        let location = self.disassembly_controls.location.clone();
        self.disassembly_controls.go.connect_clicked(move |_| {
            let expression = location.text().trim().to_owned();
            if !expression.is_empty()
                && let Some(handler) = handler.borrow().as_ref()
            {
                handler(DisassemblyRequest::Navigate(expression));
            }
        });
        let go = self.disassembly_controls.go.clone();
        self.disassembly_controls
            .location
            .connect_activate(move |_| {
                if go.is_sensitive() {
                    go.emit_clicked();
                }
            });
        for (button, request) in [
            (&self.disassembly_controls.back, DisassemblyRequest::Back),
            (
                &self.disassembly_controls.forward,
                DisassemblyRequest::Forward,
            ),
            (
                &self.disassembly_controls.previous_function,
                DisassemblyRequest::PreviousFunction,
            ),
            (
                &self.disassembly_controls.next_function,
                DisassemblyRequest::NextFunction,
            ),
            (
                &self.disassembly_controls.current_pc,
                DisassemblyRequest::Navigate(String::from("$pc")),
            ),
        ] {
            let handler = Rc::clone(&self.disassembly_handler);
            button.connect_clicked(move |_| {
                if let Some(handler) = handler.borrow().as_ref() {
                    handler(request.clone());
                }
            });
        }
        let handler = Rc::clone(&self.disassembly_handler);
        self.disassembly_controls
            .mixed
            .connect_toggled(move |button| {
                if let Some(handler) = handler.borrow().as_ref() {
                    handler(DisassemblyRequest::Mixed(button.is_active()));
                }
            });
        let handler = Rc::clone(&self.disassembly_handler);
        let setting_syntax = Rc::clone(&self.disassembly_controls.setting_syntax);
        self.disassembly_controls
            .syntax
            .connect_selected_notify(move |syntax| {
                if setting_syntax.get() {
                    return;
                }
                let syntax = if syntax.selected() == 0 {
                    DisassemblySyntax::Intel
                } else {
                    DisassemblySyntax::Att
                };
                if let Some(handler) = handler.borrow().as_ref() {
                    handler(DisassemblyRequest::Syntax(syntax));
                }
            });
        let ui = self.clone();
        self.disassembly_controls.follow.connect_clicked(move |_| {
            let Some(instruction) = ui.selected_instruction() else {
                return;
            };
            let Some(target) = instruction_flow_target(&instruction, ui.target_architecture())
            else {
                return;
            };
            if let Some(handler) = ui.disassembly_handler.borrow().as_ref() {
                handler(DisassemblyRequest::Navigate(target));
            }
        });
        let ui = self.clone();
        self.disassembly_controls
            .open_memory
            .connect_clicked(move |_| {
                if ui.disassembly_commands_available() {
                    ui.open_selected_instruction_memory();
                }
            });
        self.disassembly_controls.back.set_sensitive(false);
        self.disassembly_controls.forward.set_sensitive(false);
        self.disassembly_controls
            .previous_function
            .set_sensitive(false);
        self.disassembly_controls.next_function.set_sensitive(false);
        self.disassembly_controls.follow.set_sensitive(false);
        self.disassembly_controls.open_memory.set_sensitive(false);
    }

    fn selected_instruction(&self) -> Option<Instruction> {
        self.selected_instruction_row().map(|row| row.instruction)
    }

    fn selected_instruction_row(&self) -> Option<InstructionRowData> {
        let position = self.instructions_selection.selected();
        self.instructions_store
            .item(position)
            .and_then(|item| item.downcast::<glib::BoxedAnyObject>().ok())
            .map(|item| item.borrow::<InstructionRowData>().clone())
    }

    fn update_disassembly_selection(&self) {
        clear_label_selections(&self.instructions_view);
        let row = self.selected_instruction_row();
        if let Some(row) = row.as_ref() {
            self.disassembly_controls
                .location
                .set_text(&row.instruction.address);
        }
        let instruction = row.as_ref().map(|row| &row.instruction);
        let registers = self.latest_registers.borrow();
        let architecture = self.target_architecture();
        self.disassembly_controls.follow.set_sensitive(
            instruction
                .as_ref()
                .and_then(|instruction| instruction_flow_target(instruction, architecture))
                .is_some(),
        );
        self.disassembly_controls.open_memory.set_sensitive(
            instruction
                .as_ref()
                .and_then(|instruction| {
                    instruction_memory_expression(instruction, &registers, architecture)
                })
                .is_some(),
        );
    }

    fn open_selected_instruction_memory(&self) {
        let Some(instruction) = self.selected_instruction() else {
            return;
        };
        let Some(expression) = instruction_memory_expression(
            &instruction,
            &self.latest_registers.borrow(),
            self.target_architecture(),
        ) else {
            return;
        };
        if add_memory_watch(
            &self.memory_watch_container,
            &self.memory_watches,
            &self.memory_watch_handler,
            expression.clone(),
            128,
            MemoryWatchFormat::Bytes,
        ) {
            self.inspector_notebook.set_current_page(Some(4));
            self.set_status(
                "Memory",
                &format!("Opened effective address {expression}"),
                Some("status-ready"),
            );
        } else {
            self.set_status(
                "Memory watch limit",
                "Remove a memory watch before adding another (limit 256)",
                Some("status-error"),
            );
        }
    }

    pub(crate) fn set_disassembly_loading(&self, loading: bool) {
        self.disassembly_controls.loading.set(loading);
    }

    pub(crate) fn set_disassembly_history(&self, can_back: bool, can_forward: bool) {
        self.disassembly_controls.back.set_sensitive(can_back);
        self.disassembly_controls.forward.set_sensitive(can_forward);
    }

    pub(crate) fn set_disassembly_syntax(&self, syntax: DisassemblySyntax) {
        self.disassembly_controls.setting_syntax.set(true);
        self.disassembly_controls.syntax.set_selected(match syntax {
            DisassemblySyntax::Intel => 0,
            DisassemblySyntax::Att => 1,
        });
        self.disassembly_controls.setting_syntax.set(false);
    }

    pub(crate) fn show_disassembly_error(&self, message: &str) {
        self.set_disassembly_loading(false);
        self.disassembly_controls
            .location
            .add_css_class("input-error");
        self.set_status("Disassembly failed", message, Some("status-error"));
    }

    pub(crate) fn clear_disassembly_error(&self) {
        self.disassembly_controls
            .location
            .remove_css_class("input-error");
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

    pub fn show_registers(&self, registers: &[Register]) -> bool {
        self.latest_registers_generation.set(None);
        if registers.is_empty() {
            for group in &self.register_groups {
                if group.store.n_items() != 0 {
                    group.store.remove_all();
                }
                if group.panel.is_visible() {
                    group.panel.set_visible(false);
                }
            }
            if !self.registers_empty.is_visible() {
                self.registers_empty.set_visible(true);
            }
        } else {
            if self.registers_empty.is_visible() {
                self.registers_empty.set_visible(false);
            }
            let architecture = self.target_architecture();
            let endian = self.target_endian();
            let pointer_bits = self.target_pointer_bits();
            let previous = self.previous_registers.borrow();
            let ring = registers
                .iter()
                .find(|register| register.name == "cs")
                .and_then(|register| hex_value(&register.value))
                .map(|value| value & 0x3);
            for group in self.register_groups.iter() {
                let grouped = registers.iter().filter(|register| {
                    register_in_group(group.kind, &register.name, architecture)
                        && (group.kind != RegisterGroupKind::Other
                            || !self.register_groups.iter().any(|candidate| {
                                candidate.kind != RegisterGroupKind::Other
                                    && register_in_group(
                                        candidate.kind,
                                        &register.name,
                                        architecture,
                                    )
                            }))
                });
                populate_register_group(
                    group,
                    grouped,
                    &previous,
                    ring,
                    architecture,
                    endian,
                    pointer_bits,
                );
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
        values_changed
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
        self.call_abi_instruction.replace(None);
        self.call_abi_instruction_generation.set(None);
        self.misc_view.show_call_abi_pending();
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
            let first_for_generation = self.latest_registers_generation.get() != Some(generation);
            let values_changed = self.show_registers(registers);
            self.latest_registers_generation.set(Some(generation));
            if first_for_generation || values_changed {
                self.refresh_call_abi_transfer();
            }
        }
    }

    pub(crate) fn registers_for_details(&self, generation: u64) -> Option<Vec<Register>> {
        (self.latest_registers_generation.get() == Some(generation))
            .then(|| self.latest_registers.borrow().clone())
            .filter(|registers| !registers.is_empty())
    }

    pub(crate) fn claim_register_details(&self, generation: u64) -> bool {
        self.is_stop_refresh_current(generation)
            && self.register_details_generation.replace(Some(generation)) != Some(generation)
    }

    pub fn show_stack(&self, entries: &[StackEntry]) {
        self.latest_stack_generation.set(None);
        if self.latest_stack.borrow().as_slice() != entries {
            self.latest_stack.replace(entries.to_vec());
            replace_boxed_store(&self.stack_store, entries.iter().cloned());
        }
        if entries.is_empty() {
            self.stack_empty
                .set_text("Stack values appear when the target is paused");
            self.stack_empty.set_visible(true);
            return;
        }
        self.stack_empty.set_visible(false);
    }

    pub fn show_stack_for_refresh(&self, generation: u64, entries: &[StackEntry]) {
        if self.is_stop_refresh_current(generation) {
            self.show_stack(entries);
            self.latest_stack_generation.set(Some(generation));
        }
    }

    pub(crate) fn stack_for_details(&self, generation: u64) -> Option<Vec<StackEntry>> {
        (self.latest_stack_generation.get() == Some(generation))
            .then(|| self.latest_stack.borrow().clone())
            .filter(|entries| !entries.is_empty())
    }

    pub(crate) fn claim_stack_details(&self, generation: u64) -> bool {
        self.is_stop_refresh_current(generation)
            && self.stack_details_generation.replace(Some(generation)) != Some(generation)
    }

    pub fn show_stack_unavailable_for_refresh(&self, generation: u64, reason: &str) {
        if !self.is_stop_refresh_current(generation) {
            return;
        }
        self.latest_stack.replace(Vec::new());
        self.latest_stack_generation.set(Some(generation));
        self.stack_store.remove_all();
        self.stack_empty.set_text(reason);
        self.stack_empty.set_visible(true);
    }

    pub fn show_memory_regions_for_refresh(&self, generation: u64, regions: &[MemoryRegion]) {
        if !self.is_stop_refresh_current(generation) {
            return;
        }
        if self.memory_regions.borrow().as_slice() != regions {
            replace_boxed_store(&self.memory_region_store, regions.iter().cloned());
            self.memory_regions.replace(regions.to_vec());
        }
        self.memory_regions_empty.set_visible(regions.is_empty());
    }

    pub(super) fn connect_memory_controls(&self) {
        let container = self.memory_watch_container.clone();
        let watches = Rc::clone(&self.memory_watches);
        let handler = Rc::clone(&self.memory_watch_handler);
        let expression = self.memory_address_entry.clone();
        let size = self.memory_size.clone();
        let format = self.memory_format.clone();
        let status_label = self.status_label.clone();
        let status_detail = self.status_detail.clone();
        self.memory_add_button.connect_clicked(move |_| {
            let expression_text = expression.text().trim().to_owned();
            if expression_text.is_empty() {
                return;
            }
            let byte_count = usize::try_from(size.value_as_int()).unwrap_or(128);
            let format = match format.selected() {
                1 => MemoryWatchFormat::U16,
                2 => MemoryWatchFormat::U32,
                3 => MemoryWatchFormat::U64,
                4 => MemoryWatchFormat::F32,
                5 => MemoryWatchFormat::F64,
                6 => MemoryWatchFormat::Pointers,
                _ => MemoryWatchFormat::Bytes,
            };
            let added = add_memory_watch(
                &container,
                &watches,
                &handler,
                expression_text,
                byte_count,
                format,
            );
            if added {
                expression.set_text("");
            } else {
                set_status_widgets(
                    &status_label,
                    &status_detail,
                    "Memory watch limit",
                    "Remove a memory watch before adding another (limit 256)",
                    Some("status-error"),
                );
            }
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

        let ui = self.clone();
        self.memory_watch_container
            .refresh_all
            .connect_clicked(move |_| ui.refresh_memory_watches());

        let container = self.memory_watch_container.clone();
        let watches = Rc::clone(&self.memory_watches);
        self.memory_watch_container
            .clear_all
            .connect_clicked(move |_| clear_memory_watches(&container, &watches));

        self.memory_regions_view.set_single_click_activate(false);
        let container = self.memory_watch_container.clone();
        let watches = Rc::clone(&self.memory_watches);
        let handler = Rc::clone(&self.memory_watch_handler);
        let size = self.memory_size.clone();
        self.memory_regions_view
            .connect_activate(move |view, position| {
                let Some(region) = view
                    .model()
                    .and_then(|model| model.item(position))
                    .and_downcast::<glib::BoxedAnyObject>()
                else {
                    return;
                };
                let region = region.borrow::<MemoryRegion>();
                let requested = usize::try_from(size.value_as_int()).unwrap_or(128);
                let region_size =
                    usize::try_from(region.end.saturating_sub(region.start)).unwrap_or(usize::MAX);
                let byte_count = requested.min(region_size).max(1);
                let _ = add_memory_watch(
                    &container,
                    &watches,
                    &handler,
                    format!("0x{:x}", region.start),
                    byte_count,
                    MemoryWatchFormat::Bytes,
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
        let parent = self.window.clone();
        let handler = Rc::clone(&self.breakpoint_editor_handler);
        self.add_breakpoint_button.connect_clicked(move |_| {
            open_breakpoint_editor(&parent, None, Rc::clone(&handler));
        });
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
        let requests = {
            let watches = self.memory_watches.borrow();
            if watches.is_empty() {
                return;
            }
            update_memory_container_state(&self.memory_watch_container, true);
            watches
                .iter()
                .map(|watch| {
                    set_memory_watch_reading(watch);
                    (
                        watch.id,
                        memory_watch_request_expression(watch),
                        watch.byte_count,
                    )
                })
                .collect::<Vec<_>>()
        };
        if let Some(handler) = self.memory_watch_handler.borrow().as_ref() {
            for (id, expression, byte_count) in requests {
                handler(id, expression, byte_count);
            }
        }
    }

    pub fn show_memory_watch(&self, id: u64, result: Result<MemoryBlock, &str>) {
        let watch = self
            .memory_watches
            .borrow()
            .iter()
            .find(|watch| watch.id == id)
            .cloned();
        let Some(watch) = watch else {
            return;
        };
        match result {
            Ok(memory) => {
                let endian = self.target_endian().or_else(|| {
                    (watch.format == MemoryWatchFormat::Bytes).then_some(TargetEndian::Little)
                });
                let Some(endian) = endian else {
                    show_memory_watch_error(
                        &watch,
                        "Target byte order is unavailable. Use Hex bytes display",
                    );
                    update_memory_container_state(&self.memory_watch_container, false);
                    return;
                };
                show_memory_watch_data(
                    &watch,
                    memory,
                    &self.memory_regions.borrow(),
                    self.target_pointer_bits.get(),
                    endian,
                );
            }
            Err(error) => show_memory_watch_error(&watch, error),
        }
        update_memory_container_state(&self.memory_watch_container, false);
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
            let mut locations_by_parent: HashMap<&str, Vec<&Breakpoint>> = HashMap::new();
            for location in breakpoints
                .iter()
                .filter(|breakpoint| breakpoint.is_location())
            {
                if let Some(parent) = location.parent_number.as_deref() {
                    locations_by_parent
                        .entry(parent)
                        .or_default()
                        .push(location);
                }
            }
            for breakpoint in breakpoints
                .iter()
                .filter(|breakpoint| !breakpoint.is_location())
            {
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
                    _ if breakpoint.location_count > 0 => format!(
                        "{} resolved location{}",
                        breakpoint.location_count,
                        if breakpoint.location_count == 1 {
                            ""
                        } else {
                            "s"
                        }
                    ),
                    _ if let Some(pending) = breakpoint.pending.as_deref() => {
                        format!("pending · {pending}")
                    }
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
                if breakpoint.pending.is_some() {
                    row.add_css_class("breakpoint-row-pending");
                }
                let heading_row = gtk::Box::new(gtk::Orientation::Horizontal, 3);
                let kind = if breakpoint.is_logpoint() {
                    String::from("LOGPOINT")
                } else if breakpoint.is_watchpoint() || breakpoint.is_catchpoint() {
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
                let condition_button = gtk::Button::with_label(
                    if breakpoint.is_watchpoint() || breakpoint.is_catchpoint() {
                        if breakpoint.condition.is_some() {
                            "Edit condition"
                        } else {
                            "Condition"
                        }
                    } else {
                        "Edit"
                    },
                );
                condition_button.add_css_class("inline-action");
                condition_button.set_tooltip_text(Some(
                    if breakpoint.is_watchpoint() || breakpoint.is_catchpoint() {
                        "Add, edit, or clear a GDB condition"
                    } else {
                        "Edit location, behavior, restrictions, commands, or logpoint settings"
                    },
                ));
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
                if let Some(inferior) = breakpoint.inferior.as_deref() {
                    metadata.push(format!("INFERIOR {inferior}"));
                }
                if breakpoint.ignore_count > 0 {
                    metadata.push(format!(
                        "STOP ON HIT {}",
                        breakpoint.ignore_count.saturating_add(1)
                    ));
                }
                if breakpoint.disposition.as_deref() == Some("del") {
                    metadata.push(String::from("TEMPORARY"));
                }
                if breakpoint.pending.is_some() {
                    metadata.push(String::from("PENDING"));
                }
                if breakpoint.location_count > 0 {
                    metadata.push(format!(
                        "{} LOCATION{}",
                        breakpoint.location_count,
                        if breakpoint.location_count == 1 {
                            ""
                        } else {
                            "S"
                        }
                    ));
                }
                if breakpoint.is_logpoint() {
                    metadata.push(String::from("AUTO-CONTINUE"));
                } else if !breakpoint.commands.is_empty() {
                    metadata.push(format!(
                        "{} COMMAND{}",
                        breakpoint.commands.len(),
                        if breakpoint.commands.len() == 1 {
                            ""
                        } else {
                            "S"
                        }
                    ));
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
                if !breakpoint.commands.is_empty() {
                    let command_text = breakpoint
                        .commands
                        .iter()
                        .map(|command| command.trim())
                        .collect::<Vec<_>>()
                        .join("  ·  ");
                    let commands = gtk::Label::new(Some(&format!("DO  {command_text}")));
                    commands.add_css_class("breakpoint-commands");
                    commands.set_halign(gtk::Align::Start);
                    commands.set_ellipsize(pango::EllipsizeMode::End);
                    commands.set_tooltip_text(Some(&command_text));
                    enable_stable_text_selection(&commands);
                    row.append(&commands);
                }

                let parent = self.window.clone();
                let breakpoint_for_condition = breakpoint.clone();
                let condition_handler = Rc::clone(&self.breakpoint_condition_handler);
                let editor_handler = Rc::clone(&self.breakpoint_editor_handler);
                condition_button.connect_clicked(move |_| {
                    if breakpoint_for_condition.is_watchpoint()
                        || breakpoint_for_condition.is_catchpoint()
                    {
                        open_breakpoint_condition_editor(
                            &parent,
                            breakpoint_for_condition.clone(),
                            Rc::clone(&condition_handler),
                        );
                    } else {
                        open_breakpoint_editor(
                            &parent,
                            Some(breakpoint_for_condition.clone()),
                            Rc::clone(&editor_handler),
                        );
                    }
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

                for location in locations_by_parent
                    .get(breakpoint.number.as_str())
                    .into_iter()
                    .flatten()
                {
                    let location_row = gtk::Box::new(gtk::Orientation::Horizontal, 4);
                    location_row.add_css_class("breakpoint-location-row");
                    if !location.enabled {
                        location_row.add_css_class("breakpoint-row-disabled");
                    }
                    let badge = gtk::Button::with_label(&format!("#{}", location.number));
                    badge.add_css_class("breakpoint-badge");
                    badge.add_css_class("breakpoint-location-badge");
                    badge.set_focus_on_click(false);
                    badge.add_css_class(if location.enabled {
                        "breakpoint-badge-enabled"
                    } else {
                        "breakpoint-badge-disabled"
                    });
                    badge.set_tooltip_text(Some(if location.enabled {
                        "Disable only this resolved location"
                    } else {
                        "Enable only this resolved location"
                    }));
                    let details = gtk::Box::new(gtk::Orientation::Vertical, 0);
                    details.set_hexpand(true);
                    let function_text = location.function.as_deref().unwrap_or("resolved location");
                    let function = gtk::Label::new(Some(&compact_function_name(function_text)));
                    function.set_halign(gtk::Align::Start);
                    function.set_ellipsize(pango::EllipsizeMode::End);
                    function.set_tooltip_text(Some(function_text));
                    let source = match (location.source_path(), location.line) {
                        (Some(path), Some(line)) => format!(
                            "{path}:{line}  ·  {}",
                            location.address.as_deref().unwrap_or("resolved")
                        ),
                        _ => location
                            .address
                            .clone()
                            .unwrap_or_else(|| String::from("resolved")),
                    };
                    let source_label = gtk::Label::new(Some(&source));
                    source_label.add_css_class("muted");
                    source_label.set_halign(gtk::Align::Start);
                    source_label.set_ellipsize(pango::EllipsizeMode::Middle);
                    source_label.set_tooltip_text(Some(&source));
                    enable_stable_text_selection(&source_label);
                    details.append(&function);
                    details.append(&source_label);
                    location_row.append(&badge);
                    location_row.append(&details);
                    let number = location.number.clone();
                    let enable = !location.enabled;
                    let enabled_handler = Rc::clone(&self.breakpoint_enabled_handler);
                    badge.connect_clicked(move |_| {
                        if let Some(handler) = enabled_handler.borrow().as_ref() {
                            handler(number.clone(), enable);
                        }
                    });
                    self.breakpoints_list.append(&location_row);
                }
            }
        }
        for (button, signal, description) in &self.signal_buttons {
            if let Some(number) = signal_catchpoint_command_number(&breakpoints, signal) {
                button.add_css_class("signal-caught");
                button.set_tooltip_text(Some(&format!(
                    "{description}\nCatchpoint #{number} is active. Click to remove it"
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
                    "{} catchpoint #{number} is active. Click to remove it",
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

fn frame_location_text(frame: &StackFrame) -> String {
    frame.line.map_or_else(
        || frame.address.clone(),
        |line| {
            format!(
                "{}:{line}",
                frame.source_path().unwrap_or(frame.address.as_str())
            )
        },
    )
}

fn update_frame_button(button: &gtk::Button, frame: &StackFrame) {
    let Some(row) = button.child().and_downcast::<gtk::Box>() else {
        return;
    };
    let Some(function) = row.first_child().and_downcast::<gtk::Label>() else {
        return;
    };
    let Some(location) = row.last_child().and_downcast::<gtk::Label>() else {
        return;
    };
    function.set_text(&format!(
        "#{}  {}",
        frame.level,
        compact_function_name(&frame.function)
    ));
    function.set_tooltip_text(Some(&frame.function));
    let location_text = frame_location_text(frame);
    location.set_text(&location_text);
    location.set_tooltip_text(Some(&location_text));
}

fn thread_button_content(thread: &ThreadInfo, stop_reason: Option<&str>) -> gtk::Box {
    let marker = if thread.current { "*" } else { " " };
    let tid = thread_os_id(&thread.target_id).unwrap_or_else(|| String::from("?"));
    let name = thread.name.as_deref().unwrap_or("<unnamed>");
    let detail = thread_detail(thread, stop_reason);
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
    let detail_widget = thread_detail_widget(thread, stop_reason);
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
    row
}

fn update_thread_button(button: &gtk::Button, thread: &ThreadInfo, stop_reason: Option<&str>) {
    button.set_child(Some(&thread_button_content(thread, stop_reason)));
    if thread.current {
        button.add_css_class("current-debug-item");
    } else {
        button.remove_css_class("current-debug-item");
    }
}
