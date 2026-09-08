use super::*;

const VARIABLE_CHILD_PAGE_SIZE: usize = 128;
const MAX_VARIABLE_CHILDREN: usize = 4096;

// Keep stop refresh responsive even in generated or macro-heavy frames.
// Remaining aggregate roots stay visible and create their varobj lazily when
// the user expands them.
const MAX_AUTOMATIC_VARIABLE_OBJECTS: usize = 32;

pub(in crate::app) struct VariableRefresh {
    ui: Weak<Ui>,
    requests: StopRequests,
    target: VariableRefreshTarget,
    symbol_revision: u64,
    variables: Vec<Variable>,
    fallbacks: Vec<Variable>,
    needs_update: Vec<bool>,
    next_index: usize,
    created: usize,
    automatic_creation_indices: HashSet<usize>,
    created_varobjs: HashSet<String>,
    update_batch: Option<Rc<VariableUpdateBatch>>,
    bulk_completed: bool,
}

#[derive(Clone)]
enum VariableRefreshTarget {
    Locals,
    ExpressionWatches(Vec<String>),
}

pub(in crate::app) struct VariableUpdateBatch {
    requests: StopRequests,
    remaining_preparations: Cell<usize>,
    states: RefCell<Vec<Rc<RefCell<VariableRefresh>>>>,
    requested: Cell<bool>,
}

pub(in crate::app) fn variable_update_batch(
    requests: StopRequests,
    preparations: usize,
) -> Rc<VariableUpdateBatch> {
    Rc::new(VariableUpdateBatch {
        requests,
        remaining_preparations: Cell::new(preparations),
        states: RefCell::new(Vec::with_capacity(preparations)),
        requested: Cell::new(false),
    })
}

pub(in crate::app) fn refresh_variable_objects(
    ui: Weak<Ui>,
    client: &MiClient,
    generation: u64,
    fallbacks: Vec<Variable>,
    update_batch: Rc<VariableUpdateBatch>,
) {
    let existing = ui
        .upgrade()
        .map(|ui| ui.local_variable_objects_for_refresh())
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

pub(in crate::app) fn refresh_expression_variable_objects(
    ui: Weak<Ui>,
    client: &MiClient,
    generation: u64,
    expressions: Vec<String>,
    update_batch: Rc<VariableUpdateBatch>,
) {
    let existing = ui
        .upgrade()
        .map(|ui| ui.expression_watch_variable_objects_for_refresh())
        .unwrap_or_default();

    let fallbacks = expressions
        .iter()
        .map(|expression| Variable {
            local_index: None,
            name: expression.clone(),
            value: String::from("<not available>"),
            type_name: None,
            argument: false,
            varobj: None,
            num_children: 0,
            has_more: false,
            display_hint: None,
            dynamic: false,
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
    mut fallbacks: Vec<Variable>,
    existing: Vec<Variable>,
    target: VariableRefreshTarget,
    update_batch: Rc<VariableUpdateBatch>,
) {
    if let Some(ui) = ui.upgrade() {
        for varobj in ui.take_deferred_variable_object_deletions() {
            delete_variable_object(client, &varobj);
        }
    }

    if matches!(target, VariableRefreshTarget::Locals) {
        for (index, variable) in fallbacks.iter_mut().enumerate() {
            variable.local_index = Some(index);
        }
    }

    let (variables, needs_update, stale) = reuse_variable_objects(&fallbacks, existing);

    for varobj in stale {
        delete_variable_object(client, &varobj);
    }

    // Locals publish one completed snapshot. The simple-values response omits
    // aggregates, so publishing it here replaces good values with placeholders.
    if matches!(target, VariableRefreshTarget::ExpressionWatches(_))
        && let Some(ui) = ui.upgrade()
    {
        show_variable_refresh(&ui, generation, &target, &variables);
    }

    let automatic_creation_indices = match &target {
        VariableRefreshTarget::Locals => ui.upgrade().map_or_else(HashSet::new, |ui| {
            ui.local_variable_refresh_indices(&variables)
        }),
        VariableRefreshTarget::ExpressionWatches(_) => (0..variables.len()).collect(),
    };

    let symbol_revision = ui.upgrade().map_or(0, |ui| ui.model.symbols.revision());

    let state = Rc::new(RefCell::new(VariableRefresh {
        ui,
        requests: update_batch.requests.clone(),
        target,
        symbol_revision,
        variables,
        fallbacks,
        needs_update,
        next_index: 0,
        created: 0,
        automatic_creation_indices,
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
    state.requests.is_current()
        && state.ui.upgrade().is_some_and(|ui| {
            ui.model.symbols.revision() == state.symbol_revision
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
    // Borrow keys from the stable incoming snapshot. Scalars need no reuse
    // bucket, and matching an existing object needs no temporary name clone.
    let mut reusable = HashMap::new();
    let mut buckets: Vec<Vec<Variable>> = Vec::new();

    for variable in fallbacks
        .iter()
        .filter(|variable| variable.needs_variable_object())
    {
        let key = (
            variable.name.as_str(),
            variable.argument,
            variable.local_index,
        );

        if let std::collections::hash_map::Entry::Vacant(entry) = reusable.entry(key) {
            entry.insert(buckets.len());
            buckets.push(Vec::new());
        }
    }

    let mut stale = Vec::new();

    for variable in existing {
        let key = (
            variable.name.as_str(),
            variable.argument,
            variable.local_index,
        );

        if let Some(&index) = reusable.get(&key) {
            buckets[index].push(variable);
        } else if let Some(varobj) = variable.varobj {
            stale.push(varobj);
        }
    }

    let (variables, needs_update) = fallbacks
        .iter()
        .map(|fallback| {
            if !fallback.needs_variable_object() {
                return (fallback.clone(), false);
            }

            let key = (
                fallback.name.as_str(),
                fallback.argument,
                fallback.local_index,
            );

            let Some(mut variable) = reusable.get(&key).and_then(|&index| buckets[index].pop())
            else {
                return (fallback.clone(), false);
            };

            if fallback.type_name.is_some() {
                variable.type_name.clone_from(&fallback.type_name);
            }

            (variable, true)
        })
        .unzip();

    stale.extend(
        buckets
            .into_iter()
            .flatten()
            .filter_map(|variable| variable.varobj),
    );

    (variables, needs_update, stale)
}

pub(in crate::app) fn request_next_variable_object(
    client: &MiClient,
    state: Rc<RefCell<VariableRefresh>>,
) {
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
            && (!state.automatic_creation_indices.contains(&state.next_index)
                || !state
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
            finish_variable_refresh(client, state);
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

    let requests = state.borrow().requests.clone();

    let state_for_response = Rc::clone(&state);
    let state_for_guard = Rc::clone(&state);
    let varobj_for_response = varobj_name;
    if requests
        .frame(&command)
        .when(move || {
            let state = state_for_guard.borrow();

            variable_refresh_is_current(&state)
        })
        .with_print_limit(AUTOMATIC_PRINT_ELEMENTS, move |client, record| {
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
                    variable.local_index = state.fallbacks[index].local_index;

                    if let Some(varobj) = variable.varobj.as_ref() {
                        state.created_varobjs.insert(varobj.clone());
                    }

                    state.variables[index] = variable;
                    state.needs_update[index] = false;

                    (
                        state.ui.clone(),
                        state.requests.generation(),
                        state.target.clone(),
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
        })
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
        finish_variable_refresh(client, state);
        return;
    };

    variable_update_batch_ready(client, &batch, Some(state));
}

pub(super) fn variable_update_batch_ready(
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
            finish_variable_refresh(client, state);
        }

        return;
    }

    let requests = batch.requests.clone();

    let batch_for_guard = Rc::clone(batch);
    let states_for_response = states.clone();

    if requests
        .unscoped("-var-update --all-values *")
        .when(move || variable_update_batch_is_current(&batch_for_guard))
        .with_print_limit(AUTOMATIC_PRINT_ELEMENTS, move |client, record| {
            apply_bulk_variable_updates(
                client,
                states_for_response,
                record.is_done(),
                crate::debugger::variable_updates(&record),
            );
        })
        .is_err()
    {
        for state in states {
            finish_variable_refresh(client, state);
        }
    }
}

fn has_persistent_variable_objects(variables: &[Variable]) -> bool {
    variables.iter().any(|variable| variable.varobj.is_some())
}

fn variable_update_batch_is_current(batch: &VariableUpdateBatch) -> bool {
    batch.requests.is_current()
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

    // Dynamic pretty printers reuse child slots in GDB, even when a variant
    // changes their types. Recreate the owning root before publishing any of
    // its descendants, including changes in nested variants.
    let changed_variants = updates
        .iter()
        .filter(|update| update.invalidates_variant_children())
        .filter_map(|update| variable_object_owned_root(&roots, &update.varobj).cloned())
        .collect::<HashSet<_>>();

    let descendants = updates
        .iter()
        .filter(|update| {
            !roots.contains(&update.varobj)
                && variable_object_has_owned_ancestor(&roots, &update.varobj)
                && variable_object_owned_root(&changed_variants, &update.varobj).is_none()
        })
        .cloned()
        .collect::<Vec<_>>();

    if let Some(state) = states.first()
        && let Some(ui) = state.borrow().ui.upgrade()
    {
        ui.show_variable_descendant_updates_for_refresh(
            state.borrow().requests.generation(),
            &descendants,
        );
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
                    || changed_variants.contains(&varobj)
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
            finish_variable_refresh(client, state);
        }
    }
}

#[cfg(test)]
fn variable_object_owns_update(root: &str, candidate: &str) -> bool {
    candidate == root
        || candidate
            .strip_prefix(root)
            .is_some_and(|suffix| suffix.starts_with('.'))
}

fn variable_object_has_owned_ancestor(roots: &HashSet<String>, candidate: &str) -> bool {
    candidate
        .rsplit_once('.')
        .is_some_and(|(parent, _)| variable_object_owned_root(roots, parent).is_some())
}

fn variable_object_owned_root<'a>(
    roots: &'a HashSet<String>,
    mut candidate: &str,
) -> Option<&'a String> {
    loop {
        if let Some(root) = roots.get(candidate) {
            return Some(root);
        }

        candidate = candidate.rsplit_once('.')?.0;
    }
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

    if let Some(display_hint) = update.display_hint.as_ref() {
        variable.display_hint = Some(display_hint.clone());
    }

    if let Some(dynamic) = update.dynamic {
        variable.dynamic = dynamic;
    }
}

fn finish_variable_refresh(client: &MiClient, state: Rc<RefCell<VariableRefresh>>) {
    if !variable_refresh_is_current(&state.borrow()) {
        discard_variable_refresh(client, &state);
        return;
    }

    let (ui, generation, target, variables) = {
        let mut state = state.borrow_mut();

        (
            state.ui.clone(),
            state.requests.generation(),
            state.target.clone(),
            std::mem::take(&mut state.variables),
        )
    };

    if let Some(ui) = ui.upgrade() {
        show_variable_refresh(&ui, generation, &target, &variables);
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

pub(in crate::app) fn delete_variable_object(client: &MiClient, varobj: &str) {
    let objects = OWNED_VARIABLE_OBJECTS
        .with(|owned| take_owned_variable_objects(&mut owned.borrow_mut(), varobj));

    for object in objects.into_iter().rev() {
        client.delete_variable_object(object);
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

pub(in crate::app) fn discard_variable_refresh(
    client: &MiClient,
    state: &Rc<RefCell<VariableRefresh>>,
) {
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

pub(in crate::app) fn request_variable_children(
    ui: Weak<Ui>,
    client: Rc<MiClient>,
    variable: Variable,
    from: usize,
) {
    if let Some(ui) = ui.upgrade()
        && !ui.variable_action_is_current(&variable)
    {
        ui.cancel_variable_children_request(&variable);
        return;
    }

    let Some(varobj) = variable.varobj.clone() else {
        request_lazy_local_variable_children(ui, client, variable, from);
        return;
    };

    let Some(generation) = ui.upgrade().and_then(|current_ui| {
        let generation = current_ui.model.current_stop_refresh_generation();

        current_ui
            .model
            .stop_context(generation)
            .map(|_| generation)
    }) else {
        return;
    };

    let Some(requests) = stop_requests(&ui, &client, generation) else {
        return;
    };

    // Dynamic varobjs may advertise available pretty-printed children only
    // through `has_more`. GDB documents `numchild` as unreliable for them.
    if variable.num_children > 0 || variable.has_more || variable.dynamic {
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

        if let Err(error) = requests
            .unscoped(&command)
            .when(move || {
                ui_for_guard
                    .upgrade()
                    .is_some_and(|ui| ui.has_variable_object(&varobj_for_guard))
            })
            .with_print_limit(to, move |client, record| {
                // Cancellation must also resolve the loading row. A retained
                // varobj needs a retry entry when its request is superseded.
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

                        set_variable_update_range(
                            client,
                            &ui_for_response,
                            generation,
                            &varobj,
                            next,
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
            })
            && let Some(ui) = ui.upgrade()
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
    let requests_for_path = requests.clone();
    let varobj_for_path = varobj.clone();
    let display_name = variable.name;
    let ui_for_path_guard = ui.clone();
    let varobj_for_path_guard = varobj.clone();

    if requests
        .unscoped(&command)
        .when(move || {
            ui_for_path_guard
                .upgrade()
                .is_some_and(|ui| ui.has_variable_object(&varobj_for_path_guard))
        })
        .request(move |_, record| {
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

            let requests = requests_for_path;

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

            if requests
                .frame(&command)
                .when(move || {
                    ui_for_guard
                        .upgrade()
                        .is_some_and(|ui| ui.has_variable_object(&varobj_for_guard))
                })
                .with_print_limit(AUTOMATIC_PRINT_ELEMENTS, move |client, record| {
                    let child = record
                        .is_done()
                        .then(|| {
                            crate::debugger::variable_object(&record, &format!("*{display_name}"))
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
                            delete_variable_object(client, &dereference_varobj_for_response);
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
                })
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

fn set_variable_update_range(
    client: &MiClient,
    ui: &Weak<Ui>,
    generation: u64,
    varobj: &str,
    loaded_children: usize,
) {
    if loaded_children == 0 {
        return;
    }

    let command = format!(
        "-var-set-update-range {} 0 {loaded_children}",
        crate::debugger::quote(varobj)
    );

    let Some(requests) = stop_requests(ui, client, generation) else {
        return;
    };

    let ui_for_guard = ui.clone();
    let varobj_for_guard = varobj.to_owned();

    let _ = requests
        .unscoped(&command)
        .when(move || {
            ui_for_guard
                .upgrade()
                .is_some_and(|ui| ui.has_variable_object(&varobj_for_guard))
        })
        .request(|_, _| {});
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

    let generation = current_ui.model.current_stop_refresh_generation();

    if from != 0 || !current_ui.claim_local_variable_object(generation, &variable) {
        return;
    }

    drop(current_ui);
    let varobj = next_variable_object_name();

    let command = format!(
        "-var-create {varobj} * {}",
        crate::debugger::quote(&variable.name)
    );

    let Some(requests) = stop_requests(&ui, &client, generation) else {
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

    if requests
        .frame(&command)
        .when(move || {
            ui_for_guard
                .upgrade()
                .is_some_and(|ui| ui.has_local_variable_identity(&variable_for_guard))
        })
        .with_print_limit(AUTOMATIC_PRINT_ELEMENTS, move |client, record| {
            if let Some(ui) = ui_for_response.upgrade() {
                ui.finish_local_variable_object(generation, &variable_for_response);
            }

            let created = record
                .is_done()
                .then(|| crate::debugger::variable_object(&record, &variable_for_response.name))
                .flatten()
                .map(|mut created| {
                    created.argument = variable_for_response.argument;
                    created.local_index = variable_for_response.local_index;

                    created
                });

            let Some(created) = created else {
                delete_variable_object(client, &varobj_for_response);

                if let Some(ui) = ui_for_response.upgrade()
                    && ui.model.is_stop_refresh_current(generation)
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
        })
        .is_err()
        && let Some(ui) = ui.upgrade()
    {
        ui.finish_local_variable_object(generation, &variable);

        if ui.has_local_variable_identity(&variable) {
            ui.show_lazy_variable_children_error(&variable, "The MI channel is unavailable");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use super::{
        Variable, VariableRefreshTarget, apply_variable_update, has_persistent_variable_objects,
        reuse_variable_objects, take_owned_variable_objects, variable_child_page_end,
        variable_object_has_owned_ancestor, variable_object_owned_root,
        variable_object_owns_update,
    };

    fn variable(
        name: &str,
        value: &str,
        type_name: Option<&str>,
        varobj: Option<&str>,
    ) -> Variable {
        Variable {
            local_index: None,
            name: name.to_owned(),
            value: value.to_owned(),
            type_name: type_name.map(str::to_owned),
            argument: false,
            varobj: varobj.map(str::to_owned),
            num_children: usize::from(varobj.is_some()),
            has_more: false,
            display_hint: None,
            dynamic: false,
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
            variable("count", "0x40", Some("Node *"), Some("var3")),
        ];

        let (reused, needs_update, mut stale) = reuse_variable_objects(&fallbacks, existing);
        assert_eq!(reused[0].varobj.as_deref(), Some("var1"));
        assert_eq!(reused[0].value, "0x10");
        assert_eq!(reused[1], fallbacks[1]);
        assert_eq!(needs_update, [true, false]);
        stale.sort_unstable();
        assert_eq!(stale, [String::from("var2"), String::from("var3")]);
    }

    #[test]
    fn duplicate_local_names_reuse_their_own_occurrence() {
        let mut first = variable("value", "{...}", Some("Value"), Some("var1"));
        first.local_index = Some(0);
        let mut second = first.clone();
        second.local_index = Some(1);
        second.varobj = Some("var2".into());
        let mut fallbacks = vec![first.clone(), second.clone()];

        for variable in &mut fallbacks {
            variable.varobj = None;
            variable.value = "<not available>".into();
        }

        let (reused, _, stale) = reuse_variable_objects(&fallbacks, vec![second, first]);
        assert!(stale.is_empty());
        assert_eq!(reused[0].varobj.as_deref(), Some("var1"));
        assert_eq!(reused[1].varobj.as_deref(), Some("var2"));

        let first = variable("pointer", "0x10", Some("Node *"), Some("first"));
        let mut second = first.clone();
        second.varobj = Some(String::from("second"));
        let mut argument = first.clone();
        argument.argument = true;
        argument.varobj = Some(String::from("argument"));
        let fallback = variable("pointer", "0x20", Some("Node *"), None);
        let (reused, needs_update, stale) =
            reuse_variable_objects(&[fallback.clone(), fallback], vec![first, second, argument]);
        assert_eq!(reused[0].varobj.as_deref(), Some("second"));
        assert_eq!(reused[1].varobj.as_deref(), Some("first"));
        assert_eq!(needs_update, [true, true]);
        assert_eq!(stale, [String::from("argument")]);
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
        let roots = HashSet::from([String::from("fgdb_var_1"), String::from("fgdb_var_20")]);

        assert_eq!(
            variable_object_owned_root(&roots, "fgdb_var_1.choice.value").map(String::as_str),
            Some("fgdb_var_1"),
        );

        assert_eq!(
            variable_object_owned_root(&roots, "fgdb_var_20").map(String::as_str),
            Some("fgdb_var_20"),
        );

        assert!(variable_object_has_owned_ancestor(
            &roots,
            "fgdb_var_1.public.next.value"
        ));

        assert!(!variable_object_has_owned_ancestor(
            &roots,
            "fgdb_var_10.public"
        ));
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
            display_hint: Some(String::from("array")),
            dynamic: Some(true),
        };

        apply_variable_update(&mut root, &update);
        assert_eq!(root.value, "new");
        assert_eq!(root.type_name.as_deref(), Some("New"));
        assert_eq!(root.num_children, 4);
        assert!(root.has_more);
        assert_eq!(root.display_hint.as_deref(), Some("array"));
        assert!(root.dynamic);
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
