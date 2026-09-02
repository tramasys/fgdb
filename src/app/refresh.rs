use super::*;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct LazyStopNeeds {
    stack: bool,
    memory: bool,
    tls: bool,
}

impl LazyStopNeeds {
    fn for_visibility(stack: bool, memory: bool, tls: bool) -> Self {
        Self { stack, memory, tls }
    }

    fn any(self) -> bool {
        self.stack || self.memory || self.tls
    }
}

pub(crate) fn refresh_stopped_state(ui: &Weak<Ui>, client: &MiClient) {
    let Some(current_ui) = ui.upgrade() else {
        return;
    };
    if current_ui.inferior_is_running() {
        return;
    }
    let Some(context) = current_ui.begin_stop_refresh(client.transport_epoch()) else {
        current_ui.set_debug_state_stale(true);
        current_ui.set_status(
            "Waiting for a stopped thread",
            "GDB did not identify a safe thread context for inspection. Refreshing the thread list.",
            Some("status-ready"),
        );
        drop(current_ui);
        refresh_threads(ui, client);
        return;
    };
    let generation = context.generation();
    client.cancel_stale_stop_requests(generation);
    drop(current_ui);

    let variable_update_batch = variable_update_batch(ui, generation, 2);

    let stack_inputs = Rc::new(RefCell::new(StackInputs {
        ui: ui.clone(),
        generation,
        frames: None,
        registers: None,
    }));

    let weak_ui = ui.clone();
    let weak_ui_for_guard = ui.clone();
    let stack_inputs_for_frames = Rc::clone(&stack_inputs);
    let frames_command = context.scope_thread("-stack-list-frames 0 24");
    if client
        .request_for_stop(
            &frames_command,
            generation,
            move || stop_refresh_is_current(&weak_ui_for_guard, generation),
            move |client, record| {
                if !stop_refresh_is_current(&weak_ui, generation) {
                    return;
                }
                let frames = if record.is_done() {
                    crate::debugger::stack_frames(&record)
                } else {
                    Vec::new()
                };
                if let Some(ui) = weak_ui.upgrade() {
                    ui.show_frames_for_refresh(generation, &frames);
                }
                stack_inputs_for_frames.borrow_mut().frames = Some(frames);
                start_stack_refresh_if_ready(&stack_inputs_for_frames, client);
            },
        )
        .is_err()
    {
        if let Some(ui) = stack_inputs.borrow().ui.upgrade() {
            ui.show_frames_for_refresh(generation, &[]);
        }
        stack_inputs.borrow_mut().frames = Some(Vec::new());
        start_stack_refresh_if_ready(&stack_inputs, client);
    }

    let weak_ui = ui.clone();
    let weak_ui_for_guard = ui.clone();
    let frame_command = context.scope_frame("-stack-info-frame");
    let _ = client.request_for_stop(
        &frame_command,
        generation,
        move || stop_refresh_is_current(&weak_ui_for_guard, generation),
        move |_, record| {
            if !stop_refresh_is_current(&weak_ui, generation) {
                return;
            }
            if let (Some(ui), Some(frame)) = (
                weak_ui.upgrade(),
                record
                    .is_done()
                    .then(|| crate::debugger::current_frame(&record))
                    .flatten(),
            ) {
                ui.show_execution_location(&frame);
                let pc = frame.address.clone();
                let architecture = frame.architecture;
                ui.request_disassembly_for_stop(pc, architecture);
            } else if let Some(ui) = weak_ui.upgrade() {
                ui.clear_execution_location();
            }
        },
    );

    let weak_ui = ui.clone();
    let weak_ui_for_guard = ui.clone();
    let variable_update_batch_for_locals = Rc::clone(&variable_update_batch);
    let variables_command = context.scope_frame("-stack-list-variables --simple-values");
    if client
        .request_with_print_limit_for_stop(
            &variables_command,
            AUTOMATIC_PRINT_ELEMENTS,
            generation,
            move || stop_refresh_is_current(&weak_ui_for_guard, generation),
            move |client, record| {
                if record.is_done() {
                    refresh_variable_objects(
                        weak_ui.clone(),
                        client,
                        generation,
                        crate::debugger::variables(&record),
                        variable_update_batch_for_locals,
                    );
                } else {
                    variable_update_batch_ready(client, &variable_update_batch_for_locals, None);
                }
            },
        )
        .is_err()
    {
        variable_update_batch_ready(client, &variable_update_batch, None);
    }

    refresh_registers(ui, client, generation, stack_inputs);

    refresh_expression_watches(
        ui.clone(),
        client,
        generation,
        Rc::clone(&variable_update_batch),
    );

    refresh_threads(ui, client);
}

pub(super) fn variable_update_batch(
    ui: &Weak<Ui>,
    generation: u64,
    preparations: usize,
) -> Rc<VariableUpdateBatch> {
    Rc::new(VariableUpdateBatch {
        ui: ui.clone(),
        generation,
        remaining_preparations: Cell::new(preparations),
        states: RefCell::new(Vec::with_capacity(preparations)),
        requested: Cell::new(false),
    })
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
        3 | 4 | 7 => {
            let Some(registers) = current_ui.registers_for_details(generation) else {
                return;
            };
            let Some(frames) = current_ui.frames_for_details(generation) else {
                return;
            };
            let pid = current_ui.inferior_pid();
            let debugger_pid = current_ui.debugger_pid();
            drop(current_ui);
            refresh_visible_stop_details(
                ui.clone(),
                client,
                generation,
                registers,
                frames,
                pid,
                debugger_pid,
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
    let weak_ui_for_guard = ui.clone();
    let stack_inputs_for_names = Rc::clone(&stack_inputs);
    if client
        .request_for_stop(
            "-data-list-register-names",
            generation,
            move || stop_refresh_is_current(&weak_ui_for_guard, generation),
            move |client, record| {
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
            },
        )
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
    let Some(command) = frame_scoped_stop_command(&ui, generation, &command) else {
        finish_empty_register_refresh(&ui, client, generation, &stack_inputs);
        return;
    };
    let ui_for_response = ui.clone();
    let ui_for_guard = ui.clone();
    let stack_inputs_for_response = Rc::clone(&stack_inputs);
    if client
        .request_for_stop(
            &command,
            generation,
            move || stop_refresh_is_current(&ui_for_guard, generation),
            move |client, record| {
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
                let registers_for_enrichment = ui_for_response.upgrade().and_then(|ui| {
                    ui.show_registers_for_refresh(generation, &registers);
                    ui.register_details_visible().then(|| registers.clone())
                });
                stack_inputs_for_response.borrow_mut().registers = Some(registers);
                start_stack_refresh_if_ready(&stack_inputs_for_response, client);
                if let Some(registers) = registers_for_enrichment {
                    enrich_registers(ui_for_response, client, generation, registers);
                }
            },
        )
        .is_err()
    {
        finish_empty_register_refresh(&ui, client, generation, &stack_inputs);
    }
}

pub(super) fn refresh_variable_objects(
    ui: Weak<Ui>,
    client: &MiClient,
    generation: u64,
    fallbacks: Vec<Variable>,
    update_batch: Rc<VariableUpdateBatch>,
) {
    let existing = ui
        .upgrade()
        .map(|ui| ui.local_variable_objects())
        .unwrap_or_default();
    refresh_persistent_variable_objects(
        ui,
        client,
        generation,
        fallbacks,
        existing,
        VariableRefreshTarget::Locals,
        update_batch,
    );
}

pub(super) fn refresh_expression_variable_objects(
    ui: Weak<Ui>,
    client: &MiClient,
    generation: u64,
    expressions: Vec<String>,
    update_batch: Rc<VariableUpdateBatch>,
) {
    let existing = ui
        .upgrade()
        .map(|ui| ui.expression_watch_variable_objects())
        .unwrap_or_default();
    let fallbacks = expressions
        .iter()
        .map(|expression| Variable {
            name: expression.clone(),
            value: String::from("<not available>"),
            type_name: None,
            argument: false,
            varobj: None,
            num_children: 0,
            has_more: false,
        })
        .collect();
    refresh_persistent_variable_objects(
        ui,
        client,
        generation,
        fallbacks,
        existing,
        VariableRefreshTarget::ExpressionWatches(expressions),
        update_batch,
    );
}

fn refresh_persistent_variable_objects(
    ui: Weak<Ui>,
    client: &MiClient,
    generation: u64,
    fallbacks: Vec<Variable>,
    existing: Vec<Variable>,
    target: VariableRefreshTarget,
    update_batch: Rc<VariableUpdateBatch>,
) {
    if let Some(ui) = ui.upgrade() {
        for varobj in ui.take_deferred_variable_object_deletions() {
            delete_variable_object(client, &varobj);
        }
    }
    let (variables, needs_update, stale) = reuse_variable_objects(&fallbacks, existing);
    for varobj in stale {
        delete_variable_object(client, &varobj);
    }
    if let Some(ui) = ui.upgrade() {
        show_variable_refresh(&ui, generation, &target, &variables);
    }
    let state = Rc::new(RefCell::new(VariableRefresh {
        ui,
        generation,
        target,
        variables,
        fallbacks,
        needs_update,
        next_index: 0,
        created: 0,
        created_varobjs: HashSet::new(),
        update_batch: Some(update_batch),
        bulk_completed: false,
    }));
    let requires_refresh = {
        let state = state.borrow();
        !state.fallbacks.is_empty()
            && state
                .target
                .requires_refresh(&state.fallbacks, &state.needs_update)
    };
    if requires_refresh {
        request_next_variable_object(client, state);
    } else {
        ready_variable_refresh_state(client, state);
    }
}

fn variable_refresh_is_current(state: &VariableRefresh) -> bool {
    state.ui.upgrade().is_some_and(|ui| {
        ui.is_stop_refresh_current(state.generation)
            && !ui.inferior_is_running()
            && match &state.target {
                VariableRefreshTarget::Locals => true,
                VariableRefreshTarget::ExpressionWatches(expressions) => {
                    ui.expression_watches_match(expressions)
                }
            }
    })
}

fn show_variable_refresh(
    ui: &Ui,
    generation: u64,
    target: &VariableRefreshTarget,
    variables: &[Variable],
) {
    match target {
        VariableRefreshTarget::Locals => ui.show_locals_for_refresh(generation, variables),
        VariableRefreshTarget::ExpressionWatches(_) => {
            ui.show_expression_watches_for_refresh(generation, variables);
        }
    }
}

fn reuse_variable_objects(
    fallbacks: &[Variable],
    existing: Vec<Variable>,
) -> (Vec<Variable>, Vec<bool>, Vec<String>) {
    let mut existing = existing.into_iter().fold(
        HashMap::<(String, bool), Vec<Variable>>::new(),
        |mut by_name, variable| {
            by_name
                .entry((variable.name.clone(), variable.argument))
                .or_default()
                .push(variable);
            by_name
        },
    );
    let reused = fallbacks
        .iter()
        .map(|fallback| {
            if !fallback.needs_variable_object() {
                return (fallback.clone(), false);
            }
            let key = (fallback.name.clone(), fallback.argument);
            let Some(mut variable) = existing.get_mut(&key).and_then(Vec::pop) else {
                return (fallback.clone(), false);
            };
            variable.name.clone_from(&fallback.name);
            variable.argument = fallback.argument;
            if fallback.type_name.is_some() {
                variable.type_name.clone_from(&fallback.type_name);
            }
            (variable, true)
        })
        .collect::<Vec<_>>();
    let (variables, needs_update) = reused.into_iter().unzip();
    let stale = existing
        .into_values()
        .flatten()
        .filter_map(|variable| variable.varobj)
        .collect();
    (variables, needs_update, stale)
}

pub(super) fn request_next_variable_object(client: &MiClient, state: Rc<RefCell<VariableRefresh>>) {
    if !variable_refresh_is_current(&state.borrow()) {
        discard_variable_refresh(client, &state);
        return;
    }
    let next = {
        let mut state = state.borrow_mut();
        if state.created >= MAX_AUTOMATIC_VARIABLE_OBJECTS {
            state.next_index = state.variables.len();
        }
        while state.next_index < state.variables.len()
            && (!state
                .target
                .creates_missing_variable_object(&state.fallbacks[state.next_index])
                || state.variables[state.next_index].varobj.is_some())
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
        if state.borrow().bulk_completed {
            finish_variable_refresh(state);
        } else {
            ready_variable_refresh_state(client, state);
        }
        return;
    };
    let varobj_name = next_variable_object_name();
    let command = format!(
        "-var-create {varobj_name} * {}",
        crate::debugger::quote(&display_name)
    );
    let command = {
        let state = state.borrow();
        frame_scoped_stop_command(&state.ui, state.generation, &command)
    };
    let Some(command) = command else {
        discard_variable_refresh(client, &state);
        return;
    };
    let state_for_response = Rc::clone(&state);
    let state_for_guard = Rc::clone(&state);
    let varobj_for_response = varobj_name;
    let generation = state.borrow().generation;
    if client
        .request_with_print_limit_for_stop(
            &command,
            AUTOMATIC_PRINT_ELEMENTS,
            generation,
            move || {
                let state = state_for_guard.borrow();
                variable_refresh_is_current(&state)
            },
            move |client, record| {
                let variable = record
                    .is_done()
                    .then(|| crate::debugger::variable_object(&record, &display_name))
                    .flatten();
                if !variable_refresh_is_current(&state_for_response.borrow()) {
                    // The scoped callback can be superseded after GDB already
                    // created the object. Its explicit name remains safe to
                    // delete even when the real response was quarantined.
                    delete_variable_object(client, &varobj_for_response);
                    discard_variable_refresh(client, &state_for_response);
                    return;
                }
                if let Some(mut variable) = variable {
                    let (ui, generation, target, shown) = {
                        let mut state = state_for_response.borrow_mut();
                        variable.argument = state.fallbacks[index].argument;
                        if let Some(varobj) = variable.varobj.as_ref() {
                            state.created_varobjs.insert(varobj.clone());
                        }
                        state.variables[index] = variable;
                        state.needs_update[index] = false;
                        (
                            state.ui.clone(),
                            state.generation,
                            variable_refresh_target_clone(&state.target),
                            state.variables[index].clone(),
                        )
                    };
                    if let Some(ui) = ui.upgrade() {
                        show_variable_root_refresh(&ui, generation, &target, index, &shown);
                    }
                } else {
                    delete_variable_object(client, &varobj_for_response);
                    if !record.is_done() {
                        state_for_response.borrow_mut().variables[index].value = format!(
                            "<error: {}>",
                            record
                                .error_message()
                                .unwrap_or("expression is unavailable")
                        );
                    }
                }
                request_next_variable_object(client, state_for_response);
            },
        )
        .is_err()
    {
        state.borrow_mut().variables[index].value =
            String::from("<error: MI channel is unavailable>");
        request_next_variable_object(client, state);
    }
}

fn ready_variable_refresh_state(client: &MiClient, state: Rc<RefCell<VariableRefresh>>) {
    let batch = state.borrow_mut().update_batch.take();
    let Some(batch) = batch else {
        finish_variable_refresh(state);
        return;
    };
    variable_update_batch_ready(client, &batch, Some(state));
}

fn variable_update_batch_ready(
    client: &MiClient,
    batch: &Rc<VariableUpdateBatch>,
    state: Option<Rc<RefCell<VariableRefresh>>>,
) {
    if let Some(state) = state {
        if variable_refresh_is_current(&state.borrow()) {
            batch.states.borrow_mut().push(state);
        } else {
            discard_variable_refresh(client, &state);
        }
    }
    let remaining = batch.remaining_preparations.get();
    if remaining == 0 {
        return;
    }
    batch.remaining_preparations.set(remaining - 1);
    if remaining != 1 || batch.requested.replace(true) {
        return;
    }

    let states = std::mem::take(&mut *batch.states.borrow_mut());
    if !variable_update_batch_is_current(batch) {
        for state in states {
            discard_variable_refresh(client, &state);
        }
        return;
    }
    if !states
        .iter()
        .any(|state| has_persistent_variable_objects(&state.borrow().variables))
    {
        for state in states {
            finish_variable_refresh(state);
        }
        return;
    }

    let batch_for_guard = Rc::clone(batch);
    let states_for_response = states.clone();
    if client
        .request_with_print_limit_for_stop(
            "-var-update --all-values *",
            AUTOMATIC_PRINT_ELEMENTS,
            batch.generation,
            move || variable_update_batch_is_current(&batch_for_guard),
            move |client, record| {
                apply_bulk_variable_updates(
                    client,
                    states_for_response,
                    record.is_done(),
                    crate::debugger::variable_updates(&record),
                );
            },
        )
        .is_err()
    {
        for state in states {
            finish_variable_refresh(state);
        }
    }
}

fn has_persistent_variable_objects(variables: &[Variable]) -> bool {
    variables.iter().any(|variable| variable.varobj.is_some())
}

fn variable_update_batch_is_current(batch: &VariableUpdateBatch) -> bool {
    batch
        .ui
        .upgrade()
        .is_some_and(|ui| ui.is_stop_refresh_current(batch.generation) && !ui.inferior_is_running())
}

fn apply_bulk_variable_updates(
    client: &MiClient,
    states: Vec<Rc<RefCell<VariableRefresh>>>,
    succeeded: bool,
    updates: Vec<crate::debugger::VariableUpdate>,
) {
    let mut current_states = Vec::with_capacity(states.len());
    for state in states {
        if variable_refresh_is_current(&state.borrow()) {
            current_states.push(state);
        } else {
            discard_variable_refresh(client, &state);
        }
    }
    let states = current_states;
    let roots = states
        .iter()
        .flat_map(|state| {
            state
                .borrow()
                .variables
                .iter()
                .filter_map(|variable| variable.varobj.clone())
                .collect::<Vec<_>>()
        })
        .collect::<HashSet<_>>();
    let descendants = updates
        .iter()
        .filter(|update| {
            !roots.contains(&update.varobj)
                && roots
                    .iter()
                    .any(|root| variable_object_owns_update(root, &update.varobj))
        })
        .cloned()
        .collect::<Vec<_>>();
    if let Some(state) = states.first()
        && let Some(ui) = state.borrow().ui.upgrade()
    {
        ui.show_variable_descendant_updates_for_refresh(state.borrow().generation, &descendants);
    }
    let updates = updates
        .iter()
        .map(|update| (update.varobj.as_str(), update))
        .collect::<HashMap<_, _>>();

    for state in states {
        let recreate = {
            let mut state = state.borrow_mut();
            let mut recreate = false;
            for index in 0..state.variables.len() {
                let Some(varobj) = state.variables[index].varobj.clone() else {
                    continue;
                };
                let reused = state.needs_update.get(index).copied().unwrap_or(false);
                let created = state.created_varobjs.contains(&varobj);
                if !reused && !created {
                    continue;
                }
                let update = updates.get(varobj.as_str()).copied();
                let invalid = (!succeeded && reused)
                    || update.is_some_and(|update| {
                        update.in_scope == Some(false) || update.type_changed
                    });
                if invalid {
                    delete_variable_object(client, &varobj);
                    state.created_varobjs.remove(&varobj);
                    state.variables[index] = state.fallbacks[index].clone();
                    recreate |= state
                        .target
                        .creates_missing_variable_object(&state.fallbacks[index]);
                } else if let Some(update) = update {
                    apply_variable_update(&mut state.variables[index], update);
                }
                state.needs_update[index] = false;
            }
            state.bulk_completed = true;
            recreate
        };
        if recreate {
            let mut refresh = state.borrow_mut();
            refresh.next_index = 0;
            refresh.created = 0;
            drop(refresh);
            request_next_variable_object(client, state);
        } else {
            finish_variable_refresh(state);
        }
    }
}

fn variable_object_owns_update(root: &str, candidate: &str) -> bool {
    candidate == root
        || candidate
            .strip_prefix(root)
            .is_some_and(|suffix| suffix.starts_with('.'))
}

fn apply_variable_update(variable: &mut Variable, update: &crate::debugger::VariableUpdate) {
    if let Some(value) = update.value.as_ref() {
        variable.value.clone_from(value);
    }
    if let Some(new_type) = update.new_type.as_ref() {
        variable.type_name = Some(new_type.clone());
    }
    if let Some(children) = update.new_num_children {
        variable.num_children = children;
    }
    if let Some(has_more) = update.has_more {
        variable.has_more = has_more;
    }
}

fn finish_variable_refresh(state: Rc<RefCell<VariableRefresh>>) {
    let (ui, generation, target, variables) = {
        let mut state = state.borrow_mut();
        (
            state.ui.clone(),
            state.generation,
            variable_refresh_target_clone(&state.target),
            std::mem::take(&mut state.variables),
        )
    };
    if let Some(ui) = ui.upgrade() {
        show_variable_refresh(&ui, generation, &target, &variables);
    }
}

fn variable_refresh_target_clone(target: &VariableRefreshTarget) -> VariableRefreshTarget {
    match target {
        VariableRefreshTarget::Locals => VariableRefreshTarget::Locals,
        VariableRefreshTarget::ExpressionWatches(expressions) => {
            VariableRefreshTarget::ExpressionWatches(expressions.clone())
        }
    }
}

impl VariableRefreshTarget {
    fn creates_missing_variable_object(&self, variable: &Variable) -> bool {
        match self {
            Self::Locals => variable.needs_eager_local_variable_object(),
            Self::ExpressionWatches(_) => variable.needs_variable_object(),
        }
    }

    fn requires_refresh(&self, fallbacks: &[Variable], needs_update: &[bool]) -> bool {
        needs_update.iter().any(|needs_update| *needs_update)
            || fallbacks
                .iter()
                .any(|variable| self.creates_missing_variable_object(variable))
    }
}

fn show_variable_root_refresh(
    ui: &Ui,
    generation: u64,
    target: &VariableRefreshTarget,
    index: usize,
    variable: &Variable,
) {
    match target {
        VariableRefreshTarget::Locals => {
            ui.show_local_root_for_refresh(generation, index, variable);
        }
        VariableRefreshTarget::ExpressionWatches(_) => {
            ui.show_expression_watch_root_for_refresh(generation, index, variable);
        }
    }
}

pub(super) fn stop_refresh_is_current(ui: &Weak<Ui>, generation: u64) -> bool {
    ui.upgrade()
        .is_some_and(|ui| ui.is_stop_refresh_current(generation))
}

pub(crate) fn frame_scoped_stop_command(
    ui: &Weak<Ui>,
    generation: u64,
    command: &str,
) -> Option<String> {
    ui.upgrade()?
        .stop_context(generation)
        .map(|context| context.scope_frame(command))
}

thread_local! {
    static OWNED_VARIABLE_OBJECTS: RefCell<HashMap<String, HashSet<String>>> =
        RefCell::new(HashMap::new());
}

fn register_owned_variable_object(owner: &str, child: &str) {
    if owner == child {
        return;
    }
    OWNED_VARIABLE_OBJECTS.with(|owned| {
        owned
            .borrow_mut()
            .entry(owner.to_owned())
            .or_default()
            .insert(child.to_owned());
    });
}

pub(super) fn delete_variable_object(client: &MiClient, varobj: &str) {
    let objects = OWNED_VARIABLE_OBJECTS
        .with(|owned| take_owned_variable_objects(&mut owned.borrow_mut(), varobj));
    for object in objects.into_iter().rev() {
        let command = format!("-var-delete {}", crate::debugger::quote(&object));
        // Parent deletion can legitimately remove child objects also present
        // in the expanded UI tree, so consume any resulting errors locally.
        let _ = client.request(&command, |_, _| {});
    }
}

fn take_owned_variable_objects(
    owned: &mut HashMap<String, HashSet<String>>,
    root: &str,
) -> Vec<String> {
    let mut objects = vec![root.to_owned()];
    let mut visited = HashSet::from([root.to_owned()]);
    let mut index = 0;
    while index < objects.len() {
        if let Some(children) = owned.remove(&objects[index]) {
            for child in children {
                if visited.insert(child.clone()) {
                    objects.push(child);
                }
            }
        }
        index += 1;
    }
    owned.retain(|_, children| {
        children.retain(|child| !visited.contains(child));
        !children.is_empty()
    });
    objects
}

pub(super) fn discard_variable_refresh(client: &MiClient, state: &Rc<RefCell<VariableRefresh>>) {
    let (created_varobjs, batch) = {
        let mut state = state.borrow_mut();
        (
            std::mem::take(&mut state.created_varobjs),
            state.update_batch.take(),
        )
    };
    for varobj in &created_varobjs {
        delete_variable_object(client, varobj);
    }
    if let Some(batch) = batch {
        variable_update_batch_ready(client, &batch, None);
    }
}

pub(super) fn request_variable_children(
    ui: Weak<Ui>,
    client: Rc<MiClient>,
    variable: Variable,
    from: usize,
) {
    let Some(varobj) = variable.varobj.clone() else {
        request_lazy_local_variable_children(ui, client, variable, from);
        return;
    };
    let Some(generation) = ui.upgrade().and_then(|current_ui| {
        let generation = current_ui.current_stop_refresh_generation();
        current_ui.stop_context(generation).map(|_| generation)
    }) else {
        return;
    };
    // Dynamic varobjs may advertise available pretty-printed children only
    // through `has_more`; GDB documents `numchild` as unreliable for them.
    if variable.num_children > 0 || variable.has_more {
        let Some(to) = variable_child_page_end(from) else {
            if let Some(ui) = ui.upgrade() {
                ui.show_variable_children_page(&variable, from, &[], false);
            }
            return;
        };
        let command = format!(
            "-var-list-children --all-values {} {from} {to}",
            crate::debugger::quote(&varobj),
        );
        let ui_for_response = ui.clone();
        let ui_for_guard = ui.clone();
        let varobj_for_guard = varobj.clone();
        let variable_for_response = variable.clone();
        if let Err(error) = client.request_with_print_limit_for_stop(
            &command,
            to,
            generation,
            move || {
                ui_for_guard.upgrade().is_some_and(|ui| {
                    ui.is_stop_refresh_current(generation)
                        && ui.has_variable_object(&varobj_for_guard)
                })
            },
            move |_, record| {
                if let Some(ui) = ui_for_response.upgrade() {
                    if record.is_done() {
                        let children = crate::debugger::variable_children(&record);
                        let next = from.saturating_add(children.len());
                        let has_more = next < MAX_VARIABLE_CHILDREN
                            && !children.is_empty()
                            && (crate::debugger::variable_children_have_more(&record)
                                || next < variable_for_response.num_children);
                        ui.show_variable_children_page(
                            &variable_for_response,
                            from,
                            &children,
                            has_more,
                        );
                    } else {
                        ui.show_variable_children_page_error(
                            &variable_for_response,
                            from,
                            record
                                .error_message()
                                .unwrap_or("GDB could not expand this value"),
                        );
                    }
                }
            },
        ) && let Some(ui) = ui.upgrade()
        {
            ui.show_variable_children_page_error(&variable, from, &error.to_string());
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
    let ui_for_path_guard = ui.clone();
    let varobj_for_path_guard = varobj.clone();
    if client
        .request_for_stop(
            &command,
            generation,
            move || {
                ui_for_path_guard.upgrade().is_some_and(|ui| {
                    ui.is_stop_refresh_current(generation)
                        && ui.has_variable_object(&varobj_for_path_guard)
                })
            },
            move |_, record| {
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
                let dereference_varobj = next_variable_object_name();
                let command = format!(
                    "-var-create {dereference_varobj} * {}",
                    crate::debugger::quote(&format!("*({path})"))
                );
                let command = ui_for_path.upgrade().and_then(|ui| {
                    ui.stop_context(generation)
                        .map(|context| context.scope_frame(&command))
                });
                let Some(command) = command else {
                    return;
                };
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
                let dereference_varobj_for_response = dereference_varobj;
                if client_for_path
                    .request_with_print_limit_for_stop(
                        &command,
                        AUTOMATIC_PRINT_ELEMENTS,
                        generation,
                        move || {
                            ui_for_guard.upgrade().is_some_and(|ui| {
                                ui.is_stop_refresh_current(generation)
                                    && ui.has_variable_object(&varobj_for_guard)
                            })
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
                                if attached {
                                    register_owned_variable_object(
                                        &varobj_for_dereference,
                                        &dereference_varobj_for_response,
                                    );
                                } else {
                                    delete_variable_object(
                                        client,
                                        &dereference_varobj_for_response,
                                    );
                                }
                            } else if let Some(ui) = ui_for_dereference.upgrade() {
                                delete_variable_object(client, &dereference_varobj_for_response);
                                ui.show_variable_children_error(
                                    &varobj_for_dereference,
                                    record
                                        .error_message()
                                        .unwrap_or("GDB cannot dereference this pointer"),
                                );
                            } else {
                                delete_variable_object(client, &dereference_varobj_for_response);
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
            },
        )
        .is_err()
        && let Some(ui) = ui.upgrade()
    {
        ui.show_variable_children_error(&varobj, "The MI channel is unavailable");
    }
}

fn variable_child_page_end(from: usize) -> Option<usize> {
    (from < MAX_VARIABLE_CHILDREN).then(|| {
        from.saturating_add(VARIABLE_CHILD_PAGE_SIZE)
            .min(MAX_VARIABLE_CHILDREN)
    })
}

fn request_lazy_local_variable_children(
    ui: Weak<Ui>,
    client: Rc<MiClient>,
    variable: Variable,
    from: usize,
) {
    let Some(current_ui) = ui.upgrade() else {
        return;
    };
    let generation = current_ui.current_stop_refresh_generation();
    if from != 0 || !current_ui.claim_local_variable_object(generation, &variable) {
        return;
    }
    drop(current_ui);

    let varobj = next_variable_object_name();
    let command = format!(
        "-var-create {varobj} * {}",
        crate::debugger::quote(&variable.name)
    );
    let Some(command) = frame_scoped_stop_command(&ui, generation, &command) else {
        if let Some(ui) = ui.upgrade() {
            ui.finish_local_variable_object(generation, &variable);
        }
        return;
    };
    let ui_for_guard = ui.clone();
    let variable_for_guard = variable.clone();
    let ui_for_response = ui.clone();
    let variable_for_response = variable.clone();
    let client_for_response = Rc::clone(&client);
    let varobj_for_response = varobj.clone();
    if client
        .request_with_print_limit_for_stop(
            &command,
            AUTOMATIC_PRINT_ELEMENTS,
            generation,
            move || {
                ui_for_guard.upgrade().is_some_and(|ui| {
                    ui.is_stop_refresh_current(generation)
                        && ui.has_local_variable_identity(&variable_for_guard)
                })
            },
            move |client, record| {
                if let Some(ui) = ui_for_response.upgrade() {
                    ui.finish_local_variable_object(generation, &variable_for_response);
                }
                let created = record
                    .is_done()
                    .then(|| crate::debugger::variable_object(&record, &variable_for_response.name))
                    .flatten()
                    .map(|mut created| {
                        created.argument = variable_for_response.argument;
                        created
                    });
                let Some(created) = created else {
                    delete_variable_object(client, &varobj_for_response);
                    if let Some(ui) = ui_for_response.upgrade()
                        && ui.is_stop_refresh_current(generation)
                    {
                        ui.show_lazy_variable_children_error(
                            &variable_for_response,
                            record
                                .error_message()
                                .unwrap_or("GDB could not inspect this pointer"),
                        );
                    }
                    return;
                };
                let attached = ui_for_response.upgrade().is_some_and(|ui| {
                    ui.attach_local_variable_object(generation, &variable_for_response, &created)
                });
                if attached {
                    request_variable_children(
                        ui_for_response.clone(),
                        Rc::clone(&client_for_response),
                        created,
                        0,
                    );
                } else {
                    delete_variable_object(client, &varobj_for_response);
                }
            },
        )
        .is_err()
        && let Some(ui) = ui.upgrade()
    {
        ui.finish_local_variable_object(generation, &variable);
        if ui.is_stop_refresh_current(generation) && ui.has_local_variable_identity(&variable) {
            ui.show_lazy_variable_children_error(&variable, "The MI channel is unavailable");
        }
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
    let Some(command) = frame_scoped_stop_command(&ui, generation, &command) else {
        complete_register_sequence(client, &refresh);
        return;
    };
    let refresh_for_guard = Rc::clone(&refresh);
    let refresh_for_handler = Rc::clone(&refresh);
    if client
        .request_for_stop(
            &command,
            generation,
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
    let Some(command) = frame_scoped_stop_command(&ui, generation, &command) else {
        complete_register_sequence(client, &refresh);
        return;
    };
    let refresh_for_handler = Rc::clone(&refresh);
    let refresh_for_guard = Rc::clone(&refresh);
    if client
        .request_with_print_limit_for_stop(
            &command,
            POINTER_STRING_PREVIEW_ELEMENTS,
            generation,
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
    let cached = ui.upgrade().map(|ui| {
        (
            ui.inferior_pid(),
            ui.debugger_pid(),
            ui.selected_inferior_id(),
        )
    });
    if let Some((Some(pid), debugger_pid, _)) = cached.as_ref() {
        continue_stack_refresh(
            ui,
            client,
            generation,
            registers,
            frames,
            Some(*pid),
            *debugger_pid,
        );
        return;
    }
    let selected_inferior = cached.and_then(|(_, _, selected)| selected);
    let ui_for_request = ui.clone();
    let ui_for_guard = ui.clone();
    if client
        .request_for_stop(
            "-list-thread-groups",
            generation,
            move || stop_refresh_is_current(&ui_for_guard, generation),
            move |client, record| {
                if !stop_refresh_is_current(&ui, generation) {
                    return;
                }
                let pid = selected_inferior
                    .as_deref()
                    .and_then(|id| crate::debugger::inferior_pid_for_group(&record, id))
                    .or_else(|| crate::debugger::inferior_pid(&record));
                let debugger_pid = ui.upgrade().and_then(|ui| ui.debugger_pid());
                continue_stack_refresh(
                    ui,
                    client,
                    generation,
                    registers,
                    frames,
                    pid,
                    debugger_pid,
                );
            },
        )
        .is_err()
        && let Some(ui) = ui_for_request.upgrade()
    {
        let needs = LazyStopNeeds::for_visibility(
            ui.stack_details_visible(),
            ui.memory_details_visible(),
            ui.tls_details_visible(),
        );
        if needs.stack {
            ui.show_stack_for_refresh(generation, &[]);
        }
        if needs.any() {
            ui.show_memory_regions_for_refresh(generation, &[]);
        }
        if needs.memory && ui.claim_memory_watches_refresh(generation) {
            ui.refresh_memory_watches();
        }
        if needs.tls && ui.claim_tls_runtime_refresh(generation) {
            ui.show_tls_runtime_unavailable_for_refresh(
                generation,
                "The inferior process identity is unavailable",
            );
        }
        ui.refresh_kernel_after_stop();
        ui.refresh_misc_after_stop();
    }
}

fn continue_stack_refresh(
    ui: Weak<Ui>,
    client: &MiClient,
    generation: u64,
    registers: Vec<Register>,
    frames: Vec<StackFrame>,
    pid: Option<u32>,
    debugger_pid: Option<u32>,
) {
    if !stop_refresh_is_current(&ui, generation) {
        return;
    }
    if let Some((architecture, endian, pointer_bits)) = pid
        .zip(debugger_pid)
        .and_then(|(pid, debugger_pid)| crate::kernel::read_local_target_abi(pid, debugger_pid))
        && let Some(current_ui) = ui.upgrade()
    {
        let previous = (
            current_ui.target_architecture(),
            current_ui.target_endian(),
            current_ui.target_pointer_bits(),
        );
        // An ELF class and byte order remain useful even when this fgdb build
        // does not recognize e_machine. Do not let a future machine erase a
        // more specific GDB result.
        if architecture != TargetArchitecture::Unknown {
            current_ui.set_target_architecture(architecture);
        }
        current_ui.set_target_endian(Some(endian));
        current_ui.set_target_pointer_bits(pointer_bits);
        let current = (
            current_ui.target_architecture(),
            current_ui.target_endian(),
            current_ui.target_pointer_bits(),
        );
        // Rebind only when ELF discovery actually refined the target. The
        // former unconditional pass rebuilt every register row on each stop.
        if previous != current
            && let Some(current_registers) = current_ui.registers_for_details(generation)
        {
            current_ui.show_registers_for_refresh(generation, &current_registers);
            // ABI discovery may make pointer enrichment runnable after the
            // initial register response. The generation claim prevents a
            // duplicate active or completed attempt.
            enrich_registers(ui.clone(), client, generation, current_registers);
        }
    }
    if let Some(current_ui) = ui.upgrade() {
        current_ui.set_inferior_pid(pid);
        if pid.is_some() {
            current_ui.set_inferior_started(true);
        }
        current_ui.show_call_abi_for_refresh(generation, &frames);
        current_ui.refresh_kernel_after_stop();
        current_ui.refresh_misc_after_stop();
    }
    refresh_visible_stop_details(ui, client, generation, registers, frames, pid, debugger_pid);
}

fn refresh_visible_stop_details(
    ui: Weak<Ui>,
    client: &MiClient,
    generation: u64,
    registers: Vec<Register>,
    frames: Vec<StackFrame>,
    pid: Option<u32>,
    debugger_pid: Option<u32>,
) {
    if !stop_refresh_is_current(&ui, generation) {
        return;
    }
    let Some(current_ui) = ui.upgrade() else {
        return;
    };
    let needs = LazyStopNeeds::for_visibility(
        current_ui.stack_details_visible(),
        current_ui.memory_details_visible(),
        current_ui.tls_details_visible(),
    );
    if !needs.any() {
        return;
    }
    let architecture = current_ui.target_architecture();
    let architecture = if architecture == TargetArchitecture::Unknown {
        TargetArchitecture::infer_from_register_names_with_bits(
            registers.iter().map(|register| register.name.as_str()),
            Some(current_ui.target_pointer_bits()),
        )
    } else {
        architecture
    };
    let regions =
        memory_regions_for_stop(&ui, generation, pid, debugger_pid, &registers, architecture);

    if needs.memory && current_ui.claim_memory_watches_refresh(generation) {
        current_ui.refresh_memory_watches();
    }
    if needs.tls && current_ui.claim_tls_runtime_refresh(generation) {
        drop(current_ui);
        request_tls_runtime(&ui, client, generation, &registers, &regions, architecture);
    } else {
        drop(current_ui);
    }

    if !needs.stack {
        return;
    }
    let Some(current_ui) = ui.upgrade() else {
        return;
    };
    if let Some(entries) = current_ui.stack_for_details(generation) {
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
            ui,
            client,
            generation,
            entries,
            stack_register,
            word_size,
            endian,
        );
    } else if current_ui.claim_stack_memory_refresh(generation) {
        drop(current_ui);
        request_stack_memory(ui, client, generation, registers, frames, regions);
    }
}

fn memory_regions_for_stop(
    ui: &Weak<Ui>,
    generation: u64,
    pid: Option<u32>,
    debugger_pid: Option<u32>,
    registers: &[Register],
    architecture: TargetArchitecture,
) -> Vec<MemoryRegion> {
    let Some(current_ui) = ui.upgrade() else {
        return Vec::new();
    };
    if let Some(regions) = current_ui.memory_regions_for_details(generation) {
        return regions;
    }
    drop(current_ui);

    let mut regions = pid
        .zip(debugger_pid)
        .map(|(pid, debugger_pid)| read_memory_regions(pid, debugger_pid))
        .unwrap_or_default();
    annotate_memory_regions(&mut regions, registers, architecture);
    if let Some(current_ui) = ui.upgrade() {
        current_ui.show_memory_regions_for_refresh(generation, &regions);
    }
    regions
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
    let Some(command) = frame_scoped_stop_command(ui, generation, &command) else {
        return;
    };
    let ui_for_response = ui.clone();
    let ui_for_guard = ui.clone();
    let ui_for_error = ui.clone();
    let mapping_for_response = mapping.clone();
    if client
        .request_for_stop(
            &command,
            generation,
            move || stop_refresh_is_current(&ui_for_guard, generation),
            move |_, record| {
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
            },
        )
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
    let Some(command) = frame_scoped_stop_command(&ui, generation, &command) else {
        return;
    };
    let ui_for_request = ui.clone();
    let ui_for_guard = ui.clone();
    if client
        .request_for_stop(
            &command,
            generation,
            move || stop_refresh_is_current(&ui_for_guard, generation),
            move |client, record| {
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
            },
        )
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
    let Some(command) = frame_scoped_stop_command(&ui, generation, &command) else {
        complete_stack_sequence(client, &refresh);
        return;
    };
    let refresh_for_guard = Rc::clone(&refresh);
    let refresh_for_handler = Rc::clone(&refresh);
    if client
        .request_for_stop(
            &command,
            generation,
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
    let Some(command) = frame_scoped_stop_command(&ui, generation, &command) else {
        complete_stack_sequence(client, &refresh);
        return;
    };
    let refresh_for_handler = Rc::clone(&refresh);
    let refresh_for_guard = Rc::clone(&refresh);
    if client
        .request_with_print_limit_for_stop(
            &command,
            POINTER_STRING_PREVIEW_ELEMENTS,
            generation,
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

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use super::{
        LazyStopNeeds, Variable, VariableRefreshTarget, apply_variable_update,
        has_persistent_variable_objects, reuse_variable_objects, take_owned_variable_objects,
        variable_child_page_end, variable_object_owns_update,
    };

    fn variable(
        name: &str,
        value: &str,
        type_name: Option<&str>,
        varobj: Option<&str>,
    ) -> Variable {
        Variable {
            name: name.to_owned(),
            value: value.to_owned(),
            type_name: type_name.map(str::to_owned),
            argument: false,
            varobj: varobj.map(str::to_owned),
            num_children: usize::from(varobj.is_some()),
            has_more: false,
        }
    }

    #[test]
    fn reuses_live_roots_and_discards_only_stale_variable_objects() {
        let fallbacks = vec![
            variable("pointer", "0x20", Some("Node *"), None),
            variable("count", "7", Some("int"), None),
        ];
        let existing = vec![
            variable("pointer", "0x10", Some("Node *"), Some("var1")),
            variable("removed", "0x30", Some("Node *"), Some("var2")),
        ];

        let (reused, needs_update, stale) = reuse_variable_objects(&fallbacks, existing);

        assert_eq!(reused[0].varobj.as_deref(), Some("var1"));
        assert_eq!(reused[0].value, "0x10");
        assert_eq!(reused[1], fallbacks[1]);
        assert_eq!(needs_update, [true, false]);
        assert_eq!(stale, [String::from("var2")]);
    }

    #[test]
    fn creates_local_pointer_objects_only_after_they_are_requested() {
        let pointer = variable("pointer", "0x20", Some("Node *"), None);
        let aggregate = variable("fixture", "<not available>", Some("struct Fixture"), None);

        assert!(!VariableRefreshTarget::Locals.creates_missing_variable_object(&pointer));
        assert!(VariableRefreshTarget::Locals.creates_missing_variable_object(&aggregate));
        assert!(
            VariableRefreshTarget::ExpressionWatches(Vec::new())
                .creates_missing_variable_object(&pointer)
        );
    }

    #[test]
    fn bounds_dynamic_variable_pages_while_allowing_later_pages() {
        assert_eq!(variable_child_page_end(0), Some(128));
        assert_eq!(variable_child_page_end(128), Some(256));
        assert_eq!(variable_child_page_end(4_000), Some(4_096));
        assert_eq!(variable_child_page_end(4_096), None);
        assert_eq!(variable_child_page_end(usize::MAX), None);
    }

    #[test]
    fn refreshes_existing_lazy_local_objects_without_creating_new_ones() {
        let pointer = variable("pointer", "0x20", Some("Node *"), None);
        let target = VariableRefreshTarget::Locals;

        assert!(!target.requires_refresh(std::slice::from_ref(&pointer), &[false]));
        assert!(target.requires_refresh(std::slice::from_ref(&pointer), &[true]));
    }

    #[test]
    fn bulk_updates_route_only_to_owned_roots_and_descendants() {
        assert!(variable_object_owns_update("fgdb_var_1", "fgdb_var_1"));
        assert!(variable_object_owns_update(
            "fgdb_var_1",
            "fgdb_var_1.public.next"
        ));
        assert!(!variable_object_owns_update(
            "fgdb_var_1",
            "fgdb_var_10.public"
        ));
        assert!(!variable_object_owns_update("fgdb_var_1", "temporary"));
    }

    #[test]
    fn newly_created_persistent_roots_participate_in_the_bulk_update() {
        let created = variable("items", "{...}", Some("Vec<int>"), Some("fgdb_var_1"));
        let scalar = variable("count", "4", Some("int"), None);

        assert!(has_persistent_variable_objects(&[created]));
        assert!(!has_persistent_variable_objects(&[scalar]));
    }

    #[test]
    fn bulk_root_updates_preserve_the_existing_update_semantics() {
        let mut root = variable("value", "old", Some("Old"), Some("fgdb_var_1"));
        let update = crate::debugger::VariableUpdate {
            varobj: String::from("fgdb_var_1"),
            value: Some(String::from("new")),
            in_scope: Some(true),
            type_changed: false,
            new_type: Some(String::from("New")),
            new_num_children: Some(4),
            has_more: Some(true),
        };

        apply_variable_update(&mut root, &update);

        assert_eq!(root.value, "new");
        assert_eq!(root.type_name.as_deref(), Some("New"));
        assert_eq!(root.num_children, 4);
        assert!(root.has_more);
    }

    #[test]
    fn hidden_inspectors_schedule_no_memory_heavy_stop_work() {
        let needs = LazyStopNeeds::for_visibility(false, false, false);

        assert!(!needs.any());
        assert!(!needs.stack);
        assert!(!needs.memory);
        assert!(!needs.tls);
    }

    #[test]
    fn each_visible_inspector_requests_only_its_lazy_stop_work() {
        assert_eq!(
            LazyStopNeeds::for_visibility(true, false, false),
            LazyStopNeeds {
                stack: true,
                memory: false,
                tls: false,
            }
        );
        assert_eq!(
            LazyStopNeeds::for_visibility(false, true, false),
            LazyStopNeeds {
                stack: false,
                memory: true,
                tls: false,
            }
        );
        assert_eq!(
            LazyStopNeeds::for_visibility(false, false, true),
            LazyStopNeeds {
                stack: false,
                memory: false,
                tls: true,
            }
        );
    }

    #[test]
    fn deletes_independent_dereference_objects_with_their_owner() {
        let mut owned = HashMap::from([
            (
                String::from("root"),
                HashSet::from([String::from("child"), String::from("sibling")]),
            ),
            (
                String::from("child"),
                HashSet::from([String::from("grandchild")]),
            ),
            (
                String::from("other"),
                HashSet::from([String::from("sibling")]),
            ),
        ]);

        let removed = take_owned_variable_objects(&mut owned, "root");

        assert_eq!(removed.first().map(String::as_str), Some("root"));
        assert_eq!(removed.iter().collect::<HashSet<_>>().len(), 4);
        assert!(owned.is_empty());
    }
}
