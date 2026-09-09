use super::*;

fn snapshot(start_time: u64) -> Snapshot {
    let uid = rustix::process::getuid().as_raw();

    let processes = [(10, uid), (2, uid.wrapping_add(1))]
        .into_iter()
        .map(|(pid, uid)| Process {
            identity: ProcessIdentity { pid, start_time },
            uid: Some(uid),
            executable: format!("/tmp/process-{pid}"),
            command: format!("process-{pid} 'two words'"),
            owner: format!("UID {uid}"),
            detail: String::new(),
            search: format!("{pid} process-{pid} two words {uid}"),
        })
        .collect();

    Snapshot {
        processes,
        skipped: 0,
        limited: false,
    }
}

#[test]
#[ignore = "requires a GTK display, run separately from other GTK tests"]
fn filtering_and_refresh_never_silently_replace_the_selected_identity() {
    gtk::init().unwrap();
    Theme::graphite().install();
    let pid = gtk::Entry::new();
    let picker = ProcessPicker::new(
        &pid,
        None,
        &crate::ui::ColumnLayouts::default().table(crate::ui::TableId::AttachProcesses),
    );

    picker.finish_refresh(Ok(snapshot(100)));
    assert_eq!(picker.filtered.n_items(), 1);
    assert_eq!(picker.store.n_items(), 2);
    let controls = picker.root.first_child().unwrap();
    let search = controls
        .first_child()
        .and_downcast::<gtk::SearchEntry>()
        .unwrap();

    let own = search
        .next_sibling()
        .and_downcast::<gtk::CheckButton>()
        .unwrap();

    own.set_active(false);
    assert_eq!(picker.filtered.n_items(), 2);
    picker.selection.set_selected(0);
    assert_eq!(pid.text(), "2");
    picker.selection.set_selected(1);
    assert_eq!(pid.text(), "10");
    let original = *picker.selected.borrow();
    let row = picker.store.item(0).unwrap();
    picker.finish_refresh(Ok(snapshot(100)));
    assert_eq!(picker.store.item(0), Some(row));
    assert_eq!(*picker.selected.borrow(), original);
    search.set_text("not in the snapshot");
    search.emit_by_name::<()>("search-changed", &[]);
    assert_eq!(picker.filtered.n_items(), 0);
    assert_eq!(*picker.selected.borrow(), original);
    assert_eq!(pid.text(), "10");
    search.set_text("PROCESS-10 TWO");
    search.emit_by_name::<()>("search-changed", &[]);
    assert_eq!(picker.filtered.n_items(), 1);
    picker.finish_refresh(Ok(snapshot(101)));
    assert!(picker.selection.selected_item().is_none());
    assert_eq!(*picker.selected.borrow(), original);
    picker.selection.set_selected(0);
    assert_eq!(picker.selected.borrow().unwrap().start_time, 101);
    pid.set_text("42");
    assert!(picker.selected.borrow().is_none());
    assert!(picker.selection.selected_item().is_none());

    let window = gtk::Window::builder()
        .default_width(720)
        .default_height(500)
        .child(&picker.root)
        .build();

    window.present();
    let main = glib::MainContext::default();
    main.block_on(glib::timeout_future(Duration::from_millis(50)));
    assert!(picker.root.width() <= 720);
    assert!(!picker.loading.get());
    let input = search.compute_bounds(&picker.root).unwrap();
    let checkbox = own
        .first_child()
        .unwrap()
        .compute_bounds(&picker.root)
        .unwrap();

    assert_eq!(
        input.y() + input.height() / 2.0,
        checkbox.y() + checkbox.height() / 2.0
    );

    let weak = Rc::downgrade(&picker);
    window.close();
    drop(picker);
    assert!(weak.upgrade().is_none());
}
