use super::*;
use crate::debugger::context::build_stack_entries;

fn entries(index: usize, count: usize) -> Vec<StackEntry> {
    let memory = MemoryBlock {
        begin: 0x1000 + (index * 8) as u64,
        bytes: vec![0; count * 8],
    };
    let mut entries = build_stack_entries(
        &memory,
        8,
        TargetEndian::Little,
        TargetArchitecture::X86_64,
        &[],
        &[],
        &[],
    );
    for entry in &mut entries {
        entry.index += index;
        entry.offset += index * 8;
    }

    entries
}

fn settle() {
    glib::MainContext::default().block_on(glib::timeout_future(Duration::from_millis(100)));
}

#[test]
fn scalar_stack_words_do_not_claim_their_storage_is_unmapped() {
    let mut entry = entries(0, 1).remove(0);
    assert_eq!(entry.address, 0x1000);
    assert_eq!(entry.region, None);
    assert_eq!(
        stack_pointer_target(&entry),
        "No known mapped pointer target (value may be a scalar)"
    );
    assert!(!stack_tooltip(&entry).contains("unmapped"));
    entry.region = Some("[heap]".into());
    assert_eq!(stack_pointer_target(&entry), "[heap]");
    assert!(stack_tooltip(&entry).contains("Pointer target: [heap]"));
}

#[test]
#[ignore = "requires a GTK display, run separately from other GTK tests"]
fn paging_preserves_selection_scroll_and_column_widths() {
    gtk::init().unwrap();
    Theme::graphite().install();
    let columns = ColumnLayouts::default();
    let (view, store, inspector) = views::build_stack_view(&columns.table(TableId::Stack));
    let scrolled = gtk::ScrolledWindow::builder()
        .child(&view)
        .vexpand(true)
        .overlay_scrolling(false)
        .build();
    let paging = Paging::new(&scrolled);
    let root = components::panel();
    root.append(&scrolled);
    root.append(&paging.root);
    root.append(&inspector.root);
    let window = gtk::Window::builder()
        .default_width(900)
        .default_height(650)
        .child(&root)
        .build();
    let displayed = RefCell::new(Vec::new());
    append_page(&store, &displayed, 0, &entries(0, 64));
    let selection = view.model().and_downcast::<gtk::SingleSelection>().unwrap();
    selection.set_selected(20);
    let selected = selection.selected_item().unwrap();
    let first = store.item(0).unwrap();
    let column = view
        .columns()
        .item(2)
        .and_downcast::<gtk::ColumnViewColumn>()
        .unwrap();
    column.set_fixed_width(360);
    window.present();
    settle();
    let adjustment = scrolled.vadjustment();
    adjustment.set_value(200.0);
    settle();
    let scroll = adjustment.value();
    append_page(&store, &displayed, 64, &entries(64, 64));
    settle();
    assert_eq!(store.n_items(), 128);
    assert_eq!(selection.selected(), 20);
    assert_eq!(selection.selected_item().unwrap(), selected);
    assert_eq!(store.item(0).unwrap(), first);
    assert!((adjustment.value() - scroll).abs() < 1.0);
    assert_eq!(column.fixed_width(), 360);

    let last = store.item(127).unwrap();
    let mut details = entries(0, 64);
    details[20].pointer_chain.push("0x1234".into());
    // Progress can arrive out of order and must only replace those rows.
    details[7].pointer_chain.push("0x5678".into());
    update_details(&store, &displayed, &details[20..21]);
    update_details(&store, &displayed, &details[7..8]);
    settle();
    assert_eq!(selection.selected(), 20);
    assert_eq!(displayed.borrow()[7].pointer_chain, ["0x5678"]);
    assert!(displayed.borrow()[8].pointer_chain.is_empty());
    update_details(&store, &displayed, &details);
    settle();
    assert_eq!(store.n_items(), 128);
    assert_eq!(store.item(0).unwrap(), first);
    assert_eq!(store.item(127).unwrap(), last);
    assert_eq!(selection.selected(), 20);
    assert!((adjustment.value() - scroll).abs() < 1.0);

    let requests = Rc::new(RefCell::new(Vec::new()));
    let received = Rc::clone(&requests);
    paging.connect(move |automatic| received.borrow_mut().push(automatic));
    paging.update(&StackPageStatus {
        loaded: 128,
        can_load: true,
        ..Default::default()
    });
    paging.more.emit_clicked();
    scrolled.emit_by_name::<()>("edge-reached", &[&gtk::PositionType::Bottom]);
    assert_eq!(*requests.borrow(), [false, true]);
    paging.update(&StackPageStatus {
        loaded: 128,
        loading: true,
        ..Default::default()
    });
    assert!(
        paging.more.is_sensitive(),
        "transient work must not drop hover or focus"
    );
    paging.more.emit_clicked();
    scrolled.emit_by_name::<()>("edge-reached", &[&gtk::PositionType::Bottom]);
    assert_eq!(
        *requests.borrow(),
        [false, true],
        "busy paging accepted another request"
    );
    assert!(paging.status.text().contains("Loading"));
    paging.update(&StackPageStatus {
        loaded: 128,
        can_load: true,
        error: Some("Unreadable".into()),
        ..Default::default()
    });
    assert_eq!(paging.more.label().as_deref(), Some("Retry"));
    assert_eq!(paging.root.margin_start(), paging.root.margin_end());
    assert_eq!(paging.root.margin_top(), paging.root.margin_bottom());

    let detached = gtk::Window::builder()
        .default_width(900)
        .default_height(650)
        .build();
    window.set_child(None::<&gtk::Widget>);
    detached.set_child(Some(&root));
    detached.present();
    settle();
    assert_eq!(selection.selected(), 20);
    assert_eq!(column.fixed_width(), 360);
    paging.update(&StackPageStatus {
        loaded: 128,
        stop: Some(StackStop::MappingEnd),
        total: Some(128),
        range: Some(0x1000..0x1400),
        ..Default::default()
    });
    assert!(!paging.more.is_sensitive());
    assert!(
        paging
            .status
            .text()
            .contains("128 / 128 words loaded · End of stack mapping")
    );
    assert!(paging.status.tooltip_text().unwrap().contains("0x1000"));
    paging.update(&StackPageStatus {
        loaded: 128,
        stop: Some(StackStop::MappingEnd),
        total: Some(128),
        range: Some(0x2000..0x2400),
        ..Default::default()
    });

    assert!(paging.status.tooltip_text().unwrap().contains("0x2000"));
    assert!(!paging.status.tooltip_text().unwrap().contains("0x1000"));
    // Refreshing at an unchanged SP keeps equal rows and the selection while
    // fresh pointer details are pending, even after a larger window was loaded.
    append_page(&store, &displayed, 0, &entries(0, 64));
    assert_eq!(store.n_items(), 64);
    assert_eq!(store.item(0).unwrap(), first);
    assert_eq!(selection.selected(), 20);
    assert_eq!(displayed.borrow()[20].pointer_chain, ["0x1234"]);
    update_details(&store, &displayed, &entries(0, 64));
    assert!(displayed.borrow()[20].pointer_chain.is_empty());
    assert_eq!(selection.selected(), 20);
    detached.close();
    window.close();
}
