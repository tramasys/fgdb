use super::*;

const MAX_DISASSEMBLY_EXPRESSION_BYTES: usize = 512;
const MAX_DISASSEMBLY_HISTORY: usize = 128;
const SYMBOLLESS_DISASSEMBLY_BYTES: u64 = 256;

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
    ui: Weak<Ui>,
    client: Rc<MiClient>,
    state: RefCell<DisassemblyState>,
    generation: std::cell::Cell<u64>,
}

impl DisassemblyController {
    pub(super) fn new(ui: Weak<Ui>, client: Rc<MiClient>) -> Rc<Self> {
        Rc::new(Self {
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
            DisassemblyRequest::Navigate(expression) => {
                self.resolve_and_show(expression, HistoryUpdate::Push);
            }
            DisassemblyRequest::Back => self.move_history(-1),
            DisassemblyRequest::Forward => self.move_history(1),
            DisassemblyRequest::PreviousFunction => self.adjacent_function(false),
            DisassemblyRequest::NextFunction => self.adjacent_function(true),
            DisassemblyRequest::Mixed(mixed) => {
                self.state.borrow_mut().mixed = mixed;
                if let Some(current) = self.state.borrow().current.clone() {
                    self.resolve_and_show(current, HistoryUpdate::Keep);
                }
            }
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
        if ui.inferior_is_running() {
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
        if ui.inferior_is_running() {
            return;
        }
        ui.clear_disassembly_error();
        ui.set_disassembly_loading(true);
        drop(ui);

        let generation = self.generation.get().wrapping_add(1);
        self.generation.set(generation);
        let command = format!(
            "-data-evaluate-expression {}",
            crate::debugger::quote(&format!("(void*)({expression})"))
        );
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
            })
            .is_err()
        {
            self.fail("The GDB/MI channel is unavailable");
        }
    }

    fn request_function(self: &Rc<Self>, address: u64, generation: u64, history: HistoryUpdate) {
        let mixed = self.state.borrow().mixed;
        let command = if mixed {
            format!("-data-disassemble -a 0x{address:x} --source --opcodes bytes -- 0")
        } else {
            format!("-data-disassemble -a 0x{address:x} --opcodes bytes -- 0")
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
                if ui.inferior_is_running() {
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
                let instructions = crate::debugger::instructions(&record);
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
                    crate::debugger::instructions(&record)
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
        if ui.inferior_is_running() {
            return;
        }
        let focus = format!("0x{address:x}");
        let (pc, architecture) = {
            let state = self.state.borrow();
            (state.pc.clone(), state.architecture.clone())
        };
        self.commit_location(&focus, history, &instructions);
        ui.show_instructions(&instructions, &pc, &focus, architecture.as_deref(), mixed);
        ui.clear_disassembly_error();
        ui.set_disassembly_loading(false);
        self.update_history_buttons(&ui);
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
