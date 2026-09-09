use super::*;
use pointers::reads;

#[cfg(test)]
mod tests;

pub(in crate::app) struct RegisterRefresh {
    ui: Weak<Ui>,
    requests: StopRequests,
    registers: Vec<Register>,
    pending: VecDeque<usize>,
    active: usize,
    architecture: TargetArchitecture,
    endian: TargetEndian,
    pointer_bits: u32,
    reads: reads::Reads,
}

pub(in crate::app) fn refresh_registers(
    ui: &Weak<Ui>,
    client: &MiClient,
    generation: u64,
    stack_inputs: Rc<RefCell<StackInputs>>,
) {
    let requests = stack_inputs.borrow().requests.clone();

    if let Some(names) = ui.upgrade().and_then(|ui| ui.model.cached_register_names()) {
        request_register_values(ui.clone(), client, generation, stack_inputs, names);
        return;
    }

    let weak_ui = ui.clone();

    let stack_inputs_for_names = Rc::clone(&stack_inputs);

    if requests
        .unscoped("-data-list-register-names")
        .request(move |client, record| {
            if record.class == "superseded" {
                return;
            }

            if !record.is_done() {
                finish_empty_register_refresh(
                    &weak_ui,
                    client,
                    generation,
                    &stack_inputs_for_names,
                );

                return;
            }

            let names = Rc::new(crate::debugger::register_names(&record));

            if let Some(ui) = weak_ui.upgrade() {
                ui.model.cache_register_names(Rc::clone(&names));
            }

            request_register_values(
                weak_ui.clone(),
                client,
                generation,
                Rc::clone(&stack_inputs_for_names),
                names,
            );
        })
        .is_err()
    {
        finish_empty_register_refresh(ui, client, generation, &stack_inputs);
    }
}

fn request_register_values(
    ui: Weak<Ui>,
    client: &MiClient,
    generation: u64,
    stack_inputs: Rc<RefCell<StackInputs>>,
    names: Rc<Vec<String>>,
) {
    if !stack_inputs.borrow().requests.is_current() {
        return;
    }

    let architecture = ui.upgrade().map_or(TargetArchitecture::Unknown, |ui| {
        let current = ui.target_architecture();

        let detected = if current == TargetArchitecture::Unknown {
            TargetArchitecture::infer_from_register_names_with_bits(
                names.iter(),
                Some(ui.target_pointer_bits()),
            )
        } else {
            current
        };

        if detected != TargetArchitecture::Unknown {
            ui.set_target_architecture(detected);

            if ui.target_endian().is_none() {
                ui.set_target_endian(detected.default_endian());
            }
        }

        detected
    });

    let numbers = crate::debugger::compact_register_numbers(&names, architecture);

    if numbers.is_empty() {
        finish_empty_register_refresh(&ui, client, generation, &stack_inputs);
        return;
    }

    let mut command = String::with_capacity(32 + numbers.len() * 4);
    command.push_str("-data-list-register-values x");

    for number in numbers {
        let _ = write!(command, " {number}");
    }

    let requests = stack_inputs.borrow().requests.clone();

    let ui_for_response = ui.clone();

    let stack_inputs_for_response = Rc::clone(&stack_inputs);

    let requests_for_response = requests.clone();

    if requests
        .frame(&command)
        .request(move |client, record| {
            if record.class == "superseded" {
                return;
            }

            if !record.is_done() {
                finish_empty_register_refresh(
                    &ui_for_response,
                    client,
                    generation,
                    &stack_inputs_for_response,
                );

                return;
            }

            let registers = crate::debugger::registers(&record, &names);

            let registers_for_enrichment = ui_for_response.upgrade().and_then(|ui| {
                ui.show_registers_for_refresh(generation, &registers);

                ui.register_details_visible().then(|| registers.clone())
            });

            stack_inputs_for_response.borrow_mut().registers = Some(registers);
            start_stack_refresh_if_ready(&stack_inputs_for_response, client);

            if let Some(registers) = registers_for_enrichment {
                enrich_registers(ui_for_response, client, requests_for_response, registers);
            }
        })
        .is_err()
    {
        finish_empty_register_refresh(&ui, client, generation, &stack_inputs);
    }
}

pub(in crate::app) fn finish_empty_register_refresh(
    ui: &Weak<Ui>,
    client: &MiClient,
    generation: u64,
    stack_inputs: &Rc<RefCell<StackInputs>>,
) {
    if let Some(ui) = ui.upgrade() {
        ui.show_registers_for_refresh(generation, &[]);
    }

    stack_inputs.borrow_mut().registers = Some(Vec::new());
    start_stack_refresh_if_ready(stack_inputs, client);
}

pub(in crate::app) fn enrich_registers(
    ui: Weak<Ui>,
    client: &MiClient,
    requests: StopRequests,
    registers: Vec<Register>,
) {
    let generation = requests.generation();
    if registers.is_empty() || !requests.is_current() {
        return;
    }

    let Some(current_ui) = ui.upgrade() else {
        return;
    };

    if !current_ui.register_details_visible()
        || !current_ui.model.register_details_pending(generation)
    {
        return;
    }

    let Some(endian) = current_ui.target_endian() else {
        return;
    };

    let architecture = current_ui.target_architecture();
    let pointer_bits = current_ui.target_pointer_bits();

    let indices = registers
        .iter()
        .enumerate()
        .filter(|(_, register)| {
            is_pointer_register(&register.name, architecture)
                && pointer_address(&register.value).is_some_and(|address| address != 0)
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();

    // Do not consume this generation's one enrichment attempt until all
    // prerequisites are present and there is actual work to schedule. ABI
    // discovery can finish after the first register response.
    if indices.is_empty() || !current_ui.model.claim_register_details(generation) {
        return;
    }

    drop(current_ui);

    let refresh = Rc::new(RefCell::new(RegisterRefresh {
        ui,
        requests,
        registers,
        pending: indices.into(),
        active: 0,
        architecture,
        endian,
        pointer_bits,
        reads: reads::Reads::default(),
    }));

    schedule_register_chains(client, refresh);
}

fn schedule_register_chains(client: &MiClient, refresh: Rc<RefCell<RegisterRefresh>>) {
    loop {
        let next = {
            let mut state = refresh.borrow_mut();

            if state.active >= POINTER_ENRICHMENT_CONCURRENCY {
                None
            } else {
                let next = state.pending.pop_front();

                if next.is_some() {
                    state.active += 1;
                }

                next
            }
        };

        let Some(index) = next else {
            return;
        };

        request_register_chain(client, Rc::clone(&refresh), index, 0);
    }
}

pub(in crate::app) fn request_register_chain(
    client: &MiClient,
    refresh: Rc<RefCell<RegisterRefresh>>,
    register_index: usize,
    depth: usize,
) {
    if !refresh.borrow().requests.is_current() {
        return;
    }

    let position = reads::Position {
        index: register_index,
        depth,
    };
    let address = (depth > 0).then(|| {
        let state = refresh.borrow();
        state.registers[register_index]
            .pointer_chain
            .last()
            .and_then(|value| pointer_address(value))
            .expect("a continued register chain has a numeric address")
    });

    if let Some(address) = address {
        let lookup = refresh.borrow_mut().reads.request(address, position);
        match lookup {
            reads::Lookup::Pending => return,
            reads::Lookup::Ready(value) => {
                apply_register_chain_value(client, &refresh, position, value);
                return;
            }
            reads::Lookup::Start => {}
        }
    }

    let command = address.map_or_else(
        || pointers::register_command(&refresh.borrow().registers[register_index].name, 0),
        pointers::read_command,
    );

    let requests = refresh.borrow().requests.clone();

    let refresh_for_handler = Rc::clone(&refresh);

    if requests
        .frame(&command)
        .enrich(move |client, record| {
            if record.class == "superseded" {
                return;
            }

            let value = record
                .is_done()
                .then(|| crate::debugger::evaluated_value(&record))
                .flatten();

            register_chain_reply(client, &refresh_for_handler, address, position, value);
        })
        .is_err()
    {
        register_chain_reply(client, &refresh, address, position, None);
    }
}

fn register_chain_reply(
    client: &MiClient,
    refresh: &Rc<RefCell<RegisterRefresh>>,
    address: Option<u64>,
    position: reads::Position,
    value: Option<String>,
) {
    if let Some(address) = address {
        let waiters = refresh.borrow_mut().reads.complete(address, value.clone());
        for position in waiters {
            apply_register_chain_value(client, refresh, position, value.clone());
        }
    } else {
        apply_register_chain_value(client, refresh, position, value);
    }
}

fn apply_register_chain_value(
    client: &MiClient,
    refresh: &Rc<RefCell<RegisterRefresh>>,
    position: reads::Position,
    value: Option<String>,
) {
    if !refresh.borrow().requests.is_current() {
        return;
    }

    let reads::Position { index, depth } = position;
    let mut continue_chain = false;
    let mut string_address = None;

    if let Some(value) = value
        && let Some(address) = pointer_address(&value)
    {
        let mut state = refresh.borrow_mut();
        let endian = state.endian;
        let architecture = state.architecture;
        let pointer_bits = state.pointer_bits;
        let register = &mut state.registers[index];
        let chain = &mut register.pointer_chain;

        if chain
            .iter()
            .filter_map(|previous| pointer_address(previous))
            .any(|previous| previous == address)
        {
            chain.push(String::from("[loop detected]"));
        } else {
            chain.push(value);
            string_address = register_string_address(
                register,
                address,
                depth,
                endian,
                pointer_bits,
                architecture,
            );
            continue_chain =
                string_address.is_none() && address != 0 && depth < MAX_POINTER_CHAIN_DEPTH;
        }
    }

    if let Some(address) = string_address {
        request_register_string(client, Rc::clone(refresh), index, address);
    } else if continue_chain {
        request_register_chain(client, Rc::clone(refresh), index, depth + 1);
    } else {
        complete_register_sequence(client, refresh);
    }
}

pub(in crate::app) fn register_string_address(
    register: &Register,
    decoded_word: u64,
    depth: usize,
    endian: TargetEndian,
    pointer_bits: u32,
    architecture: TargetArchitecture,
) -> Option<u64> {
    if depth == 0
        || architecture.is_program_counter(&register.name)
        || !looks_like_string_word(
            decoded_word,
            endian,
            usize::try_from(pointer_bits / 8).unwrap_or(8).clamp(4, 8),
        )
    {
        return None;
    }

    register
        .pointer_chain
        .len()
        .checked_sub(2)
        .and_then(|index| register.pointer_chain.get(index))
        .filter(|value| !value.contains('<'))
        .and_then(|value| pointer_address(value))
}

pub(in crate::app) fn request_register_string(
    client: &MiClient,
    refresh: Rc<RefCell<RegisterRefresh>>,
    register_index: usize,
    address: u64,
) {
    if !refresh.borrow().requests.is_current() {
        return;
    }

    let command = pointers::string_command(address);

    let requests = refresh.borrow().requests.clone();

    let refresh_for_handler = Rc::clone(&refresh);

    if requests
        .frame(&command)
        .with_print_limit(POINTER_STRING_PREVIEW_ELEMENTS, move |client, record| {
            if record.class == "superseded" {
                return;
            }

            if let Some(value) = record
                .is_done()
                .then(|| crate::debugger::evaluated_value(&record))
                .flatten()
                .filter(|value| value.contains('"'))
            {
                let mut state = refresh_for_handler.borrow_mut();
                let chain = &mut state.registers[register_index].pointer_chain;
                chain.pop();
                chain.push(value);
            }

            complete_register_sequence(client, &refresh_for_handler);
        })
        .is_err()
    {
        complete_register_sequence(client, &refresh);
    }
}

pub(in crate::app) fn complete_register_sequence(
    client: &MiClient,
    refresh: &Rc<RefCell<RegisterRefresh>>,
) {
    let completed = {
        let mut state = refresh.borrow_mut();
        if !state.requests.is_current() {
            return;
        }

        state.active = state.active.saturating_sub(1);
        if state.active == 0 && state.pending.is_empty() {
            if state.ui.upgrade().is_none() {
                return;
            }

            let ui = state.ui.clone();
            let generation = state.requests.generation();
            Some((ui, generation, std::mem::take(&mut state.registers)))
        } else {
            None
        }
    };
    if let Some((ui, generation, registers)) = completed
        && let Some(ui) = ui.upgrade()
    {
        ui.show_register_details_for_refresh(generation, &registers);
    } else {
        schedule_register_chains(client, Rc::clone(refresh));
    }
}
