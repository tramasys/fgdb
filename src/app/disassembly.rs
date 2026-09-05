use super::*;

const MAX_DISASSEMBLY_EXPRESSION_BYTES: usize = 512;
const MAX_DISASSEMBLY_HISTORY: usize = 128;
const SYMBOLLESS_DISASSEMBLY_BYTES: u64 = 256;
const FUNCTION_DISASSEMBLY_BEFORE_BYTES: u64 = 1024;
const FUNCTION_DISASSEMBLY_AFTER_BYTES: u64 = 3072;

#[derive(Default)]
struct DisassemblyState {
    history: Vec<String>,
    history_position: Option<usize>,
    current: Option<String>,
    pc: String,
    architecture: Option<String>,
    mixed: bool,
    range_start: Option<u64>,
    range_end: Option<u64>,
    function: Option<String>,
    syntax_queried: bool,
}

#[derive(Clone, Copy)]
enum HistoryUpdate {
    Reset,
    Push,
    MoveTo(usize),
    Keep,
}

pub(super) struct DisassemblyController {
    model: Rc<crate::model::DebuggerModel>,
    ui: Weak<Ui>,
    client: Rc<MiClient>,
    state: RefCell<DisassemblyState>,
    generation: std::cell::Cell<u64>,
}

impl DisassemblyController {
    pub(super) fn new(
        ui: Weak<Ui>,
        client: Rc<MiClient>,
        model: Rc<crate::model::DebuggerModel>,
    ) -> Rc<Self> {
        Rc::new(Self {
            model,
            ui,
            client,
            state: RefCell::new(DisassemblyState::default()),
            generation: std::cell::Cell::new(0),
        })
    }

    pub(super) fn handle(self: &Rc<Self>, request: DisassemblyRequest) {
        match request {
            DisassemblyRequest::Stopped { pc, architecture } => {
                {
                    let mut state = self.state.borrow_mut();
                    state.pc.clone_from(&pc);
                    state.architecture = architecture;
                }

                self.query_syntax_once();
                self.resolve_and_show(pc, HistoryUpdate::Reset);
            }
            DisassemblyRequest::Clear => {
                self.generation.set(self.generation.get().wrapping_add(1));

                let (mixed, syntax_queried) = {
                    let state = self.state.borrow();

                    (state.mixed, state.syntax_queried)
                };

                *self.state.borrow_mut() = DisassemblyState {
                    mixed,
                    syntax_queried,
                    ..DisassemblyState::default()
                };

                if let Some(ui) = self.ui.upgrade() {
                    ui.set_disassembly_loading(false);
                    ui.set_disassembly_history(false, false);
                }
            }
            DisassemblyRequest::Mixed(mixed) => {
                self.state.borrow_mut().mixed = mixed;

                if self
                    .ui
                    .upgrade()
                    .is_some_and(|ui| ui.disassembly_commands_available())
                    && let Some(current) = self.state.borrow().current.clone()
                {
                    self.resolve_and_show(current, HistoryUpdate::Keep);
                }
            }

            _request
                if !self
                    .ui
                    .upgrade()
                    .is_some_and(|ui| ui.disassembly_commands_available()) => {}
            DisassemblyRequest::Navigate(expression) => {
                self.resolve_and_show(expression, HistoryUpdate::Push);
            }
            DisassemblyRequest::Back => self.move_history(-1),
            DisassemblyRequest::Forward => self.move_history(1),
            DisassemblyRequest::PreviousFunction => self.adjacent_function(false),
            DisassemblyRequest::NextFunction => self.adjacent_function(true),
            DisassemblyRequest::Syntax(syntax) => self.set_syntax(syntax),
        }
    }

    fn query_syntax_once(self: &Rc<Self>) {
        {
            let mut state = self.state.borrow_mut();

            if state.syntax_queried {
                return;
            }

            state.syntax_queried = true;
        }

        let controller = Rc::clone(self);

        if self
            .client
            .request("-gdb-show disassembly-flavor", move |_, record| {
                if !record.is_done() {
                    return;
                }

                let syntax = crate::debugger::evaluated_value(&record)
                    .filter(|value| value.eq_ignore_ascii_case("att"))
                    .map_or(DisassemblySyntax::Intel, |_| DisassemblySyntax::Att);

                if let Some(ui) = controller.ui.upgrade() {
                    ui.set_disassembly_syntax(syntax);
                }
            })
            .is_err()
        {
            self.state.borrow_mut().syntax_queried = false;
        }
    }

    fn set_syntax(self: &Rc<Self>, syntax: DisassemblySyntax) {
        let flavor = match syntax {
            DisassemblySyntax::Intel => "intel",
            DisassemblySyntax::Att => "att",
        };

        let controller = Rc::clone(self);
        let command = format!("-gdb-set disassembly-flavor {flavor}");

        if self
            .client
            .request(&command, move |_, record| {
                let Some(ui) = controller.ui.upgrade() else {
                    return;
                };

                if !record.is_done() {
                    ui.show_disassembly_error(
                        record
                            .error_message()
                            .unwrap_or("GDB rejected the disassembly syntax"),
                    );

                    return;
                }

                ui.set_disassembly_syntax(syntax);

                if let Some(current) = controller.state.borrow().current.clone() {
                    controller.resolve_and_show(current, HistoryUpdate::Keep);
                }
            })
            .is_err()
            && let Some(ui) = self.ui.upgrade()
        {
            ui.show_disassembly_error("The GDB/MI channel is unavailable");
        }
    }

    fn move_history(self: &Rc<Self>, delta: isize) {
        let (target, position) = {
            let state = self.state.borrow();

            let Some(position) = state.history_position else {
                return;
            };

            let Some(position) = position.checked_add_signed(delta) else {
                return;
            };

            let Some(target) = state.history.get(position).cloned() else {
                return;
            };

            (target, position)
        };

        self.resolve_and_show(target, HistoryUpdate::MoveTo(position));
    }

    fn adjacent_function(self: &Rc<Self>, next: bool) {
        const SYMBOL_SCAN_BYTES: u64 = 4096;

        let (start, end, current_function) = {
            let state = self.state.borrow();

            let Some(range_start) = state.range_start else {
                return;
            };

            let Some(range_end) = state.range_end else {
                return;
            };

            let range = if next {
                (range_end, range_end.saturating_add(SYMBOL_SCAN_BYTES))
            } else {
                (range_start.saturating_sub(SYMBOL_SCAN_BYTES), range_start)
            };

            (range.0, range.1, state.function.clone())
        };

        if start >= end {
            return;
        }

        let Some(ui) = self.ui.upgrade() else {
            return;
        };

        if !self.model.stopped_inspection_available() {
            return;
        }

        ui.set_disassembly_loading(true);
        drop(ui);
        let generation = self.generation.get().wrapping_add(1);
        self.generation.set(generation);
        let command = format!("-data-disassemble -s 0x{start:x} -e 0x{end:x} --opcodes bytes -- 0");
        let controller = Rc::clone(self);

        if self
            .client
            .request(&command, move |_, record| {
                if controller.generation.get() != generation {
                    return;
                }

                if !record.is_done() {
                    controller.fail(
                        record
                            .error_message()
                            .unwrap_or("GDB could not scan adjacent symbols"),
                    );

                    return;
                }

                let instructions = crate::debugger::instructions(&record);

                let candidates = instructions.iter().filter(|instruction| {
                    instruction.function != "??"
                        && current_function
                            .as_ref()
                            .is_none_or(|current| instruction.function != *current)
                });

                let candidate = if next {
                    candidates.into_iter().next()
                } else {
                    candidates.into_iter().next_back()
                };

                let Some(candidate) = candidate else {
                    controller.fail("No adjacent function was found within 4 KiB");
                    return;
                };

                controller.resolve_and_show(candidate.address.clone(), HistoryUpdate::Push);
            })
            .is_err()
        {
            self.fail("The GDB/MI channel is unavailable");
        }
    }

    fn resolve_and_show(self: &Rc<Self>, expression: String, history: HistoryUpdate) {
        let expression = expression.trim();

        if let Err(message) = validate_disassembly_expression(expression) {
            if let Some(ui) = self.ui.upgrade() {
                ui.show_disassembly_error(message);
            }

            return;
        }

        let Some(ui) = self.ui.upgrade() else {
            return;
        };

        if !self.model.stopped_inspection_available() {
            return;
        }

        ui.clear_disassembly_error();
        ui.set_disassembly_loading(true);
        let stop_generation = ui.model.current_stop_refresh_generation();
        drop(ui);
        let generation = self.generation.get().wrapping_add(1);
        self.generation.set(generation);

        if let Some(address) = parse_address(expression) {
            self.request_function(address, generation, history);
            return;
        }

        let command = format!(
            "-data-evaluate-expression {}",
            crate::debugger::quote(&format!("(void*)({expression})"))
        );

        let Some(command) = frame_scoped_stop_command(&self.ui, stop_generation, &command) else {
            self.fail("The selected stop context changed");
            return;
        };

        let controller = Rc::clone(self);

        if self
            .client
            .request_for_stop(
                &command,
                stop_generation,
                {
                    let ui = self.ui.clone();

                    move || {
                        ui.upgrade()
                            .is_some_and(|ui| ui.model.is_stop_refresh_current(stop_generation))
                    }
                },
                move |_, record| {
                    if controller.generation.get() != generation {
                        return;
                    }

                    if !record.is_done() {
                        controller.fail(
                            record
                                .error_message()
                                .unwrap_or("GDB could not resolve that location"),
                        );

                        return;
                    }

                    let Some(value) = crate::debugger::evaluated_value(&record) else {
                        controller.fail("GDB returned no address for that location");
                        return;
                    };

                    let Some(address) = evaluated_address(&value) else {
                        controller.fail("The expression does not resolve to an address");
                        return;
                    };

                    controller.request_function(address, generation, history);
                },
            )
            .is_err()
        {
            self.fail("The GDB/MI channel is unavailable");
        }
    }

    fn request_function(self: &Rc<Self>, address: u64, generation: u64, history: HistoryUpdate) {
        let mixed = self.state.borrow().mixed;
        let start = address.saturating_sub(FUNCTION_DISASSEMBLY_BEFORE_BYTES);
        let end = address.saturating_add(FUNCTION_DISASSEMBLY_AFTER_BYTES);

        let command = if mixed {
            format!("-data-disassemble -s 0x{start:x} -e 0x{end:x} --source --opcodes bytes -- 0")
        } else {
            format!("-data-disassemble -s 0x{start:x} -e 0x{end:x} --opcodes bytes -- 0")
        };

        let controller = Rc::clone(self);

        if self
            .client
            .request(&command, move |_, record| {
                if controller.generation.get() != generation {
                    return;
                }

                let Some(ui) = controller.ui.upgrade() else {
                    return;
                };

                if !controller.model.stopped_inspection_available() {
                    return;
                }

                if !record.is_done() {
                    drop(ui);

                    controller.request_address_window(
                        address,
                        generation,
                        history,
                        SYMBOLLESS_DISASSEMBLY_BYTES,
                    );

                    return;
                }

                let instructions =
                    instructions_for_focus(crate::debugger::instructions(&record), address);

                if instructions.is_empty() {
                    drop(ui);

                    controller.request_address_window(
                        address,
                        generation,
                        history,
                        SYMBOLLESS_DISASSEMBLY_BYTES,
                    );

                    return;
                }

                controller.present(address, history, instructions, mixed);
            })
            .is_err()
        {
            self.fail("The GDB/MI channel is unavailable");
        }
    }

    fn request_address_window(
        self: &Rc<Self>,
        address: u64,
        generation: u64,
        history: HistoryUpdate,
        bytes: u64,
    ) {
        let end = address.saturating_add(bytes.max(1));

        let command =
            format!("-data-disassemble -s 0x{address:x} -e 0x{end:x} --opcodes bytes -- 0");

        let controller = Rc::clone(self);

        if self
            .client
            .request(&command, move |_, record| {
                if controller.generation.get() != generation {
                    return;
                }

                let instructions = if record.is_done() {
                    instructions_for_focus(crate::debugger::instructions(&record), address)
                } else {
                    Vec::new()
                };

                if instructions.is_empty() {
                    if bytes > 1 {
                        controller.request_address_window(
                            address,
                            generation,
                            history,
                            bytes.div_ceil(2),
                        );
                    } else {
                        controller.fail(
                            record
                                .error_message()
                                .unwrap_or("GDB cannot read an instruction at that address"),
                        );
                    }

                    return;
                }

                controller.present(address, history, instructions, false);
            })
            .is_err()
        {
            self.fail("The GDB/MI channel is unavailable");
        }
    }

    fn present(
        &self,
        address: u64,
        history: HistoryUpdate,
        instructions: Vec<crate::debugger::Instruction>,
        mixed: bool,
    ) {
        let Some(ui) = self.ui.upgrade() else {
            return;
        };

        if !self.model.stopped_inspection_available() {
            return;
        }

        let focus = format!("0x{address:x}");

        let (pc, architecture) = {
            let state = self.state.borrow();

            (state.pc.clone(), state.architecture.clone())
        };

        self.commit_location(&focus, history, &instructions);
        ui.show_instructions(instructions, &pc, &focus, architecture.as_deref(), mixed);

        if let Some(request) = ui.take_call_abi_target_request() {
            self.resolve_call_abi_target(request);
        }

        ui.clear_disassembly_error();
        ui.set_disassembly_loading(false);
        self.update_history_buttons(&ui);
    }

    fn resolve_call_abi_target(&self, request: CallAbiTargetRequest) {
        if validate_disassembly_expression(&request.expression).is_err() {
            if let Some(ui) = self.ui.upgrade() {
                ui.show_call_abi_target_resolution(&request, None);
            }

            return;
        }

        let command = format!(
            "-data-evaluate-expression {}",
            crate::debugger::quote(&format!("(void*)({})", request.expression))
        );

        let weak_ui_for_guard = self.ui.clone();
        let weak_ui_for_response = self.ui.clone();
        let generation = request.generation;

        let Some(command) = frame_scoped_stop_command(&self.ui, generation, &command) else {
            if let Some(ui) = self.ui.upgrade() {
                ui.show_call_abi_target_resolution(&request, None);
            }

            return;
        };

        let request_for_response = request.clone();

        if self
            .client
            .request_for_stop(
                &command,
                generation,
                move || {
                    weak_ui_for_guard
                        .upgrade()
                        .is_some_and(|ui| ui.model.is_stop_refresh_current(generation))
                },
                move |_, record| {
                    let Some(ui) = weak_ui_for_response.upgrade() else {
                        return;
                    };

                    let display = record
                        .is_done()
                        .then(|| crate::debugger::evaluated_value(&record))
                        .flatten()
                        .as_deref()
                        .and_then(|value| {
                            resolved_call_target_display(&request_for_response.expression, value)
                        });

                    ui.show_call_abi_target_resolution(&request_for_response, display);
                },
            )
            .is_err()
            && let Some(ui) = self.ui.upgrade()
        {
            ui.show_call_abi_target_resolution(&request, None);
        }
    }

    fn commit_location(
        &self,
        location: &str,
        update: HistoryUpdate,
        instructions: &[crate::debugger::Instruction],
    ) {
        let mut state = self.state.borrow_mut();
        state.current = Some(location.to_owned());

        state.range_start = instructions
            .first()
            .and_then(|instruction| parse_address(&instruction.address));

        state.range_end = instructions.last().and_then(instruction_end_address);

        state.function = instructions
            .first()
            .map(|instruction| instruction.function.clone())
            .filter(|function| function != "??");

        match update {
            HistoryUpdate::Reset => {
                state.history.clear();
                state.history.push(location.to_owned());
                state.history_position = Some(0);
            }
            HistoryUpdate::Push => {
                if state
                    .history_position
                    .and_then(|position| state.history.get(position))
                    .is_some_and(|current| current == location)
                {
                    return;
                }

                if let Some(position) = state.history_position {
                    state.history.truncate(position.saturating_add(1));
                } else {
                    state.history.clear();
                }

                state.history.push(location.to_owned());

                if state.history.len() > MAX_DISASSEMBLY_HISTORY {
                    state.history.remove(0);
                }

                state.history_position = state.history.len().checked_sub(1);
            }
            HistoryUpdate::MoveTo(position) => state.history_position = Some(position),
            HistoryUpdate::Keep => {}
        }
    }

    fn update_history_buttons(&self, ui: &Ui) {
        let state = self.state.borrow();
        let position = state.history_position.unwrap_or(0);
        ui.set_disassembly_history(position > 0, position + 1 < state.history.len());
    }

    fn fail(&self, message: &str) {
        if let Some(ui) = self.ui.upgrade() {
            ui.show_disassembly_error(message);
            self.update_history_buttons(&ui);
        }
    }
}

fn validate_disassembly_expression(expression: &str) -> Result<(), &'static str> {
    if expression.is_empty() {
        return Err("Enter an address, expression, function, or symbol");
    }

    if expression.len() > MAX_DISASSEMBLY_EXPRESSION_BYTES {
        return Err("The disassembly expression is too long");
    }

    if expression
        .chars()
        .any(|character| character == '\0' || character == '\r' || character == '\n')
    {
        return Err("The disassembly expression must fit on one line");
    }

    Ok(())
}

fn evaluated_address(value: &str) -> Option<u64> {
    value
        .split(|character: char| {
            character.is_whitespace() || matches!(character, '<' | '>' | '(' | ')' | ',')
        })
        .find_map(parse_address)
}

fn parse_address(value: &str) -> Option<u64> {
    let value = value.trim_matches(|character: char| {
        !character.is_ascii_hexdigit() && !matches!(character, 'x' | 'X')
    });

    value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
        .filter(|digits| {
            !digits.is_empty() && digits.chars().all(|digit| digit.is_ascii_hexdigit())
        })
        .and_then(|digits| u64::from_str_radix(digits, 16).ok())
}

fn instruction_end_address(instruction: &crate::debugger::Instruction) -> Option<u64> {
    let address = parse_address(&instruction.address)?;

    let bytes = instruction
        .opcodes
        .as_deref()
        .map(opcode_byte_count)
        .unwrap_or(1);

    address.checked_add(u64::try_from(bytes).ok()?)
}

fn instructions_for_focus(
    mut instructions: Vec<crate::debugger::Instruction>,
    focus: u64,
) -> Vec<crate::debugger::Instruction> {
    let Some(focus_position) = instructions
        .iter()
        .position(|instruction| parse_address(&instruction.address) == Some(focus))
    else {
        return Vec::new();
    };

    let function = instructions[focus_position].function.as_str();
    let start = instructions[..focus_position]
        .iter()
        .rposition(|instruction| instruction.function != function)
        .map_or(0, |position| position + 1);

    let end = instructions[focus_position + 1..]
        .iter()
        .position(|instruction| instruction.function != function)
        .map_or(instructions.len(), |position| focus_position + position + 1);

    instructions.drain(start..end).collect()
}

fn opcode_byte_count(opcodes: &str) -> usize {
    opcodes
        .split_ascii_whitespace()
        .filter_map(|word| {
            let digits = word
                .strip_prefix("0x")
                .or_else(|| word.strip_prefix("0X"))
                .unwrap_or(word);

            (!digits.is_empty()
                && digits.len().is_multiple_of(2)
                && digits.chars().all(|digit| digit.is_ascii_hexdigit()))
            .then_some(digits.len() / 2)
        })
        .sum::<usize>()
        .max(1)
}

fn resolved_call_target_display(expression: &str, value: &str) -> Option<String> {
    const MAX_RESOLVED_TARGET_BYTES: usize = 512;
    let start = value.find("0x").or_else(|| value.find("0X"))?;
    let value = value.get(start..)?.trim();

    if value.is_empty() || value.len() > MAX_RESOLVED_TARGET_BYTES {
        return None;
    }

    let expression_address = evaluated_address(expression);
    let value_address = evaluated_address(value);
    let has_symbol = value.contains('<') && value.contains('>');

    if !has_symbol && expression_address == value_address {
        return None;
    }

    if expression.trim().starts_with(['$', '%']) {
        Some(format!("{} → {value}", expression.trim()))
    } else {
        Some(value.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn instruction(address: &str, function: &str) -> crate::debugger::Instruction {
        crate::debugger::Instruction {
            address: address.to_owned(),
            function: function.to_owned(),
            offset: String::new(),
            opcodes: Some(String::from("90")),
            text: String::from("nop"),
            source: None,
        }
    }

    #[test]
    fn extracts_addresses_from_gdb_values() {
        assert_eq!(evaluated_address("0x401126 <main>"), Some(0x401126));
        assert_eq!(evaluated_address("0X401126"), Some(0x401126));

        assert_eq!(
            evaluated_address("(void *) 0x7ffff7e12340 <malloc>"),
            Some(0x7fff_f7e1_2340)
        );

        assert_eq!(evaluated_address("void"), None);
    }

    #[test]
    fn rejects_multiline_or_unbounded_expressions() {
        assert!(validate_disassembly_expression("main").is_ok());
        assert!(validate_disassembly_expression("main\nrun").is_err());
        assert!(validate_disassembly_expression(&"x".repeat(513)).is_err());
    }

    #[test]
    fn counts_bytes_for_variable_gdb_opcode_formats() {
        assert_eq!(opcode_byte_count("48 89 e5"), 3);
        assert_eq!(opcode_byte_count("e92d4800"), 4);
        assert_eq!(opcode_byte_count("0x1234 0xabcd"), 4);
        assert_eq!(opcode_byte_count("unavailable"), 1);
    }

    #[test]
    fn keeps_only_the_function_containing_the_requested_instruction() {
        let instructions = vec![
            instruction("0x401000", "_start"),
            instruction("0x401005", "_start"),
            instruction("0x401038", "fill_loop"),
            instruction("0x401039", "fill_loop"),
            instruction("0x40105b", "??"),
            instruction("0x40105d", "??"),
        ];

        let focused = instructions_for_focus(instructions, 0x401039);

        assert_eq!(
            focused
                .iter()
                .map(|instruction| instruction.address.as_str())
                .collect::<Vec<_>>(),
            ["0x401038", "0x401039"]
        );
    }

    #[test]
    fn rejects_disassembly_windows_that_do_not_contain_the_focus() {
        assert!(
            instructions_for_focus(vec![instruction("0x401000", "_start")], 0x402000).is_empty()
        );
    }

    #[test]
    fn formats_resolved_direct_and_register_call_targets() {
        assert_eq!(
            resolved_call_target_display("0x5555555550a0", "(void *) 0x5555555550a0 <malloc@plt>")
                .as_deref(),
            Some("0x5555555550a0 <malloc@plt>")
        );

        assert_eq!(
            resolved_call_target_display("$rax", "0x7ffff7e12340 <malloc>").as_deref(),
            Some("$rax → 0x7ffff7e12340 <malloc>")
        );

        assert_eq!(
            resolved_call_target_display("0x401000", "(void *) 0x401000"),
            None
        );
    }
}
