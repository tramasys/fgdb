use super::*;
use crate::debugger::linked::{
    FIELD_BUDGET, LinkedListAction, LinkedListProgress, LinkedListQuery, NODE_LIMIT, REQUEST_BUDGET,
};

mod navigation;
mod objects;
use navigation::render_linked;
pub(super) use navigation::{LinkedListSettings, start_linked_list};
use objects::{prepare_linked_root, request_linked_dereference, request_linked_raw_wrapper};

struct NodeAddress {
    base: u64,
    type_name: String,
    members: Vec<String>,
}

impl NodeAddress {
    fn from_pointer(variable: &Variable) -> Option<Self> {
        Some(Self {
            base: variable.pointer_address()?,
            type_name: variable.type_name.clone()?,
            members: Vec::new(),
        })
    }
}

const LINKED_NODE_FIELD_LIMIT: usize = 256;
const MAX_LINK_WRAPPER_DEPTH: usize = 16;

struct LinkedTraversal {
    ui: Weak<Ui>,
    client: Rc<MiClient>,
    requests: StopRequests,
    session: Weak<VariableViewerSession>,
    revision: u64,
    current: Variable,
    current_address: Option<u64>,
    pending_node: Option<(Variable, Vec<Variable>)>,
    address_source: Option<NodeAddress>,
    member: String,
    next_members: HashSet<String>,
    seen_addresses: HashSet<u64>,
    seen_objects: HashSet<String>,
    owned_variable_objects: HashSet<String>,
    wrapper_depth: usize,
    page_size: usize,
    offset: usize,
    rows: Vec<VariableViewerRow>,
    message: String,
    paused: bool,
    fields_read: usize,
    requests_sent: usize,
    fields_truncated: bool,
    finished: bool,
}

impl Drop for LinkedTraversal {
    fn drop(&mut self) {
        cleanup_viewer_variable_objects(
            &self.ui,
            &self.client,
            self.owned_variable_objects.drain(),
        );
    }
}

fn linked_is_current(traversal: &Rc<RefCell<LinkedTraversal>>) -> bool {
    let traversal = traversal.borrow();

    !traversal.finished
        && !traversal.paused
        && traversal.requests.is_current()
        && traversal
            .session
            .upgrade()
            .is_some_and(|session| session.page_is_current(traversal.revision))
}

fn begin_linked_request(traversal: &Rc<RefCell<LinkedTraversal>>) -> bool {
    if !linked_is_current(traversal) {
        finish_linked(traversal, Some(String::from(STALE_VIEWER_MESSAGE)));
        return false;
    }

    let allowed = {
        let mut traversal = traversal.borrow_mut();
        traversal.requests_sent += 1;
        traversal.requests_sent <= REQUEST_BUDGET
    };

    if !allowed {
        finish_linked(
            traversal,
            Some(String::from(
                "Traversal request budget reached · Cached nodes retained",
            )),
        );
    }

    allowed
}

fn request_linked_node(traversal: Rc<RefCell<LinkedTraversal>>) {
    if !linked_is_current(&traversal) {
        finish_linked(&traversal, Some(String::from(STALE_VIEWER_MESSAGE)));
        return;
    }

    let (current, shown, page_end) = {
        let traversal = traversal.borrow();

        (
            traversal.current.clone(),
            traversal.rows.len(),
            traversal.offset.saturating_add(traversal.page_size),
        )
    };

    if shown >= NODE_LIMIT {
        finish_linked(
            &traversal,
            Some(format!("{shown} nodes cached · Node safety limit reached")),
        );

        return;
    }

    if shown >= page_end {
        {
            let mut traversal = traversal.borrow_mut();
            traversal.paused = true;
            traversal.message = String::from(
                "Page ready · More links available · Previous pages use cached values",
            );
        }

        render_linked(&traversal, false);
        return;
    }

    if linked_value_is_end(&current) {
        finish_linked(&traversal, Some(format!("{shown} nodes - reached null")));
        return;
    }

    if !current.is_available() {
        finish_linked(
            &traversal,
            Some(String::from(
                "Node memory is unreadable or unavailable · Cached nodes retained",
            )),
        );
        return;
    }

    if current.num_children == 0 && !current.has_more && current.is_pointer() {
        request_linked_dereference(traversal, current);
    } else if current.dynamic
        && super::heuristics::link_wrapper_members(&current).is_some()
        && current.varobj.as_deref().is_some_and(|name| {
            traversal
                .borrow()
                .owned_variable_objects
                .contains(name.split('.').next().unwrap_or(name))
        })
    {
        request_linked_raw_wrapper(traversal, current);
    } else {
        request_linked_children(traversal, current);
    }
}

fn request_linked_children(traversal: Rc<RefCell<LinkedTraversal>>, current: Variable) {
    if !begin_linked_request(&traversal) {
        return;
    }
    let Some(varobj) = current.varobj.as_deref() else {
        finish_linked(
            &traversal,
            Some(String::from("This node has no inspectable GDB object")),
        );

        return;
    };

    let requests = traversal.borrow().requests.clone();

    let command = format!(
        "-var-list-children --all-values {} 0 {LINKED_NODE_FIELD_LIMIT}",
        crate::debugger::quote(varobj)
    );

    let traversal_for_guard = Rc::clone(&traversal);
    let traversal_for_response = Rc::clone(&traversal);

    if let Err(error) = requests
        .unscoped(&command)
        .when(move || linked_is_current(&traversal_for_guard))
        .inspect(AUTOMATIC_PRINT_ELEMENTS, move |_, record| {
            if record.class == "superseded" || !linked_is_current(&traversal_for_response) {
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

            let Some(children) = linked_fields(&traversal_for_response, &record) else {
                return;
            };
            complete_linked_node(&traversal_for_response, current, children);
        })
    {
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

    if !begin_linked_request(&traversal) {
        return;
    }

    let Some(varobj) = group.varobj.as_deref() else {
        request_linked_access_groups(traversal, current, groups, fields);
        return;
    };

    let requests = traversal.borrow().requests.clone();

    let command = format!(
        "-var-list-children --all-values {} 0 {LINKED_NODE_FIELD_LIMIT}",
        crate::debugger::quote(varobj)
    );

    let traversal_for_guard = Rc::clone(&traversal);
    let traversal_for_response = Rc::clone(&traversal);

    if let Err(error) = requests
        .unscoped(&command)
        .when(move || linked_is_current(&traversal_for_guard))
        .inspect(AUTOMATIC_PRINT_ELEMENTS, move |_, record| {
            if record.class == "superseded" || !linked_is_current(&traversal_for_response) {
                finish_linked(
                    &traversal_for_response,
                    Some(String::from(STALE_VIEWER_MESSAGE)),
                );

                return;
            }

            if record.is_done() {
                let Some(children) = linked_fields(&traversal_for_response, &record) else {
                    return;
                };
                fields.extend(children);
                request_linked_access_groups(traversal_for_response, current, groups, fields);
            } else {
                let message = record
                    .error_message()
                    .unwrap_or("GDB could not inspect a C++ access group")
                    .to_owned();

                finish_linked(&traversal_for_response, Some(message));
            }
        })
    {
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
    let matches = {
        let traversal = traversal.borrow();

        children
            .iter()
            .enumerate()
            .filter(|(_, child)| link_matches(&traversal, &child.name))
            .map(|(index, _)| index)
            .collect::<Vec<_>>()
    };

    if matches.len() > 1 {
        finish_linked(
            traversal,
            Some(String::from(
                "Multiple possible link fields found · Specify the link member and restart",
            )),
        );
        return;
    }

    let has_next = !matches.is_empty();

    if !children.is_empty()
        && children
            .iter()
            .all(|child| child.value.is_empty() || !child.is_available())
    {
        finish_linked(
            traversal,
            Some(String::from(
                "Node memory is unreadable or unavailable · Cached nodes retained",
            )),
        );
        return;
    }

    if !has_next && super::heuristics::linked_children_are_end(&current, &children) {
        let shown = traversal.borrow().rows.len();
        finish_linked(traversal, Some(format!("{shown} nodes - reached the end")));
        return;
    }

    if !has_next && let Some(wrapper) = transparent_link_wrapper(&current, &children) {
        let address = wrapper.pointer_address().filter(|address| *address != 0);

        let (cycle, depth_exceeded, shown) = {
            let mut traversal = traversal.borrow_mut();

            // Ownership wrappers can legitimately share one allocation address.
            // Detect node-address cycles only after unwrapping a complete node.
            let cycle = if let Some(varobj) = wrapper.varobj.as_ref() {
                !traversal.seen_objects.insert(varobj.clone())
            } else {
                false
            };

            traversal.wrapper_depth = traversal.wrapper_depth.saturating_add(1);
            let depth_exceeded = traversal.wrapper_depth > MAX_LINK_WRAPPER_DEPTH;

            if !cycle && !depth_exceeded {
                if let Some(source) = NodeAddress::from_pointer(&wrapper) {
                    traversal.address_source = Some(source);
                } else {
                    if traversal.address_source.is_none() {
                        traversal.address_source = NodeAddress::from_pointer(&current);
                    }

                    if let Some(source) = traversal.address_source.as_mut() {
                        source.members.push(wrapper.name.clone());
                    }
                }

                traversal.current = wrapper;

                if address.is_some() {
                    traversal.current_address = address;
                }
            }

            (cycle, depth_exceeded, traversal.rows.len())
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

    if !has_next && super::heuristics::link_wrapper_members(&current).is_some() {
        finish_linked(
            traversal,
            Some(String::from(
                "Ownership wrapper has no readable node · Cached nodes retained",
            )),
        );
        return;
    }

    if traversal.borrow().wrapper_depth > 0 && !current.is_pointer() {
        {
            let mut traversal = traversal.borrow_mut();
            // Allocation headers and their payloads have different addresses.
            // Record the actual node, including when the root was opened by value.
            traversal.current_address = None;
            traversal.pending_node = Some((current, children));
        }

        objects::resolve_linked_node_address(Rc::clone(traversal));
    } else {
        append_linked_node(traversal, current, children);
    }
}

fn append_linked_node(
    traversal: &Rc<RefCell<LinkedTraversal>>,
    current: Variable,
    children: Vec<Variable>,
) {
    let repeated = {
        let traversal = traversal.borrow();
        traversal
            .current_address
            .is_some_and(|address| traversal.seen_addresses.contains(&address))
    };

    if repeated {
        finish_linked(
            traversal,
            Some(String::from("Cycle detected · Cached nodes retained")),
        );
        return;
    }

    let (next, row, session, shown) = {
        let mut traversal = traversal.borrow_mut();
        traversal.wrapper_depth = 0;
        traversal.seen_objects.clear();

        if let Some(address) = traversal.current_address {
            traversal.seen_addresses.insert(address);
        }

        let next = children
            .iter()
            .find(|child| link_matches(&traversal, &child.name))
            .cloned();

        let details = children
            .iter()
            .filter(|child| !link_matches(&traversal, &child.name))
            .take(6)
            .map(|child| {
                format!(
                    "{} = {}",
                    compact_viewer_text(&child.name, 72),
                    compact_viewer_text(&child.value, 72)
                )
            })
            .collect::<Vec<_>>()
            .join("  ");

        let row = VariableViewerRow {
            ordinal: traversal.rows.len().to_string(),
            name: traversal
                .current_address
                .map(|address| format!("0x{address:x}"))
                .unwrap_or_else(|| compact_viewer_text(&current.name, 160)),
            value: compact_viewer_text(&current.value, 320),
            type_name: compact_viewer_text(
                &compact_variable_type_name(current.type_name.as_deref()),
                1024,
            ),
            details,
            link: next
                .as_ref()
                .map(|next| {
                    format!(
                        "{} → {}",
                        compact_viewer_text(&next.name, 72),
                        compact_viewer_text(&next.value, 160)
                    )
                })
                .unwrap_or_else(|| String::from("No link found")),
        };

        traversal.rows.push(row.clone());

        (next, row, traversal.session.upgrade(), traversal.rows.len())
    };

    if let Some(session) = session {
        session.append([row]);
    }

    let Some(next) = next else {
        let reason = if traversal.borrow().fields_truncated {
            "Link not found within the bounded field preview"
        } else {
            "No matching link member found · Specify a field and restart"
        };

        finish_linked(
            traversal,
            Some(format!("{shown} nodes cached · {}", reason)),
        );

        return;
    };

    let next_address = next.pointer_address();

    if !next.is_available()
        || (next.is_pointer() && next_address.is_none() && !next.is_null_pointer())
    {
        finish_linked(
            traversal,
            Some(String::from(
                "Link field is unreadable or unavailable · Cached nodes retained",
            )),
        );
        return;
    }

    if linked_value_is_end(&next) {
        finish_linked(
            traversal,
            Some(format!(
                "{shown} node{} - reached the end",
                if shown == 1 { "" } else { "s" }
            )),
        );

        return;
    }

    if !next.can_expand() {
        finish_linked(
            traversal,
            Some(String::from(
                "Link member is not a pointer or an inspectable node · Specify a link field and restart",
            )),
        );
        return;
    }

    let cycle = {
        let mut traversal = traversal.borrow_mut();

        let cycle = if let Some(address) = next_address {
            traversal.seen_addresses.contains(&address)
        } else {
            false
        };

        if !cycle {
            traversal.address_source = None;
            traversal.current = next;
            traversal.current_address = next_address;
            traversal.fields_truncated = false;
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
    let (client, ui, owned) = {
        let mut traversal = traversal.borrow_mut();

        if traversal.finished {
            return;
        }

        traversal.finished = true;
        traversal.paused = false;
        traversal.pending_node = None;

        if let Some(message) = message {
            traversal.message = message;
        }

        (
            Rc::clone(&traversal.client),
            traversal.ui.clone(),
            traversal.owned_variable_objects.drain().collect::<Vec<_>>(),
        )
    };

    cleanup_viewer_variable_objects(&ui, &client, owned);
    render_linked(traversal, false);
}

fn link_matches(traversal: &LinkedTraversal, name: &str) -> bool {
    if traversal.member.is_empty() {
        traversal
            .next_members
            .contains(&normalize_member_name(name))
    } else {
        name.rsplit("::").next().unwrap_or(name) == traversal.member
    }
}

fn linked_fields(
    traversal: &Rc<RefCell<LinkedTraversal>>,
    record: &MiRecord,
) -> Option<Vec<Variable>> {
    let children = crate::debugger::variable_children(record);
    let allowed = {
        let mut traversal = traversal.borrow_mut();
        traversal.fields_read = traversal.fields_read.saturating_add(children.len());
        traversal.fields_truncated |= crate::debugger::variable_children_have_more(record);
        traversal.fields_read <= FIELD_BUDGET
    };

    if allowed {
        Some(children)
    } else {
        finish_linked(
            traversal,
            Some(String::from(
                "Field inspection budget reached · Cached nodes retained",
            )),
        );
        None
    }
}
