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
    mixed_refresh_pending: bool,
    range_start: Option<u64>,
    range_end: Option<u64>,
    function: Option<String>,
    syntax_epoch: Option<u64>,
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
    syntax_revision: Cell<u64>,
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
            syntax_revision: Cell::new(0),
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

                let (mixed, syntax_epoch) = {
                    let state = self.state.borrow();

                    (state.mixed, state.syntax_epoch)
                };

                *self.state.borrow_mut() = DisassemblyState {
                    mixed,
                    syntax_epoch,
                    ..DisassemblyState::default()
                };

                if let Some(ui) = self.ui.upgrade() {
                    ui.set_disassembly_loading(false);
                    ui.set_disassembly_history(false, false);
                }
            }
            DisassemblyRequest::Mixed(mixed) => {
                {
                    let mut state = self.state.borrow_mut();

                    if state.mixed == mixed {
                        return;
                    }

                    state.mixed = mixed;
                    state.mixed_refresh_pending = true;
                }

                let current = self.state.borrow().current.clone();

                if self
                    .ui
                    .upgrade()
                    .is_some_and(|ui| ui.disassembly_commands_available())
                    && let Some(current) = current
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
        use crate::config::settings::AssemblySyntax;

        let epoch = self.client.transport_epoch();

        {
            let mut state = self.state.borrow_mut();

            if state.syntax_epoch == Some(epoch) {
                return;
            }

            state.syntax_epoch = Some(epoch);
        }

        let preferred = self.ui.upgrade().and_then(|ui| {
            let architecture = self
                .state
                .borrow()
                .architecture
                .as_deref()
                .map(TargetArchitecture::from_gdb_description)
                .unwrap_or_else(|| ui.target_architecture());

            if !matches!(
                architecture,
                TargetArchitecture::X86 | TargetArchitecture::X86_64
            ) {
                return None;
            }

            match ui.preferred_assembly_syntax() {
                AssemblySyntax::Gdb => None,
                AssemblySyntax::Intel => Some(DisassemblySyntax::Intel),
                AssemblySyntax::Att => Some(DisassemblySyntax::Att),
            }
        });

        let command = match preferred {
            Some(DisassemblySyntax::Intel) => "-gdb-set disassembly-flavor intel",
            Some(DisassemblySyntax::Att) => "-gdb-set disassembly-flavor att",
            None => "-gdb-show disassembly-flavor",
        };

        let controller = Rc::clone(self);
        let revision = self.syntax_revision.get().wrapping_add(1);
        self.syntax_revision.set(revision);

        if self
            .client
            .request(command, move |_, record| {
                if controller.client.transport_epoch() != epoch
                    || controller.syntax_revision.get() != revision
                {
                    return;
                }

                if !record.is_done() {
                    if preferred.is_some()
                        && let Some(ui) = controller.ui.upgrade()
                    {
                        ui.show_disassembly_error(record.error_message().unwrap_or(
                            "GDB rejected the preferred assembly syntax. Use the syntax controls to retry",
                        ));
                    }

                    return;
                }

                let syntax = preferred.unwrap_or_else(|| {
                    crate::debugger::evaluated_value(&record)
                        .filter(|value| value.eq_ignore_ascii_case("att"))
                        .map_or(DisassemblySyntax::Intel, |_| DisassemblySyntax::Att)
                });

                if let Some(ui) = controller.ui.upgrade() {
                    ui.set_disassembly_syntax(syntax);
                }
            })
            .is_err()
        {
            self.state.borrow_mut().syntax_epoch = None;
        }
    }

    fn set_syntax(self: &Rc<Self>, syntax: DisassemblySyntax) {
        let flavor = match syntax {
            DisassemblySyntax::Intel => "intel",
            DisassemblySyntax::Att => "att",
        };

        let controller = Rc::clone(self);
        let command = format!("-gdb-set disassembly-flavor {flavor}");
        let epoch = self.client.transport_epoch();
        let revision = self.syntax_revision.get().wrapping_add(1);
        self.syntax_revision.set(revision);

        if self
            .client
            .request(&command, move |_, record| {
                if controller.client.transport_epoch() != epoch
                    || controller.syntax_revision.get() != revision
                {
                    return;
                }

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

                let current = controller.state.borrow().current.clone();

                if let Some(current) = current {
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

        let Some(requests) = stop_requests(
            &self.ui,
            &self.client,
            self.model.current_stop_refresh_generation(),
        ) else {
            return;
        };

        ui.set_disassembly_loading(true);
        drop(ui);
        let generation = self.generation.get().wrapping_add(1);
        self.generation.set(generation);
        let command = format!("-data-disassemble -s 0x{start:x} -e 0x{end:x} --opcodes bytes -- 0");
        let controller = Rc::clone(self);

        let guard = Rc::downgrade(self);
        if requests
            .frame(&command)
            .when(move || {
                guard
                    .upgrade()
                    .is_some_and(|controller| controller.generation.get() == generation)
            })
            .request(move |_, record| {
                if record.class == "superseded" || controller.generation.get() != generation {
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
        let Some(requests) = stop_requests(
            &self.ui,
            &self.client,
            ui.model.current_stop_refresh_generation(),
        ) else {
            ui.set_disassembly_loading(false);
            return;
        };
        drop(ui);
        let generation = self.generation.get().wrapping_add(1);
        self.generation.set(generation);

        if let Some(address) = parse_address(expression) {
            self.request_function(address, generation, requests, history);
            return;
        }

        let command = address_evaluation_command(expression);

        let controller = Rc::clone(self);
        let requests_for_response = requests.clone();
        let guard = Rc::downgrade(self);

        if requests
            .frame(&command)
            .when(move || {
                guard
                    .upgrade()
                    .is_some_and(|controller| controller.generation.get() == generation)
            })
            .request(move |_, record| {
                if record.class == "superseded" || controller.generation.get() != generation {
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

                controller.request_function(address, generation, requests_for_response, history);
            })
            .is_err()
        {
            self.fail("The GDB/MI channel is unavailable");
        }
    }

    fn request_function(
        self: &Rc<Self>,
        address: u64,
        generation: u64,
        requests: StopRequests,
        history: HistoryUpdate,
    ) {
        let mixed = {
            let mut state = self.state.borrow_mut();
            state.mixed_refresh_pending = false;
            state.mixed
        };

        let (start, end) = {
            let state = self.state.borrow();
            let previous = matches!(history, HistoryUpdate::Reset)
                .then(|| state.range_start.zip(state.range_end))
                .flatten();

            function_window(address, previous)
        };

        let command = if mixed {
            format!("-data-disassemble -s 0x{start:x} -e 0x{end:x} --source --opcodes bytes -- 0")
        } else {
            format!("-data-disassemble -s 0x{start:x} -e 0x{end:x} --opcodes bytes -- 0")
        };

        let controller = Rc::clone(self);
        let requests_for_response = requests.clone();
        let guard = Rc::downgrade(self);

        let request = requests.frame(&command).when(move || {
            guard
                .upgrade()
                .is_some_and(|controller| controller.generation.get() == generation)
        });

        let response = move |_: &MiClient, record: MiRecord| {
            if record.class == "superseded" || controller.generation.get() != generation {
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
                    requests_for_response.clone(),
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
                    requests_for_response.clone(),
                    history,
                    SYMBOLLESS_DISASSEMBLY_BYTES,
                );

                return;
            }

            controller.present(address, history, instructions, mixed);
        };

        // Automatic assembly is supplemental to this stop's values. In
        // particular, do not put a large table rebuild ahead of the bounded
        // chain of local varobj requests. Explicit navigation stays interactive.
        let result = if matches!(history, HistoryUpdate::Reset) {
            request.background(response)
        } else {
            request.request(response)
        };

        if result.is_err() {
            self.fail("The GDB/MI channel is unavailable");
        }
    }

    fn request_address_window(
        self: &Rc<Self>,
        address: u64,
        generation: u64,
        requests: StopRequests,
        history: HistoryUpdate,
        bytes: u64,
    ) {
        let end = address.saturating_add(bytes.max(1));

        let command =
            format!("-data-disassemble -s 0x{address:x} -e 0x{end:x} --opcodes bytes -- 0");

        let controller = Rc::clone(self);
        let requests_for_response = requests.clone();
        let guard = Rc::downgrade(self);

        let request = requests.frame(&command).when(move || {
            guard
                .upgrade()
                .is_some_and(|controller| controller.generation.get() == generation)
        });

        let response = move |_: &MiClient, record: MiRecord| {
            if record.class == "superseded" || controller.generation.get() != generation {
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
                        requests_for_response.clone(),
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
        };

        let result = if matches!(history, HistoryUpdate::Reset) {
            request.background(response)
        } else {
            request.request(response)
        };

        if result.is_err() {
            self.fail("The GDB/MI channel is unavailable");
        }
    }

    fn present(
        self: &Rc<Self>,
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

        if self.state.borrow().mixed_refresh_pending {
            // A preference changed during this read. Coalesce it into one new
            // request instead of displaying an obsolete column layout first.
            self.resolve_and_show(focus, history);
            return;
        }

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

        let command = address_evaluation_command(&request.expression);

        let weak_ui_for_response = self.ui.clone();
        let generation = request.generation;

        let Some(requests) = stop_requests(&self.ui, &self.client, generation) else {
            if let Some(ui) = self.ui.upgrade() {
                ui.show_call_abi_target_resolution(&request, None);
            }

            return;
        };

        let request_for_response = request.clone();

        if requests
            .frame(&command)
            .request(move |_, record| {
                if record.class == "superseded" {
                    return;
                }

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
            })
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

fn address_evaluation_command(expression: &str) -> String {
    // C++ accepts the generated C-style cast and C++ navigation expressions.
    // Scope it to this MI command, independent of the stopped frame's language.
    format!(
        "-data-evaluate-expression --language c++ {}",
        crate::debugger::quote(&format!("(void*)({expression})"))
    )
}

/// Keep nearby steps on the same instruction-aligned window. Bytes and symbols
/// are still read from GDB at every stop, only the query bounds are retained.
/// This avoids shifting hundreds of GTK rows for each instruction advanced.
fn function_window(address: u64, previous: Option<(u64, u64)>) -> (u64, u64) {
    if let Some((start, end)) = previous
        && start <= address
        && address < end
        && end - start <= FUNCTION_DISASSEMBLY_BEFORE_BYTES + FUNCTION_DISASSEMBLY_AFTER_BYTES + 32
        && address - start <= FUNCTION_DISASSEMBLY_AFTER_BYTES
        && (end - address >= SYMBOLLESS_DISASSEMBLY_BYTES || end - start <= 512)
    {
        return (start, end);
    }

    (
        address.saturating_sub(FUNCTION_DISASSEMBLY_BEFORE_BYTES),
        address.saturating_add(FUNCTION_DISASSEMBLY_AFTER_BYTES),
    )
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

    #[test]
    fn nearby_steps_reuse_bounds_but_edges_and_invalid_windows_recenter() {
        assert_eq!(
            function_window(0x2100, Some((0x2000, 0x3000))),
            (0x2000, 0x3000)
        );
        assert_eq!(
            function_window(0x2010, Some((0x2000, 0x2080))),
            (0x2000, 0x2080)
        );
        for previous in [
            None,
            Some((0x3000, 0x4000)),
            Some((0x2101, 0x2000)),
            Some((0, u64::MAX)),
        ] {
            assert_eq!(function_window(0x2100, previous), (0x1d00, 0x2d00));
        }

        assert_eq!(
            function_window(0x2ff0, Some((0x2000, 0x3000))),
            (0x2bf0, 0x3bf0)
        );
        assert_eq!(function_window(0, None), (0, 3072));
        assert_eq!(function_window(u64::MAX, None), (u64::MAX - 1024, u64::MAX));
    }

    #[test]
    fn address_queries_scope_generated_casts_and_quote_expressions() {
        assert_eq!(
            address_evaluation_command("$pc"),
            "-data-evaluate-expression --language c++ \"(void*)($pc)\""
        );

        assert_eq!(
            address_evaluation_command("'module::function'"),
            "-data-evaluate-expression --language c++ \"(void*)('module::function')\""
        );

        let context = crate::debugger::StopContext::new(1, 2, None, String::from("3"), 4).unwrap();

        assert_eq!(
            context.scope_frame(&address_evaluation_command("$pc")),
            "-data-evaluate-expression --thread 3 --frame 4 --language c++ \"(void*)($pc)\""
        );
    }

    #[test]
    #[ignore = "requires Python-enabled GDB and the Rust and C++ variable viewer fixtures"]
    fn live_show_pc_resolves_and_disassembles_without_changing_language() {
        use std::{process::Command, time::Duration};

        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let evaluations = ["$pc", "reinterpret_cast<unsigned long>($pc)"]
            .map(|expression| {
                crate::debugger::quote(&format!(
                    "interpreter-exec mi {}",
                    crate::debugger::quote(&address_evaluation_command(expression))
                ))
            })
            .join(", ");

        let invalid = format!(
            "interpreter-exec mi {}",
            crate::debugger::quote(&address_evaluation_command(
                "fgdb_missing_disassembly_symbol"
            ))
        );

        let script = format!(
            r#"import re
gdb.execute('start', to_string=True)
pc = int(gdb.parse_and_eval('$pc'))
for language in ['auto', 'rust', 'fortran', 'c++', 'c']:
    gdb.execute('set language ' + language, to_string=True)
    before = gdb.execute('show language', to_string=True)
    for command in [{evaluations}]:
        reply = gdb.execute(command, to_string=True)
        assert '^done,value=' in reply, reply
        match = re.search(r'value="(0x[0-9a-fA-F]+)', reply)
        assert match and int(match[1], 16) == pc, reply
    disassembly = gdb.execute('interpreter-exec mi "-data-disassemble -s ' + hex(pc) + ' -e ' + hex(pc + 64) + ' --opcodes bytes -- 0"', to_string=True)
    assert '^done' in disassembly, disassembly
    addresses = re.findall(r'address="(0x[0-9a-fA-F]+)', disassembly)
    assert addresses and int(addresses[0], 16) == pc, disassembly
    assert gdb.execute('show language', to_string=True) == before
    reply = gdb.execute({}, to_string=True)
    assert '^error' in reply, reply
    assert gdb.execute('show language', to_string=True) == before
    assert int(gdb.parse_and_eval('$pc')) == pc
    assert not gdb.selected_thread().is_running()
gdb.write('FGDB_SHOW_PC_OK\n')
"#,
            crate::debugger::quote(&invalid),
        );

        for fixture in ["rust-variable-viewer-target", "cpp-variable-viewer-target"] {
            let mut command = Command::new("gdb");

            command
                .args(["--nx", "--quiet", "--batch"])
                .arg(root.join("target/debug-fixtures").join(fixture))
                .args(["-ex", "set debuginfod enabled off", "-ex"])
                .arg(format!("python exec({})", crate::debugger::quote(&script)));

            let output =
                crate::language::toolchain::probe::output(&mut command, Duration::from_secs(15))
                    .expect("live Show PC check failed or timed out");

            let output = String::from_utf8(output).unwrap();
            assert!(output.contains("FGDB_SHOW_PC_OK"), "{fixture}: {output}");
        }
    }

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
