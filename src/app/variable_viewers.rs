use super::*;

mod heuristics;
mod indexed;
mod lifecycle;
mod linked;

use heuristics::{
    compact_variable_type_name, compact_viewer_text, indexed_child_ordinal, is_cpp_access_group,
    normalize_member_name, transparent_index_wrapper, transparent_link_wrapper,
    viewer_value_is_null,
};
use indexed::request_indexed_children;
use lifecycle::cleanup_viewer_variable_objects;
use linked::{LinkedListSettings, start_linked_list};

const MAX_VIEWER_ITEMS: usize = 4096;
const STALE_VIEWER_MESSAGE: &str = "Target state changed - reopen the viewer for the current stop";

pub(super) fn open_variable_viewer(
    ui: Weak<Ui>,
    client: Rc<MiClient>,
    request: VariableViewerRequest,
) {
    let Some(current_ui) = ui.upgrade() else {
        return;
    };

    let generation = current_ui.current_stop_refresh_generation();
    let session = current_ui.begin_variable_viewer(&request);

    if current_ui.inferior_is_running() {
        session.fail("Pause the target before opening a variable viewer");
        return;
    }

    drop(current_ui);

    if request.variable.varobj.is_none() {
        create_viewer_root(ui, client, generation, session, request);
        return;
    }

    start_variable_viewer_plan(ui, client, generation, session, request, None);
}

fn create_viewer_root(
    ui: Weak<Ui>,
    client: Rc<MiClient>,
    generation: u64,
    session: Rc<VariableViewerSession>,
    request: VariableViewerRequest,
) {
    let varobj_name = next_variable_object_name();

    let command = format!(
        "-var-create {varobj_name} * {}",
        crate::debugger::quote(&request.variable.name)
    );

    let Some(command) = frame_scoped_stop_command(&ui, generation, &command) else {
        session.finish(STALE_VIEWER_MESSAGE);
        return;
    };

    let ui_for_guard = ui.clone();
    let session_for_guard = Rc::clone(&session);
    let ui_for_response = ui;
    let session_for_response = Rc::clone(&session);
    let client_for_response = Rc::clone(&client);
    let varobj_for_response = varobj_name;

    if let Err(error) = client.request_with_print_limit_for_stop(
        &command,
        AUTOMATIC_PRINT_ELEMENTS,
        generation,
        move || viewer_is_current(&ui_for_guard, &session_for_guard, generation),
        move |_, record| {
            if record.class == "superseded" {
                cleanup_viewer_variable_objects(
                    &ui_for_response,
                    &client_for_response,
                    Some(varobj_for_response),
                );

                if session_for_response.is_open() {
                    session_for_response.finish(STALE_VIEWER_MESSAGE);
                }

                return;
            }

            let Some(mut variable) = record
                .is_done()
                .then(|| crate::debugger::variable_object(&record, &request.variable.name))
                .flatten()
            else {
                cleanup_viewer_variable_objects(
                    &ui_for_response,
                    &client_for_response,
                    Some(varobj_for_response),
                );

                if session_for_response.is_open() {
                    session_for_response.fail(
                        record
                            .error_message()
                            .unwrap_or("GDB could not prepare this value for inspection"),
                    );
                }

                return;
            };

            variable.argument = request.variable.argument;
            let owned = Some(varobj_for_response);

            if !viewer_is_current(&ui_for_response, &session_for_response, generation) {
                cleanup_viewer_variable_objects(&ui_for_response, &client_for_response, owned);

                if session_for_response.is_open() {
                    session_for_response.finish(STALE_VIEWER_MESSAGE);
                }

                return;
            }

            let request = VariableViewerRequest {
                descriptor: request.descriptor,
                variable,
            };

            start_variable_viewer_plan(
                ui_for_response,
                client_for_response,
                generation,
                session_for_response,
                request,
                owned,
            );
        },
    ) {
        session.fail(&format!("Could not queue variable inspection: {error}"));
    }
}

fn start_variable_viewer_plan(
    ui: Weak<Ui>,
    client: Rc<MiClient>,
    generation: u64,
    session: Rc<VariableViewerSession>,
    request: VariableViewerRequest,
    owned_root: Option<String>,
) {
    match request.descriptor.plan.clone() {
        VariableViewerPlan::IndexedChildren { limit } => request_indexed_children(
            ui,
            client,
            generation,
            session,
            request.variable,
            limit.min(MAX_VIEWER_ITEMS),
            owned_root,
        ),
        VariableViewerPlan::LinkedList {
            next_members,
            limit,
        } => start_linked_list(
            ui,
            client,
            generation,
            session,
            request.variable,
            LinkedListSettings {
                next_members,
                limit: limit.min(MAX_VIEWER_ITEMS),
                owned_root,
            },
        ),
    }
}

fn viewer_is_current(ui: &Weak<Ui>, session: &VariableViewerSession, generation: u64) -> bool {
    session.is_open()
        && ui
            .upgrade()
            .is_some_and(|ui| ui.is_stop_refresh_current(generation) && !ui.inferior_is_running())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_common_next_member_spellings() {
        assert_eq!(normalize_member_name("next"), "next");
        assert_eq!(normalize_member_name("fixture::next_"), "next_");
        assert_eq!(normalize_member_name("_M_next"), "_m_next");
        assert!(is_cpp_access_group("public"));
        assert!(is_cpp_access_group("private"));
        assert!(!is_cpp_access_group("next"));
        assert!(viewer_value_is_null("(Node *) 0x0"));
        assert!(viewer_value_is_null("nullptr"));
        assert!(viewer_value_is_null("None"));
        assert!(!viewer_value_is_null("(Node *) 0x1"));
    }

    #[test]
    fn recognizes_indexed_children_and_safe_array_wrappers() {
        assert_eq!(indexed_child_ordinal("[17]"), Some(17));
        assert_eq!(indexed_child_ordinal("public"), None);

        let wrapper = Variable {
            local_index: None,
            name: String::from("_M_elems"),
            value: String::from("{...}"),
            type_name: None,
            argument: false,
            varobj: Some(String::from("var1.public._M_elems")),
            num_children: 2,
            has_more: false,
            display_hint: None,
            dynamic: false,
        };

        assert_eq!(
            transparent_index_wrapper(std::slice::from_ref(&wrapper)),
            Some(wrapper.clone())
        );

        let access_group = Variable {
            local_index: None,
            name: String::from("public"),
            varobj: Some(String::from("var1.public")),
            ..wrapper.clone()
        };

        assert_eq!(
            transparent_index_wrapper(&[access_group, wrapper.clone()]),
            Some(wrapper)
        );
    }

    #[test]
    fn unwraps_known_rust_ownership_layers_without_guessing_user_fields() {
        let child = |name: &str, type_name: &str| Variable {
            local_index: None,
            name: name.to_owned(),
            value: String::from("{...}"),
            type_name: Some(type_name.to_owned()),
            argument: false,
            varobj: Some(format!("var1.{name}")),
            num_children: 1,
            has_more: false,
            display_hint: None,
            dynamic: false,
        };

        let option = child("next", "core::option::Option<alloc::rc::Rc<Node>>");
        let some = child("Some", "core::option::Option<alloc::rc::Rc<Node>>::Some");

        assert_eq!(
            transparent_link_wrapper(&option, std::slice::from_ref(&some)),
            Some(some)
        );

        let user = child("wrapper", "crate::UserWrapper<Node>");
        let value = child("value", "Node");

        assert_eq!(
            transparent_link_wrapper(&user, std::slice::from_ref(&value)),
            None
        );
    }

    #[test]
    fn bounds_long_viewer_values_at_character_boundaries() {
        assert_eq!(compact_viewer_text("abcdef", 4), "abcd...");
        assert_eq!(compact_viewer_text("λ-value", 1), "λ...");
        assert_eq!(compact_viewer_text("short", 10), "short");
    }
}
