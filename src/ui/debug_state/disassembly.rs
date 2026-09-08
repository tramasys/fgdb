use super::*;

impl Ui {
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

        self.disassembly_controls.columns.source.set_visible(mixed);

        let syntax_applicable = matches!(
            self.target_architecture(),
            TargetArchitecture::X86 | TargetArchitecture::X86_64
        );

        self.disassembly_controls
            .syntax_applicable
            .set(syntax_applicable);

        self.disassembly_controls
            .syntax_intel
            .set_sensitive(syntax_applicable);

        self.disassembly_controls
            .syntax_att
            .set_sensitive(syntax_applicable);

        let title = architecture.map_or_else(
            || String::from("INSTRUCTIONS"),
            |architecture| format!("INSTRUCTIONS  {architecture}"),
        );

        self.instructions_title.set_text(&title);
        self.instructions_title.set_tooltip_text(Some(&title));

        self.misc_view.cfg.show(
            &instructions,
            pc,
            self.target_architecture(),
            self.target_pointer_bits(),
        );

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
                .set(Some(self.model.current_stop_refresh_generation()));

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

        replace_boxed_store_if_changed(&self.instructions_store, rows);
        let selected = u32::try_from(selected).unwrap_or(0);
        let selection_changed = self.instructions_selection.selected() != selected;

        if selection_changed {
            self.instructions_selection.set_selected(selected);
        }

        self.center_instruction_row(selected, self.instructions_store.n_items());

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
                "{function}  {}-{}  {} instructions",
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

    pub(super) fn center_instruction_row(&self, position: u32, item_count: u32) {
        if item_count == 0 || position >= item_count {
            return;
        }

        self.instructions_view
            .scroll_to(position, None, gtk::ListScrollFlags::FOCUS, None);

        let generation = self
            .disassembly_controls
            .scroll_generation
            .get()
            .wrapping_add(1);

        self.disassembly_controls.scroll_generation.set(generation);
        center_scroll_adjustment(&self.disassembly_controls.scrolled, position, item_count);
        let scrolled = self.disassembly_controls.scrolled.clone();
        let scroll_generation = Rc::clone(&self.disassembly_controls.scroll_generation);

        glib::timeout_add_local_once(Duration::from_millis(16), move || {
            if scroll_generation.get() == generation {
                center_scroll_adjustment(&scrolled, position, item_count);
            }
        });
    }

    pub(super) fn update_instruction_insight(&self) {
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

        let registers = self.model.registers();
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
            .set_text(&format!("MEMORY  {expression}  reading…"));

        self.instruction_memory.set_visible(true);
        let handler = self.instruction_memory_handler.borrow().clone();

        if let Some(handler) = handler {
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
                    "MEMORY  {expression} = 0x{:0width$x}  {}",
                    memory.begin,
                    compact_memory_preview(&memory.bytes)
                )
            }

            Err(error) => format!("MEMORY  {expression}  {error}"),
        };

        self.instruction_memory.set_text(&text);
        self.instruction_memory.set_tooltip_text(Some(&text));
        self.instruction_memory.set_visible(true);
    }

    pub(in crate::ui) fn connect_instruction_activation(&self) {
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

            let handler = handler.borrow().clone();

            if let Some(handler) = handler {
                handler(address);
            }
        });

        let ui = self.clone();

        self.instructions_selection
            .connect_selected_notify(move |_| ui.update_disassembly_selection());
    }

    pub(in crate::ui) fn connect_disassembly_controls(&self) {
        let handler = Rc::clone(&self.disassembly_handler);
        let location = self.disassembly_controls.location.clone();

        self.disassembly_controls.go.connect_clicked(move |_| {
            let expression = location.text().trim().to_owned();
            let handler = handler.borrow().clone();

            if !expression.is_empty()
                && let Some(handler) = handler
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
                let handler = handler.borrow().clone();

                if let Some(handler) = handler {
                    handler(request.clone());
                }
            });
        }

        let handler = Rc::clone(&self.disassembly_handler);

        self.disassembly_controls
            .mixed
            .connect_toggled(move |button| {
                let handler = handler.borrow().clone();

                if let Some(handler) = handler {
                    handler(DisassemblyRequest::Mixed(button.is_active()));
                }
            });

        for (button, syntax) in [
            (
                &self.disassembly_controls.syntax_intel,
                DisassemblySyntax::Intel,
            ),
            (
                &self.disassembly_controls.syntax_att,
                DisassemblySyntax::Att,
            ),
        ] {
            let handler = Rc::clone(&self.disassembly_handler);
            let setting_syntax = Rc::clone(&self.disassembly_controls.setting_syntax);

            button.connect_toggled(move |button| {
                if setting_syntax.get() || !button.is_active() {
                    return;
                }

                let handler = handler.borrow().clone();

                if let Some(handler) = handler {
                    handler(DisassemblyRequest::Syntax(syntax));
                }
            });
        }

        let ui = self.clone();

        self.disassembly_controls.follow.connect_clicked(move |_| {
            let Some(instruction) = ui.selected_instruction() else {
                return;
            };

            let Some(target) = instruction_flow_target(&instruction, ui.target_architecture())
            else {
                return;
            };

            let handler = ui.disassembly_handler.borrow().clone();

            if let Some(handler) = handler {
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

    pub(super) fn selected_instruction(&self) -> Option<Instruction> {
        self.selected_instruction_row().map(|row| row.instruction)
    }

    pub(super) fn selected_instruction_row(&self) -> Option<InstructionRowData> {
        let position = self.instructions_selection.selected();

        self.instructions_store
            .item(position)
            .and_then(|item| item.downcast::<glib::BoxedAnyObject>().ok())
            .map(|item| item.borrow::<InstructionRowData>().clone())
    }

    pub(super) fn update_disassembly_selection(&self) {
        clear_label_selections(&self.instructions_view);
        let row = self.selected_instruction_row();

        if let Some(row) = row.as_ref() {
            self.disassembly_controls
                .location
                .set_text(&row.instruction.address);
        }

        let instruction = row.as_ref().map(|row| &row.instruction);
        let registers = self.model.registers();
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

    pub(super) fn open_selected_instruction_memory(&self) {
        let Some(instruction) = self.selected_instruction() else {
            return;
        };

        let Some(expression) = instruction_memory_expression(
            &instruction,
            &self.model.registers(),
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
            self.panels.reveal(PanelId::Memory);

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

        self.disassembly_controls
            .syntax_intel
            .set_active(syntax == DisassemblySyntax::Intel);

        self.disassembly_controls
            .syntax_att
            .set_active(syntax == DisassemblySyntax::Att);

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
}
