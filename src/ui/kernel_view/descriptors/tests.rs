use super::*;
use crate::kernel::sockets::{EpollWatch, QueueUnit, SocketInfo};

fn descriptor(number: u32, kind: &str) -> KernelFileDescriptor {
    KernelFileDescriptor {
        number,
        kind: kind.into(),
        access: "read/write".into(),
        flags: "CLOEXEC".into(),
        position: Some(123),
        target: format!("target {number}"),
        details: "mnt_id: 1".into(),
        raw_info: "pos: 123\nflags: 02000002\nmnt_id: 1\nino: 42\n".into(),
        info_warning: String::new(),
        inode: Some(42),
        mount_id: Some(1),
        eventfd_id: None,
        device: Some((0, 1)),
        socket: None,
        watches: Vec::new(),
    }
}

fn socket(number: u32, queue: u64) -> KernelFileDescriptor {
    let mut fd = descriptor(number, "socket");
    fd.socket = Some(std::sync::Arc::new(SocketInfo {
        protocol: Protocol::Tcp,
        state: 1,
        local: Some("127.0.0.1:8080".parse().unwrap()),
        peer: Some("127.0.0.1:12345".parse().unwrap()),
        unix_name: None,
        socket_type: Some(1),
        listening: false,
        receive: Some(Queue {
            unit: QueueUnit::Bytes,
            value: queue,
        }),
        send: Some(Queue {
            unit: QueueUnit::Bytes,
            value: 0,
        }),
        raw: "raw socket entry".into(),
    }));

    fd.details = fd.socket.as_ref().unwrap().summary();
    fd
}

#[test]
#[ignore = "requires a GTK display, run separately from other GTK tests"]
fn descriptor_history_restores_filters_and_exposes_partial_stale_evidence() {
    gtk::init().unwrap();
    let view = DescriptorView::build(&ColumnLayouts::default());
    let mut epoll = descriptor(12, "epoll");
    epoll.info_warning =
        "256 epoll registrations shown. Additional registrations were not captured.".into();
    epoll.watches.push(EpollWatch {
        fd: 5,
        inode: Some(42),
        device: Some((0, 1)),
        events: 1,
        data: 5,
    });

    view.show(Some((100, 1)), 1000, 1, true, vec![socket(5, 1), epoll]);
    view.details.set_active(true);
    view.sockets.set_active(true);
    view.protocol.set_selected(1);
    view.search.set_text("127.0.0.1");
    view.query.borrow_mut().text = "127.0.0.1".into();
    view.refilter();
    view.tables[1].selection.set_selected(0);
    let link = crate::ui::tests::descendants::<gtk::Button>(&view.links)
        .into_iter()
        .find(|button| {
            button
                .label()
                .is_some_and(|text| text.starts_with("Watched by FD 12"))
        })
        .unwrap();
    link.emit_clicked();
    assert_eq!(view.mode.get(), 0);
    assert_eq!(view.selected().unwrap().fd.number, 12);
    assert!(view.search.text().is_empty());
    assert!(view.details_text().contains("Additional registrations"));
    assert!(view.details_text().contains("PID 100"));
    assert!(view.back.is_sensitive());
    view.back.emit_clicked();
    assert_eq!(view.mode.get(), 1);
    assert_eq!(view.selected().unwrap().fd.number, 5);
    assert_eq!(view.search.text(), "127.0.0.1");
    assert_eq!(view.protocol.selected(), 1);
    assert!(!view.back.is_sensitive());
    view.invalidate();
    assert!(view.summary.text().contains("Stale snapshot"));
    assert!(view.copy.is_sensitive());
    assert!(!view.inspect.is_sensitive());
    view.set_refreshing(true);
    assert!(view.summary.text().contains("Refreshing"));
    view.clear();
    assert!(!view.back.is_sensitive());
    assert_eq!(view.summary.text(), "No snapshot");
}

#[test]
fn descriptor_identity_survives_renames_and_rejects_ambiguous_epoll_targets() {
    let before = descriptor(4, "file");
    let mut after = before.clone();
    after.target = "renamed file".into();
    assert!(before.same_identity(&after));
    assert_eq!(
        make_row(after.clone(), Some(&before), true).change,
        "Descriptor changed"
    );
    after.mount_id = Some(2);
    assert!(!before.same_identity(&after));
    after = before.clone();
    after.inode = Some(43);
    assert!(!before.same_identity(&after));
    let mut eventfd = descriptor(4, "eventfd");
    eventfd.eventfd_id = Some(100);
    let mut replacement = eventfd.clone();
    replacement.eventfd_id = Some(101);
    assert!(!eventfd.same_identity(&replacement));

    let watch = EpollWatch {
        fd: 4,
        inode: Some(42),
        device: Some((0, 1)),
        events: 1,
        data: 4,
    };
    assert!(!watch.matches(&eventfd));
    assert!(!watch.matches(&replacement));
    assert!(!watch.matches(&before));
    assert!(watch.matches(&socket(4, 0)));
    let mut other_device = socket(4, 0);
    other_device.device = Some((0, 2));
    assert!(!watch.matches(&other_device));
    assert!(!watch.matches(&descriptor(4, "pipe")));
    assert!(!watch.matches(&socket(5, 0)));

    let row = make_row(before, None, false);

    for column in [
        Column::Kind,
        Column::Access,
        Column::Flags,
        Column::SocketFlags,
        Column::Target,
        Column::Details,
        Column::State,
        Column::Protocol,
        Column::Change,
    ] {
        assert!(matches!(cell_text(&row, column), Cow::Borrowed(_)));
    }
}

#[test]
#[ignore = "requires a GTK display at least 1100px wide, run separately from other GTK tests"]
fn fd_modes_preserve_files_selection_layout_and_socket_changes() {
    gtk::init().unwrap();
    Theme::graphite().install();
    let columns = ColumnLayouts::default();
    let view = DescriptorView::build(&columns);
    let window = gtk::Window::builder()
        .default_width(1100)
        .default_height(700)
        .child(&view.root)
        .build();
    window.present();
    let settle = || {
        glib::MainContext::default()
            .block_on(glib::timeout_future(std::time::Duration::from_millis(100)))
    };
    let mut epoll = descriptor(12, "epoll");
    epoll.watches.push(EpollWatch {
        fd: 5,
        inode: Some(42),
        device: Some((0, 1)),
        events: 0x2001,
        data: 5,
    });
    let descriptors = vec![
        descriptor(1, "file"),
        socket(5, 21),
        descriptor(8, "socket"),
        epoll,
    ];
    let model = crate::model::DebuggerModel::new(None);
    model.observe_stop(Some("1"), Some("i1"), 0, None, true, None);
    model.start_stop_refresh();
    view.show(
        Some((100, 1)),
        1000,
        model.observed_stop_sequence(),
        true,
        descriptors.clone(),
    );
    settle();
    assert_eq!(view.mode.get(), 0);
    assert_eq!(view.tables[0].selection.n_items(), 4);
    assert!(!view.details.is_active());
    let original = view.tables[0]
        .view
        .columns()
        .item(3)
        .and_downcast::<gtk::ColumnViewColumn>()
        .unwrap();
    original.set_fixed_width(240);
    view.tables[0].selection.set_selected(0);
    view.details.set_active(true);
    assert!(view.details_text().contains("Position: 123"));
    assert!(view.details_text().contains("mnt_id: 1"));
    assert!(!view.socket.root.is_visible());
    assert!(!view.diagnostics.root.is_visible());
    assert!(!view.links.is_visible());
    assert!(!view.raw_socket.parent().unwrap().is_visible());
    view.sockets.set_active(true);
    settle();
    assert_eq!(view.tables[1].selection.n_items(), 2);
    view.tables[1].selection.set_selected(0);
    assert_eq!(view.selected().unwrap().fd.number, 5);
    assert!(view.details_text().contains("Receive queue: 21 B"));
    assert!(view.socket.root.is_visible());
    assert!(view.links.is_visible());
    assert!(
        view.links
            .first_child()
            .unwrap()
            .next_sibling()
            .and_downcast::<gtk::Button>()
            .unwrap()
            .label()
            .unwrap()
            .contains("Watched by FD 12")
    );
    view.protocol.set_selected(6);
    assert_eq!(view.tables[1].selection.n_items(), 1);
    assert!(view.tables[1].selection.selected_item().is_none());
    view.protocol.set_selected(0);
    view.tables[1].selection.set_selected(0);
    let mut drained = descriptors.clone();
    drained[1] = socket(5, 0);
    model.observe_running(Some("1"));
    model.observe_stop(Some("1"), Some("i1"), 0, None, true, None);
    model.start_stop_refresh();
    view.show(
        Some((100, 1)),
        2000,
        model.observed_stop_sequence(),
        true,
        drained.clone(),
    );
    assert_eq!(view.selected().unwrap().fd.number, 5);
    assert!(view.selected().unwrap().change.contains("RX -21 B"));
    assert!(view.selected().unwrap().receive_changed);
    model.start_stop_refresh();
    view.show(
        Some((100, 1)),
        2100,
        model.observed_stop_sequence(),
        true,
        drained.clone(),
    );
    assert!(view.selected().unwrap().change.contains("RX -21 B"));
    assert!(view.selected().unwrap().receive_changed);
    view.all.set_active(true);
    assert_eq!(view.selected().unwrap().fd.number, 1);
    assert_eq!(original.fixed_width(), 240);
    view.search.set_text("target 1");
    view.search.emit_by_name::<()>("search-changed", &[]);
    assert_eq!(view.tables[0].selection.n_items(), 2);
    view.search.set_text("");
    view.search.emit_by_name::<()>("search-changed", &[]);
    view.sockets.set_active(true);
    view.tables[1].selection.set_selected(0);
    assert_eq!(view.selected().unwrap().fd.number, 5);
    let row = view.tables[1]
        .view
        .columns()
        .item(0)
        .and_downcast::<gtk::ColumnViewColumn>()
        .unwrap();
    view.tables[1]
        .view
        .sort_by_column(Some(&row), gtk::SortType::Descending);
    assert_eq!(
        selected_row(&view.tables[1].selection).unwrap().fd.number,
        5
    );
    drained[1].inode = Some(99);
    view.show(Some((100, 1)), 3000, 3000, true, drained);
    assert!(view.selected().is_none());
    assert!(
        view.rows
            .borrow()
            .iter()
            .any(|row| row.change == "FD reused")
    );
    view.tables[1].selection.set_selected(1);
    view.invalidate();
    assert!(!view.inspect.is_sensitive());
    view.show(Some((101, 2)), 4000, 4000, true, descriptors);
    assert!(view.selected().is_none());
    assert!(!view.changes.is_visible());
    view.clear();
    assert_eq!(view.tables[0].selection.n_items(), 0);

    let mut many = (0..128)
        .map(|number| descriptor(number, "file"))
        .collect::<Vec<_>>();
    many.push(socket(130, 1));
    many.push(socket(131, 100));
    view.all.set_active(true);
    view.details.set_active(false);
    view.show(Some((101, 2)), 5000, 5000, true, many.clone());
    settle();
    let vertical = view.tables[0].scroll.vadjustment();
    let horizontal = view.tables[0].scroll.hadjustment();
    vertical.set_value(1000.0);
    horizontal.set_value(100.0);
    settle();
    let position = (vertical.value(), horizontal.value());
    view.sockets.set_active(true);
    settle();
    view.all.set_active(true);
    settle();
    assert_eq!((vertical.value(), horizontal.value()), position);
    view.show(Some((101, 2)), 6000, 6000, true, many);
    settle();
    assert_eq!((vertical.value(), horizontal.value()), position);
    view.sockets.set_active(true);
    let queue = view.tables[1]
        .view
        .columns()
        .item(5)
        .and_downcast::<gtk::ColumnViewColumn>()
        .unwrap();
    view.tables[1]
        .view
        .sort_by_column(Some(&queue), gtk::SortType::Descending);
    let first = view.tables[1]
        .selection
        .item(0)
        .and_downcast::<glib::BoxedAnyObject>()
        .unwrap();
    assert_eq!(first.borrow::<Row>().fd.number, 131);

    view.tables[1].selection.set_selected(0);
    view.details.set_active(true);
    window.set_default_size(1100, 700);
    settle();
    let descriptor_bounds = view.descriptor.root.compute_bounds(&window).unwrap();
    let socket_bounds = view.socket.root.compute_bounds(&window).unwrap();
    assert!((descriptor_bounds.y() - socket_bounds.y()).abs() < 1.0);
    assert!((descriptor_bounds.width() - socket_bounds.width()).abs() < 1.0);
    assert!(socket_bounds.x() > descriptor_bounds.x() + descriptor_bounds.width());
    window.set_default_size(600, 700);
    settle();
    let descriptor_bounds = view.descriptor.root.compute_bounds(&window).unwrap();
    let socket_bounds = view.socket.root.compute_bounds(&window).unwrap();
    assert!((descriptor_bounds.x() - socket_bounds.x()).abs() < 1.0);
    assert!(socket_bounds.y() > descriptor_bounds.y() + descriptor_bounds.height());
    view.all.set_active(true);
    view.tables[0].selection.set_selected(0);
    assert!(!view.socket.root.is_visible());
    assert!(!view.socket.root.parent().unwrap().is_visible());
    assert!(!view.links.is_visible());
    assert!(!view.diagnostics.root.is_visible());
    window.set_default_size(1100, 700);
    settle();
    let descriptor_bounds = view.descriptor.root.compute_bounds(&window).unwrap();
    assert!(descriptor_bounds.width() > window.width() as f32 - 40.0);

    view.clear();
    let mut listener = socket(5, 0);
    let info = std::sync::Arc::make_mut(listener.socket.as_mut().unwrap());
    info.listening = true;
    info.state = 10;
    info.peer = None;
    info.send = None;
    let mut file = descriptor(8, "file");
    view.show(
        Some((100, 1)),
        1,
        1,
        true,
        vec![listener.clone(), file.clone()],
    );
    view.tables[0].selection.set_selected(0);
    window.set_default_size(1000, 700);
    settle();
    let descriptor_bounds = view.descriptor.root.compute_bounds(&window).unwrap();
    let socket_bounds = view.socket.root.compute_bounds(&window).unwrap();
    assert!((descriptor_bounds.y() - socket_bounds.y()).abs() < 1.0);
    assert!((descriptor_bounds.width() - socket_bounds.width()).abs() < 1.0);

    view.commands_allowed.set(true);
    let stamp = view.begin_diagnostics();
    assert!(view.pending.get());
    view.cancel_diagnostics(stamp);
    assert!(!view.pending.get());
    assert!(view.inspect.is_sensitive());
    let next = view.begin_diagnostics();
    assert_ne!(next, stamp);
    view.cancel_diagnostics(stamp);
    assert!(view.pending.get());
    view.cancel_diagnostics(next);
    assert!(!view.pending.get());

    view.tables[0].selection.set_selected(1);
    file.target = "renamed file".into();
    view.show(
        Some((100, 1)),
        2,
        2,
        true,
        vec![listener.clone(), file.clone()],
    );
    assert_eq!(view.selected().unwrap().fd.number, 8);
    assert_eq!(view.selected().unwrap().change, "Descriptor changed");
    view.show(Some((100, 1)), 3, 3, false, vec![listener.clone()]);
    assert!(!view.changes.is_visible());
    assert!(view.summary.text().contains("partial snapshot"));
    assert!(view.rows.borrow().iter().all(|row| row.change.is_empty()));
    view.show(
        Some((100, 1)),
        4,
        4,
        true,
        vec![listener.clone(), file.clone()],
    );
    assert!(!view.changes.is_visible());
    assert!(view.rows.borrow().iter().all(|row| row.change.is_empty()));
    view.show(Some((100, 1)), 5, 5, true, vec![listener.clone()]);
    assert!(view.changes.is_visible());
    assert!(view.changes.text().contains("1 closed"));
    view.show(Some((100, 1)), 6, 6, false, vec![]);
    assert!(!view.changes.is_visible());
    view.show(Some((100, 1)), 7, 6, true, vec![listener]);
    assert!(!view.changes.is_visible());

    // Model splices must still refilter changed rows without a global filter invalidation.
    view.search.set_text("renamed");
    view.search.emit_by_name::<()>("search-changed", &[]);
    assert_eq!(view.tables[0].selection.n_items(), 0);
    view.show(Some((100, 1)), 8, 7, true, vec![file]);
    assert_eq!(view.tables[0].selection.n_items(), 1);
    window.close();
}

#[test]
#[ignore = "requires a GTK display at least 1100px wide, run separately from other GTK tests"]
fn unchanged_fd_refresh_reuses_models_and_epoll_widgets() {
    gtk::init().unwrap();
    Theme::graphite().install();
    let view = DescriptorView::build(&ColumnLayouts::default());
    let window = gtk::Window::builder()
        .default_width(1000)
        .default_height(700)
        .child(&view.root)
        .build();
    window.present();
    let mut many = (0..4096)
        .map(|number| descriptor(number, "file"))
        .collect::<Vec<_>>();
    many[5] = socket(5, 21);

    for fd in &mut many[100..228] {
        fd.kind = "epoll".into();
        fd.watches.push(EpollWatch {
            fd: 5,
            inode: Some(42),
            device: Some((0, 1)),
            events: 0x2001,
            data: 5,
        });
    }

    view.show(Some((100, 1)), 1, 1, true, many.clone());
    let settle = || {
        glib::MainContext::default()
            .block_on(glib::timeout_future(std::time::Duration::from_millis(100)))
    };
    settle();
    let first = view.store.item(0).unwrap();
    let states = view.state.model().unwrap();
    let changes = Rc::new(Cell::new(0));
    let observed = Rc::clone(&changes);
    view.tables[0]
        .selection
        .connect_items_changed(move |_, _, _, _| observed.set(observed.get() + 1));
    let mut times = Vec::new();

    for _ in 0..5 {
        let input = many.clone();
        let start = std::time::Instant::now();
        view.show(Some((100, 1)), 2, 1, true, input);
        times.push(start.elapsed().as_micros());
    }

    times.sort_unstable();
    eprintln!("4096 unchanged FDs: {} µs median", times[2]);
    assert_eq!(changes.get(), 0);
    assert_eq!(first, view.store.item(0).unwrap());
    assert_eq!(states, view.state.model().unwrap());
    view.tables[0].selection.set_selected(5);
    view.details.set_active(true);
    settle();
    let link = view.links.first_child().unwrap().next_sibling().unwrap();
    times.clear();

    for _ in 0..5 {
        let start = std::time::Instant::now();
        view.render_details();
        times.push(start.elapsed().as_micros());
    }

    times.sort_unstable();
    eprintln!(
        "Unchanged details with 128 epoll parents: {} µs median",
        times[2]
    );
    assert_eq!(
        link,
        view.links.first_child().unwrap().next_sibling().unwrap()
    );
    view.show(Some((100, 1)), 3, 2, true, many);
    assert_eq!(
        link,
        view.links.first_child().unwrap().next_sibling().unwrap()
    );
    window.close();
}
