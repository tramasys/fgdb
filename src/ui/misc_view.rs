use super::*;
use components::build_subtab_navigation;

const MISC_PAGES: [(&str, &str); 8] = [
    ("startup-vectors", "Args / Env"),
    ("auxv", "Auxv"),
    ("call-abi", "Call ABI"),
    ("cfg", "CFG"),
    ("allocator", "Allocator"),
    ("locks", "Locks"),
    ("syscalls", "Syscalls"),
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

struct LocksWidgets {
    root: gtk::Paned,
    summary: gtk::Label,
    note: gtk::Label,
    store: gio::ListStore,
    empty: gtk::Label,
    graph_summary: gtk::Label,
    dependency_store: gio::ListStore,
    graph_empty: gtk::Label,
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

struct AllocatorWidgets {
    root: gtk::Box,
    implementation: gtk::Label,
    basis: gtk::Label,
    bindings: gtk::Label,
    runtimes: gtk::Label,
    frontends: gtk::Label,
    evidence: gtk::Label,
    safety: gtk::Label,
    heap_bytes: gtk::Label,
    anonymous_bytes: gtk::Label,
    mapping_count: gtk::Label,
    store: gio::ListStore,
    empty: gtk::Label,
    inspector: HeapInspectorWidgets,
}

struct HeapInspectorWidgets {
    root: gtk::Box,
    actions: Vec<(gtk::Button, HeapInspectionAction)>,
    expression: gtk::Entry,
    status: gtk::Label,
    command: gtk::Label,
    store: gio::ListStore,
    empty: gtk::Label,
    in_flight: Rc<Cell<Option<HeapInspectionId>>>,
    selection: Rc<AllocatorSelection>,
    backend_selector: gtk::DropDown,
}

#[derive(Default)]
pub(super) struct AllocatorSelection {
    selected: Cell<Option<crate::misc::HeapBackend>>,
    available: RefCell<Vec<crate::misc::HeapBackend>>,
    manual: Cell<bool>,
    updating: Cell<bool>,
}

impl AllocatorSelection {
    fn update(&self, selector: &gtk::DropDown, snapshot: &AllocatorSnapshot) {
        let manual = self
            .selected
            .get()
            .filter(|backend| self.manual.get() && snapshot.available_backends.contains(backend));

        self.manual.set(manual.is_some());

        let selected = manual
            .or(snapshot.selected_backend)
            .or_else(|| snapshot.available_backends.first().copied());

        self.updating.set(true);

        if *self.available.borrow() != snapshot.available_backends {
            let names = snapshot
                .available_backends
                .iter()
                .map(|backend| backend.name())
                .collect::<Vec<_>>();

            let model = gtk::StringList::new(&names);
            self.available.replace(snapshot.available_backends.clone());
            selector.set_model(Some(&model));
            selector.set_sensitive(!names.is_empty());
        }

        let index = selected.and_then(|backend| {
            snapshot
                .available_backends
                .iter()
                .position(|candidate| *candidate == backend)
        });

        selector.set_selected(index.map_or(gtk::INVALID_LIST_POSITION, |index| index as u32));
        self.updating.set(false);
    }

    fn clear(&self, selector: &gtk::DropDown) {
        self.updating.set(true);
        self.manual.set(false);
        self.selected.set(None);
        self.available.borrow_mut().clear();
        selector.set_selected(gtk::INVALID_LIST_POSITION);
        selector.set_sensitive(false);
        self.updating.set(false);
    }
}

struct HeapTableWidgets {
    root: gtk::Box,
    store: gio::ListStore,
    empty: gtk::Label,
    view: gtk::ColumnView,
    selection: gtk::SingleSelection,
    copy_selected: gtk::Button,
    inspect_selected: gtk::Button,
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

pub(super) fn build_misc_view(theme: &Theme, columns: &ColumnLayouts) -> MiscView {
    let active = Rc::new(Cell::new(false));
    let in_flight = Rc::new(Cell::new(false));
    let needs_refresh = Rc::new(Cell::new(true));
    let allocator_requested = Rc::new(Cell::new(false));
    let allocator_probe_fresh = Rc::new(Cell::new(false));
    let locks_requested = Rc::new(Cell::new(false));
    let root = workspace::ResponsiveBox::new();
    root.set_size_request(0, 0);
    root.add_css_class("panel");
    root.add_css_class("kernel-page");
    root.add_css_class("misc-page");
    let pages = gtk::Stack::new();
    connect_stack_text_selection_cleanup(&pages);
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
    root.bind_navigation(&navigation.root, &navigation.compact_root);
    let startup = build_startup_page(columns);
    pages.add_titled(&startup.root, Some("startup-vectors"), "Args / Env");
    let auxv = build_auxv_page(columns);
    pages.add_titled(&auxv.root, Some("auxv"), "Auxv");
    let call_abi = build_call_abi_page(columns);
    pages.add_titled(&call_abi.root, Some("call-abi"), "Call ABI");
    let cfg = build_cfg_view(theme);
    pages.add_titled(&cfg.root, Some("cfg"), "CFG");
    let allocator = build_allocator_page(columns);
    pages.add_titled(&allocator.root, Some("allocator"), "Allocator");
    let locks = build_locks_page(columns);
    pages.add_titled(&locks.root, Some("locks"), "Locks");
    let syscalls = syscall_view::SyscallView::new(&columns.table(TableId::Syscalls));
    pages.add_titled(&syscalls.root, Some("syscalls"), "Syscalls");
    let core = build_core_page(columns);
    pages.add_titled(&core.root, Some("core-dump"), "Core dump");
    root.append(&pages);

    MiscView {
        root,
        active,
        in_flight,
        needs_refresh,
        pages,
        cfg,
        syscalls,
        allocator_requested,
        allocator_probe_fresh,
        allocator_probe_cache: Rc::new(RefCell::new(None)),
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
        allocator_implementation: allocator.implementation,
        allocator_basis: allocator.basis,
        allocator_bindings: allocator.bindings,
        allocator_runtimes: allocator.runtimes,
        allocator_frontends: allocator.frontends,
        allocator_evidence: allocator.evidence,
        allocator_safety: allocator.safety,
        allocator_heap_bytes: allocator.heap_bytes,
        allocator_anonymous_bytes: allocator.anonymous_bytes,
        allocator_mapping_count: allocator.mapping_count,
        allocator_store: allocator.store,
        allocator_empty: allocator.empty,
        heap_inspector_actions: allocator.inspector.actions,
        heap_inspector_expression: allocator.inspector.expression,
        heap_inspector_status: allocator.inspector.status,
        heap_inspector_command: allocator.inspector.command,
        heap_inspector_store: allocator.inspector.store,
        heap_inspector_empty: allocator.inspector.empty,
        heap_inspector_in_flight: allocator.inspector.in_flight,
        heap_inspector_serial: Cell::new(0),
        heap_inspector_snapshot_stop: Cell::new(None),
        heap_selection: allocator.inspector.selection,
        heap_backend_selector: allocator.inspector.backend_selector,
        lock_summary: locks.summary,
        lock_note: locks.note,
        lock_store: locks.store,
        lock_empty: locks.empty,
        lock_graph_summary: locks.graph_summary,
        lock_dependency_store: locks.dependency_store,
        lock_graph_empty: locks.graph_empty,
        lock_split: locks.root,
        core_summary: core.summary,
        core_warning: core.warning,
        core_note_store: core.note_store,
        core_file_store: core.file_store,
        core_empty: core.empty,
        core_split: core.split,
    }
}

fn build_startup_page(columns: &ColumnLayouts) -> StartupWidgets {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.set_vexpand(true);
    let controls = gtk::Box::new(gtk::Orientation::Vertical, 3);
    controls.add_css_class("misc-startup-controls");
    let (summary_view, summary) = build_startup_summary();

    let search = components::delayed_search_entry("Filter argument, variable, value, or address");

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
        build_arguments_section(columns, Rc::clone(&query));

    let (environment, environment_store, environment_empty, environment_filter) =
        build_environment_section(columns, Rc::clone(&query));

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

fn build_auxv_page(columns: &ColumnLayouts) -> MiscTablePage {
    let page = build_misc_table_page("Auxiliary vector data is unavailable");

    page.note.set_text(
        "Kernel-supplied process-entry values. Pointer interpretations are limited to known mappings.",
    );

    let layout = columns.table(TableId::Auxv);

    layout.append(
        &page.view,
        "entry",
        &misc_column::<AuxvEntry>("ENTRY", 170, |row| row.name.clone()),
    );

    layout.append(
        &page.view,
        "type",
        &misc_column::<AuxvEntry>("TYPE", 72, |row| row.kind.to_string()),
    );

    layout.append(
        &page.view,
        "raw-value",
        &misc_column::<AuxvEntry>("RAW VALUE", 190, |row| format!("0x{:016x}", row.value)),
    );

    layout.append(
        &page.view,
        "interpretation",
        &misc_column::<AuxvEntry>("INTERPRETATION", 420, |row| row.interpretation.clone()),
    );

    page
}

fn build_call_abi_page(columns: &ColumnLayouts) -> CallAbiWidgets {
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

    let layout = columns.table(TableId::CallArguments);

    layout.append(
        &register_view,
        "role",
        &misc_column::<CallAbiRegister>("ROLE", 250, |row| row.role.clone()),
    );

    layout.append(
        &register_view,
        "register",
        &misc_column::<CallAbiRegister>("REGISTER", 120, |row| row.name.clone()),
    );

    layout.append(
        &register_view,
        "value",
        &misc_column::<CallAbiRegister>("VALUE", 420, |row| row.value.clone()),
    );

    let (contract, contract_store, _, contract_view) =
        build_misc_table("Call ABI metadata is unavailable for this target");

    contract.prepend(&section_title("ABI CONTRACT"));

    let layout = columns.table(TableId::CallAbi);

    layout.append(
        &contract_view,
        "aspect",
        &misc_column::<CallAbiFact>("ASPECT", 260, |row| row.aspect.clone()),
    );

    layout.append(
        &contract_view,
        "convention",
        &misc_column::<CallAbiFact>("CONVENTION", 620, |row| row.value.clone()),
    );

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

fn build_allocator_page(columns: &ColumnLayouts) -> AllocatorWidgets {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.set_vexpand(true);
    let views = gtk::Stack::new();
    connect_stack_text_selection_cleanup(&views);
    views.set_vexpand(true);
    views.set_vhomogeneous(false);
    views.set_hhomogeneous(false);
    views.set_transition_type(gtk::StackTransitionType::None);
    let switcher = gtk::StackSwitcher::new();
    switcher.add_css_class("allocator-view-tabs");
    switcher.set_stack(Some(&views));
    switcher.set_hexpand(true);
    root.append(&switcher);
    let summary = gtk::Box::new(gtk::Orientation::Vertical, 0);
    summary.set_vexpand(true);
    let detection = gtk::Box::new(gtk::Orientation::Vertical, components::CONTROL_GAP);
    detection.add_css_class("allocator-detection-card");
    let caption = gtk::Label::new(Some("C ALLOCATOR BINDING"));
    caption.add_css_class("allocator-detection-caption");
    caption.set_halign(gtk::Align::Start);
    let implementation = allocator_value_label("allocator-detection-identity");
    let basis = allocator_value_label("allocator-detection-basis");
    detection.append(&caption);
    detection.append(&implementation);
    detection.append(&basis);
    let runtimes = append_allocator_detail(&detection, "Loaded runtimes", None);

    let frontends = append_allocator_detail(
        &detection,
        "Allocation frontends",
        Some("allocator-frontend-value"),
    );

    let details = gtk::Box::new(gtk::Orientation::Vertical, components::CONTROL_GAP);
    let bindings = append_allocator_detail(&details, "C bindings", None);

    let evidence = append_allocator_detail(&details, "Evidence", Some("allocator-evidence-value"));

    detection.append(&build_disclosure(
        "Detection evidence",
        &details,
        false,
        "allocator-evidence",
    ));
    let safety = allocator_value_label("allocator-detection-safety");
    detection.append(&safety);

    let detection_scroll = gtk::ScrolledWindow::builder()
        .child(&detection)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(true)
        .min_content_height(100)
        .overlay_scrolling(false)
        .build();

    configure_misc_scroller(&detection_scroll);
    summary.append(&detection_scroll);

    let metrics = gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .homogeneous(true)
        .min_children_per_line(1)
        .max_children_per_line(3)
        .column_spacing(1)
        .row_spacing(1)
        .build();

    metrics.add_css_class("allocator-metrics");
    let heap_bytes = append_allocator_metric(&metrics, "BRK HEAP");
    let anonymous_bytes = append_allocator_metric(&metrics, "ANONYMOUS WRITABLE");
    let mapping_count = append_allocator_metric(&metrics, "RELEVANT MAPPINGS");
    summary.append(&metrics);

    let (mappings, store, empty, view) =
        build_misc_table("No allocator-related mappings are available");

    mappings.prepend(&section_title("ALLOCATOR-RELATED MAPPINGS"));

    let layout = columns.table(TableId::AllocatorMappings);

    layout.append(
        &view,
        "address-range",
        &misc_column::<AllocatorRegion>("ADDRESS RANGE", 270, |row| {
            format!("0x{:x}-0x{:x}", row.start, row.end)
        }),
    );

    layout.append(
        &view,
        "size",
        &misc_column::<AllocatorRegion>("SIZE", 95, |row| crate::kernel::format_bytes(row.size())),
    );

    layout.append(
        &view,
        "perm",
        &misc_column::<AllocatorRegion>("PERM", 65, |row| row.permissions.clone()),
    );

    layout.append(
        &view,
        "role",
        &misc_column::<AllocatorRegion>("ROLE", 150, |row| row.role.clone()),
    );

    layout.append(
        &view,
        "backing",
        &misc_column::<AllocatorRegion>("BACKING", 180, |row| row.path.clone()),
    );

    summary.append(&mappings);
    views.add_titled(&summary, Some("detection"), "Detection");
    let inspector = build_heap_inspector(columns);
    views.add_titled(&inspector.root, Some("structures"), "Heap structures");
    root.append(&views);

    AllocatorWidgets {
        root,
        implementation,
        basis,
        bindings,
        runtimes,
        frontends,
        evidence,
        safety,
        heap_bytes,
        anonymous_bytes,
        mapping_count,
        store,
        empty,
        inspector,
    }
}

fn build_heap_inspector(columns: &ColumnLayouts) -> HeapInspectorWidgets {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.set_vexpand(true);
    let controls = gtk::Box::new(gtk::Orientation::Vertical, components::CONTROL_GAP);
    controls.add_css_class("heap-inspector-controls");
    let actions = RefCell::new(Vec::new());
    let selection = Rc::new(AllocatorSelection::default());
    let backend_selector = gtk::DropDown::from_strings(&["Awaiting allocator detection"]);
    backend_selector.set_hexpand(true);
    backend_selector.set_sensitive(false);
    backend_selector.set_tooltip_text(Some(
        "Choose a detected runtime. A Rust global allocator may differ from the C malloc binding",
    ));

    let backend_row = gtk::Box::new(gtk::Orientation::Horizontal, components::CONTROL_GAP);
    let backend_label = gtk::Label::new(Some("Runtime"));
    backend_label.add_css_class("heap-inspector-group-title");
    backend_row.append(&backend_label);
    backend_row.append(&backend_selector);
    let inspect = heap_action_button("Read state", HeapInspectionAction::Backend, &actions);
    inspect.set_tooltip_text(Some(
        "Read a bounded snapshot without executing allocator code",
    ));
    backend_row.append(&inspect);
    controls.append(&backend_row);

    let note = gtk::Label::new(Some(
        "Pause the target to detect allocator runtimes. No allocator functions are called.",
    ));

    note.add_css_class("heap-inspector-note");
    note.set_halign(gtk::Align::Fill);
    note.set_xalign(0.0);
    note.set_wrap(true);
    controls.append(&note);
    let glibc_controls = gtk::Box::new(gtk::Orientation::Vertical, components::CONTROL_GAP);

    let structures = heap_action_group(
        "GLIBC STRUCTURES",
        &[
            ("Arenas", HeapInspectionAction::Arenas),
            ("Arena detail", HeapInspectionAction::Arena),
            ("Top chunk", HeapInspectionAction::Top),
            ("Chunks", HeapInspectionAction::Chunks),
            ("Parsed heap", HeapInspectionAction::Parsed),
        ],
        &actions,
    );

    glibc_controls.append(&structures);

    let bins = heap_action_group(
        "FREE LISTS / BINS",
        &[
            ("Compact", HeapInspectionAction::CompactBins),
            ("All", HeapInspectionAction::AllBins),
            ("Tcache", HeapInspectionAction::TcacheBins),
            ("Fast", HeapInspectionAction::FastBins),
            ("Unsorted", HeapInspectionAction::UnsortedBin),
            ("Small", HeapInspectionAction::SmallBins),
            ("Large", HeapInspectionAction::LargeBins),
        ],
        &actions,
    );

    glibc_controls.append(&bins);
    let expression_group = gtk::Box::new(gtk::Orientation::Vertical, components::CONTROL_GAP);
    let expression_title = gtk::Label::new(Some("TARGETED INSPECTION"));
    expression_title.add_css_class("heap-inspector-group-title");
    expression_title.set_halign(gtk::Align::Start);
    expression_group.append(&expression_title);

    let expression = gtk::Entry::builder()
        .placeholder_text("chunk address or side-effect-free GDB expression")
        .hexpand(true)
        .build();

    expression.add_css_class("heap-inspector-expression");

    expression.set_tooltip_text(Some(
        "Inspect the allocation containing this user pointer or chunk address",
    ));

    let targeted_actions = gtk::Box::new(gtk::Orientation::Horizontal, components::CONTROL_GAP);

    let chunk = heap_action_button("Inspect chunk", HeapInspectionAction::Chunk, &actions);
    chunk.set_hexpand(false);
    targeted_actions.append(&expression);
    targeted_actions.append(&chunk);
    expression_group.append(&targeted_actions);
    glibc_controls.append(&expression_group);
    glibc_controls.set_visible(false);
    controls.append(&glibc_controls);

    let selected_backend = Rc::clone(&selection);
    let action_buttons = actions.borrow().clone();

    backend_selector.connect_selected_item_notify(move |selector| {
        let selected = selected_backend
            .available
            .borrow()
            .get(selector.selected() as usize)
            .copied();
        selected_backend.selected.set(selected);

        if !selected_backend.updating.get() {
            selected_backend.manual.set(true);
        }

        let is_glibc = selected == Some(crate::misc::HeapBackend::Glibc);
        glibc_controls.set_visible(is_glibc);

        for (button, action) in &action_buttons {
            button.set_visible(*action == HeapInspectionAction::Backend || is_glibc);
        }

        note.set_text(
            selected.map_or("No supported allocator runtime was detected", |backend| {
                backend.scope()
            }),
        );
    });
    root.append(&controls);
    let result_header = gtk::Box::new(gtk::Orientation::Vertical, 1);
    result_header.add_css_class("heap-inspector-result-header");
    let command = gtk::Label::new(Some("No heap structure query has run"));
    command.add_css_class("heap-inspector-command");
    command.set_halign(gtk::Align::Fill);
    command.set_xalign(0.0);
    command.set_ellipsize(pango::EllipsizeMode::Middle);
    let status = gtk::Label::new(Some("Choose a heap view above while the target is paused"));
    status.add_css_class("heap-inspector-status");
    status.set_halign(gtk::Align::Fill);
    status.set_xalign(0.0);
    status.set_wrap(true);
    result_header.append(&command);
    result_header.append(&status);
    root.append(&result_header);
    let table = build_heap_inspection_table();

    let layout = columns.table(TableId::Heap);

    layout.append(
        &table.view,
        "structure",
        &heap_inspection_column("STRUCTURE", 100, HeapCellKind::Structure, |row| &row.kind),
    );

    layout.append(
        &table.view,
        "address-index",
        &heap_inspection_column("ADDRESS / INDEX", 160, HeapCellKind::Location, |row| {
            &row.location
        }),
    );

    layout.append(
        &table.view,
        "size-count",
        &heap_inspection_column("SIZE / COUNT", 135, HeapCellKind::Metric, |row| &row.metric),
    );

    layout.append(
        &table.view,
        "state-meaning",
        &heap_inspection_column("STATE / MEANING", 150, HeapCellKind::State, |row| {
            &row.state
        }),
    );

    layout.append(
        &table.view,
        "details-links",
        &heap_inspection_column("DETAILS / LINKS", 180, HeapCellKind::Details, |row| {
            &row.details
        }),
    );

    connect_heap_table_interactions(&table, &expression, &chunk);
    root.append(&table.root);

    HeapInspectorWidgets {
        root,
        actions: actions.into_inner(),
        expression,
        status,
        command,
        store: table.store,
        empty: table.empty,
        in_flight: Rc::new(Cell::new(None)),
        selection,
        backend_selector,
    }
}

fn build_heap_inspection_table() -> HeapTableWidgets {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.set_vexpand(true);
    let controls = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    controls.add_css_class("heap-table-controls");

    let search = components::delayed_search_entry("Filter structures, addresses, states, or links");
    search.set_hexpand(true);

    search.add_css_class("kernel-change-search");
    search.add_css_class("kernel-table-search");
    search.set_tooltip_text(Some("Filter the current heap result"));
    let copy_selected = gtk::Button::with_label("Copy row");
    copy_selected.add_css_class("inline-action");
    copy_selected.set_tooltip_text(Some("Copy every field from the selected row"));
    copy_selected.set_sensitive(false);
    let inspect_selected = gtk::Button::with_label("Inspect chunk");
    inspect_selected.add_css_class("inline-action");

    inspect_selected.set_tooltip_text(Some(
        "Inspect the selected chunk, or the first chunk in an occupied bin",
    ));

    inspect_selected.set_sensitive(false);
    controls.append(&search);
    controls.append(&copy_selected);
    controls.append(&inspect_selected);
    root.append(&controls);
    let query = Rc::new(RefCell::new(String::new()));
    let query_for_filter = Rc::clone(&query);

    let filter = gtk::CustomFilter::new(move |object| {
        object
            .downcast_ref::<glib::BoxedAnyObject>()
            .is_some_and(|row| {
                heap_row_matches(
                    &row.borrow::<HeapInspectionRow>(),
                    &query_for_filter.borrow(),
                )
            })
    });

    let store = gio::ListStore::new::<glib::BoxedAnyObject>();
    let filtered = gtk::FilterListModel::new(Some(store.clone()), Some(filter.clone()));
    let selection = gtk::SingleSelection::new(Some(filtered.clone()));
    selection.set_autoselect(false);
    selection.set_can_unselect(true);
    let view = components::column_view(selection.clone());
    view.add_css_class("debug-table");
    view.add_css_class("misc-data-table");
    view.add_css_class("heap-inspector-table");
    view.set_vexpand(true);
    view.set_reorderable(true);
    view.set_single_click_activate(false);
    let empty = empty_label("No heap rows match the current view or filter");
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
    root.append(&empty);
    root.append(&scrolled);

    HeapTableWidgets {
        root,
        store,
        empty,
        view,
        selection,
        copy_selected,
        inspect_selected,
    }
}

fn heap_row_matches(row: &HeapInspectionRow, query: &str) -> bool {
    query.is_empty()
        || [
            row.kind.as_str(),
            row.location.as_str(),
            row.metric.as_str(),
            row.state.as_str(),
            row.details.as_str(),
        ]
        .into_iter()
        .any(|field| contains_case_insensitive(field, query))
}

fn contains_case_insensitive(value: &str, lowercase_query: &str) -> bool {
    if lowercase_query.is_ascii() {
        let query = lowercase_query.as_bytes();

        return query.is_empty()
            || value
                .as_bytes()
                .windows(query.len())
                .any(|window| window.eq_ignore_ascii_case(query));
    }

    value.to_lowercase().contains(lowercase_query)
}

fn selected_heap_row(selection: &gtk::SingleSelection) -> Option<glib::BoxedAnyObject> {
    selection
        .selected_item()
        .and_downcast::<glib::BoxedAnyObject>()
}

fn selected_heap_inspect_address(selection: &gtk::SingleSelection) -> Option<u64> {
    selected_heap_row(selection).and_then(|row| row.borrow::<HeapInspectionRow>().inspect_address)
}

fn update_heap_row_actions(
    selection: &gtk::SingleSelection,
    copy: &gtk::Button,
    inspect: &gtk::Button,
    targeted_inspect: &gtk::Button,
) {
    let selected = selected_heap_row(selection);
    copy.set_sensitive(selected.is_some());

    inspect.set_sensitive(
        targeted_inspect.is_sensitive()
            && targeted_inspect.is_visible()
            && selected
                .is_some_and(|row| row.borrow::<HeapInspectionRow>().inspect_address.is_some()),
    );
}

fn activate_selected_heap_chunk(
    selection: &gtk::SingleSelection,
    expression: &gtk::Entry,
    targeted_inspect: &gtk::Button,
) {
    let Some(address) = selected_heap_inspect_address(selection) else {
        return;
    };

    if targeted_inspect.is_sensitive() && targeted_inspect.is_visible() {
        expression.set_text(&format_address(Some(address)));
        targeted_inspect.emit_clicked();
    }
}

fn connect_heap_table_interactions(
    table: &HeapTableWidgets,
    expression: &gtk::Entry,
    targeted_inspect: &gtk::Button,
) {
    let selection = table.selection.clone();
    let copy = table.copy_selected.clone();
    let inspect = table.inspect_selected.clone();
    let targeted = targeted_inspect.clone();

    table.selection.connect_selected_item_notify(move |_| {
        update_heap_row_actions(&selection, &copy, &inspect, &targeted);
    });

    let selection = table.selection.clone();
    let copy = table.copy_selected.clone();
    let inspect = table.inspect_selected.clone();
    let targeted = targeted_inspect.clone();

    targeted_inspect.connect_sensitive_notify(move |_| {
        update_heap_row_actions(&selection, &copy, &inspect, &targeted);
    });

    let selection = table.selection.clone();
    let copy = table.copy_selected.clone();
    let inspect = table.inspect_selected.clone();
    let targeted = targeted_inspect.clone();

    targeted_inspect.connect_visible_notify(move |_| {
        inspect.set_visible(targeted.is_visible());
        update_heap_row_actions(&selection, &copy, &inspect, &targeted);
    });

    let selection = table.selection.clone();

    table.copy_selected.connect_clicked(move |_| {
        let Some(row) = selected_heap_row(&selection) else {
            return;
        };

        let row = row.borrow::<HeapInspectionRow>();

        let text = [
            row.kind.as_str(),
            row.location.as_str(),
            row.metric.as_str(),
            row.state.as_str(),
            row.details.as_str(),
        ]
        .into_iter()
        .filter(|field| !field.is_empty())
        .collect::<Vec<&str>>()
        .join("\t");

        if let Some(display) = gtk::gdk::Display::default() {
            display.clipboard().set_text(&text);
        }
    });

    let selection = table.selection.clone();
    let expression_for_button = expression.clone();
    let targeted = targeted_inspect.clone();

    table.inspect_selected.connect_clicked(move |_| {
        activate_selected_heap_chunk(&selection, &expression_for_button, &targeted);
    });

    let selection = table.selection.clone();
    let expression = expression.clone();
    let targeted = targeted_inspect.clone();

    table.view.connect_activate(move |_, _| {
        activate_selected_heap_chunk(&selection, &expression, &targeted);
    });
}

#[derive(Clone, Copy)]
enum HeapCellKind {
    Structure,
    Location,
    Metric,
    State,
    Details,
}

fn heap_inspection_column(
    title: &str,
    width: i32,
    cell_kind: HeapCellKind,
    value: impl Fn(&HeapInspectionRow) -> &str + Copy + 'static,
) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();

    factory.connect_setup(move |_, object| {
        let Some(item) = object.downcast_ref::<gtk::ListItem>() else {
            return;
        };

        let label = gtk::Label::new(None);
        label.add_css_class("debug-table-cell");
        label.add_css_class("heap-inspector-cell");

        label.add_css_class(match cell_kind {
            HeapCellKind::Structure => "heap-inspector-structure-cell",
            HeapCellKind::Location => "heap-inspector-location-cell",
            HeapCellKind::Metric => "heap-inspector-metric-cell",
            HeapCellKind::State => "heap-inspector-state-cell",
            HeapCellKind::Details => "heap-inspector-details-cell",
        });

        label.set_halign(gtk::Align::Fill);
        label.set_xalign(0.0);

        if matches!(cell_kind, HeapCellKind::State | HeapCellKind::Details) {
            label.set_wrap(true);
            label.set_wrap_mode(pango::WrapMode::WordChar);
            label.set_lines(3);
            label.set_ellipsize(pango::EllipsizeMode::End);
        } else {
            label.set_ellipsize(pango::EllipsizeMode::Middle);
        }

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

        for class in [
            "heap-inspector-section-cell",
            "heap-inspector-error-cell",
            "heap-inspector-warning-cell",
            "heap-state-active",
            "heap-state-idle",
            "heap-state-free",
            "heap-state-special",
        ] {
            label.remove_css_class(class);
        }

        clear_label_selection(&label);
        let row = data.borrow::<HeapInspectionRow>();
        let text = value(&row);
        label.set_text(text);
        label.set_tooltip_text(Some(text));

        if row.kind == "Section" {
            label.add_css_class("heap-inspector-section-cell");
        } else if row.kind == "Error" {
            label.add_css_class("heap-inspector-error-cell");
        } else if row.state == "warning" {
            label.add_css_class("heap-inspector-warning-cell");
        }

        if matches!(cell_kind, HeapCellKind::State) {
            match row.state.to_ascii_lowercase().as_str() {
                "used" | "occupied" | "main" | "native" | "inside chunk" => {
                    label.add_css_class("heap-state-active");
                }
                "empty" | "thread" => label.add_css_class("heap-state-idle"),
                "freed" => label.add_css_class("heap-state-free"),
                "top" | "mapped" => label.add_css_class("heap-state-special"),
                _ => {}
            }
        }
    });

    components::table_column(title, width, factory)
}

fn heap_action_group(
    title: &str,
    definitions: &[(&str, HeapInspectionAction)],
    actions: &RefCell<Vec<(gtk::Button, HeapInspectionAction)>>,
) -> gtk::Box {
    let group = gtk::Box::new(gtk::Orientation::Vertical, 2);
    let title = gtk::Label::new(Some(title));
    title.add_css_class("heap-inspector-group-title");
    title.set_halign(gtk::Align::Start);
    group.append(&title);

    let buttons = gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .homogeneous(false)
        .min_children_per_line(1)
        .max_children_per_line(8)
        .column_spacing(3)
        .row_spacing(3)
        .build();

    buttons.add_css_class("heap-inspector-actions");

    for (label, action) in definitions {
        buttons.insert(&heap_action_button(label, *action, actions), -1);
    }

    group.append(&buttons);

    group
}

fn heap_action_button(
    label: &str,
    action: HeapInspectionAction,
    actions: &RefCell<Vec<(gtk::Button, HeapInspectionAction)>>,
) -> gtk::Button {
    let button = gtk::Button::with_label(label);
    button.add_css_class("heap-inspector-action");

    // Native heap inspection does not depend on command discovery. Keep every
    // supported action present and only gate interaction on the inferior state.
    button.set_visible(true);
    button.set_sensitive(false);
    actions.borrow_mut().push((button.clone(), action));

    button
}

fn allocator_value_label(class: &str) -> gtk::Label {
    let label = gtk::Label::new(Some("-"));
    label.add_css_class(class);
    label.set_halign(gtk::Align::Fill);
    label.set_xalign(0.0);
    label.set_wrap(true);
    label.set_wrap_mode(pango::WrapMode::WordChar);
    enable_stable_text_selection(&label);

    label
}

fn append_allocator_detail(
    parent: &gtk::Box,
    title: &str,
    value_class: Option<&str>,
) -> gtk::Label {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, components::CONTENT_INSET);
    row.add_css_class("allocator-detail");
    let key = gtk::Label::new(Some(title));
    key.add_css_class("allocator-detail-key");
    key.set_halign(gtk::Align::Start);
    key.set_valign(gtk::Align::Start);
    key.set_width_chars(20);
    key.set_xalign(0.0);
    let value = allocator_value_label("allocator-detail-value");
    value.set_hexpand(true);

    if let Some(value_class) = value_class {
        value.add_css_class(value_class);
    }

    row.append(&key);
    row.append(&value);
    parent.append(&row);

    value
}

fn append_allocator_metric(metrics: &gtk::FlowBox, title: &str) -> gtk::Label {
    let cell = gtk::Box::new(gtk::Orientation::Vertical, 0);
    cell.add_css_class("allocator-metric-cell");
    let key = gtk::Label::new(Some(title));
    key.add_css_class("allocator-metric-key");
    key.set_halign(gtk::Align::Start);
    let value = allocator_value_label("allocator-metric-value");
    cell.append(&key);
    cell.append(&value);
    metrics.insert(&cell, -1);

    value
}

fn set_allocator_value(label: &gtk::Label, value: &str) {
    if label.text() != value {
        label.set_text(value);
        label.set_tooltip_text(Some(value));
    }
}

fn set_allocator_class(label: &gtk::Label, class: &str, enabled: bool) {
    if label.has_css_class(class) == enabled {
        return;
    }

    if enabled {
        label.add_css_class(class);
    } else {
        label.remove_css_class(class);
    }
}

fn build_locks_page(columns: &ColumnLayouts) -> LocksWidgets {
    let page = build_misc_table_page("Open this tab to inspect kernel-visible futex waits");
    page.note.set_text(LOCKS_NOTE);

    let layout = columns.table(TableId::LockWaits);

    layout.append(
        &page.view,
        "tid",
        &misc_column::<LockWait>("TID", 90, |row| row.tid.to_string()),
    );

    layout.append(
        &page.view,
        "thread",
        &misc_column::<LockWait>("THREAD", 180, |row| row.thread.clone()),
    );

    layout.append(
        &page.view,
        "state",
        &misc_column::<LockWait>("STATE", 150, |row| row.state.clone()),
    );

    layout.append(
        &page.view,
        "wait-address",
        &misc_column::<LockWait>("WAIT ADDRESS", 190, |row| {
            row.address
                .map_or_else(|| String::from("-"), |value| format!("0x{value:016x}"))
        }),
    );

    layout.append(
        &page.view,
        "operation",
        &misc_column::<LockWait>("OPERATION", 190, |row| row.operation.clone()),
    );

    layout.append(
        &page.view,
        "expected-count",
        &misc_column::<LockWait>("EXPECTED / COUNT", 140, |row| {
            row.expected
                .map_or_else(|| String::from("-"), |value| format!("0x{value:x}"))
        }),
    );

    layout.append(
        &page.view,
        "details",
        &misc_column::<LockWait>("DETAILS", 360, |row| row.details.clone()),
    );

    let graph = gtk::Box::new(gtk::Orientation::Vertical, 4);
    graph.add_css_class("lock-graph");
    graph.append(&section_title("WAIT-FOR GRAPH"));
    let graph_summary = misc_note_label();

    graph_summary
        .set_text("Owner edges appear only when the live futex word identifies a scanned thread");

    graph.append(&graph_summary);
    let dependency_store = gio::ListStore::new::<glib::BoxedAnyObject>();
    let dependency_selection = gtk::NoSelection::new(Some(dependency_store.clone()));
    let dependency_view = components::column_view(dependency_selection);
    dependency_view.add_css_class("debug-table");
    dependency_view.set_vexpand(true);
    dependency_view.set_reorderable(true);

    let layout = columns.table(TableId::LockDependencies);

    layout.append(
        &dependency_view,
        "waiter",
        &misc_column::<LockDependency>("WAITER", 200, |row| {
            format!("{}  {}", row.waiter_tid, row.waiter)
        }),
    );

    layout.append(
        &dependency_view,
        "relation",
        &misc_column::<LockDependency>("RELATION", 110, |_| String::from("waits for")),
    );

    layout.append(
        &dependency_view,
        "owner",
        &misc_column::<LockDependency>("OWNER", 200, |row| {
            format!("{}  {}", row.owner_tid, row.owner)
        }),
    );

    layout.append(
        &dependency_view,
        "address",
        &misc_column::<LockDependency>("ADDRESS", 190, |row| format!("0x{:016x}", row.address)),
    );

    layout.append(
        &dependency_view,
        "futex-word",
        &misc_column::<LockDependency>("FUTEX WORD", 130, |row| {
            format!("0x{:08x}", row.futex_value)
        }),
    );

    let graph_empty = empty_label("No reliable thread-owner edges were found");
    graph.append(&graph_empty);

    let dependency_scrolled = gtk::ScrolledWindow::builder()
        .child(&dependency_view)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .build();

    configure_misc_scroller(&dependency_scrolled);
    graph.append(&dependency_scrolled);
    let root = gtk::Paned::new(gtk::Orientation::Vertical);
    root.set_wide_handle(true);
    root.set_shrink_start_child(false);
    root.set_shrink_end_child(false);
    root.set_resize_start_child(true);
    root.set_resize_end_child(true);
    root.set_start_child(Some(&page.root));
    root.set_end_child(Some(&graph));
    root.set_position(390);

    LocksWidgets {
        root,
        summary: page.summary,
        note: page.note,
        store: page.store,
        empty: page.empty,
        graph_summary,
        dependency_store,
        graph_empty,
    }
}

fn build_core_page(columns: &ColumnLayouts) -> CoreWidgets {
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

    let layout = columns.table(TableId::CoreNotes);

    layout.append(
        &note_view,
        "owner",
        &misc_column::<CoreNote>("OWNER", 150, |row| row.owner.clone()),
    );

    layout.append(
        &note_view,
        "type",
        &misc_column::<CoreNote>("TYPE", 210, |row| row.kind.clone()),
    );

    layout.append(
        &note_view,
        "bytes",
        &misc_column::<CoreNote>("BYTES", 110, |row| row.bytes.to_string()),
    );

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

    let layout = columns.table(TableId::CoreMappings);

    layout.append(
        &file_view,
        "address-range",
        &misc_column::<CoreMappedFile>("ADDRESS RANGE", 330, |row| {
            format!("0x{:016x}-0x{:016x}", row.start, row.end)
        }),
    );

    layout.append(
        &file_view,
        "file-offset",
        &misc_column::<CoreMappedFile>("FILE OFFSET", 160, |row| {
            format!("0x{:x}", row.file_offset)
        }),
    );

    layout.append(
        &file_view,
        "path",
        &misc_column::<CoreMappedFile>("PATH", 520, |row| row.path.clone()),
    );

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
    let view = components::column_view(selection);
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

    let search = components::delayed_search_entry(placeholder);

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
    let view = components::column_view(selection);
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
    let label = gtk::Label::new(Some("-"));
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

    components::table_column(title, width, factory)
}

fn build_startup_summary() -> (gtk::Grid, MiscStartupSummary) {
    let summary = gtk::Grid::builder()
        .column_homogeneous(true)
        .column_spacing(1)
        .build();

    summary.add_css_class("misc-startup-summary");
    let argc = append_startup_summary_cell(&summary, "ARGC", 0, 1);
    let env = append_startup_summary_cell(&summary, "ENV", 1, 1);
    let argv = append_startup_summary_cell(&summary, "ARGV RANGE", 2, 2);
    let envp = append_startup_summary_cell(&summary, "ENVP RANGE", 4, 2);
    set_startup_summary_value(&argc, "-");
    set_startup_summary_value(&env, "-");
    set_startup_summary_value(&argv, "-");
    set_startup_summary_value(&envp, "-");

    (
        summary,
        MiscStartupSummary {
            argc,
            argv,
            envp,
            env,
        },
    )
}

fn append_startup_summary_cell(
    summary: &gtk::Grid,
    title: &str,
    column: i32,
    width: i32,
) -> gtk::Label {
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
    summary.attach(&cell, column, 0, width, 1);

    value
}

fn set_startup_summary_value(label: &gtk::Label, value: &str) {
    label.set_text(value);
    label.set_tooltip_text(Some(value));
}

fn build_arguments_section(
    columns: &ColumnLayouts,
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
    let view = components::column_view(selection);
    view.add_css_class("debug-table");
    view.add_css_class("misc-vector-table");
    view.set_vexpand(true);
    view.set_reorderable(true);

    let layout = columns.table(TableId::Arguments);

    layout.append(
        &view,
        "entry",
        &argument_column("ENTRY", 100, |row, label| {
            label.set_text(&argument_label(row.index));
            label.add_css_class("misc-vector-name");
        }),
    );

    layout.append(
        &view,
        "address",
        &argument_column("ADDRESS", 180, |row, label| {
            label.set_text(&format_address(row.address));
            label.add_css_class("kernel-numeric");
        }),
    );

    layout.append(
        &view,
        "bytes",
        &argument_column("BYTES", 70, |row, label| {
            label.set_text(&row.byte_len.to_string());
        }),
    );

    layout.append(
        &view,
        "value",
        &argument_column("VALUE", 420, |row, label| {
            label.set_text(&row.value);
        }),
    );

    let empty = empty_label("No argument entries are available");
    let empty_for_filter = empty.clone();

    filtered.connect_items_changed(move |model, _, _, _| {
        empty_for_filter.set_visible(model.n_items() == 0);
    });

    let scrolled = gtk::ScrolledWindow::builder()
        .child(&view)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .overlay_scrolling(false)
        .build();

    configure_misc_scroller(&scrolled);
    section.append(&empty);
    section.append(&scrolled);

    (section, store, empty, filter)
}

fn build_environment_section(
    columns: &ColumnLayouts,
    query: Rc<RefCell<String>>,
) -> (gtk::Box, gio::ListStore, gtk::Label, gtk::CustomFilter) {
    let section = gtk::Box::new(gtk::Orientation::Vertical, 0);
    section.add_css_class("misc-vector-section");
    section.append(&section_title("ENVP / ENV"));
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
    let view = components::column_view(selection);
    view.add_css_class("debug-table");
    view.add_css_class("misc-vector-table");
    view.set_vexpand(true);
    view.set_reorderable(true);

    let layout = columns.table(TableId::Environment);

    layout.append(
        &view,
        "entry",
        &environment_column("ENTRY", 100, |row, label| {
            label.set_text(&format!("envp[{}]", row.index));
        }),
    );

    layout.append(
        &view,
        "address",
        &environment_column("ADDRESS", 180, |row, label| {
            label.set_text(&format_address(row.address));
            label.add_css_class("kernel-numeric");
        }),
    );

    layout.append(
        &view,
        "bytes",
        &environment_column("BYTES", 70, |row, label| {
            label.set_text(&row.byte_len.to_string());
        }),
    );

    layout.append(
        &view,
        "name",
        &environment_column("NAME", 210, |row, label| {
            label.set_text(&row.name);
            label.add_css_class("misc-vector-name");
        }),
    );

    layout.append(
        &view,
        "value",
        &environment_column("VALUE", 420, |row, label| {
            label.set_text(&row.value);
        }),
    );

    let empty = empty_label("No env entries are available");
    let empty_for_filter = empty.clone();

    filtered.connect_items_changed(move |model, _, _, _| {
        empty_for_filter.set_visible(model.n_items() == 0);
    });

    let scrolled = gtk::ScrolledWindow::builder()
        .child(&view)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .overlay_scrolling(false)
        .build();

    configure_misc_scroller(&scrolled);
    section.append(&empty);
    section.append(&scrolled);

    (section, store, empty, filter)
}

pub(super) fn configure_misc_scroller(scrolled: &gtk::ScrolledWindow) {
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
    bind: impl Fn(&ProcessArgument, &gtk::Label) + Copy + 'static,
) -> gtk::ColumnViewColumn {
    vector_column(title, width, move |data, label| {
        bind(&data.borrow::<ProcessArgument>(), label);
    })
}

fn environment_column(
    title: &str,
    width: i32,
    bind: impl Fn(&ProcessEnvironment, &gtk::Label) + Copy + 'static,
) -> gtk::ColumnViewColumn {
    vector_column(title, width, move |data, label| {
        bind(&data.borrow::<ProcessEnvironment>(), label);
    })
}

fn vector_column(
    title: &str,
    width: i32,
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

    components::table_column(title, width, factory)
}

fn argument_label(index: usize) -> String {
    format!("argv[{index}]")
}

fn format_address(address: Option<u64>) -> String {
    address.map_or_else(|| String::from("-"), |address| format!("0x{address:016x}"))
}

fn format_range(range: Option<(u64, u64)>) -> String {
    range.map_or_else(
        || String::from("range unavailable"),
        |(start, end)| format!("0x{start:016x}-0x{end:016x}"),
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
    view: &MiscView,
    refresh_handler: &Rc<RefCell<Option<MiscRefreshHandler>>>,
) {
    let active = Rc::clone(&view.active);
    let needs_refresh = Rc::clone(&view.needs_refresh);
    let allocator_probe_fresh = Rc::clone(&view.allocator_probe_fresh);
    let pages = view.pages.clone();
    let handler = Rc::clone(refresh_handler);

    workspace::connect_presentation(&view.root, move |now_active| {
        active.set(now_active);

        if now_active
            && pages.visible_child_name().as_deref() == Some("allocator")
            && !allocator_probe_fresh.get()
        {
            needs_refresh.set(true);
        }

        if now_active && needs_refresh.get() {
            let handler = handler.borrow().clone();

            if let Some(handler) = handler {
                handler();
            }
        }
    });

    let active = Rc::clone(&view.active);
    let allocator_requested = Rc::clone(&view.allocator_requested);
    let allocator_probe_fresh = Rc::clone(&view.allocator_probe_fresh);
    let locks_requested = Rc::clone(&view.locks_requested);
    let needs_refresh = Rc::clone(&view.needs_refresh);
    let in_flight = Rc::clone(&view.in_flight);
    let handler = Rc::clone(refresh_handler);

    view.pages.connect_visible_child_name_notify(move |pages| {
        let newly_requested = match pages.visible_child_name().as_deref() {
            Some("allocator") => {
                let first_request = !allocator_requested.replace(true);

                first_request || !allocator_probe_fresh.get()
            }
            Some("locks") => !locks_requested.replace(true),
            _ => false,
        };

        if newly_requested {
            needs_refresh.set(true);

            if active.get() && !in_flight.get() {
                let handler = handler.borrow().clone();

                if let Some(handler) = handler {
                    handler();
                }
            }
        }
    });
}

impl MiscView {
    pub(super) fn set_heap_inspector_sensitive(&self, sensitive: bool, busy: bool) {
        let inspection_in_flight = self.heap_inspector_in_flight.get().is_some();
        let sensitive = sensitive && !inspection_in_flight;

        for (button, _) in &self.heap_inspector_actions {
            // Heap action handlers revalidate the stopped state before doing
            // any work. Keep previously available buttons sensitive across a
            // short run or inspection transition so GTK never repaints every
            // blue action as disabled and enabled again.
            set_transient_execution_sensitive(
                button,
                sensitive && button.is_visible(),
                busy || inspection_in_flight,
            );
        }

        set_execution_sensitive(
            &self.heap_inspector_expression,
            sensitive
                && self.heap_inspector_actions.iter().any(|(button, action)| {
                    *action == HeapInspectionAction::Chunk && button.is_visible()
                }),
            busy,
        );
    }

    fn show_startup(&self, snapshot: ProcessStartupSnapshot) {
        let argument_count = snapshot.arguments.len();
        let environment_count = snapshot.environment.len();
        set_startup_summary_value(&self.summary.argc, &argument_count.to_string());
        set_startup_summary_value(&self.summary.argv, &format_range(snapshot.argument_range));

        set_startup_summary_value(
            &self.summary.envp,
            &format_range(snapshot.environment_range),
        );

        set_startup_summary_value(&self.summary.env, &format!("{environment_count} entries"));
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

        self.heap_selection
            .update(&self.heap_backend_selector, &allocator);

        let basis_class = match allocator.implementation.as_str() {
            "split allocator bindings" => Some("allocator-detection-error"),
            "allocator binding unresolved"
            | "multiple allocator runtimes detected"
            | "conflicting allocator evidence" => Some("allocator-detection-warning"),
            _ => None,
        };

        set_allocator_class(
            &self.allocator_basis,
            "allocator-detection-warning",
            basis_class == Some("allocator-detection-warning"),
        );

        set_allocator_class(
            &self.allocator_basis,
            "allocator-detection-error",
            basis_class == Some("allocator-detection-error"),
        );

        set_allocator_value(&self.allocator_implementation, &allocator.implementation);

        set_allocator_value(&self.allocator_basis, &allocator.detection_basis);

        let bindings = if allocator.default_bindings.is_empty() {
            if allocator.probe_complete {
                String::from("malloc / free could not be resolved by GDB")
            } else {
                String::from("Awaiting a paused target")
            }
        } else {
            allocator
                .default_bindings
                .iter()
                .map(|binding| {
                    let resolution = if binding.indirect {
                        "  [PLT / GOT  owner unproven]"
                    } else {
                        ""
                    };

                    format!(
                        "{}  0x{:x}  →  {}{}",
                        binding.symbol, binding.address, binding.owner, resolution
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        };

        set_allocator_value(&self.allocator_bindings, &bindings);

        let runtimes = if allocator.detected_runtimes.is_empty() {
            String::from("No recognized allocator runtime")
        } else {
            allocator.detected_runtimes.join("  ")
        };

        set_allocator_value(&self.allocator_runtimes, &runtimes);

        let frontends = if allocator.allocation_frontends.is_empty() {
            String::from("No language or managed-runtime allocation frontend detected")
        } else {
            allocator.allocation_frontends.join("  ")
        };

        set_allocator_value(&self.allocator_frontends, &frontends);

        let evidence = if allocator.evidence.is_empty() {
            String::from("No additional allocator-specific symbols or modules")
        } else {
            allocator.evidence.join("\n")
        };

        set_allocator_value(&self.allocator_evidence, &evidence);

        set_allocator_value(
            &self.allocator_safety,
            if allocator.probe_dispatch_failures > 0 {
                "Partial read-only detection. Some optional GDB probes were skipped"
            } else if allocator.probe_complete {
                "Read-only detection cached until the session or loaded modules change. C bindings do not prove a language's global allocator"
            } else {
                "Mapping evidence only. No allocator code was executed"
            },
        );

        set_allocator_class(
            &self.allocator_safety,
            "allocator-safety-warning",
            allocator.probe_dispatch_failures > 0,
        );

        set_allocator_value(
            &self.allocator_heap_bytes,
            &crate::kernel::format_bytes(allocator.heap_bytes),
        );

        set_allocator_value(
            &self.allocator_anonymous_bytes,
            &crate::kernel::format_bytes(allocator.anonymous_writable_bytes),
        );

        set_allocator_value(&self.allocator_mapping_count, &region_count.to_string());
        replace_boxed_store_if_changed(&self.allocator_store, allocator.regions);
        self.allocator_empty.set_visible(region_count == 0);
    }

    fn show_heap_inspection(&self, snapshot: HeapInspectionSnapshot) {
        self.heap_inspector_command
            .set_text(&format!("FGDB  {}", snapshot.command));

        self.heap_inspector_command
            .set_tooltip_text(Some(&snapshot.command));

        self.heap_inspector_status
            .remove_css_class("heap-inspector-error");

        self.heap_inspector_status
            .remove_css_class("heap-inspector-warning");

        if let Some(diagnostic) = snapshot.diagnostic.as_deref() {
            self.heap_inspector_status
                .add_css_class("heap-inspector-error");

            self.heap_inspector_status.set_text(diagnostic);
        } else {
            if snapshot.truncated {
                self.heap_inspector_status
                    .add_css_class("heap-inspector-warning");
            }

            self.heap_inspector_status.set_text(&snapshot.summary);
        }

        let row_count = snapshot.rows.len();
        replace_boxed_store_if_changed(&self.heap_inspector_store, snapshot.rows);
        self.heap_inspector_empty.set_visible(row_count == 0);
    }

    fn show_locks(&self, locks: LockSnapshot) {
        let wait_count = locks.waits.len();
        let dependency_count = locks.dependencies.len();
        let deadlock_count = locks.deadlocks.len();

        let address_count = locks
            .waits
            .iter()
            .filter_map(|wait| wait.address)
            .collect::<HashSet<_>>()
            .len();

        self.lock_summary.set_text(&format!(
            "{} thread{} scanned  {wait_count} kernel-visible waits  {address_count} wait addresses",
            locks.threads_scanned,
            if locks.threads_scanned == 1 { "" } else { "s" }
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

        let graph_summary = if deadlock_count == 0 {
            format!("{dependency_count} reliable wait-for edges  no deadlock cycles detected")
        } else {
            format!(
                "{dependency_count} reliable wait-for edges  {deadlock_count} deadlock cycle(s)\n{}",
                locks
                    .deadlocks
                    .iter()
                    .map(|cycle| cycle.description.as_str())
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        };

        self.lock_graph_summary.set_text(&graph_summary);

        if deadlock_count > 0 {
            self.lock_graph_summary.add_css_class("status-error");
        } else {
            self.lock_graph_summary.remove_css_class("status-error");
        }

        replace_boxed_store_if_changed(&self.lock_dependency_store, locks.dependencies);
        self.lock_graph_empty.set_visible(dependency_count == 0);
    }

    fn show_call_abi(&self, snapshot: CallAbiSnapshot) {
        let current = snapshot.current_frame.as_ref().map_or_else(
            || String::from("no selected frame"),
            |frame| format!("#{} {}", frame.level, frame.function),
        );

        self.call_abi_summary.set_text(&format!(
            "{}  {}  {}-bit pointers  {current}",
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

    fn show_core(&self, mut snapshot: CoreDumpSnapshot) {
        let auxv_count = snapshot.auxv.len();

        self.auxv_summary.set_text(&format!(
            "{auxv_count} auxiliary-vector entries recovered from NT_AUXV"
        ));

        replace_boxed_store_if_changed(&self.auxv_store, std::mem::take(&mut snapshot.auxv));
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
            "{}  {} / {} / {}  {process}{pid}  {signal}  {} threads  {}  {}",
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
            &self.summary.env,
        ] {
            set_startup_summary_value(value, "-");
        }

        self.warning.set_visible(false);
        self.warning.set_text("");
        self.arguments_store.remove_all();
        self.environment_store.remove_all();
        self.arguments_empty.set_visible(true);
        self.environment_empty.set_visible(true);
        self.auxv_summary.set_text("-");
        self.auxv_store.remove_all();
        self.auxv_empty.set_visible(true);

        for value in [
            &self.allocator_implementation,
            &self.allocator_basis,
            &self.allocator_bindings,
            &self.allocator_runtimes,
            &self.allocator_frontends,
            &self.allocator_evidence,
            &self.allocator_safety,
            &self.allocator_heap_bytes,
            &self.allocator_anonymous_bytes,
            &self.allocator_mapping_count,
        ] {
            set_allocator_value(value, "-");
        }

        self.allocator_store.remove_all();
        self.allocator_empty.set_visible(true);
        self.heap_selection.clear(&self.heap_backend_selector);
        self.heap_inspector_in_flight.set(None);
        self.heap_inspector_snapshot_stop.set(None);

        self.heap_inspector_command
            .set_text("No heap structure query has run");

        self.heap_inspector_status
            .set_text("Choose a detected runtime while the target is paused");

        self.heap_inspector_status
            .remove_css_class("heap-inspector-error");

        self.heap_inspector_status
            .remove_css_class("heap-inspector-warning");

        self.heap_inspector_store.remove_all();
        self.heap_inspector_empty.set_visible(true);
        self.lock_summary.set_text("-");
        self.lock_store.remove_all();
        self.lock_empty.set_visible(true);

        self.lock_graph_summary.set_text(
            "Owner edges appear only when the live futex word identifies a scanned thread",
        );

        self.lock_graph_summary.remove_css_class("status-error");
        self.lock_dependency_store.remove_all();
        self.lock_graph_empty.set_visible(true);
    }

    fn clear_core(&self) {
        self.core_summary.set_text("No core dump loaded");
        self.core_warning.set_text("");
        self.core_warning.set_visible(false);
        self.core_note_store.remove_all();
        self.core_file_store.remove_all();
        self.core_empty.set_visible(true);
    }
}

impl Ui {
    pub(crate) fn set_heap_inspection_handler(
        &self,
        handler: impl Fn(HeapInspectionRequest) + 'static,
    ) {
        self.heap_inspection_handler.replace(Some(Rc::new(handler)));
        let weak_ui = Rc::clone(&self.self_weak);

        self.misc_view
            .heap_backend_selector
            .connect_selected_item_notify(move |_| {
                if let Some(ui) = weak_ui.borrow().upgrade() {
                    if let Some(request) = ui.misc_view.heap_inspector_in_flight.get() {
                        ui.finish_heap_inspection(request);
                    }

                    ui.update_control_sensitivity();
                }
            });

        for (button, action) in &self.misc_view.heap_inspector_actions {
            let action = *action;
            let expression = self.misc_view.heap_inspector_expression.clone();
            let callback = Rc::clone(&self.heap_inspection_handler);
            let selection = Rc::clone(&self.misc_view.heap_selection);

            button.connect_clicked(move |_| {
                let Some(callback) = callback.borrow().clone() else {
                    return;
                };

                callback(HeapInspectionRequest {
                    action,
                    expression: expression.text().trim().to_owned(),
                    backend: selection.selected.get(),
                });
            });
        }

        let chunk_button = self
            .misc_view
            .heap_inspector_actions
            .iter()
            .find(|(_, action)| *action == HeapInspectionAction::Chunk)
            .map(|(button, _)| button.clone());

        self.misc_view
            .heap_inspector_expression
            .connect_activate(move |_| {
                if let Some(button) = chunk_button.as_ref().filter(|button| button.is_sensitive()) {
                    button.emit_clicked();
                }
            });
    }

    pub(crate) fn begin_heap_inspection(&self, command: &str) -> Option<HeapInspectionId> {
        if !self.misc_refresh_allowed() || self.misc_view.heap_inspector_in_flight.get().is_some() {
            return None;
        }

        let serial = self.misc_view.heap_inspector_serial.get().wrapping_add(1);
        self.misc_view.heap_inspector_serial.set(serial);

        let generation =
            HeapInspectionId::new(self.model.current_stop_refresh_generation(), serial);
        self.misc_view
            .heap_inspector_in_flight
            .set(Some(generation));

        if self.misc_view.heap_inspector_store.n_items() == 0 {
            self.misc_view
                .heap_inspector_command
                .set_text(&format!("FGDB  {command}"));

            self.misc_view
                .heap_inspector_command
                .set_tooltip_text(Some(command));
        }

        self.misc_view
            .heap_inspector_status
            .set_text(&format!("Reading {command}…"));

        self.misc_view
            .heap_inspector_status
            .remove_css_class("heap-inspector-error");

        self.misc_view
            .heap_inspector_status
            .remove_css_class("heap-inspector-warning");

        self.misc_view.heap_inspector_empty.set_visible(false);
        self.update_control_sensitivity();

        Some(generation)
    }

    pub(crate) fn heap_inspection_is_current(&self, generation: HeapInspectionId) -> bool {
        self.model
            .is_stop_refresh_current(generation.stop_generation)
            && self.misc_view.heap_inspector_in_flight.get() == Some(generation)
            && !self.model.inferior_is_running()
    }

    pub(crate) fn show_heap_inspection(
        &self,
        generation: HeapInspectionId,
        snapshot: HeapInspectionSnapshot,
    ) {
        if !self.heap_inspection_is_current(generation) {
            self.finish_heap_inspection(generation);
            return;
        }

        if snapshot.rows.is_empty()
            && let Some(diagnostic) = snapshot.diagnostic.as_deref()
        {
            self.show_heap_inspection_error(generation, &snapshot.command, diagnostic);
            return;
        }

        self.misc_view.heap_inspector_in_flight.set(None);
        self.misc_view
            .heap_inspector_snapshot_stop
            .set(Some(generation.stop_generation));
        self.misc_view.show_heap_inspection(snapshot);
        self.update_control_sensitivity();
    }

    pub(crate) fn show_heap_inspection_error(
        &self,
        generation: HeapInspectionId,
        command: &str,
        error: &str,
    ) {
        if !self.heap_inspection_is_current(generation) {
            self.finish_heap_inspection(generation);
            return;
        }

        self.misc_view.heap_inspector_in_flight.set(None);

        let has_previous = self.misc_view.heap_inspector_store.n_items() > 0;

        if !has_previous {
            self.misc_view
                .heap_inspector_command
                .set_text(&format!("FGDB  {command}"));

            self.misc_view
                .heap_inspector_command
                .set_tooltip_text(Some(command));
        }

        self.misc_view
            .heap_inspector_status
            .add_css_class("heap-inspector-error");

        self.misc_view
            .heap_inspector_status
            .set_text(&if has_previous {
                format!("{command} failed. Previous snapshot retained. {error}")
            } else {
                error.to_owned()
            });

        self.application_log
            .record(LogLevel::Error, "Heap inspection failed", error);
        self.misc_view
            .heap_inspector_empty
            .set_visible(!has_previous);
        self.update_control_sensitivity();
    }

    pub(crate) fn finish_heap_inspection(&self, generation: HeapInspectionId) {
        if self.misc_view.heap_inspector_in_flight.get() == Some(generation) {
            self.misc_view.heap_inspector_in_flight.set(None);
            self.misc_view.heap_inspector_status.set_text(
                "Inspection cancelled after the stop context changed. Read state to refresh",
            );
            self.update_control_sensitivity();
        }
    }

    pub fn set_misc_refresh_handler(&self, handler: impl Fn() + 'static) {
        self.misc_refresh_handler.replace(Some(Rc::new(handler)));
    }

    pub fn misc_locks_requested(&self) -> bool {
        self.misc_view.locks_requested.get()
    }

    pub fn misc_allocator_requested(&self) -> bool {
        self.misc_view.active.get()
            && self.misc_view.pages.visible_child_name().as_deref() == Some("allocator")
    }

    pub(crate) fn cached_allocator_probe(&self) -> Option<crate::misc::AllocatorProbe> {
        self.misc_view.allocator_probe_cache.borrow().clone()
    }

    pub(crate) fn cache_allocator_probe(&self, probe: crate::misc::AllocatorProbe) {
        if probe.complete {
            self.misc_view.allocator_probe_cache.replace(Some(probe));
            self.misc_view.allocator_probe_fresh.set(true);
        }
    }

    pub(crate) fn invalidate_allocator_probe_cache(&self) {
        self.misc_view.allocator_probe_cache.replace(None);
        self.misc_view.allocator_probe_fresh.set(false);

        if self.misc_view.allocator_requested.get() {
            self.misc_view.needs_refresh.set(true);
        }
    }

    pub fn begin_misc_refresh(&self) -> Option<u64> {
        if !self.misc_refresh_allowed() || self.misc_view.in_flight.get() {
            return None;
        }

        let generation = self.misc_refresh_generation.get().wrapping_add(1);
        self.misc_refresh_generation.set(generation);
        self.misc_view.in_flight.set(true);

        Some(generation)
    }

    pub fn show_misc_snapshot(&self, generation: u64, snapshot: LiveMiscSnapshot) {
        if generation != self.misc_refresh_generation.get() {
            self.finish_stale_misc_refresh();
            return;
        }

        let needs_locks = self.misc_view.locks_requested.get() && snapshot.locks.is_none();

        if snapshot.allocator.probe_complete {
            self.misc_view.allocator_probe_fresh.set(true);
        }

        let needs_allocator =
            self.misc_allocator_requested() && !self.misc_view.allocator_probe_fresh.get();

        self.misc_view.in_flight.set(false);

        self.misc_view
            .needs_refresh
            .set(needs_locks || needs_allocator);

        self.misc_view.clear_core();
        self.misc_view.show_live_snapshot(snapshot);
        self.update_control_sensitivity();

        if needs_locks || needs_allocator {
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
        if !self.model.is_stop_refresh_current(generation) {
            return;
        }

        self.misc_view.show_call_abi(crate::misc::call_abi_snapshot(
            self.target_architecture(),
            self.target_pointer_bits(),
            self.model.selected_frame_level(),
            frames,
        ));

        self.refresh_call_abi_transfer();
    }

    pub(super) fn refresh_call_abi_transfer(&self) {
        let generation = self.model.current_stop_refresh_generation();

        if !self.model.registers_are_current(generation)
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

        let registers = self.model.registers();
        let mut transfer = crate::misc::call_abi_transfer(architecture, phase, &registers);
        let address = full_address(&context.current.address, self.target_pointer_bits());
        transfer.context = format!("{}  instruction {address}", transfer.context);
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
        let generation = self.model.current_stop_refresh_generation();

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
        if !self.model.is_stop_refresh_current(request.generation)
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
        self.application_log
            .record(LogLevel::Error, "Misc snapshot failed", error);
        self.misc_view.warning.set_visible(true);
        self.misc_view.clear_core();
        self.misc_view.core_warning.set_text(error);
        self.misc_view.core_warning.set_visible(true);
        self.update_control_sensitivity();
    }

    pub fn refresh_misc_after_stop(&self) {
        if self.misc_view.active.get()
            && self.misc_view.needs_refresh.get()
            && self.misc_refresh_allowed()
        {
            let handler = self.misc_refresh_handler.borrow().clone();

            if let Some(handler) = handler {
                handler();
            }
        }
    }

    pub fn invalidate_misc_refresh(&self) {
        self.misc_refresh_generation
            .set(self.misc_refresh_generation.get().wrapping_add(1));

        self.misc_view.needs_refresh.set(true);
    }

    pub fn misc_refresh_is_current(&self, generation: u64) -> bool {
        generation == self.misc_refresh_generation.get()
    }

    pub fn finish_stale_misc_refresh(&self) {
        self.misc_view.in_flight.set(false);
        self.refresh_misc_after_stop();
    }

    pub fn clear_misc_snapshot(&self) {
        self.invalidate_misc_refresh();
        self.invalidate_allocator_probe_cache();
        self.misc_view.clear();
        self.misc_view.clear_core();
        self.misc_view.call_abi_summary.set_text("-");
        self.misc_view.call_abi_context.set_text("");
        self.misc_view.call_abi_context.set_tooltip_text(None);
        self.misc_view.call_abi_register_store.remove_all();
        self.misc_view.call_abi_register_empty.set_visible(true);
        self.misc_view.call_abi_contract_store.remove_all();
    }

    fn misc_refresh_allowed(&self) -> bool {
        self.model.execution().ready
            && self.model.inferior_has_started()
            && !self.model.inferior_is_running()
            && !self.model.execution().command_pending
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
