use super::*;
use crate::app::test_support::{open_debugger, open_debugger_with_printers, request, wait_until};
use crate::debugger::StopContext;
use gtk::prelude::*;
use std::cell::Cell;

fn pager(
    client: &Rc<MiClient>,
    session: &Rc<VariableViewerSession>,
    expression: &str,
    valid: &Rc<Cell<bool>>,
) -> LinkedPager {
    let frame = crate::debugger::current_frame(&request(client, "-stack-info-frame")).unwrap();
    let record = request(
        client,
        &format!("-var-create - * {}", crate::debugger::quote(expression)),
    );
    assert!(record.is_done(), "{record:?}");
    let root = crate::debugger::variable_object(&record, expression).unwrap();
    let valid = Rc::clone(valid);
    let requests = client.bind_stop_requests(
        StopContext::new(
            client.transport_epoch(),
            1,
            Some(String::from("i1")),
            String::from("1"),
            frame.level,
        )
        .unwrap(),
        move |_| valid.get(),
    );

    LinkedPager {
        ui: Weak::new(),
        client: Rc::clone(client),
        requests,
        session: Rc::downgrade(session),
        root,
        settings: LinkedListSettings {
            next_members: vec![String::from("next"), String::from("link")],
            limit: 128,
            owned_root: None,
        },
        active: RefCell::new(None),
    }
}

fn restart(pager: &LinkedPager, member: &str, page_size: usize) -> Rc<RefCell<LinkedTraversal>> {
    pager.handle(LinkedListAction::Restart(LinkedListQuery {
        member: member.to_owned(),
        page_size,
    }));
    let active = pager.active.borrow().clone().unwrap();
    wait_until(|| {
        let active = active.borrow();
        active.finished || active.paused
    });
    active
}

fn next(pager: &LinkedPager, active: &Rc<RefCell<LinkedTraversal>>) {
    pager.handle(LinkedListAction::Next);
    wait_until(|| {
        let active = active.borrow();
        active.finished || active.paused
    });
}

#[test]
#[ignore = "requires GDB, linked-list fixture, and a GTK display, run separately"]
fn live_linked_pages_cycles_cancellation_and_stop_safety() {
    gtk::init().unwrap();
    let (_debugger, client) =
        open_debugger("c-linked-list-viewer-target", "linked_list_checkpoint");
    let (session, window) = VariableViewerSession::linked_test_session(128);
    let valid = Rc::new(Cell::new(true));
    let linear = pager(&client, &session, "linear_head", &valid);
    let null = request(&client, "-var-create null_tail * custom_head[4].tailward");
    let null = crate::debugger::variable_object(&null, "tailward").unwrap();
    assert!(
        null.num_children > 0,
        "The fixture should expose type-defined children"
    );
    assert!(null.is_null_pointer());
    assert!(!null.can_expand());
    let empty = crate::debugger::variable_children(&request(
        &client,
        "-var-list-children --all-values null_tail 0 2",
    ));
    assert!(empty.iter().all(|child| !child.can_expand()));
    let result = Rc::new(RefCell::new(None));
    let response = Rc::clone(&result);
    linear
        .requests
        .frame("-data-evaluate-expression linked_list_probe()")
        .inspect(32, move |_, record| {
            response.replace(Some(record));
        })
        .unwrap();
    wait_until(|| result.borrow().is_some());
    let rejected = result.take().unwrap();
    assert!(
        rejected
            .error_message()
            .is_some_and(|message| message.contains("may-call-functions")),
        "{rejected:?}"
    );
    assert_eq!(
        crate::debugger::evaluated_value(&request(&client, "-gdb-show may-call-functions"))
            .as_deref(),
        Some("on")
    );

    let response = Rc::clone(&result);
    linear
        .requests
        .frame("-data-evaluate-expression \"linear_head->value = 99\"")
        .inspect(32, move |_, record| {
            response.replace(Some(record));
        })
        .unwrap();
    wait_until(|| result.borrow().is_some());
    assert!(!result.take().unwrap().is_done());

    for parameter in ["may-write-memory", "may-write-registers"] {
        assert_eq!(
            crate::debugger::evaluated_value(&request(&client, &format!("-gdb-show {parameter}")))
                .as_deref(),
            Some("on")
        );
    }

    let active = restart(&linear, "", 128);
    assert_eq!(
        active.borrow().rows.len(),
        128,
        "{}",
        active.borrow().message
    );
    assert!(active.borrow().paused);
    assert!(active.borrow().rows[0].details.contains("value = 0"));
    assert!(active.borrow().rows[0].link.starts_with("next → 0x"));
    next(&linear, &active);
    assert_eq!(active.borrow().rows.len(), 256);
    assert_eq!(active.borrow().owned_variable_objects.len(), 1);
    let requests_sent = active.borrow().requests_sent;
    linear.handle(LinkedListAction::Previous);
    assert_eq!(active.borrow().offset, 0);
    linear.handle(LinkedListAction::Next);
    assert_eq!(active.borrow().offset, 128);
    assert_eq!(active.borrow().requests_sent, requests_sent);
    next(&linear, &active);
    assert_eq!(active.borrow().rows.len(), 300);
    assert!(
        active.borrow().message.contains("end"),
        "{}: {:?}",
        active.borrow().message,
        active.borrow().current
    );
    assert!(active.borrow().owned_variable_objects.is_empty());
    assert!(
        request(
            &client,
            &format!("-var-info-type {}", linear.root.varobj.as_ref().unwrap())
        )
        .is_done()
    );
    linear.handle(LinkedListAction::First);
    assert_eq!(active.borrow().offset, 0);

    for (expression, member, expected, message) in [
        ("cyclic_head", "", 3, "cycle"),
        ("*cyclic_head", "", 3, "cycle"),
        ("empty_head", "", 0, "null"),
        ("custom_head", "tailward", 5, "end"),
        ("linear_head", "value", 1, "not a pointer"),
        ("reverse_head", "prev", 300, "end"),
    ] {
        let pager = pager(&client, &session, expression, &valid);
        let active = restart(&pager, member, 128);

        while !active.borrow().finished {
            next(&pager, &active);
        }

        assert_eq!(
            active.borrow().rows.len(),
            expected,
            "{expression}: {}",
            active.borrow().message
        );
        assert!(
            active.borrow().message.contains(message),
            "{expression}: {}",
            active.borrow().message
        );
    }

    let broken = pager(&client, &session, "broken_head", &valid);
    let broken_page = restart(&broken, "", 128);
    assert!(broken_page.borrow().finished);
    assert!(broken_page.borrow().rows.len() <= 2);
    assert!(!broken_page.borrow().message.contains("reached the end"));

    let active = restart(&linear, "", 2);
    let requests_sent = active.borrow().requests_sent;
    valid.set(false);
    next(&linear, &active);
    assert_eq!(active.borrow().requests_sent, requests_sent);
    assert_eq!(active.borrow().message, STALE_VIEWER_MESSAGE);
    linear.handle(LinkedListAction::First);
    assert_eq!(active.borrow().offset, 0);
    linear.handle(LinkedListAction::Restart(LinkedListQuery {
        member: String::new(),
        page_size: 2,
    }));
    assert_eq!(active.borrow().message, STALE_VIEWER_MESSAGE);
    valid.set(true);

    let budget = restart(&linear, "", 2);
    budget.borrow_mut().requests_sent = REQUEST_BUDGET;
    next(&linear, &budget);
    assert!(budget.borrow().message.contains("request budget"));
    assert_eq!(budget.borrow().rows.len(), 2);
    let budget = restart(&linear, "", 2);
    budget.borrow_mut().fields_read = FIELD_BUDGET;
    next(&linear, &budget);
    assert!(budget.borrow().message.contains("Field inspection budget"));
    assert_eq!(budget.borrow().rows.len(), 2);

    linear.handle(LinkedListAction::Restart(LinkedListQuery {
        member: String::new(),
        page_size: 128,
    }));
    let cancelled = linear.active.borrow().clone().unwrap();
    wait_until(|| cancelled.borrow().rows.len() >= 2);
    let cached = cancelled.borrow().rows.len();
    let owned = cancelled
        .borrow()
        .owned_variable_objects
        .iter()
        .cloned()
        .collect::<Vec<_>>();
    linear.handle(LinkedListAction::Cancel);
    assert!(cancelled.borrow().finished);
    request(&client, "-gdb-show language");
    assert!(cancelled.borrow().message.starts_with("Cancelled"));
    assert!(cancelled.borrow().owned_variable_objects.is_empty());
    assert_eq!(cancelled.borrow().rows.len(), cached);

    for name in owned {
        assert!(!request(&client, &format!("-var-info-type {name}")).is_done());
    }

    linear.handle(LinkedListAction::Restart(LinkedListQuery {
        member: String::new(),
        page_size: 128,
    }));
    let closed = linear.active.borrow().clone().unwrap();
    window.close();
    wait_until(|| closed.borrow().finished);
    assert!(closed.borrow().owned_variable_objects.is_empty());
}

#[test]
#[ignore = "requires GDB, Rust fixture, and a GTK display, run separately"]
fn live_rust_shared_nodes_are_not_mistaken_for_wrapper_cycles() {
    gtk::init().unwrap();
    for rust_printers in [false, true] {
        check_rust_shared_nodes(rust_printers);
    }
}

fn check_rust_shared_nodes(rust_printers: bool) {
    let (_debugger, client) = open_debugger_with_printers(
        "rust-variable-viewer-target",
        "rust_variable_viewer_checkpoint",
        rust_printers,
    );
    // The first statement enters an inline allocation frame on some rustc versions.
    let frames = crate::debugger::stack_frames(&request(&client, "-stack-list-frames"));
    let frame = frames
        .iter()
        .find(|frame| frame.function.ends_with("rust_variable_viewer_checkpoint"))
        .unwrap();
    assert!(request(&client, &format!("-stack-select-frame {}", frame.level)).is_done());
    let (session, window) = VariableViewerSession::linked_test_session(128);
    let valid = Rc::new(Cell::new(true));
    let pager = pager(&client, &session, "shared_arg", &valid);
    let active = restart(&pager, "", 2);

    while !active.borrow().finished {
        next(&pager, &active);
    }

    assert_eq!(
        active.borrow().rows.len(),
        2,
        "{}: {:?}",
        active.borrow().message,
        active.borrow().rows
    );
    assert!(
        active.borrow().rows[0].details.contains("value = 1"),
        "{:?}",
        active.borrow().rows
    );
    assert!(
        active.borrow().rows[1].details.contains("value = 2"),
        "{:?}",
        active.borrow().rows
    );
    assert!(
        active.borrow().message.contains("end"),
        "{}: {:?}",
        active.borrow().message,
        active.borrow().current
    );
    assert!(
        active
            .borrow()
            .rows
            .iter()
            .all(|row| pointer_address(&row.name).is_some())
    );
    window.close();
}

#[test]
#[ignore = "requires GDB, C++ fixture, and a GTK display, run separately"]
fn live_cpp_access_groups_keep_their_links_across_pages() {
    gtk::init().unwrap();
    let (_debugger, client) =
        open_debugger("cpp-variable-viewer-target", "variable_viewer_checkpoint");
    let (session, window) = VariableViewerSession::linked_test_session(128);
    let valid = Rc::new(Cell::new(true));

    for (expression, expected, message) in [("linear_head", 4, "end"), ("cycle_head", 3, "cycle")] {
        let pager = pager(&client, &session, expression, &valid);
        let active = restart(&pager, "", 2);

        while !active.borrow().finished {
            next(&pager, &active);
        }

        assert_eq!(
            active.borrow().rows.len(),
            expected,
            "{}: {:?}",
            active.borrow().message,
            active.borrow().rows
        );
        assert!(active.borrow().message.contains(message));
    }

    window.close();
}

#[test]
#[ignore = "requires GDB, Rust fixture, and a GTK display, run separately"]
fn live_rust_cycles_use_node_identity_across_ownership_wrappers() {
    gtk::init().unwrap();

    for printers in [false, true] {
        let (_debugger, client) = open_debugger_with_printers(
            "rust-variable-viewer-target",
            "rust_linked_cycle_checkpoint",
            printers,
        );
        let frames = crate::debugger::stack_frames(&request(&client, "-stack-list-frames"));
        let frame = frames
            .iter()
            .find(|frame| frame.function.ends_with("rust_linked_cycle_checkpoint"))
            .unwrap();
        assert!(request(&client, &format!("-stack-select-frame {}", frame.level)).is_done());
        let (session, window) = VariableViewerSession::linked_test_session(128);
        let valid = Rc::new(Cell::new(true));

        for expression in ["node", "shared"] {
            let pager = pager(&client, &session, expression, &valid);
            let active = restart(&pager, "", 1);

            while !active.borrow().finished {
                assert!(
                    active.borrow().rows.len() <= 2,
                    "{}: {:?}",
                    active.borrow().message,
                    active.borrow().rows
                );
                next(&pager, &active);
            }

            let traversal = active.borrow();
            assert_eq!(
                traversal.rows.len(),
                2,
                "{expression}: {} {:?}",
                traversal.message,
                traversal.rows
            );
            assert!(
                traversal.message.to_ascii_lowercase().contains("cycle"),
                "{}",
                traversal.message
            );
            assert!(traversal.owned_variable_objects.is_empty());
        }

        window.close();
    }
}
