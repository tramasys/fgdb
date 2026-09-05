use super::*;

/// Bind model authority once at the application boundary. Transport guards then
/// validate the same identity at submission, dispatch, and completion.
pub(super) fn stop_requests(
    ui: &Weak<Ui>,
    client: &MiClient,
    generation: u64,
) -> Option<StopRequests> {
    bind_requests(ui, client, generation, false)
}

pub(super) fn edit_requests(
    ui: &Weak<Ui>,
    client: &MiClient,
    generation: u64,
) -> Option<StopRequests> {
    bind_requests(ui, client, generation, true)
}

fn bind_requests(
    ui: &Weak<Ui>,
    client: &MiClient,
    generation: u64,
    editing: bool,
) -> Option<StopRequests> {
    let current_ui = ui.upgrade()?;
    let context = current_ui.model.stop_context(generation)?;
    let model = Rc::downgrade(&current_ui.model);
    let ui = ui.clone();

    Some(client.bind_stop_requests(context, move |context| {
        ui.strong_count() != 0
            && model.upgrade().is_some_and(|model| {
                model.is_stop_context_current(context)
                    && (!editing || model.can_edit_variable(context.generation()))
            })
    }))
}
