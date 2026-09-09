use super::*;
use crate::ui::{components, layout::Persistence};
use std::time::{Duration, Instant};

#[test]
fn widths_round_trip_with_bounded_keys_and_values() {
    let mut widths = Widths::default();
    assert!(!widths.parse("window", "800,600,0"));
    assert!(widths.parse("column.locals.type", "360"));
    assert!(widths.parse("column.watches.type", "240"));
    assert!(widths.parse("column.locals.type", "420"));

    for (key, value) in [
        ("column.locals.name", "-1"),
        ("column.locals.name", "0"),
        ("column.locals.name", "8193"),
        ("column.locals.name", "999999999999999"),
        ("column.locals.name", "one"),
        ("column.unknown.name", "200"),
        ("column.locals.", "200"),
        ("column.locals.nested.name", "200"),
        ("column.locals.Name", "200"),
    ] {
        assert!(widths.parse(key, value));
    }

    let mut text = String::new();
    widths.write(&mut text);
    assert_eq!(text, "column.locals.type=420\ncolumn.watches.type=240\n");
    let mut parsed = Widths::default();

    for line in text.lines() {
        let (key, value) = line.split_once('=').unwrap();
        assert!(parsed.parse(key, value));
    }

    assert_eq!(parsed, widths);

    for index in 0..MAX_COLUMNS + 10 {
        widths.parse(&format!("column.locals.field-{index}"), "100");
    }

    assert_eq!(widths.0[&TableId::Locals].len(), MAX_COLUMNS);
    widths.parse("column.locals.type", "500");
    assert_eq!(widths.get(TableId::Locals, "type"), Some(500));
}

fn wait(ready: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);

    while !ready() && Instant::now() < deadline {
        settle();
    }

    assert!(ready(), "column layout did not settle");
}

fn settle() {
    glib::MainContext::default().block_on(glib::timeout_future(Duration::from_millis(40)));
}

fn table(layouts: &ColumnLayouts, id: TableId) -> (gtk::ColumnView, Vec<gtk::ColumnViewColumn>) {
    let view = components::column_view(gtk::NoSelection::new(Some(gtk::StringList::new(&["row"]))));
    let layout = layouts.table(id);
    let mut columns = Vec::new();

    for (key, title, width) in [
        ("name", "NAME", 140),
        ("value", "VALUE", 240),
        ("type", "TYPE", 180),
        ("location", "LOCATION", 190),
    ] {
        let factory = gtk::SignalListItemFactory::new();
        factory.connect_setup(|_, item| {
            item.downcast_ref::<gtk::ListItem>()
                .unwrap()
                .set_child(Some(&gtk::Label::new(Some("value"))));
        });

        let column = components::table_column(title, width, factory);
        column.set_visible(key != "location");
        layout.append(&view, key, &column);
        columns.push(column);
    }

    (view, columns)
}

#[test]
#[ignore = "requires a GTK display, run separately from other GTK tests"]
fn kernel_misc_and_register_tables_register_independent_widths() {
    gtk::init().unwrap();
    let theme = crate::theme::Theme::graphite();
    theme.install();
    let layouts = ColumnLayouts::default();
    let kernel = crate::ui::kernel_view::build_kernel_view(&crate::ui::KernelViewBindings {
        columns: &layouts,
        refresh_handler: &Rc::new(RefCell::new(None)),
        remembered_disclosures: &HashMap::new(),
        section_handler: &Rc::new(RefCell::new(None)),
    });

    let misc = crate::ui::misc_view::build_misc_view(&theme, &layouts);
    let registers = crate::ui::views::build_register_view(&layouts);
    let expected = [
        TableId::TlsModules,
        TableId::TlsSymbols,
        TableId::MappingChanges,
        TableId::KernelThreads,
        TableId::KernelSignals,
        TableId::ProcessTree,
        TableId::PrivateCategories,
        TableId::PrivateMappings,
        TableId::KernelMappings,
        TableId::FileDescriptors,
        TableId::Limits,
        TableId::Arguments,
        TableId::Environment,
        TableId::Auxv,
        TableId::CallArguments,
        TableId::CallAbi,
        TableId::AllocatorMappings,
        TableId::Heap,
        TableId::LockWaits,
        TableId::LockDependencies,
        TableId::CoreNotes,
        TableId::CoreMappings,
        TableId::Syscalls,
        TableId::GeneralRegisters,
        TableId::BaseRegisters,
        TableId::FlagRegisters,
        TableId::SegmentRegisters,
        TableId::FloatRegisters,
        TableId::OtherRegisters,
    ];

    for id in expected {
        assert!(
            layouts
                .0
                .bindings
                .borrow()
                .keys()
                .any(|(table, _)| *table == id),
            "unregistered table: {id:?}"
        );
    }

    let bound = layouts
        .0
        .bindings
        .borrow()
        .iter()
        .flat_map(|(&(table, key), bindings)| {
            bindings.iter().map(move |binding| {
                (
                    table,
                    key,
                    binding.column.upgrade().unwrap(),
                    binding.default_width,
                )
            })
        })
        .collect::<Vec<_>>();

    for (table, key, column, default) in &bound {
        column.set_fixed_width(default + 8);
        assert_eq!(
            layouts.0.widths.borrow().get(*table, key),
            Some(default + 8)
        );
    }

    layouts.reset();

    for (_, _, column, default) in bound {
        assert_eq!(column.fixed_width(), default);
    }

    assert!(layouts.0.widths.borrow().0.is_empty());
    drop((kernel, misc, registers));
}

#[test]
#[ignore = "requires a GTK display, run separately from other GTK tests"]
fn requested_widths_survive_reopening_reordering_popouts_and_reset() {
    gtk::init().unwrap();
    crate::theme::Theme::graphite().install();
    let saturated = ColumnLayouts::default();
    let mut obsolete = Widths::default();

    for index in 0..MAX_COLUMNS {
        obsolete.parse(&format!("column.locals.old-{index}"), "100");
    }

    saturated.restore(obsolete);
    let (old_view, current_columns) = table(&saturated, TableId::Locals);
    current_columns[0].set_fixed_width(280);
    assert_eq!(
        saturated.0.widths.borrow().0[&TableId::Locals].len(),
        MAX_COLUMNS
    );

    assert_eq!(
        saturated.0.widths.borrow().get(TableId::Locals, "name"),
        Some(280)
    );

    drop((saturated, old_view, current_columns));

    let application = gtk::Application::builder()
        .application_id("dev.fgdb.ColumnLayoutTest")
        .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
        .build();

    application
        .register(None::<&gtk::gio::Cancellable>)
        .unwrap();

    let temporary = glib::mkdtemp(std::env::temp_dir().join("fgdb-column-layout-XXXXXX")).unwrap();
    let path = temporary.join("layout.conf");
    std::fs::write(&path, "# fgdb layout v8\nwindow=1100,500,0\nnotebook.left_sidebar=threads\ncolumn.locals.type=360\ncolumn.locals.location=240\ncolumn.locals.name=0\ncolumn.watches.type=250\ncolumn.array-viewer.value=410\n").unwrap();

    let layouts = ColumnLayouts::default();
    let (locals, local_columns) = table(&layouts, TableId::Locals);
    let (watches, watch_columns) = table(&layouts, TableId::Watches);
    let scroll = gtk::ScrolledWindow::builder().child(&locals).build();
    let window = gtk::ApplicationWindow::builder()
        .application(&application)
        .child(&scroll)
        .build();

    let persistence = Persistence::install_at(&window, Vec::new(), path.clone(), &layouts);
    assert_eq!(
        local_columns[0].fixed_width(),
        140,
        "invalid width replaced the default"
    );

    assert_eq!(
        local_columns[2].fixed_width(),
        360,
        "restore was delayed until map"
    );

    assert_eq!(
        local_columns[3].fixed_width(),
        240,
        "hidden column lost its width"
    );

    assert_eq!(
        watch_columns[2].fixed_width(),
        250,
        "locals and watches shared a key"
    );

    window.present();
    wait(|| persistence.0.ready_to_save.get());
    window.set_default_size(1500, 600);
    settle();
    assert_eq!(
        layouts.0.widths.borrow().get(TableId::Locals, "value"),
        None,
        "automatic allocation became a preference"
    );

    assert_eq!(local_columns[2].fixed_width(), 360);

    local_columns[2].set_title(Some("RENAMED TYPE"));
    locals.insert_column(0, &local_columns[2]);
    local_columns[0].set_fixed_width(280);
    local_columns[3].set_visible(true);
    assert_eq!(local_columns[3].fixed_width(), 240);
    local_columns[3].set_fixed_width(275);
    local_columns[3].set_visible(false);
    assert_eq!(watch_columns[0].fixed_width(), 140);

    scroll.set_child(gtk::Widget::NONE);
    let floating_scroll = gtk::ScrolledWindow::builder().child(&locals).build();
    let floating = gtk::Window::builder()
        .child(&floating_scroll)
        .default_width(750)
        .default_height(300)
        .build();

    floating.present();
    settle();
    assert_eq!(
        local_columns[0].fixed_width(),
        280,
        "popout allocation replaced the requested width"
    );

    floating_scroll.set_child(gtk::Widget::NONE);
    scroll.set_child(Some(&locals));
    floating.close();
    settle();

    let (array, array_columns) = table(&layouts, TableId::ArrayViewer);
    let (peer, peer_columns) = table(&layouts, TableId::ArrayViewer);
    assert_eq!(
        array_columns[1].fixed_width(),
        410,
        "late viewer did not restore"
    );

    array_columns[1].set_fixed_width(580);
    assert_eq!(peer_columns[1].fixed_width(), 580, "open viewers diverged");
    let restored = layouts.0.widths.borrow().clone();
    let changes = Rc::new(Cell::new(0));
    let changed = Rc::clone(&changes);
    peer_columns[1].connect_fixed_width_notify(move |_| changed.set(changed.get() + 1));
    layouts.restore(restored);
    assert_eq!(changes.get(), 0, "unchanged restore touched widgets");

    wait(|| {
        std::fs::read_to_string(&path).is_ok_and(|text| {
            text.contains("column.array-viewer.value=580")
                && text.contains("column.locals.name=280")
        })
    });

    persistence.finish();
    wait(|| persistence.final_save_done());
    let saved = std::fs::read_to_string(&path).unwrap();
    assert!(saved.contains("column.locals.location=275"));
    assert!(saved.contains("notebook.left_sidebar=threads"));
    assert!(!saved.contains("column.locals.value="));
    assert!(!saved.contains("RENAMED"));
    local_columns[0].set_fixed_width(999);
    layouts.reset();
    assert_eq!(
        layouts.0.widths.borrow().get(TableId::Locals, "name"),
        Some(280),
        "teardown changed saved widths"
    );

    window.close();
    let weak = Rc::downgrade(&layouts.0);
    let weak_view = locals.downgrade();
    drop((
        persistence,
        window,
        scroll,
        floating,
        floating_scroll,
        locals,
        local_columns,
        watches,
        watch_columns,
        array,
        array_columns,
        peer,
        peer_columns,
        layouts,
    ));

    settle();
    assert!(weak.upgrade().is_none(), "column registry retained itself");
    assert!(
        weak_view.upgrade().is_none(),
        "registry retained a closed view"
    );

    let layouts = ColumnLayouts::default();
    let window = gtk::ApplicationWindow::builder()
        .application(&application)
        .build();

    let persistence = Persistence::install_at(&window, Vec::new(), path.clone(), &layouts);
    let (locals, columns) = table(&layouts, TableId::Locals);
    assert_eq!(columns[0].fixed_width(), 280);
    assert_eq!(columns[2].fixed_width(), 360);
    assert_eq!(columns[3].fixed_width(), 275);
    window.set_child(Some(&gtk::ScrolledWindow::builder().child(&locals).build()));
    window.present();
    wait(|| persistence.0.ready_to_save.get());
    columns[0].set_fixed_width(300);
    layouts.reset();

    for (column, expected) in columns.iter().zip([140, 240, 180, 190]) {
        assert_eq!(column.fixed_width(), expected);
    }

    let (array, array_columns) = table(&layouts, TableId::ArrayViewer);
    assert_eq!(
        array_columns[1].fixed_width(),
        240,
        "reset missed a closed viewer"
    );

    wait(|| std::fs::read_to_string(&path).is_ok_and(|text| !text.contains("column.")));
    // Closing immediately after a reset must also retire an older snapshot
    // that has already reached the shared background writer.
    columns[0].set_fixed_width(350);
    persistence.save();
    layouts.reset();
    persistence.finish();
    wait(|| persistence.final_save_done());
    assert!(!std::fs::read_to_string(&path).unwrap().contains("column."));
    window.close();
    drop((
        persistence,
        window,
        locals,
        columns,
        array,
        array_columns,
        layouts,
    ));

    std::fs::remove_dir_all(temporary).unwrap();
}
