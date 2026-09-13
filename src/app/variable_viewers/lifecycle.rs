use super::*;

pub(super) fn cleanup_viewer_variable_objects(
    client: &MiClient,
    owned: impl IntoIterator<Item = String>,
) {
    for varobj in owned {
        delete_variable_object(client, &varobj);
    }
}
