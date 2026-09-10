use super::*;

#[test]
#[ignore = "requires a GTK display, run separately from other GTK tests"]
fn memory_snapshots_refresh_selected_pointers_and_copyable_cells() {
    gtk::init().unwrap();
    Theme::graphite().install();
    let (view, store, selection) =
        build_memory_watch_table(&ColumnLayouts::default().table(TableId::MemoryBytes));
    let items = Rc::new(RefCell::new(Vec::<glib::WeakRef<gtk::ListItem>>::new()));
    for column in view.columns().iter::<gtk::ColumnViewColumn>() {
        let items = Rc::clone(&items);
        column
            .unwrap()
            .factory()
            .and_downcast::<gtk::SignalListItemFactory>()
            .unwrap()
            .connect_setup(move |_, item| {
                items
                    .borrow_mut()
                    .push(item.downcast_ref::<gtk::ListItem>().unwrap().downgrade());
            });
    }

    let scroll = gtk::ScrolledWindow::builder().child(&view).build();
    let window = gtk::Window::builder()
        .default_width(1200)
        .default_height(350)
        .child(&scroll)
        .build();
    window.present();
    let main = glib::MainContext::default();
    let mut previous = Vec::new();

    for pointer in [0x1234_u64, 0x5678, 0] {
        let bytes = pointer.to_le_bytes();
        let context = MemoryRenderContext {
            pointer_bits: 64,
            endian: TargetEndian::Little,
            previous_begin: Some(0x1000),
            previous: &previous,
            regions: &[],
        };
        let rows = format_memory_rows(0x1000, &bytes, MemoryWatchFormat::Pointers, &context);
        let expected = rows[0].value.clone();
        let changed = rows[0].changed;
        components::replace_snapshot_store(&store, rows);
        selection.set_selected(0);
        assert_eq!(
            selected_memory_pointer(&selection),
            (pointer != 0).then_some(pointer)
        );
        main.block_on(glib::timeout_future(Duration::from_millis(50)));
        let mut checked = 0;

        for item in items.borrow().iter().filter_map(glib::WeakRef::upgrade) {
            let Some(label) = item.child().and_downcast::<gtk::Label>() else {
                continue;
            };
            if item.item().is_some() {
                assert!(label.is_selectable());
                assert_eq!(label.has_css_class("memory-row-changed"), changed);
                assert!(label.selection_bounds().is_none());
                if label.text() == expected {
                    label.select_region(0, -1);
                    label.emit_by_name::<()>("copy-clipboard", &[]);
                    assert_eq!(
                        main.block_on(label.clipboard().read_text_future())
                            .unwrap()
                            .as_deref(),
                        Some(expected.as_str())
                    );
                    checked += 1;
                }
            }
        }

        assert!(checked > 0);
        previous = bytes.to_vec();
    }

    store.remove_all();
    assert_eq!(selected_memory_pointer(&selection), None);
    let weak = view.downgrade();
    scroll.set_child(gtk::Widget::NONE);
    window.close();
    drop(view);
    drop(scroll);
    drop(window);
    main.block_on(glib::timeout_future(Duration::from_millis(50)));
    assert!(weak.upgrade().is_none());
    assert!(items.borrow().iter().all(|item| item.upgrade().is_none()));
}
