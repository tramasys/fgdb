use super::*;
use crate::debugger::{parse_record, variable_object, variable_updates};
use crate::ui::dialogs::clear_variable_change_markers;

fn root() -> VariableNode {
    let record = parse_record(
        r#"^done,name="root",numchild="300",value="[300]",type="int [300]",has_more="0""#,
    )
    .unwrap();

    VariableNode::new(variable_object(&record, "items").unwrap())
}

#[test]
fn pages_require_the_original_parent_and_expected_offset() {
    let node = root();
    let parent = &node.variable;
    assert!(node.accepts_child_page(parent, 0));
    assert!(!node.accepts_child_page(parent, 128));
    node.children_loaded.set(true);
    assert!(!node.accepts_child_page(parent, 0));

    node.children
        .append(&glib::BoxedAnyObject::new(VariableNode::load_more(
            parent.clone(),
            128,
        )));

    assert!(node.accepts_child_page(parent, 128));
    assert!(!node.accepts_child_page(parent, 0));
    assert!(!node.accepts_child_page(parent, 256));

    for stale in [
        Variable {
            varobj: Some("previous".into()),
            ..parent.clone()
        },
        Variable {
            type_name: Some("Other".into()),
            ..parent.clone()
        },
        Variable {
            num_children: 2,
            ..parent.clone()
        },
        Variable {
            local_index: Some(3),
            ..parent.clone()
        },
        Variable {
            value: "<out of scope>".into(),
            ..parent.clone()
        },
    ] {
        assert!(!node.accepts_child_page(&stale, 128), "{stale:?}");
    }

    let retry = VariableNode::retry_expansion(parent.clone(), "Cannot read memory");
    assert!(retry.search_text.contains("cannot read memory"));
    node.children
        .splice(0, 1, &[glib::BoxedAnyObject::new(retry)]);

    assert!(node.accepts_child_page(parent, 0));
    assert!(!node.accepts_child_page(parent, 128));
}

#[test]
fn metadata_only_updates_reuse_search_text_but_invalidate_child_layout() {
    let node = root();
    node.children_loaded.set(true);
    let mut metadata = node.variable.clone();
    metadata.num_children += 1;
    let updated = node.updated(metadata, false);
    assert!(Rc::ptr_eq(&node.search_text, &updated.search_text));
    assert_ne!(node.children, updated.children);
    assert!(!updated.children_loaded.get());

    let same = node.updated(node.variable.clone(), true);
    assert!(Rc::ptr_eq(&node.search_text, &same.search_text));
    assert_eq!(node.children, same.children);
    assert!(same.children_loaded.get());

    let changed = node.updated(
        Variable {
            value: "[301]".into(),
            ..node.variable.clone()
        },
        true,
    );

    assert!(!Rc::ptr_eq(&node.search_text, &changed.search_text));
    assert!(changed.changed);
}

#[test]
fn pointer_and_variant_target_changes_reject_old_pages() {
    for (type_name, hint, before, after) in [
        ("Node *", None, "0x1000", "0x2000"),
        ("Choice", Some("fgdb-variant"), "Some", "None"),
    ] {
        let mut node = root();
        node.variable.type_name = Some(type_name.into());
        node.variable.display_hint = hint.map(str::to_owned);
        node.variable.value = before.into();
        let stale = Variable {
            value: after.into(),
            ..node.variable.clone()
        };

        assert!(!node.accepts_child_page(&stale, 0));
    }
}

#[test]
fn independent_descendants_retain_their_local_snapshot_authority() {
    let mut variable = root().variable;
    variable.local_index = Some(4);
    let local = VariableNode::new(variable);
    let child = local.child(root().variable);
    let nested = child.child(root().variable);
    assert!(local.local);
    assert!(child.local);
    assert!(nested.local);
    assert!(nested.updated(nested.variable.clone(), false).local);
    assert!(nested.without_change_marker().local);
    assert_eq!(nested.variable.local_index, None);
    assert!(!root().child(root().variable).local);
}

#[test]
fn expansion_rebinding_does_not_duplicate_requests_or_hold_handler_borrows() {
    use crate::ui::{VariableChildrenHandler, views::request_variable_children_if_needed};
    use std::cell::RefCell;

    let node = root();
    let calls = Rc::new(Cell::new(0));
    let observed = Rc::clone(&calls);
    let handler: Rc<RefCell<Option<VariableChildrenHandler>>> = Rc::new(RefCell::new(None));
    let weak_handler = Rc::downgrade(&handler);

    *handler.borrow_mut() = Some(Rc::new(move |_, from| {
        assert_eq!(from, 0);
        observed.set(observed.get() + 1);
        weak_handler.upgrade().unwrap().borrow_mut().take();
    }));

    request_variable_children_if_needed(&node, &handler);
    request_variable_children_if_needed(&node, &handler);
    assert_eq!(calls.get(), 1);
    assert_eq!(node.children.n_items(), 1);
    assert!(node.children_loading.get());
    assert!(handler.borrow().is_none());

    let null = node.updated(
        Variable {
            value: "0x0".into(),
            type_name: Some("Node *".into()),
            ..node.variable.clone()
        },
        true,
    );

    request_variable_children_if_needed(&null, &handler);
    assert_eq!(null.children.n_items(), 0);
    assert!(!null.children_loading.get());
}

#[test]
fn type_and_scope_updates_retire_obsolete_printer_metadata() {
    let mut variable = root().variable;
    variable.dynamic = true;
    variable.display_hint = Some("array".into());
    variable.has_more = true;
    let record = parse_record(
        r#"^done,changelist=[{name="root",type_changed="true",new_type="int",value="7"}]"#,
    )
    .unwrap();

    variable.apply_update(&variable_updates(&record)[0]);
    assert_eq!(variable.type_name.as_deref(), Some("int"));
    assert_eq!(variable.value, "7");
    assert_eq!(variable.num_children, 0);
    assert!(!variable.dynamic);
    assert!(!variable.has_more);
    assert_eq!(variable.display_hint, None);
    assert!(!variable.can_expand());

    let record = parse_record(
        r#"^done,changelist=[{name="root",in_scope="false",dynamic="1",has_more="1",new_num_children="12"}]"#,
    )
    .unwrap();

    variable.apply_update(&variable_updates(&record)[0]);
    assert_eq!(variable.value, "<out of scope>");
    assert_eq!(variable.num_children, 0);
    assert!(!variable.can_expand());
}

#[test]
fn explicit_type_changes_invalidate_children_even_if_the_printed_type_is_unchanged() {
    let node = root();
    node.children_loaded.set(true);
    let record = parse_record(
        r#"^done,changelist=[{name="root",type_changed="true",new_type="int [300]",new_num_children="300",value="[300]"}]"#,
    )
    .unwrap();

    let updated = node.apply_update(&variable_updates(&record)[0]);
    assert_eq!(updated.variable, node.variable);
    assert_ne!(updated.children, node.children);
    assert!(!updated.children_loaded.get());
}

#[test]
fn clearing_descendant_changes_notifies_roots_once_after_children_are_clear() {
    let root = root();
    let mut child = VariableNode::new(root.variable.clone());
    child.changed = true;
    root.children.append(&glib::BoxedAnyObject::new(child));
    let store = gio::ListStore::new::<glib::BoxedAnyObject>();
    store.append(&glib::BoxedAnyObject::new(root));
    store.append(&glib::BoxedAnyObject::new(VariableNode::placeholder(
        "other", "",
    )));

    let notifications = Rc::new(Cell::new(0));
    let observed = Rc::clone(&notifications);

    store.connect_items_changed(move |store, position, removed, added| {
        assert_eq!((position, removed, added), (0, 1, 1));
        let item = store
            .item(position)
            .and_downcast::<glib::BoxedAnyObject>()
            .unwrap();

        assert!(!item.borrow::<VariableNode>().has_changes());
        observed.set(observed.get() + 1);
    });

    clear_variable_change_markers(&store);
    assert_eq!(notifications.get(), 1);
    clear_variable_change_markers(&store);
    assert_eq!(notifications.get(), 1);
}

#[test]
#[ignore = "requires a GTK display"]
fn changed_filter_reacts_to_cleared_descendant_markers() {
    gtk::init().unwrap();
    let root = root();
    let mut child = VariableNode::new(root.variable.clone());
    child.changed = true;
    root.children.append(&glib::BoxedAnyObject::new(child));
    let store = gio::ListStore::new::<glib::BoxedAnyObject>();
    store.append(&glib::BoxedAnyObject::new(root));

    let filter = gtk::CustomFilter::new(|item| {
        item.downcast_ref::<glib::BoxedAnyObject>()
            .unwrap()
            .borrow::<VariableNode>()
            .has_changes()
    });

    let filtered = gtk::FilterListModel::new(Some(store.clone()), Some(filter));
    assert_eq!(filtered.n_items(), 1);
    clear_variable_change_markers(&store);
    assert_eq!(filtered.n_items(), 0);
}
