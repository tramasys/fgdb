use super::*;

const LINKED_NODE_FIELD_LIMIT: usize = 256;
const MAX_LINK_WRAPPER_DEPTH: usize = 16;
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
    let ui_for_guard = ui.clone();
    let session_for_guard = Rc::clone(&session);
    let ui_for_response = ui.clone();
    let session_for_response = Rc::clone(&session);
    let client_for_response = Rc::clone(&client);
    let varobj_for_response = varobj_name.clone();
    if let Err(error) = client.request_with_print_limit_when(
        &command,
        AUTOMATIC_PRINT_ELEMENTS,
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

fn request_indexed_children(
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
        AUTOMATIC_PRINT_ELEMENTS,
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
                format!(
                    "Showing the first {shown} elements - limited to keep GDB responsive"
                )
            } else {
                format!(
                    "{shown} element{}",
                    if shown == 1 { "" } else { "s" }
                )
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

fn indexed_child_ordinal(name: &str) -> Option<usize> {
    name.trim()
        .strip_prefix('[')?
        .strip_suffix(']')?
        .parse()
        .ok()
}

fn transparent_index_wrapper(children: &[Variable]) -> Option<Variable> {
    children
        .iter()
        .filter_map(|child| {
            let name = normalize_member_name(&child.name);
            if !child.can_expand() {
                return None;
            }
            let priority = match name.as_str() {
                "_m_elems" | "__elems" | "__elems_" | "_elems" | "elems" | "elements" => 0,
                "public" | "private" | "protected" => 1,
                _ => return None,
            };
            Some((priority, child))
        })
        .min_by_key(|(priority, _)| *priority)
        .map(|(_, child)| child.clone())
}

struct LinkedTraversal {
    ui: Weak<Ui>,
    client: Rc<MiClient>,
    generation: u64,
    session: Rc<VariableViewerSession>,
    current: Variable,
    current_address: Option<u64>,
    next_members: HashSet<String>,
    seen_addresses: HashSet<u64>,
    seen_objects: HashSet<String>,
    owned_variable_objects: HashSet<String>,
    wrapper_depth: usize,
    shown: usize,
    limit: usize,
    finished: bool,
}

struct LinkedListSettings {
    next_members: Vec<String>,
    limit: usize,
    owned_root: Option<String>,
}

fn start_linked_list(
    ui: Weak<Ui>,
    client: Rc<MiClient>,
    generation: u64,
    session: Rc<VariableViewerSession>,
    variable: Variable,
    settings: LinkedListSettings,
) {
    let LinkedListSettings {
        next_members,
        limit,
        owned_root,
    } = settings;
    if limit == 0 {
        session.finish("This viewer is configured with a zero node limit");
        cleanup_viewer_variable_objects(&ui, &client, owned_root);
        return;
    }
    let address = pointer_address(&variable.value).filter(|address| *address != 0);
    let mut seen_addresses = HashSet::new();
    if let Some(address) = address {
        seen_addresses.insert(address);
    }
    let mut seen_objects = HashSet::new();
    if let Some(varobj) = variable.varobj.as_ref() {
        seen_objects.insert(varobj.clone());
    }
    let mut owned_variable_objects = HashSet::new();
    owned_variable_objects.extend(owned_root);
    let traversal = Rc::new(RefCell::new(LinkedTraversal {
        ui,
        client,
        generation,
        session,
        current: variable,
        current_address: address,
        next_members: next_members
            .into_iter()
            .map(|member| normalize_member_name(&member))
            .collect(),
        seen_addresses,
        seen_objects,
        owned_variable_objects,
        wrapper_depth: 0,
        shown: 0,
        limit,
        finished: false,
    }));
    seed_linked_root_address(traversal);
}

fn linked_is_current(traversal: &Rc<RefCell<LinkedTraversal>>) -> bool {
    let traversal = traversal.borrow();
    viewer_is_current(&traversal.ui, &traversal.session, traversal.generation)
}

fn seed_linked_root_address(traversal: Rc<RefCell<LinkedTraversal>>) {
    let (varobj, should_query, client) = {
        let traversal = traversal.borrow();
        (
            traversal.current.varobj.clone(),
            traversal.current_address.is_none() && !traversal.current.is_pointer(),
            Rc::clone(&traversal.client),
        )
    };
    let Some(varobj) = varobj.filter(|_| should_query) else {
        request_linked_node(traversal);
        return;
    };
    let command = format!(
        "-var-info-path-expression {}",
        crate::debugger::quote(&varobj)
    );
    let traversal_for_guard = Rc::clone(&traversal);
    let traversal_for_response = Rc::clone(&traversal);
    if client
        .request_when(
            &command,
            move || linked_is_current(&traversal_for_guard),
            move |client, record| {
                if record.class == "superseded" {
                    finish_linked(
                        &traversal_for_response,
                        Some(String::from(STALE_VIEWER_MESSAGE)),
                    );
                    return;
                }
                let Some(path) = crate::debugger::variable_path_expression(&record) else {
                    request_linked_node(traversal_for_response);
                    return;
                };
                let command = format!(
                    "-data-evaluate-expression {}",
                    crate::debugger::quote(&format!("&({path})"))
                );
                let traversal_for_guard = Rc::clone(&traversal_for_response);
                let traversal_for_address = Rc::clone(&traversal_for_response);
                if client
                    .request_when(
                        &command,
                        move || linked_is_current(&traversal_for_guard),
                        move |_, record| {
                            if record.class == "superseded" {
                                finish_linked(
                                    &traversal_for_address,
                                    Some(String::from(STALE_VIEWER_MESSAGE)),
                                );
                                return;
                            }
                            if let Some(address) = crate::debugger::evaluated_value(&record)
                                .as_deref()
                                .and_then(pointer_address)
                                .filter(|address| *address != 0)
                            {
                                let mut traversal = traversal_for_address.borrow_mut();
                                traversal.current_address = Some(address);
                                traversal.seen_addresses.insert(address);
                            }
                            request_linked_node(traversal_for_address);
                        },
                    )
                    .is_err()
                {
                    request_linked_node(traversal_for_response);
                }
            },
        )
        .is_err()
    {
        request_linked_node(traversal);
    }
}

fn request_linked_node(traversal: Rc<RefCell<LinkedTraversal>>) {
    if !linked_is_current(&traversal) {
        finish_linked(&traversal, None);
        return;
    }
    let (current, shown, limit) = {
        let traversal = traversal.borrow();
        (traversal.current.clone(), traversal.shown, traversal.limit)
    };
    if shown >= limit {
        finish_linked(
            &traversal,
            Some(format!(
                "Showing the first {shown} nodes - traversal limit reached"
            )),
        );
        return;
    }
    if current.is_pointer() && viewer_value_is_null(&current.value) {
        finish_linked(&traversal, Some(format!("{shown} nodes - reached null")));
        return;
    }
    if current.num_children == 0 && !current.has_more && current.is_pointer() {
        request_linked_dereference(traversal, current);
    } else {
        request_linked_children(traversal, current);
    }
}

fn request_linked_dereference(traversal: Rc<RefCell<LinkedTraversal>>, current: Variable) {
    let Some(varobj) = current.varobj.as_deref() else {
        finish_linked(
            &traversal,
            Some(String::from(
                "The next pointer has no inspectable GDB object",
            )),
        );
        return;
    };
    let client = Rc::clone(&traversal.borrow().client);
    let command = format!(
        "-var-info-path-expression {}",
        crate::debugger::quote(varobj)
    );
    let traversal_for_guard = Rc::clone(&traversal);
    let traversal_for_response = Rc::clone(&traversal);
    if let Err(error) = client.request_when(
        &command,
        move || linked_is_current(&traversal_for_guard),
        move |_client, record| {
            if record.class == "superseded" {
                finish_linked(
                    &traversal_for_response,
                    Some(String::from(STALE_VIEWER_MESSAGE)),
                );
                return;
            }
            let Some(path) = crate::debugger::variable_path_expression(&record) else {
                finish_linked(
                    &traversal_for_response,
                    Some(String::from("GDB could not dereference the next pointer")),
                );
                return;
            };
            let dereference_varobj = next_variable_object_name();
            traversal_for_response
                .borrow_mut()
                .owned_variable_objects
                .insert(dereference_varobj.clone());
            let command = format!(
                "-var-create {dereference_varobj} * {}",
                crate::debugger::quote(&format!("*({path})"))
            );
            let traversal_for_guard = Rc::clone(&traversal_for_response);
            let traversal_for_dereference = Rc::clone(&traversal_for_response);
            let client = Rc::clone(&traversal_for_response.borrow().client);
            if let Err(error) = client.request_with_print_limit_when(
                &command,
                AUTOMATIC_PRINT_ELEMENTS,
                move || linked_is_current(&traversal_for_guard),
                move |_, record| {
                    if record.class == "superseded" {
                        finish_linked(
                            &traversal_for_dereference,
                            Some(String::from(STALE_VIEWER_MESSAGE)),
                        );
                        return;
                    }
                    let name = traversal_for_dereference.borrow().current.name.clone();
                    let Some(child) = record
                        .is_done()
                        .then(|| crate::debugger::variable_object(&record, &format!("*{name}")))
                        .flatten()
                    else {
                        finish_linked(
                            &traversal_for_dereference,
                            Some(String::from("GDB could not inspect the pointed-to node")),
                        );
                        return;
                    };
                    {
                        let mut traversal = traversal_for_dereference.borrow_mut();
                        if let Some(varobj) = child.varobj.clone() {
                            traversal.owned_variable_objects.insert(varobj);
                        }
                        traversal.current = child;
                    }
                    request_linked_node(traversal_for_dereference);
                },
            ) {
                finish_linked(
                    &traversal_for_response,
                    Some(format!("Could not queue pointer inspection: {error}")),
                );
            }
        },
    ) {
        finish_linked(
            &traversal,
            Some(format!("Could not queue pointer resolution: {error}")),
        );
    }
}

fn request_linked_children(traversal: Rc<RefCell<LinkedTraversal>>, current: Variable) {
    let Some(varobj) = current.varobj.as_deref() else {
        finish_linked(
            &traversal,
            Some(String::from("This node has no inspectable GDB object")),
        );
        return;
    };
    let client = Rc::clone(&traversal.borrow().client);
    let command = format!(
        "-var-list-children --all-values {} 0 {LINKED_NODE_FIELD_LIMIT}",
        crate::debugger::quote(varobj)
    );
    let traversal_for_guard = Rc::clone(&traversal);
    let traversal_for_response = Rc::clone(&traversal);
    if let Err(error) = client.request_with_print_limit_when(
        &command,
        AUTOMATIC_PRINT_ELEMENTS,
        move || linked_is_current(&traversal_for_guard),
        move |_, record| {
            if record.class == "superseded" {
                finish_linked(
                    &traversal_for_response,
                    Some(String::from(STALE_VIEWER_MESSAGE)),
                );
                return;
            }
            if !record.is_done() {
                let message = record
                    .error_message()
                    .unwrap_or("GDB could not inspect a linked-list node")
                    .to_owned();
                finish_linked(&traversal_for_response, Some(message));
                return;
            }
            let children = crate::debugger::variable_children(&record);
            complete_linked_node(&traversal_for_response, current, children);
        },
    ) {
        finish_linked(
            &traversal,
            Some(format!("Could not queue linked-list traversal: {error}")),
        );
    }
}

fn complete_linked_node(
    traversal: &Rc<RefCell<LinkedTraversal>>,
    current: Variable,
    children: Vec<Variable>,
) {
    let access_groups = children
        .iter()
        .filter(|child| is_cpp_access_group(&child.name) && child.can_expand())
        .cloned()
        .collect::<VecDeque<_>>();
    if !access_groups.is_empty() {
        let fields = children
            .into_iter()
            .filter(|child| !is_cpp_access_group(&child.name))
            .collect();
        request_linked_access_groups(Rc::clone(traversal), current, access_groups, fields);
        return;
    }
    finish_linked_node(traversal, current, children);
}

fn request_linked_access_groups(
    traversal: Rc<RefCell<LinkedTraversal>>,
    current: Variable,
    mut groups: VecDeque<Variable>,
    mut fields: Vec<Variable>,
) {
    let Some(group) = groups.pop_front() else {
        finish_linked_node(&traversal, current, fields);
        return;
    };
    let Some(varobj) = group.varobj.as_deref() else {
        request_linked_access_groups(traversal, current, groups, fields);
        return;
    };
    let client = Rc::clone(&traversal.borrow().client);
    let command = format!(
        "-var-list-children --all-values {} 0 {LINKED_NODE_FIELD_LIMIT}",
        crate::debugger::quote(varobj)
    );
    let traversal_for_guard = Rc::clone(&traversal);
    let traversal_for_response = Rc::clone(&traversal);
    if let Err(error) = client.request_with_print_limit_when(
        &command,
        AUTOMATIC_PRINT_ELEMENTS,
        move || linked_is_current(&traversal_for_guard),
        move |_, record| {
            if record.class == "superseded" {
                finish_linked(
                    &traversal_for_response,
                    Some(String::from(STALE_VIEWER_MESSAGE)),
                );
                return;
            }
            if record.is_done() {
                fields.extend(crate::debugger::variable_children(&record));
                request_linked_access_groups(traversal_for_response, current, groups, fields);
            } else {
                let message = record
                    .error_message()
                    .unwrap_or("GDB could not inspect a C++ access group")
                    .to_owned();
                finish_linked(&traversal_for_response, Some(message));
            }
        },
    ) {
        finish_linked(
            &traversal,
            Some(format!("Could not queue C++ field inspection: {error}")),
        );
    }
}

fn finish_linked_node(
    traversal: &Rc<RefCell<LinkedTraversal>>,
    current: Variable,
    children: Vec<Variable>,
) {
    let has_next = {
        let traversal = traversal.borrow();
        children.iter().any(|child| {
            traversal
                .next_members
                .contains(&normalize_member_name(&child.name))
        })
    };
    if !has_next && let Some(wrapper) = transparent_link_wrapper(&current, &children) {
        let address = pointer_address(&wrapper.value).filter(|address| *address != 0);
        let (cycle, depth_exceeded, shown) = {
            let mut traversal = traversal.borrow_mut();
            let cycle = if let Some(address) = address {
                !traversal.seen_addresses.insert(address)
            } else if let Some(varobj) = wrapper.varobj.as_ref() {
                !traversal.seen_objects.insert(varobj.clone())
            } else {
                false
            };
            traversal.wrapper_depth = traversal.wrapper_depth.saturating_add(1);
            let depth_exceeded = traversal.wrapper_depth > MAX_LINK_WRAPPER_DEPTH;
            if !cycle && !depth_exceeded {
                traversal.current = wrapper;
                if address.is_some() {
                    traversal.current_address = address;
                }
            }
            (cycle, depth_exceeded, traversal.shown)
        };
        if cycle {
            finish_linked(
                traversal,
                Some(format!(
                    "{shown} node{} - cycle detected",
                    if shown == 1 { "" } else { "s" }
                )),
            );
        } else if depth_exceeded {
            finish_linked(
                traversal,
                Some(format!(
                    "{shown} node{} - ownership wrapper depth limit reached",
                    if shown == 1 { "" } else { "s" }
                )),
            );
        } else {
            request_linked_node(Rc::clone(traversal));
        }
        return;
    }
    if !has_next
        && current
            .type_name
            .as_deref()
            .is_some_and(|type_name| type_name.to_ascii_lowercase().contains("option<"))
    {
        let shown = traversal.borrow().shown;
        finish_linked(
            traversal,
            Some(format!(
                "{shown} node{} - reached the end",
                if shown == 1 { "" } else { "s" }
            )),
        );
        return;
    }
    let (next, row, shown) = {
        let mut traversal = traversal.borrow_mut();
        traversal.wrapper_depth = 0;
        let next = children
            .iter()
            .find(|child| {
                traversal
                    .next_members
                    .contains(&normalize_member_name(&child.name))
            })
            .cloned();
        let details = children
            .iter()
            .filter(|child| {
                !traversal
                    .next_members
                    .contains(&normalize_member_name(&child.name))
            })
            .take(6)
            .map(|child| format!("{} = {}", child.name, compact_viewer_text(&child.value, 72)))
            .collect::<Vec<_>>()
            .join("  ");
        let row = VariableViewerRow {
            ordinal: traversal.shown,
            name: traversal
                .current_address
                .map(|address| format!("0x{address:x}"))
                .unwrap_or_else(|| current.name.clone()),
            value: compact_viewer_text(&current.value, 320),
            type_name: compact_variable_type_name(current.type_name.as_deref()),
            details,
        };
        traversal.shown = traversal.shown.saturating_add(1);
        (next, row, traversal.shown)
    };
    traversal.borrow().session.append([row]);

    let Some(next) = next else {
        finish_linked(
            traversal,
            Some(format!(
                "{shown} node{} - no next-like member found",
                if shown == 1 { "" } else { "s" }
            )),
        );
        return;
    };
    let next_address = pointer_address(&next.value);
    if viewer_value_is_null(&next.value) {
        finish_linked(
            traversal,
            Some(format!(
                "{shown} node{} - reached the end",
                if shown == 1 { "" } else { "s" }
            )),
        );
        return;
    }

    let cycle = {
        let mut traversal = traversal.borrow_mut();
        let cycle = if let Some(address) = next_address {
            !traversal.seen_addresses.insert(address)
        } else if let Some(varobj) = next.varobj.as_ref() {
            !traversal.seen_objects.insert(varobj.clone())
        } else {
            false
        };
        if !cycle {
            traversal.current = next;
            traversal.current_address = next_address;
        }
        cycle
    };
    if cycle {
        finish_linked(
            traversal,
            Some(format!(
                "{shown} node{} - cycle detected",
                if shown == 1 { "" } else { "s" }
            )),
        );
    } else {
        request_linked_node(Rc::clone(traversal));
    }
}

fn transparent_link_wrapper(current: &Variable, children: &[Variable]) -> Option<Variable> {
    let type_name = current
        .type_name
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let preferred: &[&str] = if type_name.contains("option<") {
        &["some", "__0", "0"]
    } else if type_name.contains("rc<")
        || type_name.contains("arc<")
        || type_name.contains("weak<")
        || type_name.contains("box<")
    {
        &["ptr", "pointer", "__0", "0"]
    } else if type_name.contains("nonnull<") {
        &["pointer", "ptr", "__0", "0"]
    } else if ["rcinner<", "arcinner<", "refcell<", "unsafecell<"]
        .iter()
        .any(|wrapper| type_name.contains(wrapper))
    {
        &["value"]
    } else {
        return None;
    };
    preferred.iter().find_map(|preferred| {
        children
            .iter()
            .find(|child| normalize_member_name(&child.name) == *preferred && child.can_expand())
            .cloned()
    })
}

fn is_cpp_access_group(name: &str) -> bool {
    matches!(
        normalize_member_name(name).as_str(),
        "public" | "private" | "protected"
    )
}

fn finish_linked(traversal: &Rc<RefCell<LinkedTraversal>>, message: Option<String>) {
    let (client, ui, session, owned) = {
        let mut traversal = traversal.borrow_mut();
        if traversal.finished {
            return;
        }
        traversal.finished = true;
        (
            Rc::clone(&traversal.client),
            traversal.ui.clone(),
            Rc::clone(&traversal.session),
            traversal.owned_variable_objects.drain().collect::<Vec<_>>(),
        )
    };
    if let Some(message) = message
        && session.is_open()
    {
        session.finish(&message);
    }
    cleanup_viewer_variable_objects(&ui, &client, owned);
}

fn cleanup_viewer_variable_objects(
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

fn normalize_member_name(name: &str) -> String {
    name.trim()
        .trim_matches(['[', ']'])
        .rsplit("::")
        .next()
        .unwrap_or(name)
        .to_ascii_lowercase()
}

fn compact_variable_type_name(type_name: Option<&str>) -> String {
    type_name
        .map(super::compact_variable_type)
        .filter(|type_name| !type_name.is_empty())
        .unwrap_or_else(|| String::from("<unknown>"))
}

fn compact_viewer_text(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars();
    let compact = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{compact}...")
    } else {
        compact
    }
}

fn viewer_value_is_null(value: &str) -> bool {
    if pointer_address(value) == Some(0) {
        return true;
    }
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "0" | "null" | "nullptr" | "none" | "nil" | "<null>"
    )
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
            name: String::from("_M_elems"),
            value: String::from("{...}"),
            type_name: None,
            argument: false,
            varobj: Some(String::from("var1.public._M_elems")),
            num_children: 2,
            has_more: false,
        };
        assert_eq!(
            transparent_index_wrapper(std::slice::from_ref(&wrapper)),
            Some(wrapper.clone())
        );

        let access_group = Variable {
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
            name: name.to_owned(),
            value: String::from("{...}"),
            type_name: Some(type_name.to_owned()),
            argument: false,
            varobj: Some(format!("var1.{name}")),
            num_children: 1,
            has_more: false,
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
