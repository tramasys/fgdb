use super::*;

pub(super) fn request_indexed_children(
    ui: Weak<Ui>,
    client: Rc<MiClient>,
    generation: u64,
    session: Rc<VariableViewerSession>,
    variable: Variable,
    limit: usize,
    owned_root: Option<String>,
) {
    if variable.varobj.is_none() {
        session.fail("This value has no GDB variable object to inspect");
        cleanup_viewer_variable_objects(&ui, &client, owned_root);
        return;
    }
    if limit == 0 {
        session.finish("This viewer is configured with a zero item limit");
        cleanup_viewer_variable_objects(&ui, &client, owned_root);
        return;
    }
    request_indexed_level(
        ui,
        client,
        generation,
        session,
        variable.clone(),
        variable,
        limit,
        0,
        HashSet::new(),
        owned_root,
    );
}

#[allow(clippy::too_many_arguments)]
fn request_indexed_level(
    ui: Weak<Ui>,
    client: Rc<MiClient>,
    generation: u64,
    session: Rc<VariableViewerSession>,
    root: Variable,
    current: Variable,
    limit: usize,
    depth: usize,
    mut visited: HashSet<String>,
    owned_root: Option<String>,
) {
    const MAX_TRANSPARENT_DEPTH: usize = 8;
    let Some(varobj) = current.varobj.as_deref() else {
        finish_indexed(
            &ui,
            &client,
            &session,
            Some(String::from(
                "GDB did not expose an inspectable collection object",
            )),
            true,
            owned_root,
        );
        return;
    };
    if !visited.insert(varobj.to_owned()) {
        finish_indexed(
            &ui,
            &client,
            &session,
            Some(String::from("GDB exposed a cyclic collection wrapper")),
            true,
            owned_root,
        );
        return;
    }
    let command = format!(
        "-var-list-children --all-values {} 0 {limit}",
        crate::debugger::quote(varobj)
    );
    let ui_for_guard = ui.clone();
    let session_for_guard = Rc::clone(&session);
    let session_for_response = Rc::clone(&session);
    let client_for_response = Rc::clone(&client);
    let ui_for_response = ui.clone();
    let owned_for_response = owned_root.clone();
    if let Err(error) = client.request_with_print_limit_when(
        &command,
        limit.max(AUTOMATIC_PRINT_ELEMENTS),
        move || viewer_is_current(&ui_for_guard, &session_for_guard, generation),
        move |_, record| {
            if !session_for_response.is_open() {
                finish_indexed(
                    &ui_for_response,
                    &client_for_response,
                    &session_for_response,
                    None,
                    false,
                    owned_for_response,
                );
                return;
            }
            if record.class == "superseded" {
                finish_indexed(
                    &ui_for_response,
                    &client_for_response,
                    &session_for_response,
                    Some(String::from(STALE_VIEWER_MESSAGE)),
                    false,
                    owned_for_response,
                );
                return;
            }
            if !record.is_done() {
                finish_indexed(
                    &ui_for_response,
                    &client_for_response,
                    &session_for_response,
                    Some(
                        record
                            .error_message()
                            .unwrap_or("GDB could not read the collection elements")
                            .to_owned(),
                    ),
                    true,
                    owned_for_response,
                );
                return;
            }
            let children = crate::debugger::variable_children(&record);
            let indexed = children
                .iter()
                .filter_map(|child| indexed_child_ordinal(&child.name).map(|index| (index, child)))
                .collect::<Vec<_>>();
            if indexed.is_empty()
                && depth < MAX_TRANSPARENT_DEPTH
                && let Some(wrapper) = transparent_index_wrapper(&children)
            {
                request_indexed_level(
                    ui_for_response,
                    client_for_response,
                    generation,
                    session_for_response,
                    root,
                    wrapper,
                    limit,
                    depth + 1,
                    visited,
                    owned_for_response,
                );
                return;
            }
            if indexed.is_empty() {
                finish_indexed(
                    &ui_for_response,
                    &client_for_response,
                    &session_for_response,
                    Some(String::from(
                        "No indexed elements were exposed - the collection may be empty or require a GDB language pretty-printer",
                    )),
                    false,
                    owned_for_response,
                );
                return;
            }
            let returned = indexed.len();
            let rows = indexed
                .into_iter()
                .take(limit)
                .map(|(index, child)| VariableViewerRow {
                    ordinal: index,
                    name: child.name.clone(),
                    value: compact_viewer_text(&child.value, 320),
                    type_name: compact_variable_type_name(child.type_name.as_deref()),
                    details: String::new(),
                })
                .collect::<Vec<_>>();
            let shown = rows.len();
            session_for_response.append(rows);
            let has_more = crate::debugger::variable_children_have_more(&record)
                || returned > limit
                || (depth == 0 && root.num_children > shown);
            let message = if has_more {
                format!("Showing the first {shown} elements - limited to keep GDB responsive")
            } else {
                format!("{shown} element{}", if shown == 1 { "" } else { "s" })
            };
            finish_indexed(
                &ui_for_response,
                &client_for_response,
                &session_for_response,
                Some(message),
                false,
                owned_for_response,
            );
        },
    ) {
        finish_indexed(
            &ui,
            &client,
            &session,
            Some(format!("Could not queue the viewer request: {error}")),
            true,
            owned_root,
        );
    }
}

fn finish_indexed(
    ui: &Weak<Ui>,
    client: &MiClient,
    session: &VariableViewerSession,
    message: Option<String>,
    failed: bool,
    owned_root: Option<String>,
) {
    if let Some(message) = message
        && session.is_open()
    {
        if failed {
            session.fail(&message);
        } else {
            session.finish(&message);
        }
    }
    cleanup_viewer_variable_objects(ui, client, owned_root);
}
