use super::*;

fn snapshot() -> LockSnapshot {
    let waits = [(10, Some(20)), (20, Some(30)), (30, Some(20)), (40, None)]
        .into_iter()
        .map(|(tid, owner)| {
            let mut wait = LockWait {
                tid,
                thread: format!("worker-{tid}"),
                address: Some(u64::from(tid) * 16),
                operation: "FUTEX_WAIT".into(),
                ..Default::default()
            };

            if let Some(owner) = owner {
                wait.observation.ownership = crate::misc::LockOwnership::RobustCandidate(owner);
            }

            wait
        })
        .collect();

    LockSnapshot {
        threads_scanned: 4,
        waits,
        dependencies: vec![edge(10, 20), edge(20, 30), edge(30, 20)],
        ..Default::default()
    }
}

fn assert_selection(view: &LocksView, tid: u32, root: u32, chain_position: u32) {
    assert_eq!(view.selected_wait().unwrap().tid, tid);
    assert_eq!(view.chain_root.get().unwrap().tid, root);
    assert_eq!(view.dependency_selection.selected(), chain_position);

    assert!(
        view.word
            .text()
            .contains(&format!("Selected waiter  {tid} "))
    );

    if chain_position != gtk::INVALID_LIST_POSITION {
        let item = view
            .dependency_selection
            .selected_item()
            .and_downcast::<glib::BoxedAnyObject>()
            .unwrap();

        assert_eq!(item.borrow::<details::ChainRow>().key().tid, tid);
    }
}

fn descendants(root: &impl IsA<gtk::Widget>) -> Vec<gtk::Widget> {
    let mut widgets = Vec::new();
    let mut child = root.as_ref().first_child();

    while let Some(widget) = child {
        child = widget.next_sibling();
        widgets.extend(descendants(&widget));
        widgets.push(widget);
    }

    widgets
}

#[test]
#[ignore = "requires a GTK display, run separately from other GTK tests"]
fn lock_selection_keeps_table_chain_and_actions_aligned() {
    gtk::init().unwrap();
    Theme::graphite().install();
    let view = build_locks_page(&ColumnLayouts::default());
    let events = Rc::new(RefCell::new(Vec::new()));
    let captured = Rc::clone(&events);
    let weak = Rc::downgrade(&view);

    view.handler.replace(Some(Rc::new(move |action| {
        let view = weak.upgrade().unwrap();
        assert!(!view.updating.get());
        let tid = view.selected_wait().map(|wait| wait.tid);
        captured.borrow_mut().push((action, tid));
    })));

    let snapshot = snapshot();
    view.show(snapshot.clone());
    view.selection.set_selected(0);
    assert_selection(&view, 10, 10, 0);
    let first_edge = view.dependency_store.item(0).unwrap();
    view.dependency_selection.set_selected(1);
    assert_selection(&view, 20, 10, 1);
    assert_eq!(view.dependency_store.item(0).unwrap(), first_edge);
    view.navigate(LockAction::Follow);
    assert_selection(&view, 30, 30, 0);
    view.navigate(LockAction::Back);
    assert_selection(&view, 20, 10, 1);
    view.navigate(LockAction::Back);
    assert_selection(&view, 10, 10, 0);
    assert!(view.history.borrow().is_empty());
    view.dependency_selection.set_selected(1);
    events.borrow_mut().clear();
    let mut reordered = snapshot.clone();
    reordered.waits.reverse();
    view.show(reordered.clone());
    assert_selection(&view, 20, 10, 1);
    assert_eq!(view.selection.selected(), 2);
    assert_eq!(&*events.borrow(), &[(LockAction::Selection, Some(20))]);
    assert!(view.history.borrow().is_empty());
    events.borrow_mut().clear();
    view.show(reordered.clone());
    assert_selection(&view, 20, 10, 1);
    assert_eq!(&*events.borrow(), &[(LockAction::Selection, Some(20))]);
    reordered.dependencies.remove(0);
    view.show(reordered.clone());
    assert_selection(&view, 20, 20, 0);

    // A reused TID waiting on another address is not the selected lock.
    reordered.waits[2].address = Some(0x9999);
    view.show(reordered);
    assert!(view.selected_wait().is_none());
    assert!(view.chain_root.get().is_none());
    assert!(view.word.text().is_empty());
    view.show(snapshot.clone());
    view.selection.set_selected(0);
    view.dependency_selection.set_selected(1);
    let mut missing_root = snapshot.clone();
    missing_root.waits.remove(0);
    missing_root.dependencies.remove(0);
    view.show(missing_root);
    assert_selection(&view, 20, 20, 0);

    let window = gtk::Window::builder()
        .title("Lock selection regression")
        .default_width(1200)
        .default_height(850)
        .child(&view.root)
        .build();

    view.show(snapshot.clone());
    view.selection.set_selected(0);
    window.present();

    glib::MainContext::default()
        .block_on(glib::timeout_future(std::time::Duration::from_millis(100)));

    let tables = descendants(&view.root)
        .into_iter()
        .filter_map(|widget| widget.downcast::<gtk::ColumnView>().ok())
        .collect::<Vec<_>>();

    assert_eq!(tables.len(), 2);

    for table in &tables {
        let labels = descendants(table)
            .into_iter()
            .filter(|widget| widget.has_css_class("debug-table-cell"))
            .map(|widget| widget.downcast::<gtk::Label>().unwrap())
            .collect::<Vec<_>>();

        assert!(!labels.is_empty());
        assert!(!table.is_single_click_activate());

        for label in labels {
            assert!(!label.is_selectable());
            assert!(!label.is_focusable());
        }
    }

    assert!(view.word.is_selectable());
    let dependencies = tables.iter().find(|table| **table != view.waiters).unwrap();

    // Exercise the same row action GTK uses for pointer/keyboard selection.
    let cell = descendants(&view.waiters)
        .into_iter()
        .find(|widget| widget.has_css_class("debug-table-cell"))
        .unwrap();

    cell.activate_action(
        "list.select-item",
        Some(&(1_u32, false, false).to_variant()),
    )
    .unwrap();

    assert_selection(&view, 20, 20, 0);
    assert!(view.history.borrow().is_empty());

    cell.activate_action("list.select-item", Some(&(1_u32, true, false).to_variant()))
        .unwrap();

    assert_selection(&view, 20, 20, 0);
    view.selection.set_selected(0);
    dependencies.emit_by_name::<()>("activate", &[&1_u32]);
    assert_selection(&view, 20, 10, 1);

    assert_eq!(
        events.borrow().last(),
        Some(&(LockAction::Follow, Some(20)))
    );

    view.waiters.emit_by_name::<()>("activate", &[&3_u32]);
    assert_selection(&view, 40, 40, gtk::INVALID_LIST_POSITION);

    assert_eq!(
        events.borrow().last(),
        Some(&(LockAction::Waiter, Some(40)))
    );

    let event_count = events.borrow().len();

    view.waiters
        .emit_by_name::<()>("activate", &[&gtk::INVALID_LIST_POSITION]);

    dependencies.emit_by_name::<()>("activate", &[&0_u32]);
    assert_eq!(events.borrow().len(), event_count);
    view.selection.set_selected(0);

    for _ in 0..100 {
        view.navigate(LockAction::Follow);
    }

    assert_eq!(view.history.borrow().len(), 64);
    view.invalidate();
    let selected = view.selection_state();
    view.navigate(LockAction::Back);
    assert_eq!(view.selection_state(), selected);
    view.clear();
    assert!(view.selected_wait().is_none());
    assert!(view.history.borrow().is_empty());
    let mut self_cycle = snapshot;
    self_cycle.waits[0].observation.ownership = crate::misc::LockOwnership::RobustCandidate(10);
    self_cycle.dependencies = vec![edge(10, 10)];
    view.show(self_cycle);
    view.selection.set_selected(0);
    view.navigate(LockAction::Follow);
    assert_selection(&view, 10, 10, 0);
    assert!(view.history.borrow().is_empty());
    window.close();
    let weak = Rc::downgrade(&view);
    drop(view);
    assert!(weak.upgrade().is_none());
}
