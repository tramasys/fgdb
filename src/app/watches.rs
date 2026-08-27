use super::*;

struct ExpressionWatchRefresh {
    ui: Weak<Ui>,
    generation: u64,
    expressions: Vec<String>,
    variables: Vec<Variable>,
    next_index: usize,
}

pub(super) fn refresh_expression_watches(ui: Weak<Ui>, client: &MiClient, generation: u64) {
    let Some(current_ui) = ui.upgrade() else {
        return;
    };
    for varobj in current_ui.expression_watch_variable_object_names() {
        delete_variable_object(client, &varobj);
    }
    let expressions = current_ui.expression_watch_expressions();
    if expressions.is_empty() {
        current_ui.show_expression_watches_for_refresh(generation, &[]);
        return;
    }
    drop(current_ui);
    let state = Rc::new(RefCell::new(ExpressionWatchRefresh {
        ui,
        generation,
        variables: Vec::with_capacity(expressions.len()),
        expressions,
        next_index: 0,
    }));
    request_next_expression_watch(client, state);
}

fn expression_watch_refresh_is_current(state: &ExpressionWatchRefresh) -> bool {
    state.ui.upgrade().is_some_and(|ui| {
        ui.is_stop_refresh_current(state.generation)
            && ui.expression_watches_match(&state.expressions)
    })
}

fn request_next_expression_watch(client: &MiClient, state: Rc<RefCell<ExpressionWatchRefresh>>) {
    if !expression_watch_refresh_is_current(&state.borrow()) {
        discard_expression_watch_refresh(client, &state);
        return;
    }
    let expression = {
        let mut state = state.borrow_mut();
        let expression = state.expressions.get(state.next_index).cloned();
        state.next_index = state.next_index.saturating_add(1);
        expression
    };
    let Some(expression) = expression else {
        let (ui, generation, variables) = {
            let mut state = state.borrow_mut();
            (
                state.ui.clone(),
                state.generation,
                std::mem::take(&mut state.variables),
            )
        };
        if let Some(ui) = ui.upgrade() {
            ui.show_expression_watches_for_refresh(generation, &variables);
        }
        return;
    };
    let command = format!("-var-create - * {}", crate::debugger::quote(&expression));
    let expression_for_response = expression.clone();
    let state_for_guard = Rc::clone(&state);
    let state_for_response = Rc::clone(&state);
    if client
        .request_with_print_limit_when(
            &command,
            AUTOMATIC_PRINT_ELEMENTS,
            move || expression_watch_refresh_is_current(&state_for_guard.borrow()),
            move |client, record| {
                let variable = if record.is_done() {
                    crate::debugger::variable_object(&record, &expression_for_response)
                } else {
                    Some(Variable {
                        name: expression_for_response.clone(),
                        value: format!(
                            "<error: {}>",
                            record
                                .error_message()
                                .unwrap_or("expression is unavailable")
                        ),
                        type_name: None,
                        varobj: None,
                        num_children: 0,
                        has_more: false,
                    })
                };
                if !expression_watch_refresh_is_current(&state_for_response.borrow()) {
                    if let Some(varobj) = variable
                        .as_ref()
                        .and_then(|variable| variable.varobj.as_deref())
                    {
                        delete_variable_object(client, varobj);
                    }
                    discard_expression_watch_refresh(client, &state_for_response);
                    return;
                }
                if let Some(variable) = variable {
                    state_for_response.borrow_mut().variables.push(variable);
                }
                request_next_expression_watch(client, state_for_response);
            },
        )
        .is_err()
    {
        state.borrow_mut().variables.push(Variable {
            name: expression,
            value: String::from("<error: MI channel is unavailable>"),
            type_name: None,
            varobj: None,
            num_children: 0,
            has_more: false,
        });
        request_next_expression_watch(client, state);
    }
}

fn discard_expression_watch_refresh(
    client: &MiClient,
    state: &Rc<RefCell<ExpressionWatchRefresh>>,
) {
    for varobj in state
        .borrow()
        .variables
        .iter()
        .filter_map(|variable| variable.varobj.as_deref())
    {
        delete_variable_object(client, varobj);
    }
}
