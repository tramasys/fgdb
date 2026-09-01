use super::*;

pub(super) fn cleanup_viewer_variable_objects(
    ui: &Weak<Ui>,
    client: &MiClient,
    owned: impl IntoIterator<Item = String>,
) {
    let owned = owned.into_iter().collect::<Vec<_>>();
    if owned.is_empty() {
        return;
    }
    if let Some(ui) = ui.upgrade() {
        if ui.inferior_is_running() {
            ui.defer_variable_object_deletions(owned);
        } else {
            owned
                .iter()
                .for_each(|varobj| delete_variable_object(client, varobj));
        }
    } else {
        owned
            .iter()
            .for_each(|varobj| delete_variable_object(client, varobj));
    }
}
