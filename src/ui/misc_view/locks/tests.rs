use super::*;

mod controls;
mod interaction;

fn edge(waiter: u32, owner: u32) -> LockDependency {
    LockDependency {
        waiter_tid: waiter,
        waiter: format!("worker-{waiter}"),
        owner_tid: owner,
        owner: format!("worker-{owner}"),
        address: u64::from(waiter) * 16,
        futex_value: owner,
    }
}

#[test]
fn chain_navigation_marks_only_cycle_edges_and_terminates() {
    let snapshot = LockSnapshot {
        dependencies: vec![edge(1, 2), edge(2, 3), edge(3, 2), edge(4, 5)],
        ..Default::default()
    };

    let rows = details::chain_rows(&snapshot, 1);
    assert_eq!(rows.len(), 3);
    assert!(!rows[0].cycle);
    assert!(rows[1].cycle && rows[2].cycle);
    assert_eq!(details::chain_rows(&snapshot, 4).len(), 1);
    assert!(details::chain_rows(&snapshot, 5).is_empty());

    let snapshot = LockSnapshot {
        dependencies: vec![edge(1, 1)],
        ..Default::default()
    };

    let rows = details::chain_rows(&snapshot, 1);
    assert_eq!(rows.len(), 1);
    assert!(rows[0].cycle);
}

#[test]
fn thread_navigation_does_not_confuse_os_tids_with_gdb_ids_or_other_inferiors() {
    let thread = |id: &str, group: &str, state: &str| ThreadInfo {
        id: id.into(),
        group_id: Some(group.into()),
        target_id: "Thread 0x123 (LWP 73)".into(),
        name: None,
        state: state.into(),
        core: None,
        frame: None,
        pc_symbol: None,
        current: false,
    };

    let threads = [thread("2", "i1", "stopped"), thread("73", "i2", "stopped")];

    assert_eq!(
        actions::thread_for_tid(&threads, "i1", 73).as_deref(),
        Some("2")
    );

    assert!(actions::thread_for_tid(&threads, "i1", 2).is_none());
    assert!(actions::thread_for_tid(&[thread("2", "i1", "running")], "i1", 73).is_none());

    assert!(
        actions::thread_for_tid(
            &[thread("2", "i1", "stopped"), thread("3", "i1", "stopped")],
            "i1",
            73
        )
        .is_none()
    );
}

#[test]
fn operation_flags_preserve_unknown_bits_and_distinguish_wait_vectors() {
    let mut wait = LockWait {
        operation: "FUTEX_WAIT".into(),
        operation_flags: Some(0x380),
        ..Default::default()
    };

    assert_eq!(
        details::flags_text(&wait),
        "Flags 0x380 · private · realtime clock"
    );

    wait.operation_flags = Some(0);
    assert!(details::flags_text(&wait).contains("process-shared"));
    wait.operation = "FUTEX_WAITV".into();
    assert_eq!(details::flags_text(&wait), "Wait-vector flags 0x0");
    wait.operation_flags = None;
    assert_eq!(details::flags_text(&wait), "Operation flags unavailable");
}

#[test]
#[ignore = "requires a GTK display, run separately from other GTK tests"]
fn lock_view_groups_waiters_preserves_selection_and_clears_stale_details() {
    gtk::init().unwrap();
    let view = build_locks_page(&ColumnLayouts::default());

    let mut first = LockWait {
        tid: 10,
        thread: "alpha".into(),
        address: Some(0x1000),
        operation: "FUTEX_WAIT".into(),
        expected: Some(0x8000_0014),
        operation_flags: Some(0x80),
        ..Default::default()
    };

    first.observation.word = Some(0x8000_0014);
    first.observation.ownership = crate::misc::LockOwnership::RobustCandidate(20);

    first.observation.mapping = Some(crate::misc::ProcessMapping {
        start: 0x1000,
        end: 0x2000,
        permissions: "rw-p".into(),
        path: "[heap]".into(),
    });

    let mut second = LockWait {
        tid: 20,
        thread: "beta".into(),
        ..first.clone()
    };

    second.observation.ownership = crate::misc::LockOwnership::RobustCandidate(10);

    let third = LockWait {
        tid: 30,
        thread: "gamma".into(),
        address: Some(0x2000),
        ..Default::default()
    };

    let snapshot = LockSnapshot {
        threads_scanned: 3,
        waits: vec![first.clone(), second, third],
        dependencies: vec![
            LockDependency {
                address: 0x1000,
                ..edge(10, 20)
            },
            LockDependency {
                address: 0x1000,
                ..edge(20, 10)
            },
        ],
        ..Default::default()
    };

    view.context.replace(Some(LockContext {
        generation: 7,
        inferior: "i1".into(),
        pid: 42,
    }));

    view.show(snapshot.clone());
    view.selection.set_selected(0);
    assert!(view.detail.text().contains("2 waiter(s)"));
    assert!(view.detail.text().contains("10 alpha, 20 beta"));
    assert!(view.word.text().contains("0x80000014"));
    assert!(view.mapping.text().contains("[heap]"));
    assert!(view.evidence.text().contains("inference"));
    assert!(view.graph_summary.has_css_class("status-error"));
    assert_eq!(view.dependency_store.n_items(), 2);
    view.navigate(LockAction::Follow);
    assert_eq!(view.selected_wait().unwrap().tid, 20);
    view.navigate(LockAction::Back);
    assert_eq!(view.selected_wait().unwrap().tid, 10);
    view.dependency_selection.set_selected(1);
    assert_eq!(view.selected_wait().unwrap().tid, 20);
    view.navigate(LockAction::Follow);
    assert_eq!(view.selected_wait().unwrap().tid, 10);
    assert!(view.context_matches(7, Some("i1"), Some(42)));
    assert!(!view.context_matches(8, Some("i1"), Some(42)));
    assert!(!view.context_matches(7, Some("i2"), Some(42)));
    assert!(!view.context_matches(7, Some("i1"), Some(99)));
    let first_request = view.begin_symbol(11).unwrap();

    view.select(WaitKey {
        tid: 30,
        address: Some(0x2000),
    });

    assert!(view.begin_symbol(11).is_none());
    view.complete_symbol(first_request, "first lock symbol".into());
    assert!(!view.symbol.text().contains("first lock symbol"));
    let second_request = view.begin_symbol(11).unwrap();
    view.complete_symbol(first_request, "outdated reply".into());
    assert_eq!(view.symbol_pending.get(), Some(second_request));
    view.complete_symbol(second_request, "second lock symbol".into());
    assert!(view.symbol.text().contains("second lock symbol"));
    view.select(WaitKey::from(&first));
    assert!(view.symbol.text().contains("first lock symbol"));
    assert!(view.begin_symbol(11).is_none());

    let window = gtk::Window::builder()
        .title("Lock inspector regression")
        .default_width(1100)
        .default_height(850)
        .child(&view.root)
        .build();

    Theme::graphite().install();
    view.root.set_position(300);
    window.present();

    glib::MainContext::default()
        .block_on(glib::timeout_future(std::time::Duration::from_millis(250)));

    assert!(view.root.is_mapped());

    if let Some(path) = std::env::var_os("FGDB_LOCK_VIEW_CAPTURE") {
        let paintable = gtk::WidgetPaintable::new(Some(&view.root));
        let snapshot = gtk::Snapshot::new();

        paintable.snapshot(
            &snapshot,
            f64::from(view.root.width()),
            f64::from(view.root.height()),
        );

        let node = snapshot.to_node().expect("rendered lock view");
        window
            .renderer()
            .unwrap()
            .render_texture(&node, None)
            .save_to_png(path)
            .unwrap();
    }

    window.close();

    view.show(LockSnapshot {
        waits: snapshot.waits.into_iter().rev().collect(),
        ..snapshot
    });

    assert_eq!(view.selected_wait().unwrap().tid, 10);
    let stale_request = view.begin_symbol(12).unwrap();
    view.invalidate();
    view.complete_symbol(stale_request, "stale reply".into());
    assert!(!view.symbol.text().contains("stale reply"));
    assert!(!view.context_matches(7, Some("i1"), Some(42)));
    view.navigate(LockAction::Follow);
    assert_eq!(view.selected_wait().unwrap().tid, 10);

    assert!(
        view.actions
            .iter()
            .all(|(_, button)| !button.is_sensitive())
    );

    view.clear();
    assert!(view.selected_wait().is_none());
    assert!(view.word.text().is_empty());
    assert!(!view.graph_summary.has_css_class("status-error"));
    assert_eq!(view.dependency_store.n_items(), 0);
}
