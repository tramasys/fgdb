use super::*;
use crate::debugger::array::ArrayOrder;

#[test]
#[ignore = "requires a GTK display, run separately from other GTK tests"]
fn array_navigation_validates_slices_preserves_layout_and_invalidates_cancelled_pages() {
    gtk::init().unwrap();
    Theme::graphite().install();
    let controls = ArrayControls::new(4);
    let root = gtk::Box::new(gtk::Orientation::Vertical, 8);
    components::inset(&root, components::DIALOG_INSET);
    root.append(&controls.root);
    let status = gtk::Label::new(None);
    root.append(&status);

    let window = gtk::Window::builder()
        .default_width(900)
        .default_height(320)
        .child(&root)
        .build();

    let session = Rc::new(VariableViewerSession {
        window: window.downgrade(),
        store: gio::ListStore::new::<glib::BoxedAnyObject>(),
        status,
        shown: Cell::new(0),
        revision: Cell::new(0),
        array: Some(Rc::clone(&controls)),
        linked: None,
    });

    session.bind_window_lifetime(&window);

    session
        .configure_array(ArrayShape {
            bounds: vec![(-1, 1), (4, 6)],
            order: ArrayOrder::ColumnMajor,
            sequential: false,
            length_known: true,
        })
        .unwrap();

    let requests = Rc::new(RefCell::new(Vec::new()));
    let requested = Rc::clone(&requests);
    let weak = Rc::downgrade(&session);

    session.connect_array_query(move |page| {
        requested.borrow_mut().push(page.clone());

        if let Some(session) = weak.upgrade() {
            session.begin_page(&page);
        }
    });

    let weak = Rc::downgrade(&session);

    controls.connect_cancel(move || {
        if let Some(session) = weak.upgrade() {
            session.cancel_page();
        }
    });

    window.present();
    let main = glib::MainContext::default();
    main.block_on(glib::timeout_future(Duration::from_millis(80)));
    assert!(controls.busy.get());
    assert_eq!(requests.borrow()[0].slice.total(), Ok(9));
    assert!(controls.apply.is_sensitive());
    controls.submit(0);
    assert_eq!(requests.borrow().len(), 1);

    let arrow_geometry = || {
        [&controls.previous, &controls.next].map(|button| {
            let bounds = button.compute_bounds(&root).unwrap();
            let group = button.parent().unwrap().compute_bounds(&root).unwrap();
            let image = button.child().unwrap().compute_bounds(&root).unwrap();
            assert_eq!(image.width(), 16.0);
            assert_eq!(image.height(), 16.0);
            assert!((bounds.width() - bounds.height()).abs() <= 1.0);

            for outer in [bounds, group] {
                let above = image.y() - outer.y();
                let below = outer.y() + outer.height() - image.y() - image.height();
                assert!((above - below).abs() <= 1.0, "{above} above, {below} below");
            }

            let left = image.x() - bounds.x();
            let right = bounds.x() + bounds.width() - image.x() - image.width();
            assert!((left - right).abs() <= 1.0, "{left} left, {right} right");
            bounds
        })
    };

    let arrows_before = arrow_geometry();
    assert_eq!(arrows_before[0].y(), arrows_before[1].y());
    assert_eq!(arrows_before[0].width(), arrows_before[1].width());
    assert_eq!(arrows_before[0].height(), arrows_before[1].height());

    for entries in controls.entries.borrow().iter() {
        let first = entries[0].compute_bounds(&root).unwrap();

        for entry in &entries[1..] {
            let bounds = entry.compute_bounds(&root).unwrap();
            assert_eq!(bounds.y(), first.y());
            assert_eq!(bounds.height(), first.height());
            assert!((bounds.width() - first.width()).abs() <= 1.0);
        }
    }

    session.shown.set(4);
    session.finish_page("Loaded", false);
    assert!(controls.next.is_sensitive());
    assert!(!controls.previous.is_sensitive());
    controls
        .next
        .set_state_flags(gtk::StateFlags::PRELIGHT, false);
    main.block_on(glib::timeout_future(Duration::from_millis(50)));
    assert_eq!(arrow_geometry(), arrows_before);
    controls.next.unset_state_flags(gtk::StateFlags::PRELIGHT);
    controls.next.emit_clicked();
    assert_eq!(requests.borrow()[1].offset, 4);
    session.shown.set(4);
    session.finish_page("Loaded", false);
    controls.next.emit_clicked();
    assert_eq!(requests.borrow()[2].offset, 8);
    session.shown.set(1);
    session.finish_page("Loaded", true);
    assert!(!controls.next.is_sensitive());
    controls.previous.emit_clicked();
    assert_eq!(requests.borrow()[3].offset, 4);
    session.shown.set(4);
    session.finish_page("Loaded", false);
    let entries = controls.entries.borrow().clone();
    entries[0][0].set_text("2");
    controls.apply.emit_clicked();
    assert_eq!(requests.borrow().len(), 4);
    assert!(controls.description.has_css_class("status-error"));
    assert!(!controls.next.is_sensitive());
    assert_eq!(session.shown.get(), 4);
    entries[0][0].set_text("1");
    entries[0][1].set_text("1");
    entries[1][0].set_text("6");
    entries[1][1].set_text("2");
    entries[1][2].set_text("-1");
    controls.apply.emit_clicked();
    assert_eq!(requests.borrow()[4].slice.total(), Ok(2));
    assert_eq!(requests.borrow()[4].offset, 0);
    assert_eq!(session.shown.get(), 0);
    let revision = session.revision.get();
    assert!(session.page_is_current(revision));
    session.shown.set(1);
    controls.cancel.emit_clicked();
    assert!(!session.page_is_current(revision));
    assert!(controls.apply.is_sensitive());
    assert_eq!(session.shown.get(), 1);
    controls.next.emit_clicked();
    assert_eq!(requests.borrow()[5].offset, 1);
    assert!(session.page_is_current(session.revision.get()));
    assert!(!session.page_is_current(revision));
    session.finish_page("Loaded", true);
    let weak_controls = Rc::downgrade(&controls);
    controls.connect_query(move |_| {
        weak_controls.upgrade().unwrap().connect_query(|_| {});
    });
    // Extension callbacks may replace themselves without retaining a RefCell borrow.
    controls.submit(0);
    let weak = Rc::downgrade(&session);
    drop(session);
    assert!(weak.upgrade().is_some());
    window.close();
    main.block_on(glib::timeout_future(Duration::from_millis(50)));
    assert!(weak.upgrade().is_none());
}
