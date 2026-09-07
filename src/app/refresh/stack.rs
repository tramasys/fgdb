use super::*;

const STACK_WORD_COUNT: usize = 32;

pub(in crate::app) struct StackRefresh {
    ui: Weak<Ui>,
    requests: StopRequests,
    entries: Vec<StackEntry>,
    stack_register: &'static str,
    pending: VecDeque<usize>,
    active: usize,
    word_size: usize,
    endian: TargetEndian,
}

pub(in crate::app) fn request_stack_memory(
    ui: Weak<Ui>,
    requests: StopRequests,
    registers: Vec<Register>,
    frames: Vec<StackFrame>,
    regions: Vec<MemoryRegion>,
) {
    let generation = requests.generation();
    if !requests.is_current() {
        return;
    }

    let Some((endian, pointer_bits)) = ui.upgrade().and_then(|ui| {
        ui.target_endian()
            .map(|endian| (endian, ui.target_pointer_bits()))
    }) else {
        if let Some(ui) = ui.upgrade() {
            ui.show_stack_unavailable_for_refresh(
                generation,
                "Stack decoding is unavailable because the target byte order could not be determined",
            );
        }

        return;
    };
    let architecture = ui
        .upgrade()
        .map_or(TargetArchitecture::Unknown, |ui| ui.target_architecture());
    let architecture = if architecture == TargetArchitecture::Unknown {
        let bits = ui.upgrade().map(|ui| ui.target_pointer_bits());
        TargetArchitecture::infer_from_register_names_with_bits(
            registers.iter().map(|register| register.name.as_str()),
            bits,
        )
    } else {
        architecture
    };
    let Some(stack_register) =
        architecture.stack_pointer(registers.iter().map(|register| register.name.as_str()))
    else {
        if let Some(ui) = ui.upgrade() {
            ui.show_stack_unavailable_for_refresh(
                generation,
                "Stack decoding is unavailable because no supported stack-pointer register was identified",
            );
        }

        return;
    };
    let word_size = usize::try_from(pointer_bits / 8).unwrap_or(8).clamp(4, 8);
    let command = format!(
        "-data-read-memory-bytes ${stack_register} {}",
        word_size * STACK_WORD_COUNT
    );
    let ui_for_request = ui.clone();

    let requests_for_response = requests.clone();
    if requests
        .frame(&command)
        .request(move |client, record| {
            if record.class == "superseded" {
                return;
            }

            let Some(memory) = crate::debugger::memory_block(&record) else {
                if let Some(ui) = ui.upgrade() {
                    ui.show_stack_unavailable_for_refresh(
                        generation,
                        record
                            .error_message()
                            .unwrap_or("GDB could not read memory at the stack pointer"),
                    );
                }

                return;
            };
            let entries = build_stack_entries(
                &memory,
                word_size,
                endian,
                architecture,
                &registers,
                &frames,
                &regions,
            );
            if let Some(ui) = ui.upgrade() {
                ui.show_stack_for_refresh(generation, &entries);
            }

            enrich_stack(
                ui,
                client,
                requests_for_response,
                entries,
                stack_register,
                word_size,
                endian,
            );
        })
        .is_err()
        && let Some(ui) = ui_for_request.upgrade()
    {
        ui.show_stack_unavailable_for_refresh(
            generation,
            "The MI channel could not issue the stack-memory request",
        );
    }
}

pub(in crate::app) fn enrich_stack(
    ui: Weak<Ui>,
    client: &MiClient,
    requests: StopRequests,
    entries: Vec<StackEntry>,
    stack_register: &'static str,
    word_size: usize,
    endian: TargetEndian,
) {
    let generation = requests.generation();
    if entries.is_empty() || !requests.is_current() {
        return;
    }

    let Some(current_ui) = ui.upgrade() else {
        return;
    };
    if !current_ui.stack_details_visible() {
        return;
    }

    let indices = entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| {
            pointer_address(&entry.value).is_some_and(|value| value != 0)
                && (entry.region.is_some()
                    || !entry.value_registers.is_empty()
                    || entry.return_frame.is_some())
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    if indices.is_empty() || !current_ui.model.claim_stack_details(generation) {
        return;
    }

    drop(current_ui);
    let refresh = Rc::new(RefCell::new(StackRefresh {
        ui,
        requests,
        entries,
        stack_register,
        pending: indices.into(),
        active: 0,
        word_size,
        endian,
    }));
    schedule_stack_chains(client, refresh);
}

fn schedule_stack_chains(client: &MiClient, refresh: Rc<RefCell<StackRefresh>>) {
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

                next.map(|index| (index, state.stack_register))
            }
        };
        let Some((index, stack_register)) = next else {
            return;
        };
        request_stack_chain(client, Rc::clone(&refresh), index, stack_register, 0);
    }
}

pub(in crate::app) fn request_stack_chain(
    client: &MiClient,
    refresh: Rc<RefCell<StackRefresh>>,
    entry_index: usize,
    stack_register: &'static str,
    depth: usize,
) {
    if !refresh.borrow().requests.is_current() {
        return;
    }

    let offset = refresh.borrow().entries[entry_index].offset;
    let expression = stack_pointer_expression(stack_register, offset, depth);
    let command = format!(
        "-data-evaluate-expression {}",
        crate::debugger::quote(&expression)
    );
    let requests = refresh.borrow().requests.clone();

    let refresh_for_handler = Rc::clone(&refresh);
    if requests
        .frame(&command)
        .request(move |client, record| {
            if record.class == "superseded" {
                return;
            }

            let value = record
                .is_done()
                .then(|| crate::debugger::evaluated_value(&record))
                .flatten();
            let mut continue_chain = false;
            let mut string_address = None;
            if let Some(value) = value
                && let Some(address) = pointer_address(&value)
            {
                let mut state = refresh_for_handler.borrow_mut();
                let endian = state.endian;
                let word_size = state.word_size;
                let entry = &mut state.entries[entry_index];
                let chain = &mut entry.pointer_chain;
                if chain
                    .iter()
                    .filter_map(|previous| pointer_address(previous))
                    .any(|previous| previous == address)
                {
                    chain.push(String::from("[loop detected]"));
                } else {
                    chain.push(value);
                    string_address = stack_string_address(entry, address, depth, endian, word_size);
                    continue_chain =
                        string_address.is_none() && address != 0 && depth < MAX_POINTER_CHAIN_DEPTH;
                }
            }

            if let Some(address) = string_address {
                request_stack_string(
                    client,
                    Rc::clone(&refresh_for_handler),
                    entry_index,
                    address,
                );
            } else if continue_chain {
                request_stack_chain(
                    client,
                    Rc::clone(&refresh_for_handler),
                    entry_index,
                    stack_register,
                    depth + 1,
                );
            } else {
                complete_stack_sequence(client, &refresh_for_handler);
            }
        })
        .is_err()
    {
        complete_stack_sequence(client, &refresh);
    }
}

pub(in crate::app) fn stack_string_address(
    entry: &StackEntry,
    decoded_word: u64,
    depth: usize,
    endian: TargetEndian,
    word_size: usize,
) -> Option<u64> {
    if depth == 0
        || !looks_like_string_word(decoded_word, endian, word_size)
        || matches!(entry.memory_kind, MemoryKind::Code | MemoryKind::Rwx)
    {
        return None;
    }

    entry
        .pointer_chain
        .len()
        .checked_sub(2)
        .and_then(|index| entry.pointer_chain.get(index))
        .and_then(|value| pointer_address(value))
}

pub(in crate::app) fn request_stack_string(
    client: &MiClient,
    refresh: Rc<RefCell<StackRefresh>>,
    entry_index: usize,
    address: u64,
) {
    if !refresh.borrow().requests.is_current() {
        return;
    }

    let expression = format!("(char*)0x{address:x}");
    let command = format!(
        "-data-evaluate-expression {}",
        crate::debugger::quote(&expression)
    );
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
                let entry = &mut state.entries[entry_index];
                entry.pointer_chain.pop();
                entry.pointer_chain.push(value);
                entry.memory_kind = MemoryKind::String;
            }

            complete_stack_sequence(client, &refresh_for_handler);
        })
        .is_err()
    {
        complete_stack_sequence(client, &refresh);
    }
}

pub(in crate::app) fn complete_stack_sequence(
    client: &MiClient,
    refresh: &Rc<RefCell<StackRefresh>>,
) {
    let completed = {
        let mut state = refresh.borrow_mut();
        state.active = state.active.saturating_sub(1);

        if state.active == 0 && state.pending.is_empty() {
            let endian = state.endian;
            let word_size = state.word_size;

            for entry in &mut state.entries {
                if entry
                    .pointer_chain
                    .iter()
                    .skip(1)
                    .filter_map(|value| pointer_address(value))
                    .any(|value| looks_like_string_word(value, endian, word_size))
                {
                    entry.memory_kind = MemoryKind::String;
                }
            }

            let ui = state.ui.clone();
            let generation = state.requests.generation();

            Some((ui, generation, std::mem::take(&mut state.entries)))
        } else {
            None
        }
    };

    if let Some((ui, generation, entries)) = completed
        && let Some(ui) = ui.upgrade()
    {
        ui.show_stack_for_refresh(generation, &entries);
    } else {
        schedule_stack_chains(client, Rc::clone(refresh));
    }
}

pub(in crate::app) fn stack_pointer_expression(
    register: &str,
    offset: usize,
    depth: usize,
) -> String {
    let mut expression = format!("*(void**)(${register}+0x{offset:x})");

    for _ in 0..depth {
        expression = format!("*(void**)({expression})");
    }

    expression
}
