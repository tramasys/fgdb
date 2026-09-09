//! Persistent root refresh and shared locals/watch MI update batching.

use super::*;

mod children;
pub(in crate::app) use children::request_variable_children;
#[cfg(test)]
use children::variable_child_page_end;

mod ownership;
pub(in crate::app) use ownership::delete_variable_object;
use ownership::{register_owned_variable_object, variable_object_owned_root};

const VARIABLE_CHILD_PAGE_SIZE: usize = 128;
const MAX_VARIABLE_CHILDREN: usize = 4096;

// Keep stop refresh responsive even in generated or macro-heavy frames.
// Remaining aggregate roots stay visible and create their varobj lazily when
// the user expands them.
const MAX_AUTOMATIC_LOCAL_OBJECTS: usize = 32;

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

            let Some(variable) = reusable.get(&key).and_then(|&index| buckets[index].pop()) else {
                return (fallback.clone(), false);
            };

            if fallback.type_name.is_some() && variable.type_name != fallback.type_name {
                stale.extend(variable.varobj);
                return (fallback.clone(), false);
            }

            let needs_update = variable.varobj.is_some();

            (variable, needs_update)
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

        if state.created >= state.target.creation_budget() {
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

    let target = states.first().map(|state| {
        let state = state.borrow();

        (state.ui.clone(), state.requests.generation())
    });

    if let Some((ui, generation)) = target
        && let Some(ui) = ui.upgrade()
    {
        ui.show_variable_descendant_updates_for_refresh(generation, &descendants);
    }

    let updates = updates
        .iter()
        .map(|update| (update.varobj.as_str(), update))
        .collect::<HashMap<_, _>>();

    for state in states {
        let (recreate, retired) = {
            let mut state = state.borrow_mut();
            let mut recreate = false;
            let mut retired = Vec::new();

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
                    state.created_varobjs.remove(&varobj);
                    retired.push(varobj);
                    state.variables[index] = state.fallbacks[index].clone();

                    recreate |= state
                        .target
                        .creates_missing_variable_object(&state.fallbacks[index]);
                } else if let Some(update) = update {
                    state.variables[index].apply_update(update);
                }

                state.needs_update[index] = false;
            }

            state.bulk_completed = true;

            (recreate, retired)
        };

        for varobj in retired {
            delete_variable_object(client, &varobj);
        }

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
    !roots.contains(candidate) && variable_object_owned_root(roots, candidate).is_some()
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
    fn creation_budget(&self) -> usize {
        match self {
            Self::Locals => MAX_AUTOMATIC_LOCAL_OBJECTS,
            // Explicit watches are already bounded by the watch list. Unlike
            // local aggregates, they have no lazy root-creation path.
            Self::ExpressionWatches(expressions) => expressions.len(),
        }
    }

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

#[cfg(test)]
mod tests;
