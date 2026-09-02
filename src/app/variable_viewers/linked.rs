use super::*;

const LINKED_NODE_FIELD_LIMIT: usize = 256;
const MAX_LINK_WRAPPER_DEPTH: usize = 16;

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

pub(super) struct LinkedListSettings {
    pub(super) next_members: Vec<String>,
    pub(super) limit: usize,
    pub(super) owned_root: Option<String>,
}

pub(super) fn start_linked_list(
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
    let (varobj, should_query, client, generation) = {
        let traversal = traversal.borrow();
        (
            traversal.current.varobj.clone(),
            traversal.current_address.is_none() && !traversal.current.is_pointer(),
            Rc::clone(&traversal.client),
            traversal.generation,
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
        .request_for_stop(
            &command,
            generation,
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
                let command = {
                    let traversal = traversal_for_response.borrow();
                    frame_scoped_stop_command(&traversal.ui, traversal.generation, &command)
                };
                let Some(command) = command else {
                    finish_linked(
                        &traversal_for_response,
                        Some(String::from(STALE_VIEWER_MESSAGE)),
                    );
                    return;
                };
                let generation = traversal_for_response.borrow().generation;
                let traversal_for_guard = Rc::clone(&traversal_for_response);
                let traversal_for_address = Rc::clone(&traversal_for_response);
                if client
                    .request_for_stop(
                        &command,
                        generation,
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
    let (client, generation) = {
        let traversal = traversal.borrow();
        (Rc::clone(&traversal.client), traversal.generation)
    };
    let command = format!(
        "-var-info-path-expression {}",
        crate::debugger::quote(varobj)
    );
    let traversal_for_guard = Rc::clone(&traversal);
    let traversal_for_response = Rc::clone(&traversal);
    if let Err(error) = client.request_for_stop(
        &command,
        generation,
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
            let command = {
                let traversal = traversal_for_response.borrow();
                frame_scoped_stop_command(&traversal.ui, traversal.generation, &command)
            };
            let Some(command) = command else {
                finish_linked(
                    &traversal_for_response,
                    Some(String::from(STALE_VIEWER_MESSAGE)),
                );
                return;
            };
            let traversal_for_guard = Rc::clone(&traversal_for_response);
            let traversal_for_dereference = Rc::clone(&traversal_for_response);
            let client = Rc::clone(&traversal_for_response.borrow().client);
            if let Err(error) = client.request_with_print_limit_for_stop(
                &command,
                AUTOMATIC_PRINT_ELEMENTS,
                generation,
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
    let (client, generation) = {
        let traversal = traversal.borrow();
        (Rc::clone(&traversal.client), traversal.generation)
    };
    let command = format!(
        "-var-list-children --all-values {} 0 {LINKED_NODE_FIELD_LIMIT}",
        crate::debugger::quote(varobj)
    );
    let traversal_for_guard = Rc::clone(&traversal);
    let traversal_for_response = Rc::clone(&traversal);
    if let Err(error) = client.request_with_print_limit_for_stop(
        &command,
        AUTOMATIC_PRINT_ELEMENTS,
        generation,
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
    let (client, generation) = {
        let traversal = traversal.borrow();
        (Rc::clone(&traversal.client), traversal.generation)
    };
    let command = format!(
        "-var-list-children --all-values {} 0 {LINKED_NODE_FIELD_LIMIT}",
        crate::debugger::quote(varobj)
    );
    let traversal_for_guard = Rc::clone(&traversal);
    let traversal_for_response = Rc::clone(&traversal);
    if let Err(error) = client.request_with_print_limit_for_stop(
        &command,
        AUTOMATIC_PRINT_ELEMENTS,
        generation,
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
