use super::*;

const MISC_PAGES: [(&str, &str); 6] = [
    ("startup-vectors", "Args / Env"),
    ("auxv", "Auxv"),
    ("call-abi", "Call ABI"),
    ("allocator", "Allocator"),
    ("locks", "Locks"),
    ("core-dump", "Core dump"),
];
const LOCKS_NOTE: &str = "This is a stopped-process wait snapshot from /proc/<pid>/task. It reports futex waiters and shared wait addresses.";

struct StartupWidgets {
    root: gtk::Box,
    split: gtk::Paned,
    summary: MiscStartupSummary,
    warning: gtk::Label,
    arguments_store: gio::ListStore,
    arguments_empty: gtk::Label,
    environment_store: gio::ListStore,
    environment_empty: gtk::Label,
}

struct MiscTablePage {
    root: gtk::Box,
    summary: gtk::Label,
    note: gtk::Label,
    store: gio::ListStore,
    empty: gtk::Label,
    view: gtk::ColumnView,
}

struct CallAbiWidgets {
    root: gtk::Box,
    summary: gtk::Label,
    context: gtk::Label,
    register_store: gio::ListStore,
    register_empty: gtk::Label,
    contract_store: gio::ListStore,
    split: gtk::Paned,
}

struct CoreWidgets {
    root: gtk::Box,
    summary: gtk::Label,
    warning: gtk::Label,
    note_store: gio::ListStore,
    file_store: gio::ListStore,
    empty: gtk::Label,
    split: gtk::Paned,
}

pub(super) fn build_misc_view() -> MiscView {
    let active = Rc::new(Cell::new(false));
    let tracking_enabled = Rc::new(Cell::new(false));
    let in_flight = Rc::new(Cell::new(false));
    let needs_refresh = Rc::new(Cell::new(true));
    let locks_requested = Rc::new(Cell::new(false));
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.set_size_request(0, 0);
    root.add_css_class("sidebar");
    root.add_css_class("kernel-page");
    root.add_css_class("misc-page");

    let pages = gtk::Stack::new();
    pages.set_size_request(0, 0);
    pages.set_vexpand(true);
    pages.set_vhomogeneous(false);
    pages.set_hhomogeneous(false);
    pages.set_transition_type(gtk::StackTransitionType::None);
    let switcher = gtk::StackSwitcher::new();
    switcher.add_css_class("kernel-tabs");
    switcher.set_stack(Some(&pages));
    switcher.set_hexpand(true);
    let navigation = build_subtab_navigation(
        &switcher,
        &pages,
        &MISC_PAGES,
        "Scroll to earlier Misc views",
        "Scroll to later Misc views",
    );
    root.append(&navigation.root);
    root.append(&navigation.compact_root);

    let startup = build_startup_page();
    pages.add_titled(&startup.root, Some("startup-vectors"), "Args / Env");
    let auxv = build_auxv_page();
    pages.add_titled(&auxv.root, Some("auxv"), "Auxv");
    let call_abi = build_call_abi_page();
    pages.add_titled(&call_abi.root, Some("call-abi"), "Call ABI");
    let allocator = build_allocator_page();
    pages.add_titled(&allocator.root, Some("allocator"), "Allocator");
    let locks = build_locks_page();
    pages.add_titled(&locks.root, Some("locks"), "Locks");
    let core = build_core_page();
    pages.add_titled(&core.root, Some("core-dump"), "Core dump");
    root.append(&pages);

    MiscView {
        root,
        wide_subtabs: navigation.root,
        compact_subtabs: navigation.compact_root,
        active,
        tracking_enabled,
        in_flight,
        needs_refresh,
        pages,
        locks_requested,
        summary: startup.summary,
        warning: startup.warning,
        arguments_store: startup.arguments_store,
        arguments_empty: startup.arguments_empty,
        environment_store: startup.environment_store,
        environment_empty: startup.environment_empty,
        startup_split: startup.split,
        auxv_summary: auxv.summary,
        auxv_store: auxv.store,
        auxv_empty: auxv.empty,
        call_abi_summary: call_abi.summary,
        call_abi_context: call_abi.context,
        call_abi_register_store: call_abi.register_store,
        call_abi_register_empty: call_abi.register_empty,
        call_abi_contract_store: call_abi.contract_store,
        call_abi_split: call_abi.split,
        allocator_summary: allocator.summary,
        allocator_store: allocator.store,
        allocator_empty: allocator.empty,
        lock_summary: locks.summary,
        lock_note: locks.note,
        lock_store: locks.store,
        lock_empty: locks.empty,
        core_summary: core.summary,
        core_warning: core.warning,
        core_note_store: core.note_store,
        core_file_store: core.file_store,
        core_empty: core.empty,
        core_split: core.split,
    }
}

fn build_startup_page() -> StartupWidgets {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.set_vexpand(true);
    let controls = gtk::Box::new(gtk::Orientation::Vertical, 3);
    controls.add_css_class("misc-startup-controls");
    let (summary_view, summary) = build_startup_summary();
    let search = gtk::SearchEntry::builder()
        .placeholder_text("Filter argument, variable, value, or address")
        .build();
    search.set_hexpand(true);
    search.add_css_class("kernel-change-search");
    search.add_css_class("kernel-table-search");
    controls.append(&summary_view);
    controls.append(&search);
    root.append(&controls);

    let warning = gtk::Label::new(None);
    warning.add_css_class("misc-startup-warning");
    warning.add_css_class("status-error");
    warning.set_halign(gtk::Align::Fill);
    warning.set_xalign(0.0);
    warning.set_wrap(true);
    warning.set_visible(false);
    enable_stable_text_selection(&warning);
    root.append(&warning);

    let query = Rc::new(RefCell::new(String::new()));
    let (arguments, arguments_store, arguments_empty, arguments_filter) =
        build_arguments_section(Rc::clone(&query));
    let (environment, environment_store, environment_empty, environment_filter) =
        build_environment_section(Rc::clone(&query));
    search.connect_search_changed(move |search| {
        let text = search.text().trim().to_lowercase();
        if *query.borrow() != text {
            query.replace(text);
            arguments_filter.changed(gtk::FilterChange::Different);
            environment_filter.changed(gtk::FilterChange::Different);
        }
    });

    let split = gtk::Paned::new(gtk::Orientation::Vertical);
    split.add_css_class("misc-startup-split");
    split.set_wide_handle(false);
    split.set_resize_start_child(true);
    split.set_shrink_start_child(false);
    split.set_resize_end_child(true);
    split.set_shrink_end_child(false);
    split.set_start_child(Some(&arguments));
    split.set_end_child(Some(&environment));
    split.set_position(260);
    split.set_vexpand(true);
    root.append(&split);
    StartupWidgets {
        root,
        split,
        summary,
        warning,
        arguments_store,
        arguments_empty,
        environment_store,
        environment_empty,
    }
}

fn build_auxv_page() -> MiscTablePage {
    let page = build_misc_table_page("Auxiliary vector data is unavailable");
    page.note.set_text(
        "Kernel-supplied process-entry values. Pointer interpretations are limited to known mappings.",
    );
    page.view
        .append_column(&misc_column::<AuxvEntry>("ENTRY", 170, false, |row| {
            row.name.clone()
        }));
    page.view
        .append_column(&misc_column::<AuxvEntry>("TYPE", 72, false, |row| {
            row.kind.to_string()
        }));
    page.view
        .append_column(&misc_column::<AuxvEntry>("RAW VALUE", 190, false, |row| {
            format!("0x{:016x}", row.value)
        }));
    page.view.append_column(&misc_column::<AuxvEntry>(
        "INTERPRETATION",
        420,
        true,
        |row| row.interpretation.clone(),
    ));
    page
}

fn build_call_abi_page() -> CallAbiWidgets {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 4);
    root.set_vexpand(true);
    let summary = misc_summary_label();
    let context = misc_note_label();
    context.add_css_class("call-abi-context");
    root.append(&summary);
    root.append(&context);

    let (registers, register_store, register_empty, register_view) =
        build_misc_table("No ABI transfer at the current instruction");
    registers.prepend(&section_title("LIVE ABI TRANSFER"));
    register_view.append_column(&misc_column::<CallAbiRegister>("ROLE", 250, false, |row| {
        row.role.clone()
    }));
    register_view.append_column(&misc_column::<CallAbiRegister>(
        "REGISTER",
        120,
        false,
        |row| row.name.clone(),
    ));
    register_view.append_column(&misc_column::<CallAbiRegister>("VALUE", 420, true, |row| {
        row.value.clone()
    }));

    let (contract, contract_store, _, contract_view) =
        build_misc_table("Call ABI metadata is unavailable for this target");
    contract.prepend(&section_title("ABI CONTRACT"));
    contract_view.append_column(&misc_column::<CallAbiFact>("ASPECT", 260, false, |row| {
        row.aspect.clone()
    }));
    contract_view.append_column(&misc_column::<CallAbiFact>(
        "CONVENTION",
        620,
        true,
        |row| row.value.clone(),
    ));
    let split = gtk::Paned::new(gtk::Orientation::Vertical);
    split.add_css_class("misc-data-split");
    split.set_wide_handle(false);
    split.set_resize_start_child(true);
    split.set_resize_end_child(true);
    split.set_shrink_start_child(false);
    split.set_shrink_end_child(false);
    split.set_start_child(Some(&registers));
    split.set_end_child(Some(&contract));
    split.set_position(300);
    split.set_vexpand(true);
    root.append(&split);
    CallAbiWidgets {
        root,
        summary,
        context,
        register_store,
        register_empty,
        contract_store,
        split,
    }
}

fn build_allocator_page() -> MiscTablePage {
    let page = build_misc_table_page("No allocator-related mappings are available");
    page.note.set_visible(false);
    page.view.append_column(&misc_column::<AllocatorRegion>(
        "ADDRESS RANGE",
        330,
        false,
        |row| format!("0x{:016x}–0x{:016x}", row.start, row.end),
    ));
    page.view
        .append_column(&misc_column::<AllocatorRegion>("SIZE", 120, false, |row| {
            crate::kernel::format_bytes(row.size())
        }));
    page.view
        .append_column(&misc_column::<AllocatorRegion>("PERM", 78, false, |row| {
            row.permissions.clone()
        }));
    page.view
        .append_column(&misc_column::<AllocatorRegion>("ROLE", 260, false, |row| {
            row.role.clone()
        }));
    page.view.append_column(&misc_column::<AllocatorRegion>(
        "BACKING",
        420,
        true,
        |row| row.path.clone(),
    ));
    page
}

fn build_locks_page() -> MiscTablePage {
    let page = build_misc_table_page("Open this tab to inspect kernel-visible futex waits");
    page.note.set_text(LOCKS_NOTE);
    page.view
        .append_column(&misc_column::<LockWait>("TID", 90, false, |row| {
            row.tid.to_string()
        }));
    page.view
        .append_column(&misc_column::<LockWait>("THREAD", 180, false, |row| {
            row.thread.clone()
        }));
    page.view
        .append_column(&misc_column::<LockWait>("STATE", 150, false, |row| {
            row.state.clone()
        }));
    page.view.append_column(&misc_column::<LockWait>(
        "WAIT ADDRESS",
        190,
        false,
        |row| {
            row.address
                .map_or_else(|| String::from("—"), |value| format!("0x{value:016x}"))
        },
    ));
    page.view
        .append_column(&misc_column::<LockWait>("OPERATION", 190, false, |row| {
            row.operation.clone()
        }));
    page.view.append_column(&misc_column::<LockWait>(
        "EXPECTED / COUNT",
        140,
        false,
        |row| {
            row.expected
                .map_or_else(|| String::from("—"), |value| format!("0x{value:x}"))
        },
    ));
    page.view
        .append_column(&misc_column::<LockWait>("DETAILS", 360, true, |row| {
            row.details.clone()
        }));
    page
}

fn build_core_page() -> CoreWidgets {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 4);
    root.set_vexpand(true);
    let summary = misc_summary_label();
    let warning = misc_note_label();
    warning.add_css_class("status-error");
    warning.set_visible(false);
    let empty =
        empty_label("Core metadata is shown when the active session opened an ELF core dump");
    root.append(&summary);
    root.append(&warning);
    root.append(&empty);

    let (notes, note_store, _, note_view) = build_misc_table("No recognized ELF notes are present");
    notes.prepend(&section_title("ELF NOTES"));
    note_view.append_column(&misc_column::<CoreNote>("OWNER", 150, false, |row| {
        row.owner.clone()
    }));
    note_view.append_column(&misc_column::<CoreNote>("TYPE", 210, false, |row| {
        row.kind.clone()
    }));
    note_view.append_column(&misc_column::<CoreNote>("BYTES", 110, true, |row| {
        row.bytes.to_string()
    }));

    let (files, file_store, _, file_view) = build_searchable_misc_table::<CoreMappedFile>(
        "No NT_FILE mappings match the filter",
        "Filter address, offset, or path",
        |row, query| {
            query.is_empty()
                || row.path.to_lowercase().contains(query)
                || format!("{:x}", row.start).contains(query)
                || format!("{:x}", row.end).contains(query)
                || format!("{:x}", row.file_offset).contains(query)
        },
    );
    files.prepend(&section_title("FILE-BACKED MAPPINGS AT CAPTURE"));
    file_view.append_column(&misc_column::<CoreMappedFile>(
        "ADDRESS RANGE",
        330,
        false,
        |row| format!("0x{:016x}–0x{:016x}", row.start, row.end),
    ));
    file_view.append_column(&misc_column::<CoreMappedFile>(
        "FILE OFFSET",
        160,
        false,
        |row| format!("0x{:x}", row.file_offset),
    ));
    file_view.append_column(&misc_column::<CoreMappedFile>("PATH", 520, true, |row| {
        row.path.clone()
    }));
    let split = gtk::Paned::new(gtk::Orientation::Vertical);
    split.add_css_class("misc-data-split");
    split.set_wide_handle(false);
    split.set_resize_start_child(true);
    split.set_resize_end_child(true);
    split.set_shrink_start_child(false);
    split.set_shrink_end_child(false);
    split.set_start_child(Some(&notes));
    split.set_end_child(Some(&files));
    split.set_position(250);
    split.set_vexpand(true);
    root.append(&split);
    CoreWidgets {
        root,
        summary,
        warning,
        note_store,
        file_store,
        empty,
        split,
    }
}

fn build_misc_table_page(empty_text: &str) -> MiscTablePage {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 4);
    root.set_vexpand(true);
    let summary = misc_summary_label();
    let note = misc_note_label();
    let (table, store, empty, view) = build_misc_table(empty_text);
    root.append(&summary);
    root.append(&note);
    root.append(&table);
    MiscTablePage {
        root,
        summary,
        note,
        store,
        empty,
        view,
    }
}

fn build_misc_table(empty_text: &str) -> (gtk::Box, gio::ListStore, gtk::Label, gtk::ColumnView) {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.set_vexpand(true);
    let store = gio::ListStore::new::<glib::BoxedAnyObject>();
    let selection = gtk::SingleSelection::new(Some(store.clone()));
    selection.set_autoselect(false);
    selection.set_can_unselect(true);
    let view = gtk::ColumnView::new(Some(selection));
    view.add_css_class("debug-table");
    view.add_css_class("misc-data-table");
    view.set_vexpand(true);
    view.set_reorderable(true);
    let empty = empty_label(empty_text);
    let empty_for_store = empty.clone();
    store.connect_items_changed(move |store, _, _, _| {
        empty_for_store.set_visible(store.n_items() == 0);
    });
    let scrolled = gtk::ScrolledWindow::builder()
        .child(&view)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .build();
    configure_misc_scroller(&scrolled);
    root.append(&empty);
    root.append(&scrolled);
    (root, store, empty, view)
}

fn build_searchable_misc_table<T: 'static>(
    empty_text: &str,
    placeholder: &str,
    matches: impl Fn(&T, &str) -> bool + 'static,
) -> (gtk::Box, gio::ListStore, gtk::Label, gtk::ColumnView) {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.set_vexpand(true);
    let search = gtk::SearchEntry::builder()
        .placeholder_text(placeholder)
        .build();
    search.add_css_class("kernel-change-search");
    search.add_css_class("kernel-table-search");
    search.set_hexpand(true);
    let query = Rc::new(RefCell::new(String::new()));
    let query_for_filter = Rc::clone(&query);
    let filter = gtk::CustomFilter::new(move |object| {
        object
            .downcast_ref::<glib::BoxedAnyObject>()
            .is_some_and(|row| matches(&row.borrow::<T>(), &query_for_filter.borrow()))
    });
    let store = gio::ListStore::new::<glib::BoxedAnyObject>();
    let filtered = gtk::FilterListModel::new(Some(store.clone()), Some(filter.clone()));
    let selection = gtk::SingleSelection::new(Some(filtered.clone()));
    selection.set_autoselect(false);
    selection.set_can_unselect(true);
    let view = gtk::ColumnView::new(Some(selection));
    view.add_css_class("debug-table");
    view.add_css_class("misc-data-table");
    view.set_vexpand(true);
    view.set_reorderable(true);
    let empty = empty_label(empty_text);
    let empty_for_filter = empty.clone();
    filtered.connect_items_changed(move |model, _, _, _| {
        empty_for_filter.set_visible(model.n_items() == 0);
    });
    search.connect_search_changed(move |search| {
        let text = search.text().trim().to_lowercase();
        if *query.borrow() != text {
            query.replace(text);
            filter.changed(gtk::FilterChange::Different);
        }
    });
    let scrolled = gtk::ScrolledWindow::builder()
        .child(&view)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .build();
    configure_misc_scroller(&scrolled);
    root.append(&search);
    root.append(&empty);
    root.append(&scrolled);
    (root, store, empty, view)
}

fn misc_summary_label() -> gtk::Label {
    let label = gtk::Label::new(Some("—"));
    label.add_css_class("misc-data-summary");
    label.set_halign(gtk::Align::Fill);
    label.set_xalign(0.0);
    label.set_wrap(true);
    enable_stable_text_selection(&label);
    label
}

fn misc_note_label() -> gtk::Label {
    let label = gtk::Label::new(None);
    label.add_css_class("muted");
    label.add_css_class("misc-data-note");
    label.set_halign(gtk::Align::Fill);
    label.set_xalign(0.0);
    label.set_wrap(true);
    enable_stable_text_selection(&label);
    label
}

fn misc_column<T: 'static>(
    title: &str,
    width: i32,
    expand: bool,
    value: impl Fn(&T) -> String + Copy + 'static,
) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, object| {
        let Some(item) = object.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let label = gtk::Label::new(None);
        label.add_css_class("debug-table-cell");
        label.set_halign(gtk::Align::Start);
        label.set_ellipsize(pango::EllipsizeMode::Middle);
        enable_stable_text_selection(&label);
        item.set_child(Some(&label));
    });
    factory.connect_bind(move |_, object| {
        let Some(item) = object.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let (Some(label), Some(data)) = (
            item.child().and_downcast::<gtk::Label>(),
            item.item().and_downcast::<glib::BoxedAnyObject>(),
        ) else {
            return;
        };
        clear_label_selection(&label);
        let text = value(&data.borrow::<T>());
        label.set_text(&text);
        label.set_tooltip_text(Some(&text));
    });
    let column = gtk::ColumnViewColumn::new(Some(title), Some(factory));
    column.set_fixed_width(width);
    column.set_resizable(true);
    column.set_expand(expand);
    column
}

fn build_startup_summary() -> (gtk::FlowBox, MiscStartupSummary) {
    let summary = gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .homogeneous(true)
        .min_children_per_line(2)
        .max_children_per_line(4)
        .column_spacing(1)
        .row_spacing(1)
        .build();
    summary.add_css_class("misc-startup-summary");
    let argc = append_startup_summary_cell(&summary, "ARGC");
    let argv = append_startup_summary_cell(&summary, "ARGV RANGE");
    let envp = append_startup_summary_cell(&summary, "ENVP RANGE");
    let environment = append_startup_summary_cell(&summary, "ENVIRONMENT");
    set_startup_summary_value(&argc, "—");
    set_startup_summary_value(&argv, "—");
    set_startup_summary_value(&envp, "—");
    set_startup_summary_value(&environment, "—");
    (
        summary,
        MiscStartupSummary {
            argc,
            argv,
            envp,
            environment,
        },
    )
}

fn append_startup_summary_cell(summary: &gtk::FlowBox, title: &str) -> gtk::Label {
    let cell = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    cell.add_css_class("misc-startup-summary-cell");
    let key = gtk::Label::new(Some(title));
    key.add_css_class("misc-startup-summary-key");
    key.set_halign(gtk::Align::Start);
    let value = gtk::Label::new(None);
    value.add_css_class("misc-startup-summary-value");
    value.set_hexpand(true);
    value.set_halign(gtk::Align::Start);
    value.set_xalign(0.0);
    value.set_single_line_mode(true);
    value.set_ellipsize(pango::EllipsizeMode::Middle);
    enable_stable_text_selection(&value);
    cell.append(&key);
    cell.append(&value);
    summary.insert(&cell, -1);
    value
}

fn set_startup_summary_value(label: &gtk::Label, value: &str) {
    label.set_text(value);
    label.set_tooltip_text(Some(value));
}

fn build_arguments_section(
    query: Rc<RefCell<String>>,
) -> (gtk::Box, gio::ListStore, gtk::Label, gtk::CustomFilter) {
    let section = gtk::Box::new(gtk::Orientation::Vertical, 0);
    section.add_css_class("misc-vector-section");
    section.append(&section_title("ARGV"));
    let store = gio::ListStore::new::<glib::BoxedAnyObject>();
    let filter = gtk::CustomFilter::new(move |object| {
        let Some(data) = object.downcast_ref::<glib::BoxedAnyObject>() else {
            return false;
        };
        argument_matches(&data.borrow::<ProcessArgument>(), &query.borrow())
    });
    let filtered = gtk::FilterListModel::new(Some(store.clone()), Some(filter.clone()));
    let selection = gtk::SingleSelection::new(Some(filtered.clone()));
    selection.set_autoselect(false);
    selection.set_can_unselect(true);
    let view = gtk::ColumnView::new(Some(selection));
    view.add_css_class("debug-table");
    view.add_css_class("misc-vector-table");
    view.set_vexpand(true);
    view.set_reorderable(true);
    view.append_column(&argument_column("ENTRY", 100, false, |row, label| {
        label.set_text(&argument_label(row.index));
        label.add_css_class("misc-vector-name");
    }));
    view.append_column(&argument_column("ADDRESS", 180, false, |row, label| {
        label.set_text(&format_address(row.address));
        label.add_css_class("kernel-numeric");
    }));
    view.append_column(&argument_column("BYTES", 70, false, |row, label| {
        label.set_text(&row.byte_len.to_string());
    }));
    view.append_column(&argument_column("VALUE", 420, true, |row, label| {
        label.set_text(&row.value);
    }));
    let empty = empty_label("No argument entries are available");
    let empty_for_filter = empty.clone();
    filtered.connect_items_changed(move |model, _, _, _| {
        empty_for_filter.set_visible(model.n_items() == 0);
    });
    let scrolled = gtk::ScrolledWindow::builder()
        .child(&view)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .build();
    configure_misc_scroller(&scrolled);
    section.append(&empty);
    section.append(&scrolled);
    (section, store, empty, filter)
}

fn build_environment_section(
    query: Rc<RefCell<String>>,
) -> (gtk::Box, gio::ListStore, gtk::Label, gtk::CustomFilter) {
    let section = gtk::Box::new(gtk::Orientation::Vertical, 0);
    section.add_css_class("misc-vector-section");
    section.append(&section_title("ENVP / ENVIRONMENT"));
    let store = gio::ListStore::new::<glib::BoxedAnyObject>();
    let filter = gtk::CustomFilter::new(move |object| {
        let Some(data) = object.downcast_ref::<glib::BoxedAnyObject>() else {
            return false;
        };
        environment_matches(&data.borrow::<ProcessEnvironment>(), &query.borrow())
    });
    let filtered = gtk::FilterListModel::new(Some(store.clone()), Some(filter.clone()));
    let selection = gtk::SingleSelection::new(Some(filtered.clone()));
    selection.set_autoselect(false);
    selection.set_can_unselect(true);
    let view = gtk::ColumnView::new(Some(selection));
    view.add_css_class("debug-table");
    view.add_css_class("misc-vector-table");
    view.set_vexpand(true);
    view.set_reorderable(true);
    view.append_column(&environment_column("ENTRY", 100, false, |row, label| {
        label.set_text(&format!("envp[{}]", row.index));
    }));
    view.append_column(&environment_column("ADDRESS", 180, false, |row, label| {
        label.set_text(&format_address(row.address));
        label.add_css_class("kernel-numeric");
    }));
    view.append_column(&environment_column("BYTES", 70, false, |row, label| {
        label.set_text(&row.byte_len.to_string());
    }));
    view.append_column(&environment_column("NAME", 210, false, |row, label| {
        label.set_text(&row.name);
        label.add_css_class("misc-vector-name");
    }));
    view.append_column(&environment_column("VALUE", 420, true, |row, label| {
        label.set_text(&row.value);
    }));
    let empty = empty_label("No environment entries are available");
    let empty_for_filter = empty.clone();
    filtered.connect_items_changed(move |model, _, _, _| {
        empty_for_filter.set_visible(model.n_items() == 0);
    });
    let scrolled = gtk::ScrolledWindow::builder()
        .child(&view)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .build();
    configure_misc_scroller(&scrolled);
    section.append(&empty);
    section.append(&scrolled);
    (section, store, empty, filter)
}

fn configure_misc_scroller(scrolled: &gtk::ScrolledWindow) {
    scrolled.set_min_content_width(0);
    scrolled.set_propagate_natural_width(false);
    scrolled.set_size_request(0, -1);
}

fn argument_matches(argument: &ProcessArgument, query: &str) -> bool {
    query.is_empty()
        || argument.value.to_lowercase().contains(query)
        || argument_label(argument.index)
            .to_lowercase()
            .contains(query)
        || argument
            .address
            .is_some_and(|address| format!("0x{address:x}").contains(query))
}

fn environment_matches(entry: &ProcessEnvironment, query: &str) -> bool {
    query.is_empty()
        || entry.name.to_lowercase().contains(query)
        || entry.value.to_lowercase().contains(query)
        || format!("envp[{}]", entry.index)
            .to_lowercase()
            .contains(query)
        || entry
            .address
            .is_some_and(|address| format!("0x{address:x}").contains(query))
}

fn argument_column(
    title: &str,
    width: i32,
    expand: bool,
    bind: impl Fn(&ProcessArgument, &gtk::Label) + Copy + 'static,
) -> gtk::ColumnViewColumn {
    vector_column(title, width, expand, move |data, label| {
        bind(&data.borrow::<ProcessArgument>(), label);
    })
}

fn environment_column(
    title: &str,
    width: i32,
    expand: bool,
    bind: impl Fn(&ProcessEnvironment, &gtk::Label) + Copy + 'static,
) -> gtk::ColumnViewColumn {
    vector_column(title, width, expand, move |data, label| {
        bind(&data.borrow::<ProcessEnvironment>(), label);
    })
}

fn vector_column(
    title: &str,
    width: i32,
    expand: bool,
    bind: impl Fn(&glib::BoxedAnyObject, &gtk::Label) + Copy + 'static,
) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, object| {
        let Some(item) = object.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let label = gtk::Label::new(None);
        label.add_css_class("debug-table-cell");
        label.set_halign(gtk::Align::Start);
        label.set_ellipsize(pango::EllipsizeMode::Middle);
        enable_stable_text_selection(&label);
        item.set_child(Some(&label));
    });
    factory.connect_bind(move |_, object| {
        let Some(item) = object.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let (Some(label), Some(data)) = (
            item.child().and_downcast::<gtk::Label>(),
            item.item().and_downcast::<glib::BoxedAnyObject>(),
        ) else {
            return;
        };
        label.remove_css_class("misc-vector-name");
        label.remove_css_class("kernel-numeric");
        clear_label_selection(&label);
        bind(&data, &label);
        label.set_tooltip_text(Some(&label.text()));
    });
    let column = gtk::ColumnViewColumn::new(Some(title), Some(factory));
    column.set_fixed_width(width);
    column.set_resizable(true);
    column.set_expand(expand);
    column
}

fn argument_label(index: usize) -> String {
    format!("argv[{index}]")
}

fn format_address(address: Option<u64>) -> String {
    address.map_or_else(|| String::from("—"), |address| format!("0x{address:016x}"))
}

fn format_range(range: Option<(u64, u64)>) -> String {
    range.map_or_else(
        || String::from("range unavailable"),
        |(start, end)| format!("0x{start:016x}–0x{end:016x}"),
    )
}

fn linux_signal_name(signal: i32) -> &'static str {
    match signal {
        1 => "SIGHUP",
        2 => "SIGINT",
        3 => "SIGQUIT",
        4 => "SIGILL",
        5 => "SIGTRAP",
        6 => "SIGABRT",
        7 => "SIGBUS",
        8 => "SIGFPE",
        9 => "SIGKILL",
        10 => "SIGUSR1",
        11 => "SIGSEGV",
        12 => "SIGUSR2",
        13 => "SIGPIPE",
        14 => "SIGALRM",
        15 => "SIGTERM",
        16 => "SIGSTKFLT",
        17 => "SIGCHLD",
        18 => "SIGCONT",
        19 => "SIGSTOP",
        20 => "SIGTSTP",
        21 => "SIGTTIN",
        22 => "SIGTTOU",
        23 => "SIGURG",
        24 => "SIGXCPU",
        25 => "SIGXFSZ",
        26 => "SIGVTALRM",
        27 => "SIGPROF",
        28 => "SIGWINCH",
        29 => "SIGIO",
        30 => "SIGPWR",
        31 => "SIGSYS",
        _ => "signal",
    }
}

pub(super) fn connect_misc_tab_visibility(
    notebook: &gtk::Notebook,
    misc_page: u32,
    view: &MiscView,
    refresh_handler: &Rc<RefCell<Option<MiscRefreshHandler>>>,
) {
    let active = Rc::clone(&view.active);
    let tracking_enabled = Rc::clone(&view.tracking_enabled);
    let needs_refresh = Rc::clone(&view.needs_refresh);
    let handler = Rc::clone(refresh_handler);
    notebook.connect_switch_page(move |_, _, page| {
        let now_active = page == misc_page;
        active.set(now_active);
        if now_active {
            tracking_enabled.set(true);
        }
        if now_active
            && needs_refresh.get()
            && let Some(handler) = handler.borrow().as_ref()
        {
            handler();
        }
    });

    let active = Rc::clone(&view.active);
    let locks_requested = Rc::clone(&view.locks_requested);
    let needs_refresh = Rc::clone(&view.needs_refresh);
    let in_flight = Rc::clone(&view.in_flight);
    let handler = Rc::clone(refresh_handler);
    view.pages.connect_visible_child_name_notify(move |pages| {
        if pages.visible_child_name().as_deref() == Some("locks") && !locks_requested.replace(true)
        {
            needs_refresh.set(true);
            if active.get()
                && !in_flight.get()
                && let Some(handler) = handler.borrow().as_ref()
            {
                handler();
            }
        }
    });
}

impl MiscView {
    fn show_startup(&self, snapshot: ProcessStartupSnapshot) {
        let argument_count = snapshot.arguments.len();
        let environment_count = snapshot.environment.len();
        set_startup_summary_value(&self.summary.argc, &argument_count.to_string());
        set_startup_summary_value(&self.summary.argv, &format_range(snapshot.argument_range));
        set_startup_summary_value(
            &self.summary.envp,
            &format_range(snapshot.environment_range),
        );
        set_startup_summary_value(
            &self.summary.environment,
            &format!("{environment_count} entries"),
        );
        replace_boxed_store_if_changed(&self.arguments_store, snapshot.arguments);
        replace_boxed_store_if_changed(&self.environment_store, snapshot.environment);
        self.arguments_empty.set_visible(argument_count == 0);
        self.environment_empty.set_visible(environment_count == 0);
        let warning = snapshot.warnings.join("\n");
        self.warning.set_text(&warning);
        self.warning.set_visible(!warning.is_empty());
    }

    fn show_live_snapshot(&self, snapshot: LiveMiscSnapshot) {
        self.show_startup(snapshot.startup);
        let auxv_count = snapshot.auxv.len();
        self.auxv_summary
            .set_text(&format!("{auxv_count} kernel auxiliary-vector entries"));
        replace_boxed_store_if_changed(&self.auxv_store, snapshot.auxv);
        self.auxv_empty.set_visible(auxv_count == 0);

        self.show_allocator(snapshot.allocator);
        if let Some(locks) = snapshot.locks {
            self.show_locks(locks);
        }
        if !snapshot.warnings.is_empty() {
            let current = self.warning.text();
            let separator = if current.is_empty() { "" } else { "\n" };
            self.warning.set_text(&format!(
                "{current}{separator}{}",
                snapshot.warnings.join("\n")
            ));
            self.warning.set_visible(true);
        }
    }

    fn show_allocator(&self, allocator: AllocatorSnapshot) {
        let region_count = allocator.regions.len();
        self.allocator_summary.set_text(&format!(
            "{}  ·  brk heap {}  ·  anonymous writable {}  ·  {region_count} relevant mappings",
            allocator.implementation,
            crate::kernel::format_bytes(allocator.heap_bytes),
            crate::kernel::format_bytes(allocator.anonymous_writable_bytes),
        ));
        replace_boxed_store_if_changed(&self.allocator_store, allocator.regions);
        self.allocator_empty.set_visible(region_count == 0);
    }

    fn show_locks(&self, locks: LockSnapshot) {
        let wait_count = locks.waits.len();
        let address_count = locks
            .waits
            .iter()
            .filter_map(|wait| wait.address)
            .collect::<HashSet<_>>()
            .len();
        self.lock_summary.set_text(&format!(
            "{} threads scanned  ·  {wait_count} kernel-visible waits  ·  {address_count} wait addresses",
            locks.threads_scanned
        ));
        let mut note = String::from(LOCKS_NOTE);
        if !locks.warnings.is_empty() {
            if !note.is_empty() {
                note.push('\n');
            }
            note.push_str(&locks.warnings.join("\n"));
        }
        self.lock_note.set_text(&note);
        replace_boxed_store_if_changed(&self.lock_store, locks.waits);
        self.lock_empty
            .set_text("No kernel-visible futex waits are present");
        self.lock_empty.set_visible(wait_count == 0);
    }

    fn show_call_abi(&self, snapshot: CallAbiSnapshot) {
        let current = snapshot.current_frame.as_ref().map_or_else(
            || String::from("no selected frame"),
            |frame| format!("#{} {}", frame.level, frame.function),
        );
        self.call_abi_summary.set_text(&format!(
            "{}  ·  {}  ·  {}-bit pointers  ·  {current}",
            snapshot.architecture, snapshot.calling_convention, snapshot.pointer_bits
        ));
        replace_boxed_store_if_changed(&self.call_abi_contract_store, snapshot.contract);
    }

    fn show_call_abi_transfer(&self, transfer: crate::misc::CallAbiTransfer) {
        let empty = transfer.registers.is_empty();
        self.call_abi_context.set_text(&transfer.context);
        replace_boxed_store_if_changed(&self.call_abi_register_store, transfer.registers);
        self.call_abi_register_empty.set_visible(empty);
    }

    pub(super) fn show_call_abi_pending(&self) {
        self.call_abi_context
            .set_text("Waiting for the current stopped instruction…");
        self.call_abi_context.set_tooltip_text(None);
        self.call_abi_register_store.remove_all();
        self.call_abi_register_empty.set_visible(true);
    }

    fn show_core(&self, snapshot: CoreDumpSnapshot) {
        let auxv_count = snapshot.auxv.len();
        self.auxv_summary.set_text(&format!(
            "{auxv_count} auxiliary-vector entries recovered from NT_AUXV"
        ));
        replace_boxed_store_if_changed(&self.auxv_store, snapshot.auxv.clone());
        self.auxv_empty.set_visible(auxv_count == 0);
        let signal = snapshot.signal.map_or_else(
            || String::from("signal unavailable"),
            |signal| {
                let code = snapshot
                    .signal_code
                    .map_or_else(String::new, |code| format!(" code {code}"));
                let address = snapshot
                    .fault_address
                    .map_or_else(String::new, |address| format!(" at 0x{address:016x}"));
                format!("{} ({signal}){code}{address}", linux_signal_name(signal))
            },
        );
        let process = snapshot
            .process_name
            .as_deref()
            .unwrap_or("process unknown");
        let pid = snapshot
            .pid
            .map_or_else(String::new, |pid| format!(" PID {pid}"));
        self.core_summary.set_text(&format!(
            "{}  ·  {} / {} / {}  ·  {process}{pid}  ·  {signal}  ·  {} threads  ·  {}  ·  {}",
            snapshot.path.display(),
            snapshot.class,
            snapshot.architecture,
            snapshot.endian,
            snapshot.threads.len(),
            crate::kernel::format_bytes(snapshot.size),
            snapshot.command.as_deref().unwrap_or("command unavailable"),
        ));
        let warning = snapshot.warnings.join("\n");
        self.core_warning.set_text(&warning);
        self.core_warning.set_visible(!warning.is_empty());
        self.core_empty.set_visible(false);
        replace_boxed_store_if_changed(&self.core_note_store, snapshot.notes);
        replace_boxed_store_if_changed(&self.core_file_store, snapshot.files);
    }

    fn clear(&self) {
        for value in [
            &self.summary.argc,
            &self.summary.argv,
            &self.summary.envp,
            &self.summary.environment,
        ] {
            set_startup_summary_value(value, "—");
        }
        self.warning.set_visible(false);
        self.warning.set_text("");
        self.arguments_store.remove_all();
        self.environment_store.remove_all();
        self.arguments_empty.set_visible(true);
        self.environment_empty.set_visible(true);
        self.auxv_summary.set_text("—");
        self.auxv_store.remove_all();
        self.auxv_empty.set_visible(true);
        self.allocator_summary.set_text("—");
        self.allocator_store.remove_all();
        self.allocator_empty.set_visible(true);
        self.lock_summary.set_text("—");
        self.lock_store.remove_all();
        self.lock_empty.set_visible(true);
    }

    fn clear_core(&self) {
        self.core_summary.set_text("—");
        self.core_warning.set_text("");
        self.core_warning.set_visible(false);
        self.core_note_store.remove_all();
        self.core_file_store.remove_all();
        self.core_empty.set_visible(true);
    }
}

impl Ui {
    pub fn set_misc_refresh_handler(&self, handler: impl Fn() + 'static) {
        self.misc_refresh_handler.replace(Some(Rc::new(handler)));
    }

    pub fn misc_locks_requested(&self) -> bool {
        self.misc_view.locks_requested.get()
    }

    pub fn begin_misc_refresh(&self) -> Option<u64> {
        if !self.misc_refresh_allowed() || self.misc_view.in_flight.get() {
            return None;
        }
        let generation = self.misc_refresh_generation.get().wrapping_add(1);
        self.misc_refresh_generation.set(generation);
        self.misc_view.in_flight.set(true);
        self.update_control_sensitivity();
        Some(generation)
    }

    pub fn show_misc_snapshot(&self, generation: u64, snapshot: LiveMiscSnapshot) {
        if generation != self.misc_refresh_generation.get() {
            self.finish_stale_misc_refresh();
            return;
        }
        let needs_locks = self.misc_view.locks_requested.get() && snapshot.locks.is_none();
        self.misc_view.in_flight.set(false);
        self.misc_view.needs_refresh.set(needs_locks);
        self.misc_view.clear_core();
        self.misc_view.show_live_snapshot(snapshot);
        self.update_control_sensitivity();
        if needs_locks {
            self.refresh_misc_after_stop();
        }
    }

    pub fn show_misc_core_snapshot(&self, generation: u64, snapshot: CoreDumpSnapshot) {
        if generation != self.misc_refresh_generation.get() {
            self.finish_stale_misc_refresh();
            return;
        }
        self.misc_view.in_flight.set(false);
        self.misc_view.needs_refresh.set(false);
        self.misc_view.clear();
        self.misc_view.show_core(snapshot);
        self.update_control_sensitivity();
    }

    pub fn show_call_abi_for_refresh(&self, generation: u64, frames: &[StackFrame]) {
        if !self.is_stop_refresh_current(generation) {
            return;
        }
        self.misc_view.show_call_abi(crate::misc::call_abi_snapshot(
            self.target_architecture(),
            self.target_pointer_bits(),
            self.selected_frame_level.get(),
            frames,
        ));
        self.refresh_call_abi_transfer();
    }

    pub(super) fn refresh_call_abi_transfer(&self) {
        let generation = self.current_stop_refresh_generation();
        if self.latest_registers_generation.get() != Some(generation)
            || self.call_abi_instruction_generation.get() != Some(generation)
        {
            return;
        }
        let Some(context) = self.call_abi_instruction.borrow().clone() else {
            return;
        };
        let architecture = self.target_architecture();
        let mut phase = call_abi_phase(&context.current, context.previous.as_ref(), architecture);
        if let Some(resolution) = context.target_resolution.as_ref() {
            replace_call_abi_phase_target(&mut phase, resolution);
        }
        let registers = self.latest_registers.borrow();
        let mut transfer = crate::misc::call_abi_transfer(architecture, phase, &registers);
        let address = full_address(&context.current.address, self.target_pointer_bits());
        transfer.context = format!("{}  ·  instruction {address}", transfer.context);
        let transfer_context = transfer.context.clone();
        self.misc_view.show_call_abi_transfer(transfer);
        self.misc_view
            .call_abi_context
            .set_tooltip_text(Some(&format!(
                "{transfer_context}\n{address}  {}",
                context.current.text
            )));
    }

    pub(crate) fn take_call_abi_target_request(&self) -> Option<CallAbiTargetRequest> {
        let generation = self.current_stop_refresh_generation();
        if self.call_abi_instruction_generation.get() != Some(generation) {
            return None;
        }
        let architecture = self.target_architecture();
        let mut context = self.call_abi_instruction.borrow_mut();
        let context = context.as_mut()?;
        let phase = call_abi_phase(&context.current, context.previous.as_ref(), architecture);
        let expression = call_abi_phase_target(&phase)?.to_owned();
        if context
            .target_resolution
            .as_ref()
            .is_some_and(|resolution| resolution.expression == expression)
            || context.pending_target.as_deref() == Some(expression.as_str())
        {
            return None;
        }
        context.pending_target = Some(expression.clone());
        Some(CallAbiTargetRequest {
            generation,
            instruction_address: context.current.address.clone(),
            expression,
        })
    }

    pub(crate) fn show_call_abi_target_resolution(
        &self,
        request: &CallAbiTargetRequest,
        display: Option<String>,
    ) {
        if !self.is_stop_refresh_current(request.generation)
            || self.call_abi_instruction_generation.get() != Some(request.generation)
        {
            return;
        }
        let mut context_slot = self.call_abi_instruction.borrow_mut();
        let Some(context) = context_slot.as_mut() else {
            return;
        };
        if !addresses_equal(&context.current.address, &request.instruction_address)
            || context.pending_target.as_deref() != Some(request.expression.as_str())
        {
            return;
        }
        context.pending_target = None;
        context.target_resolution = Some(CallAbiTargetResolution {
            expression: request.expression.clone(),
            display: display.unwrap_or_else(|| request.expression.clone()),
        });
        drop(context_slot);
        self.refresh_call_abi_transfer();
    }

    pub fn show_misc_error(&self, generation: u64, error: &str) {
        if generation != self.misc_refresh_generation.get() {
            self.finish_stale_misc_refresh();
            return;
        }
        self.misc_view.in_flight.set(false);
        // Keep the diagnostic stable while the target is stopped instead of
        // repeatedly probing unsupported remote/core sessions. A new run
        // invalidates the view and triggers another attempt.
        self.misc_view.needs_refresh.set(false);
        self.misc_view.clear();
        self.misc_view.warning.set_text(error);
        self.misc_view.warning.set_visible(true);
        self.misc_view.clear_core();
        self.misc_view.core_warning.set_text(error);
        self.misc_view.core_warning.set_visible(true);
        self.update_control_sensitivity();
    }

    pub fn refresh_misc_after_stop(&self) {
        if self.misc_view.tracking_enabled.get()
            && self.misc_view.needs_refresh.get()
            && self.misc_refresh_allowed()
            && let Some(handler) = self.misc_refresh_handler.borrow().as_ref()
        {
            handler();
        }
    }

    pub fn invalidate_misc_refresh(&self) {
        self.misc_refresh_generation
            .set(self.misc_refresh_generation.get().wrapping_add(1));
        self.misc_view.needs_refresh.set(true);
        self.update_control_sensitivity();
    }

    pub fn misc_refresh_is_current(&self, generation: u64) -> bool {
        generation == self.misc_refresh_generation.get()
    }

    pub fn finish_stale_misc_refresh(&self) {
        self.misc_view.in_flight.set(false);
        self.update_control_sensitivity();
        self.refresh_misc_after_stop();
    }

    pub fn clear_misc_snapshot(&self) {
        self.invalidate_misc_refresh();
        self.misc_view.clear();
        self.misc_view.clear_core();
        self.misc_view.call_abi_summary.set_text("—");
        self.misc_view.call_abi_context.set_text("");
        self.misc_view.call_abi_context.set_tooltip_text(None);
        self.misc_view.call_abi_register_store.remove_all();
        self.misc_view.call_abi_register_empty.set_visible(true);
        self.misc_view.call_abi_contract_store.remove_all();
    }

    fn misc_refresh_allowed(&self) -> bool {
        self.debugger_ready.get()
            && self.inferior_started.get()
            && !self.inferior_running.get()
            && !self.command_pending.get()
    }
}

fn call_abi_phase_target(phase: &CallAbiPhase) -> Option<&str> {
    match phase {
        CallAbiPhase::OutgoingCall { target } | CallAbiPhase::Returned { target } => {
            target.as_deref()
        }
        CallAbiPhase::IncomingEntry { .. } | CallAbiPhase::Returning | CallAbiPhase::Sequential => {
            None
        }
    }
}

fn replace_call_abi_phase_target(phase: &mut CallAbiPhase, resolution: &CallAbiTargetResolution) {
    let target = match phase {
        CallAbiPhase::OutgoingCall { target } | CallAbiPhase::Returned { target } => target,
        CallAbiPhase::IncomingEntry { .. } | CallAbiPhase::Returning | CallAbiPhase::Sequential => {
            return;
        }
    };
    if target.as_deref() == Some(resolution.expression.as_str()) {
        *target = Some(resolution.display.clone());
    }
}
