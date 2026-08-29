use super::*;

pub(crate) fn refresh_stopped_state(ui: &Weak<Ui>, client: &MiClient) {
    let Some(current_ui) = ui.upgrade() else {
        return;
    };
    if current_ui.inferior_is_running() {
        return;
    }
    current_ui.clear_execution_location();
    let generation = current_ui.start_stop_refresh();
    for varobj in current_ui.local_variable_object_names() {
        delete_variable_object(client, &varobj);
    }
    drop(current_ui);

    let stack_inputs = Rc::new(RefCell::new(StackInputs {
        ui: ui.clone(),
        generation,
        frames: None,
        registers: None,
    }));

    let weak_ui = ui.clone();
    let stack_inputs_for_frames = Rc::clone(&stack_inputs);
    if client
        .request("-stack-list-frames 0 24", move |client, record| {
            if !stop_refresh_is_current(&weak_ui, generation) {
                return;
            }
            let frames = if record.is_done() {
                crate::debugger::stack_frames(&record)
            } else {
                Vec::new()
            };
            if record.is_done()
                && let Some(ui) = weak_ui.upgrade()
            {
                ui.show_frames(&frames);
            }
            stack_inputs_for_frames.borrow_mut().frames = Some(frames);
            start_stack_refresh_if_ready(&stack_inputs_for_frames, client);
        })
        .is_err()
    {
        if let Some(ui) = stack_inputs.borrow().ui.upgrade() {
            ui.show_frames(&[]);
        }
        stack_inputs.borrow_mut().frames = Some(Vec::new());
        start_stack_refresh_if_ready(&stack_inputs, client);
    }

    let weak_ui = ui.clone();
    let _ = client.request("-stack-info-frame", move |_, record| {
        if !stop_refresh_is_current(&weak_ui, generation) {
            return;
        }
        if record.is_done()
            && let (Some(ui), Some(frame)) =
                (weak_ui.upgrade(), crate::debugger::current_frame(&record))
        {
            ui.show_execution_location(&frame);
            let pc = frame.address.clone();
            let architecture = frame.architecture.clone();
            ui.request_disassembly_for_stop(pc, architecture);
        }
    });

    let weak_ui = ui.clone();
    let weak_ui_for_guard = ui.clone();
    let _ = client.request_with_print_limit_when(
        "-stack-list-variables --simple-values",
        AUTOMATIC_PRINT_ELEMENTS,
        move || stop_refresh_is_current(&weak_ui_for_guard, generation),
        move |client, record| {
            if record.is_done() {
                refresh_variable_objects(
                    weak_ui.clone(),
                    client,
                    generation,
                    crate::debugger::variables(&record),
                );
            }
        },
    );

    refresh_registers(ui, client, generation, stack_inputs);

    refresh_expression_watches(ui.clone(), client, generation);

    refresh_threads(ui, client);
}

/// Add the expensive pointer-chain details for an inspector page from the
/// current stop cache. Switching tabs must never invalidate and rebuild the
/// complete stopped state.
pub(crate) fn refresh_cached_inspector_details(ui: &Weak<Ui>, client: &MiClient, page: u32) {
    let Some(current_ui) = ui.upgrade() else {
        return;
    };
    let generation = current_ui.current_stop_refresh_generation();
    match page {
        2 => {
            let Some(registers) = current_ui.registers_for_details(generation) else {
                return;
            };
            drop(current_ui);
            enrich_registers(ui.clone(), client, generation, registers);
        }
        3 => {
            let Some(entries) = current_ui.stack_for_details(generation) else {
                return;
            };
            let Some(registers) = current_ui.registers_for_details(generation) else {
                return;
            };
            let architecture = current_ui.target_architecture();
            let Some(stack_register) =
                architecture.stack_pointer(registers.iter().map(|register| register.name.as_str()))
            else {
                return;
            };
            let Some(endian) = current_ui.target_endian() else {
                return;
            };
            let word_size = usize::try_from(current_ui.target_pointer_bits() / 8)
                .unwrap_or(8)
                .clamp(4, 8);
            drop(current_ui);
            enrich_stack(
                ui.clone(),
                client,
                generation,
                entries,
                stack_register,
                word_size,
                endian,
            );
        }
        _ => {}
    }
}

pub(super) fn refresh_registers(
    ui: &Weak<Ui>,
    client: &MiClient,
    generation: u64,
    stack_inputs: Rc<RefCell<StackInputs>>,
) {
    if let Some(names) = ui.upgrade().and_then(|ui| ui.cached_register_names()) {
        request_register_values(ui.clone(), client, generation, stack_inputs, names);
        return;
    }

    let weak_ui = ui.clone();
    let stack_inputs_for_names = Rc::clone(&stack_inputs);
    if client
        .request("-data-list-register-names", move |client, record| {
            if !stop_refresh_is_current(&weak_ui, generation) {
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
                ui.cache_register_names(Rc::clone(&names));
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
    if !stop_refresh_is_current(&ui, generation) {
        return;
    }
    let architecture = ui.upgrade().map_or(TargetArchitecture::Unknown, |ui| {
        let current = ui.target_architecture();
        let detected = if current == TargetArchitecture::Unknown {
            TargetArchitecture::infer_from_register_names_with_bits(
                &names,
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
    let ui_for_response = ui.clone();
    let stack_inputs_for_response = Rc::clone(&stack_inputs);
    if client
        .request(&command, move |client, record| {
            if !stop_refresh_is_current(&ui_for_response, generation) {
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
            if let Some(ui) = ui_for_response.upgrade() {
                ui.show_registers_for_refresh(generation, &registers);
            }
            stack_inputs_for_response.borrow_mut().registers = Some(registers.clone());
            start_stack_refresh_if_ready(&stack_inputs_for_response, client);
            enrich_registers(ui_for_response, client, generation, registers);
        })
        .is_err()
    {
        finish_empty_register_refresh(&ui, client, generation, &stack_inputs);
    }
}

pub(super) fn refresh_variable_objects(
    ui: Weak<Ui>,
    client: &MiClient,
    generation: u64,
    variables: Vec<Variable>,
) {
    if let Some(ui) = ui.upgrade() {
        ui.show_locals_for_refresh(generation, &variables);
    }
    if variables.is_empty() {
        return;
    }
    if !variables.iter().any(Variable::needs_variable_object) {
        return;
    }
    let state = Rc::new(RefCell::new(VariableRefresh {
        ui,
        generation,
        variables,
        next_index: 0,
        created: 0,
    }));
    request_next_variable_object(client, state);
}

pub(super) fn request_next_variable_object(client: &MiClient, state: Rc<RefCell<VariableRefresh>>) {
    let (ui, generation) = {
        let state = state.borrow();
        (state.ui.clone(), state.generation)
    };
    if !stop_refresh_is_current(&ui, generation) {
        discard_variable_refresh(client, &state);
        return;
    }
    let next = {
        let mut state = state.borrow_mut();
        if state.created >= MAX_AUTOMATIC_VARIABLE_OBJECTS {
            state.next_index = state.variables.len();
        }
        while state.next_index < state.variables.len()
            && !state.variables[state.next_index].needs_variable_object()
        {
            state.next_index += 1;
        }
        (state.next_index < state.variables.len()).then(|| {
            let index = state.next_index;
            state.next_index += 1;
            state.created += 1;
            (index, state.variables[index].name.clone())
        })
    };
    let Some((index, display_name)) = next else {
        let (ui, generation, variables) = {
            let mut state = state.borrow_mut();
            (
                state.ui.clone(),
                state.generation,
                std::mem::take(&mut state.variables),
            )
        };
        if let Some(ui) = ui.upgrade() {
            ui.show_locals_for_refresh(generation, &variables);
        }
        return;
    };
    let command = format!("-var-create - * {}", crate::debugger::quote(&display_name));
    let state_for_response = Rc::clone(&state);
    let state_for_guard = Rc::clone(&state);
    if client
        .request_with_print_limit_when(
            &command,
            AUTOMATIC_PRINT_ELEMENTS,
            move || {
                let state = state_for_guard.borrow();
                stop_refresh_is_current(&state.ui, state.generation)
            },
            move |client, record| {
                let variable = record
                    .is_done()
                    .then(|| crate::debugger::variable_object(&record, &display_name))
                    .flatten();
                let (ui, generation) = {
                    let state = state_for_response.borrow();
                    (state.ui.clone(), state.generation)
                };
                if !stop_refresh_is_current(&ui, generation) {
                    if let Some(varobj) = variable
                        .as_ref()
                        .and_then(|variable| variable.varobj.as_deref())
                    {
                        delete_variable_object(client, varobj);
                    }
                    discard_variable_refresh(client, &state_for_response);
                    return;
                }
                if let Some(variable) = variable {
                    state_for_response.borrow_mut().variables[index] = variable;
                }
                request_next_variable_object(client, state_for_response);
            },
        )
        .is_err()
    {
        request_next_variable_object(client, state);
    }
}

pub(super) fn stop_refresh_is_current(ui: &Weak<Ui>, generation: u64) -> bool {
    ui.upgrade()
        .is_some_and(|ui| ui.is_stop_refresh_current(generation))
}

pub(super) fn delete_variable_object(client: &MiClient, varobj: &str) {
    let command = format!("-var-delete {}", crate::debugger::quote(varobj));
    // Parent deletion can legitimately remove child objects also present in
    // the expanded UI tree, so consume any resulting errors locally.
    let _ = client.request(&command, |_, _| {});
}

pub(super) fn discard_variable_refresh(client: &MiClient, state: &Rc<RefCell<VariableRefresh>>) {
    let state = state.borrow();
    for varobj in state
        .variables
        .iter()
        .filter_map(|variable| variable.varobj.as_deref())
    {
        delete_variable_object(client, varobj);
    }
}

pub(super) fn request_variable_children(
    ui: Weak<Ui>,
    client: Rc<MiClient>,
    variable: Variable,
    from: usize,
) {
    let Some(varobj) = variable.varobj.clone() else {
        return;
    };
    // Dynamic varobjs may advertise available pretty-printed children only
    // through `has_more`; GDB documents `numchild` as unreliable for them.
    if variable.num_children > 0 || variable.has_more {
        let to = from.saturating_add(VARIABLE_CHILD_PAGE_SIZE);
        let command = format!(
            "-var-list-children --all-values {} {from} {to}",
            crate::debugger::quote(&varobj),
        );
        let ui_for_response = ui.clone();
        let ui_for_guard = ui.clone();
        let varobj_for_response = varobj.clone();
        let varobj_for_guard = varobj.clone();
        let variable_for_response = variable.clone();
        if let Err(error) = client.request_with_print_limit_when(
            &command,
            AUTOMATIC_PRINT_ELEMENTS,
            move || {
                ui_for_guard
                    .upgrade()
                    .is_some_and(|ui| ui.has_variable_object(&varobj_for_guard))
            },
            move |_, record| {
                if let Some(ui) = ui_for_response.upgrade() {
                    if record.is_done() {
                        let children = crate::debugger::variable_children(&record);
                        let next = from.saturating_add(children.len());
                        let has_more = !children.is_empty()
                            && (crate::debugger::variable_children_have_more(&record)
                                || next < variable_for_response.num_children);
                        ui.show_variable_children_page(
                            &variable_for_response,
                            from,
                            &children,
                            has_more,
                        );
                    } else {
                        ui.show_variable_children_error(
                            &varobj_for_response,
                            record
                                .error_message()
                                .unwrap_or("GDB could not expand this value"),
                        );
                    }
                }
            },
        ) && let Some(ui) = ui.upgrade()
        {
            ui.show_variable_children_error(&varobj, &error.to_string());
        }
        return;
    }
    if !variable.is_pointer() {
        if let Some(ui) = ui.upgrade() {
            ui.show_variable_children(&varobj, &[]);
        }
        return;
    }
    let command = format!(
        "-var-info-path-expression {}",
        crate::debugger::quote(&varobj)
    );
    let ui_for_path = ui.clone();
    let client_for_path = Rc::clone(&client);
    let varobj_for_path = varobj.clone();
    let display_name = variable.name;
    if client
        .request(&command, move |_, record| {
            let Some(path) = crate::debugger::variable_path_expression(&record) else {
                if let Some(ui) = ui_for_path.upgrade() {
                    ui.show_variable_children_error(
                        &varobj_for_path,
                        record
                            .error_message()
                            .unwrap_or("GDB cannot dereference this pointer type"),
                    );
                }
                return;
            };
            let command = format!(
                "-var-create - * {}",
                crate::debugger::quote(&format!("*({path})"))
            );
            if !ui_for_path
                .upgrade()
                .is_some_and(|ui| ui.has_variable_object(&varobj_for_path))
            {
                return;
            }
            let ui_for_dereference = ui_for_path.clone();
            let ui_for_guard = ui_for_path.clone();
            let varobj_for_dereference = varobj_for_path.clone();
            let varobj_for_guard = varobj_for_path.clone();
            let ui_for_request_error = ui_for_path.clone();
            let varobj_for_request_error = varobj_for_path.clone();
            if client_for_path
                .request_with_print_limit_when(
                    &command,
                    AUTOMATIC_PRINT_ELEMENTS,
                    move || {
                        ui_for_guard
                            .upgrade()
                            .is_some_and(|ui| ui.has_variable_object(&varobj_for_guard))
                    },
                    move |client, record| {
                        let child = record
                            .is_done()
                            .then(|| {
                                crate::debugger::variable_object(
                                    &record,
                                    &format!("*{display_name}"),
                                )
                            })
                            .flatten();
                        if let Some(child) = child {
                            let attached = ui_for_dereference.upgrade().is_some_and(|ui| {
                                ui.show_variable_children(
                                    &varobj_for_dereference,
                                    std::slice::from_ref(&child),
                                )
                            });
                            if !attached && let Some(varobj) = child.varobj.as_deref() {
                                delete_variable_object(client, varobj);
                            }
                        } else if let Some(ui) = ui_for_dereference.upgrade() {
                            ui.show_variable_children_error(
                                &varobj_for_dereference,
                                record
                                    .error_message()
                                    .unwrap_or("GDB cannot dereference this pointer"),
                            );
                        }
                    },
                )
                .is_err()
                && let Some(ui) = ui_for_request_error.upgrade()
            {
                ui.show_variable_children_error(
                    &varobj_for_request_error,
                    "The MI channel is unavailable",
                );
            }
        })
        .is_err()
        && let Some(ui) = ui.upgrade()
    {
        ui.show_variable_children_error(&varobj, "The MI channel is unavailable");
    }
}

pub(super) fn finish_empty_register_refresh(
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

pub(super) fn enrich_registers(
    ui: Weak<Ui>,
    client: &MiClient,
    generation: u64,
    registers: Vec<Register>,
) {
    if registers.is_empty() || !stop_refresh_is_current(&ui, generation) {
        return;
    }
    let Some(current_ui) = ui.upgrade() else {
        return;
    };
    if !current_ui.register_details_visible() {
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
    if indices.is_empty() || !current_ui.claim_register_details(generation) {
        return;
    }
    drop(current_ui);

    let refresh = Rc::new(RefCell::new(RegisterRefresh {
        ui,
        generation,
        registers,
        pending: indices.into(),
        active: 0,
        architecture,
        endian,
        pointer_bits,
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

pub(super) fn request_register_chain(
    client: &MiClient,
    refresh: Rc<RefCell<RegisterRefresh>>,
    register_index: usize,
    depth: usize,
) {
    let (ui, generation) = {
        let state = refresh.borrow();
        (state.ui.clone(), state.generation)
    };
    if !stop_refresh_is_current(&ui, generation) {
        return;
    }
    let name = refresh.borrow().registers[register_index].name.clone();
    let expression = pointer_expression(&name, depth);
    let command = format!(
        "-data-evaluate-expression {}",
        crate::debugger::quote(&expression)
    );
    let refresh_for_guard = Rc::clone(&refresh);
    let refresh_for_handler = Rc::clone(&refresh);
    if client
        .request_when(
            &command,
            move || {
                let state = refresh_for_guard.borrow();
                stop_refresh_is_current(&state.ui, state.generation)
            },
            move |client, record| {
                let (ui, generation) = {
                    let state = refresh_for_handler.borrow();
                    (state.ui.clone(), state.generation)
                };
                if !stop_refresh_is_current(&ui, generation) {
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
                    let architecture = state.architecture;
                    let pointer_bits = state.pointer_bits;
                    let register = &mut state.registers[register_index];
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
                        continue_chain = string_address.is_none()
                            && address != 0
                            && depth < MAX_POINTER_CHAIN_DEPTH;
                    }
                }

                if let Some(address) = string_address {
                    request_register_string(
                        client,
                        Rc::clone(&refresh_for_handler),
                        register_index,
                        address,
                    );
                } else if continue_chain {
                    request_register_chain(
                        client,
                        Rc::clone(&refresh_for_handler),
                        register_index,
                        depth + 1,
                    );
                } else {
                    complete_register_sequence(client, &refresh_for_handler);
                }
            },
        )
        .is_err()
    {
        complete_register_sequence(client, &refresh);
    }
}

pub(super) fn register_string_address(
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

pub(super) fn request_register_string(
    client: &MiClient,
    refresh: Rc<RefCell<RegisterRefresh>>,
    register_index: usize,
    address: u64,
) {
    let (ui, generation) = {
        let state = refresh.borrow();
        (state.ui.clone(), state.generation)
    };
    if !stop_refresh_is_current(&ui, generation) {
        return;
    }
    let expression = format!("(char*)0x{address:x}");
    let command = format!(
        "-data-evaluate-expression {}",
        crate::debugger::quote(&expression)
    );
    let refresh_for_handler = Rc::clone(&refresh);
    let refresh_for_guard = Rc::clone(&refresh);
    if client
        .request_with_print_limit_when(
            &command,
            POINTER_STRING_PREVIEW_ELEMENTS,
            move || {
                let state = refresh_for_guard.borrow();
                stop_refresh_is_current(&state.ui, state.generation)
            },
            move |client, record| {
                let (ui, generation) = {
                    let state = refresh_for_handler.borrow();
                    (state.ui.clone(), state.generation)
                };
                if !stop_refresh_is_current(&ui, generation) {
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
            },
        )
        .is_err()
    {
        complete_register_sequence(client, &refresh);
    }
}

pub(super) fn complete_register_sequence(
    client: &MiClient,
    refresh: &Rc<RefCell<RegisterRefresh>>,
) {
    let completed = {
        let mut state = refresh.borrow_mut();
        state.active = state.active.saturating_sub(1);
        if state.active == 0 && state.pending.is_empty() {
            let ui = state.ui.clone();
            let generation = state.generation;
            Some((ui, generation, std::mem::take(&mut state.registers)))
        } else {
            None
        }
    };
    if let Some((ui, generation, registers)) = completed
        && let Some(ui) = ui.upgrade()
    {
        ui.show_registers_for_refresh(generation, &registers);
    } else {
        schedule_register_chains(client, Rc::clone(refresh));
    }
}

pub(super) fn pointer_expression(register: &str, depth: usize) -> String {
    let mut expression = format!("${register}");
    if depth == 0 {
        return format!("(void*)({expression})");
    }
    for _ in 0..depth {
        expression = format!("*(void**)({expression})");
    }
    expression
}

pub(super) fn start_stack_refresh_if_ready(refresh: &Rc<RefCell<StackInputs>>, client: &MiClient) {
    let inputs = {
        let mut refresh = refresh.borrow_mut();
        match (refresh.frames.take(), refresh.registers.take()) {
            (Some(frames), Some(registers)) => {
                Some((refresh.ui.clone(), refresh.generation, registers, frames))
            }
            (frames, registers) => {
                refresh.frames = frames;
                refresh.registers = registers;
                None
            }
        }
    };
    let Some((ui, generation, registers, frames)) = inputs else {
        return;
    };
    if !stop_refresh_is_current(&ui, generation) {
        return;
    }
    let ui_for_request = ui.clone();
    if client
        .request("-list-thread-groups", move |client, record| {
            if !stop_refresh_is_current(&ui, generation) {
                return;
            }
            let pid = crate::debugger::inferior_pid(&record);
            let debugger_pid = ui.upgrade().and_then(|ui| ui.debugger_pid());
            if let Some((architecture, endian, pointer_bits)) =
                pid.zip(debugger_pid).and_then(|(pid, debugger_pid)| {
                    crate::kernel::read_local_target_abi(pid, debugger_pid)
                })
                && let Some(current_ui) = ui.upgrade()
            {
                // An ELF class and byte order remain useful even when this
                // fgdb build does not recognize e_machine. Do not let that
                // future/unknown machine erase a more specific GDB result.
                if architecture != TargetArchitecture::Unknown {
                    current_ui.set_target_architecture(architecture);
                }
                current_ui.set_target_endian(Some(endian));
                current_ui.set_target_pointer_bits(pointer_bits);
                // Register names can be ambiguous (notably numbered RISC,
                // MIPS, PowerPC and s390 registers). Rebind rows once the
                // executable resolves the architecture so grouping, widths
                // and semantic colors are correct on the first stop. Use the
                // current generation's cached rows rather than the captured
                // raw response: pointer-chain enrichment may already have
                // completed while the PID/ABI request was in flight.
                if let Some(current_registers) = current_ui.registers_for_details(generation) {
                    current_ui.show_registers_for_refresh(generation, &current_registers);
                    // If the initial enrichment was deferred because target
                    // byte order or architecture was not known yet, ABI
                    // discovery is the event that makes it runnable. An
                    // already active/completed attempt is rejected by the
                    // per-generation claim.
                    enrich_registers(ui.clone(), client, generation, current_registers);
                }
            }
            let mut regions = pid
                .zip(debugger_pid)
                .map(|(pid, debugger_pid)| read_memory_regions(pid, debugger_pid))
                .unwrap_or_default();
            let architecture = ui
                .upgrade()
                .map_or(TargetArchitecture::Unknown, |ui| ui.target_architecture());
            let architecture = if architecture == TargetArchitecture::Unknown {
                let names = registers
                    .iter()
                    .map(|register| register.name.as_str())
                    .collect::<Vec<_>>();
                let bits = ui.upgrade().map(|ui| ui.target_pointer_bits());
                TargetArchitecture::infer_from_register_names_with_bits(&names, bits)
            } else {
                architecture
            };
            annotate_memory_regions(&mut regions, &registers, architecture);
            if let Some(current_ui) = ui.upgrade() {
                if pid.is_some() {
                    current_ui.set_inferior_started(true);
                }
                current_ui.show_call_abi_for_refresh(generation, &frames);
                current_ui.show_memory_regions_for_refresh(generation, &regions);
                current_ui.refresh_memory_watches();
                current_ui.refresh_kernel_after_stop();
            }
            request_tls_runtime(&ui, client, generation, &registers, &regions, architecture);
            request_stack_memory(ui, client, generation, registers, frames, regions);
        })
        .is_err()
        && let Some(ui) = ui_for_request.upgrade()
    {
        ui.show_stack_for_refresh(generation, &[]);
        ui.show_memory_regions_for_refresh(generation, &[]);
        ui.refresh_memory_watches();
    }
}

fn request_tls_runtime(
    ui: &Weak<Ui>,
    client: &MiClient,
    generation: u64,
    registers: &[Register],
    regions: &[MemoryRegion],
    architecture: TargetArchitecture,
) {
    const TLS_READ_BYTES: usize = 80;
    let Some((register, base)) = architecture
        .thread_pointer_candidates()
        .iter()
        .copied()
        .find_map(|name| {
            registers
                .iter()
                .find(|register| register.name == name)
                .and_then(|register| pointer_address(&register.value))
                .filter(|address| *address != 0)
                .map(|address| (name, address))
        })
    else {
        if let Some(ui) = ui.upgrade() {
            ui.show_tls_runtime_unavailable_for_refresh(
                generation,
                "This target did not expose a supported non-zero thread-pointer register",
            );
        }
        return;
    };
    let mapping = regions
        .iter()
        .find(|region| region.contains(base))
        .map(MemoryRegion::description);
    let command = format!("-data-read-memory-bytes ${register} {TLS_READ_BYTES}");
    let ui_for_response = ui.clone();
    let ui_for_error = ui.clone();
    let mapping_for_response = mapping.clone();
    if client
        .request(&command, move |_, record| {
            if !stop_refresh_is_current(&ui_for_response, generation) {
                return;
            }
            let memory = record
                .is_done()
                .then(|| crate::debugger::memory_block(&record))
                .flatten();
            if let Some(ui) = ui_for_response.upgrade() {
                if let Some(memory) = memory.as_ref() {
                    ui.show_tls_runtime_for_refresh(
                        generation,
                        (architecture, ui.target_endian(), ui.target_pointer_bits()),
                        register,
                        base,
                        mapping_for_response.as_deref(),
                        Ok(memory),
                    );
                } else {
                    ui.show_tls_runtime_for_refresh(
                        generation,
                        (architecture, ui.target_endian(), ui.target_pointer_bits()),
                        register,
                        base,
                        mapping_for_response.as_deref(),
                        Err(record
                            .error_message()
                            .unwrap_or("GDB could not read the live TLS block")),
                    );
                }
            }
        })
        .is_err()
        && let Some(ui) = ui_for_error.upgrade()
    {
        ui.show_tls_runtime_for_refresh(
            generation,
            (architecture, ui.target_endian(), ui.target_pointer_bits()),
            register,
            base,
            mapping.as_deref(),
            Err("The MI channel is unavailable"),
        );
    }
}

pub(super) fn request_stack_memory(
    ui: Weak<Ui>,
    client: &MiClient,
    generation: u64,
    registers: Vec<Register>,
    frames: Vec<StackFrame>,
    regions: Vec<MemoryRegion>,
) {
    if !stop_refresh_is_current(&ui, generation) {
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
        let names = registers
            .iter()
            .map(|register| register.name.as_str())
            .collect::<Vec<_>>();
        let bits = ui.upgrade().map(|ui| ui.target_pointer_bits());
        TargetArchitecture::infer_from_register_names_with_bits(&names, bits)
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
    if client
        .request(&command, move |client, record| {
            if !stop_refresh_is_current(&ui, generation) {
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
                generation,
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

pub(super) fn enrich_stack(
    ui: Weak<Ui>,
    client: &MiClient,
    generation: u64,
    entries: Vec<StackEntry>,
    stack_register: &'static str,
    word_size: usize,
    endian: TargetEndian,
) {
    if entries.is_empty() || !stop_refresh_is_current(&ui, generation) {
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
    if indices.is_empty() || !current_ui.claim_stack_details(generation) {
        return;
    }
    drop(current_ui);
    let refresh = Rc::new(RefCell::new(StackRefresh {
        ui,
        generation,
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

pub(super) fn request_stack_chain(
    client: &MiClient,
    refresh: Rc<RefCell<StackRefresh>>,
    entry_index: usize,
    stack_register: &'static str,
    depth: usize,
) {
    let (ui, generation) = {
        let state = refresh.borrow();
        (state.ui.clone(), state.generation)
    };
    if !stop_refresh_is_current(&ui, generation) {
        return;
    }
    let offset = refresh.borrow().entries[entry_index].offset;
    let expression = stack_pointer_expression(stack_register, offset, depth);
    let command = format!(
        "-data-evaluate-expression {}",
        crate::debugger::quote(&expression)
    );
    let refresh_for_guard = Rc::clone(&refresh);
    let refresh_for_handler = Rc::clone(&refresh);
    if client
        .request_when(
            &command,
            move || {
                let state = refresh_for_guard.borrow();
                stop_refresh_is_current(&state.ui, state.generation)
            },
            move |client, record| {
                let (ui, generation) = {
                    let state = refresh_for_handler.borrow();
                    (state.ui.clone(), state.generation)
                };
                if !stop_refresh_is_current(&ui, generation) {
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
                        string_address =
                            stack_string_address(entry, address, depth, endian, word_size);
                        continue_chain = string_address.is_none()
                            && address != 0
                            && depth < MAX_POINTER_CHAIN_DEPTH;
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
            },
        )
        .is_err()
    {
        complete_stack_sequence(client, &refresh);
    }
}

pub(super) fn stack_string_address(
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

pub(super) fn request_stack_string(
    client: &MiClient,
    refresh: Rc<RefCell<StackRefresh>>,
    entry_index: usize,
    address: u64,
) {
    let (ui, generation) = {
        let state = refresh.borrow();
        (state.ui.clone(), state.generation)
    };
    if !stop_refresh_is_current(&ui, generation) {
        return;
    }
    let expression = format!("(char*)0x{address:x}");
    let command = format!(
        "-data-evaluate-expression {}",
        crate::debugger::quote(&expression)
    );
    let refresh_for_handler = Rc::clone(&refresh);
    let refresh_for_guard = Rc::clone(&refresh);
    if client
        .request_with_print_limit_when(
            &command,
            POINTER_STRING_PREVIEW_ELEMENTS,
            move || {
                let state = refresh_for_guard.borrow();
                stop_refresh_is_current(&state.ui, state.generation)
            },
            move |client, record| {
                let (ui, generation) = {
                    let state = refresh_for_handler.borrow();
                    (state.ui.clone(), state.generation)
                };
                if !stop_refresh_is_current(&ui, generation) {
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
            },
        )
        .is_err()
    {
        complete_stack_sequence(client, &refresh);
    }
}

pub(super) fn complete_stack_sequence(client: &MiClient, refresh: &Rc<RefCell<StackRefresh>>) {
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
            let generation = state.generation;
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

pub(super) fn stack_pointer_expression(register: &str, offset: usize, depth: usize) -> String {
    let mut expression = format!("*(void**)(${register}+0x{offset:x})");
    for _ in 0..depth {
        expression = format!("*(void**)({expression})");
    }
    expression
}
