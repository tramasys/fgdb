use super::*;

pub(super) fn refresh_expression_watches(
    ui: Weak<Ui>,
    client: &MiClient,
    generation: u64,
    update_batch: Rc<VariableUpdateBatch>,
) {
    let Some(current_ui) = ui.upgrade() else {
        return;
    };
    let expressions = current_ui.expression_watch_expressions();
    drop(current_ui);
    refresh_expression_variable_objects(ui, client, generation, expressions, update_batch);
}
