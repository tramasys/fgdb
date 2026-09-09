use super::*;

#[test]
#[ignore = "requires a GTK display, run separately from other GTK tests"]
fn linked_controls_keep_symmetric_geometry_and_release_their_handler() {
    gtk::init().unwrap();
    Theme::graphite().install();
    let (session, window) = VariableViewerSession::linked_test_session(128);
    let controls = session.linked.as_ref().unwrap();
    let requested = Rc::new(RefCell::new(Vec::new()));
    let actions = Rc::clone(&requested);
    let weak = Rc::downgrade(&session);
    session.connect_linked(move |action| {
        if matches!(action, LinkedListAction::Restart(_)) {
            weak.upgrade().unwrap().begin_linked();
        }

        actions.borrow_mut().push(action);
    });
    let main = glib::MainContext::default();
    main.block_on(glib::timeout_future(Duration::from_millis(80)));
    assert_eq!(requested.borrow().len(), 1);
    controls.apply.emit_clicked();
    assert_eq!(requested.borrow().len(), 1);
    controls.cancel.emit_clicked();
    assert_eq!(requested.borrow().last(), Some(&LinkedListAction::Cancel));

    let geometry = || {
        [&controls.first, &controls.previous, &controls.next].map(|button| {
            let bounds = button.compute_bounds(&controls.root).unwrap();
            let image = button
                .child()
                .unwrap()
                .compute_bounds(&controls.root)
                .unwrap();
            let group = button
                .parent()
                .unwrap()
                .compute_bounds(&controls.root)
                .unwrap();
            assert!((bounds.width() - bounds.height()).abs() <= 1.0);

            for outer in [bounds, group] {
                let top = image.y() - outer.y();
                let bottom = outer.y() + outer.height() - image.y() - image.height();
                assert!((top - bottom).abs() <= 1.0, "{top} above, {bottom} below");
            }

            let left = image.x() - bounds.x();
            let right = bounds.x() + bounds.width() - image.x() - image.width();
            assert!((left - right).abs() <= 1.0);
            bounds
        })
    };
    let before = geometry();
    session.append([VariableViewerRow {
        ordinal: String::from("128"),
        name: String::from("0x1234"),
        value: String::from("{...}"),
        type_name: String::from("Node"),
        details: String::from("value = 128"),
        link: String::from("next → 0x1240"),
    }]);
    let item = session.store.item(0);
    session.update_linked_progress(
        LinkedListProgress {
            offset: 128,
            shown: 1,
            cached: 129,
            busy: false,
            can_continue: true,
        },
        "Page ready",
    );
    assert_eq!(session.store.item(0), item);
    main.block_on(glib::timeout_future(Duration::from_millis(40)));
    assert_eq!(before, geometry());
    assert!(controls.first.is_sensitive());
    assert!(controls.previous.is_sensitive());
    assert!(controls.next.is_sensitive());
    controls.member.set_text("next()");
    assert!(!controls.next.is_sensitive());
    controls.apply.emit_clicked();
    assert_eq!(requested.borrow().len(), 2);
    assert!(controls.description.has_css_class("status-error"));
    controls.member.set_text("prev");
    controls.size.set_value(32.0);
    controls.apply.emit_clicked();
    assert_eq!(
        requested.borrow().last(),
        Some(&LinkedListAction::Restart(LinkedListQuery {
            member: String::from("prev"),
            page_size: 32
        }))
    );
    assert!(!controls.description.has_css_class("status-error"));
    drop(session);
    window.close();
    main.block_on(glib::timeout_future(Duration::from_millis(40)));
    assert_eq!(Rc::strong_count(&requested), 1);
}
