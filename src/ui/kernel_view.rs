use super::*;

const MIN_MAPPING_DELTA_HEIGHT: i32 = 190;
const PRIVATE_CATEGORY_TABLE_HEIGHT: i32 = 205;
const KERNEL_FACT_LABEL_MIN_WIDTH: i32 = 28;
const KERNEL_FACT_LABEL_MAX_WIDTH: i32 = 42;
const KERNEL_PAGE_NAMES: [&str; 11] = [
    "overview",
    "memory",
    "private-memory",
    "changes",
    "mappings",
    "resources",
    "threads",
    "signals",
    "file-descriptors",
    "limits",
    "process-tree",
];
const KERNEL_OVERVIEW_DISCLOSURES: [(&str, &str, bool); 7] = [
    ("PROCESS", "kernel.overview.process", true),
    (
        "MEMORY ACCOUNTING",
        "kernel.overview.memory-accounting",
        false,
    ),
    ("SCHEDULER", "kernel.overview.scheduler", false),
    ("SECURITY", "kernel.overview.security", false),
    ("I/O ACCOUNTING", "kernel.overview.io-accounting", false),
    (
        "NAMESPACES / CGROUPS",
        "kernel.overview.namespaces-cgroups",
        false,
    ),
    ("RUNTIME / ABI", "kernel.overview.runtime-abi", false),
];

#[derive(Clone, Copy)]
enum MappingColumn {
    Address,
    Permissions,
    Size,
    Rss,
    Pss,
    Private,
    PrivateDirty,
    Shared,
    Swap,
    Huge,
    Path,
    FileIdentity,
    Numa,
    Page,
    Flags,
    Anonymous,
    Referenced,
    LazyFree,
    Locked,
}

#[derive(Clone, Copy)]
enum MappingChangeColumn {
    Status,
    Address,
    Permissions,
    Size,
    Rss,
    Pss,
    Private,
    Dirty,
    Referenced,
    Huge,
    Swap,
    Path,
    FileIdentity,
}

#[derive(Clone, Copy)]
enum MemoryColumn {
    Category,
    Mappings,
    Unique,
    UniquePercent,
    PrivateClean,
    PrivateDirty,
    Virtual,
    Rss,
}

#[derive(Clone, Copy)]
enum PrivateMappingColumn {
    Address,
    Permissions,
    Unique,
    UniquePercent,
    PrivateClean,
    PrivateDirty,
    Rss,
    Virtual,
    Path,
    Anonymous,
    Referenced,
    LazyFree,
    Huge,
}

#[derive(Clone, Copy)]
enum DescriptorColumn {
    Number,
    Kind,
    Access,
    Flags,
    Position,
    Target,
    Details,
}

#[derive(Clone, Copy)]
enum LimitColumn {
    Resource,
    Soft,
    Hard,
    Units,
}

#[derive(Clone, Copy)]
enum ThreadColumn {
    Tid,
    Name,
    State,
    Cpu,
    Policy,
    Priority,
    Affinity,
    Wait,
    Syscall,
    Switches,
    Runtime,
    RunqueueWait,
    Timeslices,
}

#[derive(Clone, Copy)]
enum SignalColumn {
    Number,
    Name,
    ProcessPending,
    ThreadPending,
    Blocked,
    Ignored,
    Caught,
}

#[derive(Clone, Copy)]
enum ProcessColumn {
    Pid,
    Parent,
    Relation,
    Name,
    State,
    Threads,
}

pub(super) fn build_kernel_view(bindings: &KernelViewBindings<'_>) -> KernelView {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.set_size_request(-1, 0);
    root.add_css_class("sidebar");
    root.add_css_class("kernel-page");

    let header = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    header.add_css_class("subpanel-header");
    let title = section_title("LINUX PROCESS SNAPSHOT");
    title.set_hexpand(true);
    let status = gtk::Label::new(Some("Start the inferior to inspect procfs"));
    status.add_css_class("kernel-status");
    status.add_css_class("muted");
    status.set_ellipsize(pango::EllipsizeMode::Middle);
    status.set_tooltip_text(Some(
        "Opening Kernel enables asynchronous stop-to-stop tracking. Detailed procfs reads stay off the GTK thread.",
    ));
    let refresh_button = gtk::Button::with_label("Refresh");
    refresh_button.add_css_class("inline-action");
    refresh_button.set_tooltip_text(Some(
        "Read a new process snapshot from procfs. Detailed smaps accounting may take a moment.",
    ));
    refresh_button.set_sensitive(false);
    header.append(&title);
    header.append(&status);
    header.append(&refresh_button);
    root.append(&header);

    let warnings = gtk::Box::new(gtk::Orientation::Vertical, 2);
    warnings.add_css_class("kernel-warnings");
    warnings.set_visible(false);
    root.append(&warnings);

    let pages = gtk::Stack::new();
    pages.set_size_request(-1, 0);
    pages.set_vexpand(true);
    pages.set_vhomogeneous(false);
    pages.set_hhomogeneous(false);
    pages.set_transition_type(gtk::StackTransitionType::None);
    let page_switcher = gtk::StackSwitcher::new();
    page_switcher.add_css_class("kernel-tabs");
    page_switcher.set_stack(Some(&pages));
    let overview_section_handler: KernelSectionHandler = {
        let section_handler = Rc::clone(bindings.section_handler);
        Rc::new(move |section, expanded| {
            let Some(preference_key) = kernel_overview_preference_key(section) else {
                return;
            };
            if let Some(handler) = section_handler.borrow().as_ref() {
                handler(preference_key, expanded);
            }
        })
    };
    let (overview, overview_store) = build_overview(
        kernel_overview_collapsed(bindings.remembered_disclosures),
        Some(overview_section_handler),
    );
    pages.add_titled(&overview, Some("overview"), "Overview");

    let (
        memory,
        private_memory,
        memory_store,
        private_mapping_store,
        memory_summary,
        memory_empty,
        private_mapping_empty,
    ) = build_memory();
    pages.add_titled(&memory, Some("memory"), "Memory");
    pages.add_titled(&private_memory, Some("private-memory"), "Private memory");

    let (changes, change_store, mapping_change_store, mapping_change_count, mapping_changes_empty) =
        build_changes();
    pages.add_titled(&changes, Some("changes"), "Changes");

    let (mappings, mapping_store, mapping_count, mappings_empty) = build_mappings();
    pages.add_titled(&mappings, Some("mappings"), "Maps");

    let (resources, resource_store) = build_overview(HashSet::new(), None);
    pages.add_titled(&resources, Some("resources"), "Resources");

    let (threads, thread_store, thread_count, threads_empty) = build_threads();
    pages.add_titled(&threads, Some("threads"), "Threads");

    let (signals, signal_store, signal_count, signals_empty) = build_signals();
    pages.add_titled(&signals, Some("signals"), "Signals");

    let (descriptors, descriptor_store, descriptor_count, descriptors_empty) = build_descriptors();
    pages.add_titled(&descriptors, Some("file-descriptors"), "FDs");
    let (limits, limit_store, limit_count, limits_empty) = build_limits();
    pages.add_titled(&limits, Some("limits"), "Limits");
    let (processes, process_store, process_count, processes_empty) = build_processes();
    pages.add_titled(&processes, Some("process-tree"), "Tree");
    page_switcher.set_hexpand(true);
    let page_switcher_scroll = gtk::ScrolledWindow::new();
    page_switcher_scroll.add_css_class("kernel-tabs-scroll");
    page_switcher_scroll.set_policy(gtk::PolicyType::External, gtk::PolicyType::Never);
    page_switcher_scroll.set_overlay_scrolling(true);
    page_switcher_scroll.set_propagate_natural_height(true);
    page_switcher_scroll.set_child(Some(&page_switcher));
    page_switcher_scroll.set_hexpand(true);
    let previous_page = gtk::Button::with_label("‹");
    previous_page.add_css_class("kernel-tab-nav-button");
    previous_page.set_tooltip_text(Some("Previous Kernel view"));
    let next_page = gtk::Button::with_label("›");
    next_page.add_css_class("kernel-tab-nav-button");
    next_page.set_tooltip_text(Some("Next Kernel view"));
    let page_navigation = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    page_navigation.add_css_class("kernel-tab-navigation");
    page_navigation.append(&previous_page);
    page_navigation.append(&page_switcher_scroll);
    page_navigation.append(&next_page);
    root.append(&page_navigation);
    root.append(&pages);
    let pages_for_previous = pages.clone();
    previous_page.connect_clicked(move |_| select_relative_kernel_page(&pages_for_previous, -1));
    let pages_for_next = pages.clone();
    next_page.connect_clicked(move |_| select_relative_kernel_page(&pages_for_next, 1));
    let previous_for_page = previous_page.clone();
    let next_for_page = next_page.clone();
    let scroll_for_page = page_switcher_scroll.clone();
    pages.connect_visible_child_notify(move |pages| {
        update_kernel_page_navigation(pages, &previous_for_page, &next_for_page, &scroll_for_page);
        let pages = pages.clone();
        glib::idle_add_local_once(move || {
            clear_label_selections(&pages);
            if let Some(child) = pages.visible_child() {
                child.queue_resize();
                child.queue_allocate();
                child.queue_draw();
            }
            pages.queue_resize();
            pages.queue_allocate();
            pages.queue_draw();
        });
    });
    update_kernel_page_navigation(&pages, &previous_page, &next_page, &page_switcher_scroll);

    let handler = Rc::clone(bindings.refresh_handler);
    refresh_button.connect_clicked(move |_| {
        if let Some(handler) = handler.borrow().as_ref() {
            handler();
        }
    });

    KernelView {
        root,
        active: Rc::new(Cell::new(false)),
        tracking_enabled: Rc::new(Cell::new(false)),
        in_flight: Rc::new(Cell::new(false)),
        needs_refresh: Rc::new(Cell::new(true)),
        refresh_button,
        status,
        warnings,
        previous_snapshot: Rc::new(RefCell::new(None)),
        overview_store,
        resource_store,
        change_store,
        mapping_change_store,
        mapping_change_count,
        mapping_changes_empty,
        changes_split: changes,
        memory_store,
        private_mapping_store,
        memory_summary,
        memory_empty,
        private_mapping_empty,
        thread_store,
        thread_count,
        threads_empty,
        signal_store,
        signal_count,
        signals_empty,
        mapping_store,
        mapping_count,
        mappings_empty,
        descriptor_store,
        descriptor_count,
        descriptors_empty,
        limit_store,
        limit_count,
        limits_empty,
        process_store,
        process_count,
        processes_empty,
    }
}

pub(super) fn connect_kernel_tab_visibility(
    notebook: &gtk::Notebook,
    kernel_page: u32,
    view: &KernelView,
    refresh_handler: &Rc<RefCell<Option<KernelRefreshHandler>>>,
) {
    let active = Rc::clone(&view.active);
    let tracking_enabled = Rc::clone(&view.tracking_enabled);
    let needs_refresh = Rc::clone(&view.needs_refresh);
    let handler = Rc::clone(refresh_handler);
    notebook.connect_switch_page(move |_, _, page| {
        let now_active = page == kernel_page;
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
}

fn select_relative_kernel_page(pages: &gtk::Stack, delta: i32) {
    let current = pages
        .visible_child_name()
        .as_deref()
        .and_then(|name| {
            KERNEL_PAGE_NAMES
                .iter()
                .position(|candidate| *candidate == name)
        })
        .unwrap_or(0);
    let last = KERNEL_PAGE_NAMES.len().saturating_sub(1);
    let next = if delta < 0 {
        current.saturating_sub(delta.unsigned_abs() as usize)
    } else {
        current.saturating_add(delta as usize).min(last)
    };
    pages.set_visible_child_name(KERNEL_PAGE_NAMES[next]);
}

fn update_kernel_page_navigation(
    pages: &gtk::Stack,
    previous: &gtk::Button,
    next: &gtk::Button,
    scroll: &gtk::ScrolledWindow,
) {
    let index = pages
        .visible_child_name()
        .as_deref()
        .and_then(|name| {
            KERNEL_PAGE_NAMES
                .iter()
                .position(|candidate| *candidate == name)
        })
        .unwrap_or(0);
    previous.set_sensitive(index > 0);
    next.set_sensitive(index + 1 < KERNEL_PAGE_NAMES.len());
    let adjustment = scroll.hadjustment();
    glib::idle_add_local_once(move || {
        let range = (adjustment.upper() - adjustment.page_size() - adjustment.lower()).max(0.0);
        let fraction = index as f64 / KERNEL_PAGE_NAMES.len().saturating_sub(1).max(1) as f64;
        adjustment.set_value(adjustment.lower() + range * fraction);
    });
}

fn kernel_overview_preference_key(section: &str) -> Option<&'static str> {
    KERNEL_OVERVIEW_DISCLOSURES
        .iter()
        .find_map(|(label, key, _)| (*label == section).then_some(*key))
}

fn kernel_overview_collapsed(remembered: &HashMap<String, bool>) -> HashSet<String> {
    KERNEL_OVERVIEW_DISCLOSURES
        .iter()
        .filter_map(|(section, key, default_expanded)| {
            let expanded = remembered.get(*key).copied().unwrap_or(*default_expanded);
            (!expanded).then(|| (*section).to_owned())
        })
        .collect()
}

fn build_overview(
    initial_collapsed: HashSet<String>,
    section_handler: Option<KernelSectionHandler>,
) -> (gtk::ScrolledWindow, gio::ListStore) {
    let store = gio::ListStore::new::<glib::BoxedAnyObject>();
    let collapsed = Rc::new(RefCell::new(initial_collapsed));
    let collapsed_for_filter = Rc::clone(&collapsed);
    let filter = gtk::CustomFilter::new(move |object| {
        let Some(data) = object.downcast_ref::<glib::BoxedAnyObject>() else {
            return false;
        };
        let row = data.borrow::<KernelOverviewRow>();
        row.section || !collapsed_for_filter.borrow().contains(&row.section_key)
    });
    let filtered = gtk::FilterListModel::new(Some(store.clone()), Some(filter.clone()));
    let selection = gtk::SingleSelection::new(Some(filtered));
    selection.set_autoselect(false);
    selection.set_can_unselect(true);
    let factory = gtk::SignalListItemFactory::new();
    let collapsed_for_setup = Rc::clone(&collapsed);
    let section_handler_for_setup = section_handler.clone();
    factory.connect_setup(move |_, object| {
        let Some(item) = object.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        row.add_css_class("kernel-fact-row");
        let key = gtk::Label::new(None);
        key.add_css_class("kernel-fact-key");
        key.add_css_class("muted");
        key.set_halign(gtk::Align::Start);
        key.set_width_chars(KERNEL_FACT_LABEL_MIN_WIDTH);
        key.set_max_width_chars(KERNEL_FACT_LABEL_MAX_WIDTH);
        key.set_ellipsize(pango::EllipsizeMode::End);
        let value = gtk::Label::new(None);
        value.add_css_class("kernel-fact-value");
        value.set_halign(gtk::Align::Start);
        value.set_hexpand(true);
        enable_stable_text_selection(&value);
        value.set_ellipsize(pango::EllipsizeMode::None);
        row.append(&key);
        row.append(&value);
        let click = gtk::GestureClick::new();
        click.set_button(gtk::gdk::BUTTON_PRIMARY);
        click.set_propagation_phase(gtk::PropagationPhase::Capture);
        let item_for_click = item.clone();
        let collapsed = Rc::clone(&collapsed_for_setup);
        let filter = filter.clone();
        let section_handler = section_handler_for_setup.clone();
        click.connect_pressed(move |gesture, presses, _, _| {
            if presses != 1 {
                return;
            }
            let Some(data) = item_for_click.item().and_downcast::<glib::BoxedAnyObject>() else {
                return;
            };
            let data = data.borrow::<KernelOverviewRow>();
            if !data.section {
                return;
            }
            let now_collapsed = {
                let mut collapsed = collapsed.borrow_mut();
                if collapsed.remove(&data.section_key) {
                    false
                } else {
                    collapsed.insert(data.section_key.clone());
                    true
                }
            };
            if let Some(handler) = section_handler.as_ref() {
                handler(&data.section_key, !now_collapsed);
            }
            if let Some(row) = item_for_click.child().and_downcast::<gtk::Box>()
                && let Some(key) = row.first_child().and_downcast::<gtk::Label>()
            {
                if now_collapsed {
                    row.remove_css_class("kernel-section-expanded");
                    row.add_css_class("kernel-section-collapsed");
                } else {
                    row.remove_css_class("kernel-section-collapsed");
                    row.add_css_class("kernel-section-expanded");
                }
                key.set_text(&format!(
                    "{} {}",
                    if now_collapsed {
                        DISCLOSURE_COLLAPSED_ICON
                    } else {
                        DISCLOSURE_EXPANDED_ICON
                    },
                    data.label
                ));
                key.set_tooltip_text(Some(if now_collapsed {
                    "Click to expand this section"
                } else {
                    "Click to collapse this section"
                }));
            }
            filter.changed(gtk::FilterChange::Different);
            gesture.set_state(gtk::EventSequenceState::Claimed);
        });
        row.add_controller(click);
        item.set_child(Some(&row));
    });
    let collapsed_for_bind = Rc::clone(&collapsed);
    factory.connect_bind(move |_, object| {
        let Some(item) = object.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let (Some(row), Some(data)) = (
            item.child().and_downcast::<gtk::Box>(),
            item.item().and_downcast::<glib::BoxedAnyObject>(),
        ) else {
            return;
        };
        let data = data.borrow::<KernelOverviewRow>();
        let Some(key) = row.first_child().and_downcast::<gtk::Label>() else {
            return;
        };
        let Some(value) = key.next_sibling().and_downcast::<gtk::Label>() else {
            return;
        };
        clear_label_selection(&value);
        if data.section {
            row.add_css_class("kernel-section-heading");
            let collapsed = collapsed_for_bind.borrow().contains(&data.section_key);
            if collapsed {
                row.remove_css_class("kernel-section-expanded");
                row.add_css_class("kernel-section-collapsed");
            } else {
                row.remove_css_class("kernel-section-collapsed");
                row.add_css_class("kernel-section-expanded");
            }
            key.remove_css_class("muted");
            key.add_css_class("section-title");
            key.set_width_chars(-1);
            key.set_max_width_chars(-1);
            key.set_hexpand(true);
            key.set_ellipsize(pango::EllipsizeMode::None);
            value.set_visible(false);
            row.set_cursor_from_name(Some("pointer"));
        } else {
            row.remove_css_class("kernel-section-heading");
            row.remove_css_class("kernel-section-collapsed");
            row.remove_css_class("kernel-section-expanded");
            key.remove_css_class("section-title");
            key.add_css_class("muted");
            key.set_width_chars(KERNEL_FACT_LABEL_MIN_WIDTH);
            key.set_max_width_chars(KERNEL_FACT_LABEL_MAX_WIDTH);
            key.set_hexpand(false);
            key.set_ellipsize(pango::EllipsizeMode::End);
            value.set_visible(true);
            row.set_cursor_from_name(None);
        }
        if data.section {
            let collapsed = collapsed_for_bind.borrow().contains(&data.section_key);
            key.set_text(&format!(
                "{} {}",
                if collapsed {
                    DISCLOSURE_COLLAPSED_ICON
                } else {
                    DISCLOSURE_EXPANDED_ICON
                },
                data.label
            ));
            key.set_tooltip_text(Some(if collapsed {
                "Click to expand this section"
            } else {
                "Click to collapse this section"
            }));
        } else {
            key.set_text(&data.label);
            key.set_tooltip_text(Some(&data.label));
        }
        value.set_text(&data.value);
        value.set_tooltip_text(Some(&data.value));
    });
    let list = gtk::ListView::new(Some(selection), Some(factory));
    list.add_css_class("kernel-overview-list");
    let scrolled = gtk::ScrolledWindow::builder()
        .child(&list)
        .min_content_height(1)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .build();
    (scrolled, store)
}

fn build_changes() -> (
    gtk::Paned,
    gio::ListStore,
    gio::ListStore,
    gtk::Label,
    gtk::Label,
) {
    let (totals, change_store) = build_overview(HashSet::new(), None);
    let totals_page = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let totals_title = section_title("PROCESS DELTAS SINCE PREVIOUS SNAPSHOT");
    totals_title.add_css_class("kernel-memory-subtitle");
    totals_title.set_halign(gtk::Align::Fill);
    totals_title.set_xalign(0.0);
    totals_page.append(&totals_title);
    totals_page.append(&totals);

    let mappings_page = gtk::Box::new(gtk::Orientation::Vertical, 0);
    // Keep the detailed VMA delta table reachable even when an old persisted
    // divider position placed it at the very bottom of the window. The process
    // totals above are scrollable and can safely yield space first.
    mappings_page.set_size_request(-1, MIN_MAPPING_DELTA_HEIGHT);
    let controls = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    controls.add_css_class("kernel-table-controls");
    controls.add_css_class("kernel-change-controls");
    let title = section_title("MAPPING DELTAS");
    title.set_valign(gtk::Align::Center);
    let search = gtk::SearchEntry::builder()
        .placeholder_text("Filter mapping changes")
        .width_request(210)
        .build();
    search.add_css_class("kernel-table-search");
    search.add_css_class("kernel-change-search");
    search.set_valign(gtk::Align::Center);
    let count = gtk::Label::new(Some("Capture another snapshot to compare mappings"));
    count.add_css_class("muted");
    enable_stable_text_selection(&count);
    count.set_hexpand(true);
    count.set_halign(gtk::Align::End);
    count.set_valign(gtk::Align::Center);
    count.set_ellipsize(pango::EllipsizeMode::Middle);
    controls.append(&title);
    controls.append(&search);
    controls.append(&count);
    mappings_page.append(&controls);

    let store = gio::ListStore::new::<glib::BoxedAnyObject>();
    let query = Rc::new(RefCell::new(String::new()));
    let query_for_filter = Rc::clone(&query);
    let filter = gtk::CustomFilter::new(move |object| {
        let Some(data) = object.downcast_ref::<glib::BoxedAnyObject>() else {
            return false;
        };
        let query = query_for_filter.borrow();
        query.is_empty()
            || mapping_change_search_text(&data.borrow::<KernelMappingChange>()).contains(&*query)
    });
    let filtered = gtk::FilterListModel::new(Some(store.clone()), Some(filter.clone()));
    let selection = gtk::SingleSelection::new(Some(filtered));
    selection.set_autoselect(false);
    selection.set_can_unselect(true);
    let view = gtk::ColumnView::new(Some(selection));
    view.add_css_class("debug-table");
    view.add_css_class("kernel-change-table");
    view.set_vexpand(true);
    view.set_reorderable(true);
    for (title, width, expand, column) in [
        ("STATUS", 115, false, MappingChangeColumn::Status),
        ("ADDRESS", 285, false, MappingChangeColumn::Address),
        ("PERM", 65, false, MappingChangeColumn::Permissions),
        ("Δ VSS", 95, false, MappingChangeColumn::Size),
        ("Δ RSS", 95, false, MappingChangeColumn::Rss),
        ("Δ PSS", 95, false, MappingChangeColumn::Pss),
        ("Δ USS", 95, false, MappingChangeColumn::Private),
        ("Δ DIRTY", 95, false, MappingChangeColumn::Dirty),
        ("Δ REFERENCED", 120, false, MappingChangeColumn::Referenced),
        ("Δ HUGE", 95, false, MappingChangeColumn::Huge),
        ("Δ SWAP", 95, false, MappingChangeColumn::Swap),
        ("BACKING", 420, true, MappingChangeColumn::Path),
        (
            "DEVICE / INODE",
            170,
            false,
            MappingChangeColumn::FileIdentity,
        ),
    ] {
        view.append_column(&mapping_change_column(title, width, expand, column));
    }
    let empty = empty_label("Capture another snapshot to compare mappings");
    empty.add_css_class("kernel-change-empty");
    empty.set_halign(gtk::Align::Center);
    empty.set_valign(gtk::Align::Center);
    empty.set_justify(gtk::Justification::Center);
    empty.set_margin_start(0);
    empty.set_margin_top(26);
    let table_scroll = gtk::ScrolledWindow::builder()
        .child(&view)
        .min_content_height(1)
        .vexpand(true)
        .build();
    let table_overlay = gtk::Overlay::new();
    table_overlay.set_vexpand(true);
    table_overlay.set_child(Some(&table_scroll));
    table_overlay.add_overlay(&empty);
    mappings_page.append(&table_overlay);
    search.connect_search_changed(move |search| {
        *query.borrow_mut() = search.text().trim().to_ascii_lowercase();
        filter.changed(gtk::FilterChange::Different);
    });

    let split = gtk::Paned::new(gtk::Orientation::Vertical);
    split.add_css_class("kernel-changes-split");
    split.set_position(245);
    split.set_wide_handle(false);
    split.set_resize_start_child(false);
    split.set_shrink_start_child(true);
    split.set_shrink_end_child(false);
    split.set_start_child(Some(&totals_page));
    split.set_end_child(Some(&mappings_page));
    split.connect_map(|split| {
        let split = split.clone();
        glib::idle_add_local_once(move || {
            let maximum_top = split
                .height()
                .saturating_sub(MIN_MAPPING_DELTA_HEIGHT)
                .max(split.min_position());
            if split.height() > MIN_MAPPING_DELTA_HEIGHT && split.position() > maximum_top {
                split.set_position(maximum_top);
            }
            split.queue_allocate();
        });
    });
    (split, change_store, store, count, empty)
}

fn build_threads() -> (gtk::Box, gio::ListStore, gtk::Label, gtk::Label) {
    let page = gtk::Box::new(gtk::Orientation::Vertical, 3);
    let count = table_summary(&page);
    let store = gio::ListStore::new::<glib::BoxedAnyObject>();
    let selection = gtk::SingleSelection::new(Some(store.clone()));
    selection.set_autoselect(false);
    selection.set_can_unselect(true);
    let view = gtk::ColumnView::new(Some(selection));
    view.add_css_class("debug-table");
    view.set_vexpand(true);
    view.set_reorderable(true);
    for (title, width, expand, column) in [
        ("TID", 80, false, ThreadColumn::Tid),
        ("NAME", 130, false, ThreadColumn::Name),
        ("STATE", 150, false, ThreadColumn::State),
        ("CPU", 50, false, ThreadColumn::Cpu),
        ("CPU TIME", 110, false, ThreadColumn::Runtime),
        ("RUN-QUEUE WAIT", 125, false, ThreadColumn::RunqueueWait),
        ("SLICES", 75, false, ThreadColumn::Timeslices),
        (
            "ACTIVE SYSCALL / ARGUMENTS",
            360,
            true,
            ThreadColumn::Syscall,
        ),
        ("WAIT CHANNEL", 160, false, ThreadColumn::Wait),
        ("POLICY", 115, false, ThreadColumn::Policy),
        ("PRIORITY", 105, false, ThreadColumn::Priority),
        ("AFFINITY", 100, false, ThreadColumn::Affinity),
        ("CONTEXT SWITCHES", 220, false, ThreadColumn::Switches),
    ] {
        view.append_column(&thread_column(title, width, expand, column));
    }
    let empty = empty_label("No kernel thread information available");
    append_table(&page, &view, &empty);
    (page, store, count, empty)
}

fn build_signals() -> (gtk::Box, gio::ListStore, gtk::Label, gtk::Label) {
    let page = gtk::Box::new(gtk::Orientation::Vertical, 3);
    let controls = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    controls.add_css_class("kernel-table-controls");
    let count = gtk::Label::new(Some("No snapshot"));
    count.add_css_class("muted");
    enable_stable_text_selection(&count);
    count.set_hexpand(true);
    count.set_halign(gtk::Align::Start);
    let active_only = gtk::ToggleButton::with_label("Active only");
    active_only.add_css_class("kernel-signal-filter");
    active_only.set_active(false);
    active_only.set_tooltip_text(Some(
        "Show only signals that are pending, blocked, ignored, or caught",
    ));
    controls.append(&count);
    controls.append(&active_only);
    page.append(&controls);

    let store = gio::ListStore::new::<glib::BoxedAnyObject>();
    let filter_active = Rc::new(Cell::new(false));
    let filter_active_for_filter = Rc::clone(&filter_active);
    let filter = gtk::CustomFilter::new(move |object| {
        let Some(data) = object.downcast_ref::<glib::BoxedAnyObject>() else {
            return false;
        };
        let signal = data.borrow::<KernelSignal>();
        !filter_active_for_filter.get()
            || signal.pending_process
            || signal.pending_threads > 0
            || signal.blocked_threads > 0
            || signal.ignored
            || signal.caught
    });
    let filtered = gtk::FilterListModel::new(Some(store.clone()), Some(filter.clone()));
    let selection = gtk::SingleSelection::new(Some(filtered));
    selection.set_autoselect(false);
    selection.set_can_unselect(true);
    let view = gtk::ColumnView::new(Some(selection));
    view.add_css_class("debug-table");
    view.add_css_class("kernel-signals-table");
    view.set_vexpand(true);
    view.set_reorderable(true);
    for (title, width, expand, column) in [
        ("#", 50, false, SignalColumn::Number),
        ("SIGNAL", 170, true, SignalColumn::Name),
        ("PROCESS PENDING", 145, false, SignalColumn::ProcessPending),
        ("THREAD PENDING", 145, false, SignalColumn::ThreadPending),
        ("BLOCKED THREADS", 145, false, SignalColumn::Blocked),
        ("IGNORED", 90, false, SignalColumn::Ignored),
        ("CAUGHT", 90, false, SignalColumn::Caught),
    ] {
        view.append_column(&signal_column(title, width, expand, column));
    }
    let active_for_toggle = Rc::clone(&filter_active);
    active_only.connect_toggled(move |button| {
        active_for_toggle.set(button.is_active());
        filter.changed(gtk::FilterChange::Different);
    });
    let empty = empty_label("No signals match the current filter");
    append_table(&page, &view, &empty);
    (page, store, count, empty)
}

fn build_processes() -> (gtk::Box, gio::ListStore, gtk::Label, gtk::Label) {
    let page = gtk::Box::new(gtk::Orientation::Vertical, 3);
    let count = table_summary(&page);
    let store = gio::ListStore::new::<glib::BoxedAnyObject>();
    let selection = gtk::SingleSelection::new(Some(store.clone()));
    selection.set_autoselect(false);
    selection.set_can_unselect(true);
    let view = gtk::ColumnView::new(Some(selection));
    view.add_css_class("debug-table");
    view.add_css_class("kernel-process-table");
    view.set_vexpand(true);
    view.set_reorderable(true);
    for (title, width, expand, column) in [
        ("PID", 90, false, ProcessColumn::Pid),
        ("PPID", 90, false, ProcessColumn::Parent),
        ("RELATION", 110, false, ProcessColumn::Relation),
        ("PROCESS", 260, true, ProcessColumn::Name),
        ("STATE", 190, false, ProcessColumn::State),
        ("THREADS", 85, false, ProcessColumn::Threads),
    ] {
        view.append_column(&process_column(title, width, expand, column));
    }
    let empty = empty_label("No process hierarchy available");
    append_table(&page, &view, &empty);
    (page, store, count, empty)
}

fn table_summary(page: &gtk::Box) -> gtk::Label {
    let count = gtk::Label::new(Some("No snapshot"));
    count.add_css_class("kernel-table-summary");
    count.add_css_class("muted");
    count.set_halign(gtk::Align::Start);
    enable_stable_text_selection(&count);
    page.append(&count);
    count
}

fn append_table(page: &gtk::Box, view: &gtk::ColumnView, empty: &gtk::Label) {
    page.append(empty);
    page.append(
        &gtk::ScrolledWindow::builder()
            .child(view)
            .min_content_height(1)
            .vexpand(true)
            .build(),
    );
}

fn append_fixed_height_table(
    page: &gtk::Box,
    view: &gtk::ColumnView,
    empty: &gtk::Label,
    height: i32,
) {
    page.append(empty);
    let scrolled = gtk::ScrolledWindow::builder()
        .child(view)
        .min_content_height(1)
        .vexpand(false)
        .build();
    scrolled.set_size_request(-1, height);
    page.append(&scrolled);
}

fn build_memory() -> (
    gtk::Box,
    gtk::Box,
    gio::ListStore,
    gio::ListStore,
    KernelMemorySummaryView,
    gtk::Label,
    gtk::Label,
) {
    let summary_page = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let summary_content = gtk::Box::new(gtk::Orientation::Vertical, 4);
    summary_content.add_css_class("kernel-memory-summary");
    summary_content.set_vexpand(true);
    let meta = gtk::Label::new(Some("No snapshot"));
    meta.add_css_class("kernel-memory-meta");
    meta.add_css_class("muted");
    meta.set_halign(gtk::Align::Start);
    meta.set_xalign(0.0);
    enable_stable_text_selection(&meta);
    summary_content.append(&meta);

    let unit_grid = gtk::Grid::new();
    unit_grid.add_css_class("kernel-memory-unit-grid");
    unit_grid.set_hexpand(true);
    for (column, (heading, width)) in [
        ("METRIC", 24),
        ("SOURCE", 13),
        ("KiB", 15),
        ("MiB", 14),
        ("GiB", 14),
        ("BASE-PAGE EQUIV.", 19),
    ]
    .into_iter()
    .enumerate()
    {
        let heading = memory_unit_label(
            heading,
            "kernel-memory-unit-header",
            width,
            if column >= 2 { 1.0 } else { 0.0 },
        );
        heading.set_hexpand(column == 5);
        unit_grid.attach(&heading, column as i32, 0, 1, 1);
    }
    let mut rows = Vec::new();
    for (index, (metric, source)) in [
        ("HTOP VIRT", "/proc/statm"),
        ("HTOP RES", "/proc/statm"),
        ("VIRTUAL (VSS)", "smaps"),
        ("RESIDENT (RSS)", "smaps"),
        ("NOT RESIDENT", "VSS − RSS"),
        ("PROPORTIONAL (PSS)", "smaps"),
        ("PROCESS-PRIVATE (USS)", "smaps"),
        ("PRIVATE CLEAN", "smaps"),
        ("PRIVATE DIRTY", "smaps"),
        ("SHARED RSS", "smaps"),
        ("SHARED CLEAN", "smaps"),
        ("SHARED DIRTY", "smaps"),
        ("SWAP", "smaps"),
        ("ANON HUGE", "smaps"),
        ("ANONYMOUS", "smaps"),
        ("REFERENCED", "smaps"),
        ("LAZY FREE", "smaps"),
        ("LOCKED", "smaps"),
        ("KSM", "smaps"),
        ("HUGE / PMD", "smaps"),
        ("PAGE TABLES", "/proc/status"),
        ("PINNED", "/proc/status"),
    ]
    .into_iter()
    .enumerate()
    {
        let row = (index + 1) as i32;
        let row_class = if index % 2 == 0 {
            "kernel-memory-unit-even"
        } else {
            "kernel-memory-unit-odd"
        };
        unit_grid.attach(&memory_unit_label(metric, row_class, 24, 0.0), 0, row, 1, 1);
        let source = memory_unit_label(source, row_class, 13, 0.0);
        source.add_css_class("muted");
        unit_grid.attach(&source, 1, row, 1, 1);
        let kib = memory_unit_label("—", row_class, 15, 1.0);
        let mib = memory_unit_label("—", row_class, 14, 1.0);
        let gib = memory_unit_label("—", row_class, 14, 1.0);
        let pages = memory_unit_label("—", row_class, 19, 1.0);
        pages.set_hexpand(true);
        unit_grid.attach(&kib, 2, row, 1, 1);
        unit_grid.attach(&mib, 3, row, 1, 1);
        unit_grid.attach(&gib, 4, row, 1, 1);
        unit_grid.attach(&pages, 5, row, 1, 1);
        rows.push(KernelMemoryUnitRow {
            kib,
            mib,
            gib,
            pages,
        });
    }
    let unit_scroll = gtk::ScrolledWindow::new();
    unit_scroll.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
    unit_scroll.set_min_content_height(1);
    unit_scroll.set_vexpand(true);
    unit_scroll.set_propagate_natural_height(false);
    unit_scroll.set_child(Some(&unit_grid));
    summary_content.append(&unit_scroll);
    summary_page.append(&summary_content);

    let private_page = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let explanation = gtk::Label::new(Some(
        "USS = Private_Clean + Private_Dirty in /proc/<pid>/smaps.",
    ));
    explanation.add_css_class("kernel-memory-explanation");
    explanation.add_css_class("muted");
    explanation.set_halign(gtk::Align::Start);
    explanation.set_xalign(0.0);
    explanation.set_wrap(true);
    enable_stable_text_selection(&explanation);
    private_page.append(&explanation);
    let (private_summary_grid, private_summary) = build_private_summary();
    private_page.append(&private_summary_grid);

    let category_content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let category_title = section_title("SUMMARY BY MAPPING TYPE");
    category_title.add_css_class("kernel-memory-subtitle");
    category_title.set_halign(gtk::Align::Fill);
    category_title.set_xalign(0.0);
    category_title.set_hexpand(true);
    category_content.append(&category_title);
    let store = gio::ListStore::new::<glib::BoxedAnyObject>();
    let selection = gtk::SingleSelection::new(Some(store.clone()));
    selection.set_autoselect(false);
    selection.set_can_unselect(true);
    let view = gtk::ColumnView::new(Some(selection));
    view.add_css_class("debug-table");
    view.add_css_class("kernel-memory-table");
    view.set_vexpand(true);
    view.set_reorderable(true);
    for (title, width, expand, column) in [
        ("TYPE", 190, false, MemoryColumn::Category),
        ("VMAs", 60, false, MemoryColumn::Mappings),
        ("PRIVATE / PAGES", 185, false, MemoryColumn::Unique),
        ("% USS", 75, false, MemoryColumn::UniquePercent),
        ("CLEAN", 95, false, MemoryColumn::PrivateClean),
        ("DIRTY", 95, false, MemoryColumn::PrivateDirty),
        ("RSS / PAGES", 175, false, MemoryColumn::Rss),
        ("VSS / PAGES", 175, true, MemoryColumn::Virtual),
    ] {
        view.append_column(&memory_column(title, width, expand, column));
    }
    let empty = empty_label("No process-private mapping types are available");
    append_fixed_height_table(
        &category_content,
        &view,
        &empty,
        PRIVATE_CATEGORY_TABLE_HEIGHT,
    );

    let mapping_content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    mapping_content.set_vexpand(true);
    let mapping_header = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    mapping_header.add_css_class("kernel-memory-subtitle");
    let mapping_title = section_title("PER-MAPPING PRIVATE MEMORY");
    mapping_title.set_hexpand(true);
    let mapping_search = gtk::SearchEntry::builder()
        .placeholder_text("Filter address, permissions, or backing")
        .width_request(280)
        .build();
    mapping_search.add_css_class("kernel-table-search");
    mapping_header.append(&mapping_title);
    mapping_header.append(&mapping_search);
    mapping_content.append(&mapping_header);
    let private_mapping_store = gio::ListStore::new::<glib::BoxedAnyObject>();
    let mapping_query = Rc::new(RefCell::new(String::new()));
    let query_for_filter = Rc::clone(&mapping_query);
    let mapping_filter = gtk::CustomFilter::new(move |object| {
        let Some(data) = object.downcast_ref::<glib::BoxedAnyObject>() else {
            return false;
        };
        let query = query_for_filter.borrow();
        query.is_empty()
            || private_mapping_search_text(&data.borrow::<KernelPrivateMappingRow>())
                .contains(&*query)
    });
    let filtered = gtk::FilterListModel::new(
        Some(private_mapping_store.clone()),
        Some(mapping_filter.clone()),
    );
    let private_mapping_selection = gtk::SingleSelection::new(Some(filtered));
    private_mapping_selection.set_autoselect(false);
    private_mapping_selection.set_can_unselect(true);
    let private_mapping_view = gtk::ColumnView::new(Some(private_mapping_selection));
    private_mapping_view.add_css_class("debug-table");
    private_mapping_view.add_css_class("kernel-memory-table");
    private_mapping_view.set_vexpand(true);
    private_mapping_view.set_reorderable(true);
    for (title, width, expand, column) in [
        ("ADDRESS", 285, false, PrivateMappingColumn::Address),
        ("PERM", 65, false, PrivateMappingColumn::Permissions),
        ("PRIVATE / PAGES", 185, false, PrivateMappingColumn::Unique),
        ("% USS", 75, false, PrivateMappingColumn::UniquePercent),
        ("CLEAN", 95, false, PrivateMappingColumn::PrivateClean),
        ("DIRTY", 95, false, PrivateMappingColumn::PrivateDirty),
        ("RSS / PAGES", 175, false, PrivateMappingColumn::Rss),
        ("VSS / PAGES", 175, false, PrivateMappingColumn::Virtual),
        ("ANON", 95, false, PrivateMappingColumn::Anonymous),
        ("REFERENCED", 105, false, PrivateMappingColumn::Referenced),
        ("LAZY FREE", 95, false, PrivateMappingColumn::LazyFree),
        ("HUGE / PMD", 105, false, PrivateMappingColumn::Huge),
        ("BACKING", 420, true, PrivateMappingColumn::Path),
    ] {
        private_mapping_view.append_column(&private_mapping_column(title, width, expand, column));
    }
    let private_mapping_empty = empty_label("No process-private mappings are available");
    append_table(
        &mapping_content,
        &private_mapping_view,
        &private_mapping_empty,
    );
    mapping_search.connect_search_changed(move |search| {
        mapping_query.replace(search.text().trim().to_ascii_lowercase());
        mapping_filter.changed(gtk::FilterChange::Different);
    });

    private_page.append(&category_content);
    private_page.append(&mapping_content);
    (
        summary_page,
        private_page,
        store,
        private_mapping_store,
        KernelMemorySummaryView {
            meta,
            rows,
            private_summary,
        },
        empty,
        private_mapping_empty,
    )
}

fn build_private_summary() -> (gtk::Grid, KernelPrivateSummaryView) {
    let grid = gtk::Grid::new();
    grid.add_css_class("kernel-private-summary-grid");
    grid.set_column_homogeneous(true);
    grid.set_column_spacing(1);
    let mut values = Vec::new();
    for (column, title) in ["TOTAL USS", "PRIVATE CLEAN", "PRIVATE DIRTY", "MAPPINGS"]
        .into_iter()
        .enumerate()
    {
        let cell = gtk::Box::new(gtk::Orientation::Vertical, 1);
        cell.add_css_class("kernel-private-summary-cell");
        let title = gtk::Label::new(Some(title));
        title.add_css_class("section-title");
        title.set_halign(gtk::Align::Start);
        let value = gtk::Label::new(Some("—"));
        value.add_css_class("kernel-private-summary-value");
        value.set_halign(gtk::Align::Start);
        value.set_xalign(0.0);
        value.set_wrap(true);
        enable_stable_text_selection(&value);
        cell.append(&title);
        cell.append(&value);
        grid.attach(&cell, column as i32, 0, 1, 1);
        values.push(value);
    }
    let [total, clean, dirty, mappings]: [gtk::Label; 4] = values
        .try_into()
        .expect("private summary always has four values");
    (
        grid,
        KernelPrivateSummaryView {
            total,
            clean,
            dirty,
            mappings,
        },
    )
}

fn memory_unit_label(text: &str, class: &str, width: i32, xalign: f32) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.add_css_class("kernel-memory-unit-cell");
    label.add_css_class(class);
    label.set_width_chars(width);
    label.set_max_width_chars(width);
    label.set_halign(gtk::Align::Fill);
    label.set_xalign(xalign);
    enable_stable_text_selection(&label);
    label
}

fn build_mappings() -> (gtk::Box, gio::ListStore, gtk::Label, gtk::Label) {
    let page = gtk::Box::new(gtk::Orientation::Vertical, 3);
    let controls = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    controls.add_css_class("kernel-table-controls");
    let search = gtk::SearchEntry::builder()
        .placeholder_text("Filter address, path, permissions, or flags")
        .hexpand(true)
        .build();
    search.add_css_class("kernel-table-search");
    let count = gtk::Label::new(Some("No snapshot"));
    count.add_css_class("muted");
    enable_stable_text_selection(&count);
    controls.append(&search);
    controls.append(&count);
    page.append(&controls);

    let store = gio::ListStore::new::<glib::BoxedAnyObject>();
    let query = Rc::new(RefCell::new(String::new()));
    let query_for_filter = Rc::clone(&query);
    let filter = gtk::CustomFilter::new(move |object| {
        let Some(data) = object.downcast_ref::<glib::BoxedAnyObject>() else {
            return false;
        };
        let query = query_for_filter.borrow();
        query.is_empty() || mapping_search_text(&data.borrow::<KernelMapping>()).contains(&*query)
    });
    let filtered = gtk::FilterListModel::new(Some(store.clone()), Some(filter.clone()));
    let selection = gtk::SingleSelection::new(Some(filtered));
    selection.set_autoselect(false);
    selection.set_can_unselect(true);
    let view = gtk::ColumnView::new(Some(selection));
    view.add_css_class("debug-table");
    view.add_css_class("kernel-mappings-table");
    view.set_vexpand(true);
    view.set_reorderable(true);
    for (title, width, expand, column) in [
        ("ADDRESS RANGE", 420, false, MappingColumn::Address),
        ("PERM", 65, false, MappingColumn::Permissions),
        ("SIZE / PAGES", 180, false, MappingColumn::Size),
        ("RSS / PAGES", 180, false, MappingColumn::Rss),
        ("PSS", 90, false, MappingColumn::Pss),
        (
            "PRIVATE RSS (USS) / PAGES",
            210,
            false,
            MappingColumn::Private,
        ),
        ("PRIVATE DIRTY", 115, false, MappingColumn::PrivateDirty),
        ("SHARED / PAGES", 180, false, MappingColumn::Shared),
        ("SWAP", 90, false, MappingColumn::Swap),
        ("HUGE / PMD", 105, false, MappingColumn::Huge),
        ("ANON", 95, false, MappingColumn::Anonymous),
        ("REFERENCED", 105, false, MappingColumn::Referenced),
        ("LAZY FREE", 95, false, MappingColumn::LazyFree),
        ("LOCKED", 90, false, MappingColumn::Locked),
        ("PATH", 320, true, MappingColumn::Path),
        ("DEVICE / INODE", 170, false, MappingColumn::FileIdentity),
        ("NUMA", 250, false, MappingColumn::Numa),
        ("PAGE SAMPLE", 360, false, MappingColumn::Page),
        ("VM FLAGS", 260, false, MappingColumn::Flags),
    ] {
        view.append_column(&mapping_column(title, width, expand, column));
    }
    let empty = empty_label("No detailed mappings available");
    page.append(&empty);
    page.append(
        &gtk::ScrolledWindow::builder()
            .child(&view)
            .min_content_height(1)
            .vexpand(true)
            .build(),
    );

    let query_for_search = Rc::clone(&query);
    search.connect_search_changed(move |search| {
        query_for_search.replace(search.text().trim().to_ascii_lowercase());
        filter.changed(gtk::FilterChange::Different);
    });
    (page, store, count, empty)
}

fn build_descriptors() -> (gtk::Box, gio::ListStore, gtk::Label, gtk::Label) {
    let page = gtk::Box::new(gtk::Orientation::Vertical, 3);
    let count = gtk::Label::new(Some("No snapshot"));
    count.add_css_class("kernel-table-summary");
    count.add_css_class("muted");
    count.set_halign(gtk::Align::Start);
    page.append(&count);
    let store = gio::ListStore::new::<glib::BoxedAnyObject>();
    let selection = gtk::SingleSelection::new(Some(store.clone()));
    selection.set_autoselect(false);
    selection.set_can_unselect(true);
    let view = gtk::ColumnView::new(Some(selection));
    view.add_css_class("debug-table");
    view.set_vexpand(true);
    view.set_reorderable(true);
    for (title, width, expand, column) in [
        ("FD", 55, false, DescriptorColumn::Number),
        ("KIND", 90, false, DescriptorColumn::Kind),
        ("ACCESS", 100, false, DescriptorColumn::Access),
        ("FLAGS", 220, false, DescriptorColumn::Flags),
        ("POSITION", 110, false, DescriptorColumn::Position),
        ("TARGET", 360, true, DescriptorColumn::Target),
        ("FDINFO", 280, false, DescriptorColumn::Details),
    ] {
        view.append_column(&descriptor_column(title, width, expand, column));
    }
    let empty = empty_label("No open file descriptors available");
    page.append(&empty);
    page.append(
        &gtk::ScrolledWindow::builder()
            .child(&view)
            .min_content_height(1)
            .vexpand(true)
            .build(),
    );
    (page, store, count, empty)
}

fn build_limits() -> (gtk::Box, gio::ListStore, gtk::Label, gtk::Label) {
    let page = gtk::Box::new(gtk::Orientation::Vertical, 3);
    let count = gtk::Label::new(Some("No snapshot"));
    count.add_css_class("kernel-table-summary");
    count.add_css_class("muted");
    count.set_halign(gtk::Align::Start);
    page.append(&count);
    let store = gio::ListStore::new::<glib::BoxedAnyObject>();
    let selection = gtk::SingleSelection::new(Some(store.clone()));
    selection.set_autoselect(false);
    selection.set_can_unselect(true);
    let view = gtk::ColumnView::new(Some(selection));
    view.add_css_class("debug-table");
    view.set_vexpand(true);
    view.set_reorderable(true);
    for (title, width, expand, column) in [
        ("RESOURCE", 260, true, LimitColumn::Resource),
        ("SOFT", 160, false, LimitColumn::Soft),
        ("HARD", 160, false, LimitColumn::Hard),
        ("UNITS", 120, false, LimitColumn::Units),
    ] {
        view.append_column(&limit_column(title, width, expand, column));
    }
    let empty = empty_label("No resource limits available");
    page.append(&empty);
    page.append(
        &gtk::ScrolledWindow::builder()
            .child(&view)
            .min_content_height(1)
            .vexpand(true)
            .build(),
    );
    (page, store, count, empty)
}

fn memory_column(
    title: &str,
    width: i32,
    expand: bool,
    column: MemoryColumn,
) -> gtk::ColumnViewColumn {
    table_column(title, width, expand, move |object, label| {
        let row = object.borrow::<KernelMemoryRow>();
        let category = &row.category;
        let text = match column {
            MemoryColumn::Category => category.category.clone(),
            MemoryColumn::Mappings => category.mappings.to_string(),
            MemoryColumn::Unique => format_memory_amount(category.unique_rss(), row.page_size),
            MemoryColumn::UniquePercent => {
                format!(
                    "{:.1}%",
                    ratio(category.unique_rss(), row.total_unique) * 100.0
                )
            }
            MemoryColumn::PrivateClean => crate::kernel::format_bytes(category.private_clean),
            MemoryColumn::PrivateDirty => crate::kernel::format_bytes(category.private_dirty),
            MemoryColumn::Virtual => format_memory_amount(category.virtual_bytes, row.page_size),
            MemoryColumn::Rss => format_memory_amount(category.rss, row.page_size),
        };
        if matches!(column, MemoryColumn::Category) {
            label.add_css_class("kernel-memory-category");
        }
        if matches!(column, MemoryColumn::Unique | MemoryColumn::UniquePercent) {
            label.add_css_class("kernel-memory-exclusive");
        }
        label.set_xalign(if matches!(column, MemoryColumn::Category) {
            0.0
        } else {
            1.0
        });
        label.set_text(&text);
        label.set_tooltip_text(Some(&format!(
            "{} · {} VMAs · VSS {} · RSS {} · private RSS (USS) {} · shared RSS {} · PSS {} · {}",
            category.category,
            category.mappings,
            crate::kernel::format_bytes(category.virtual_bytes),
            crate::kernel::format_bytes(category.rss),
            crate::kernel::format_bytes(category.unique_rss()),
            crate::kernel::format_bytes(category.shared_rss()),
            crate::kernel::format_bytes(category.pss),
            category.details,
        )));
    })
}

fn private_mapping_column(
    title: &str,
    width: i32,
    expand: bool,
    column: PrivateMappingColumn,
) -> gtk::ColumnViewColumn {
    table_column(title, width, expand, move |object, label| {
        let row = object.borrow::<KernelPrivateMappingRow>();
        let mapping = &row.mapping;
        reset_semantic_css(label);
        label.remove_css_class("kernel-memory-exclusive");
        label.add_css_class(mapping_css(mapping));
        let text = match column {
            PrivateMappingColumn::Address => {
                format!("0x{:016x}–0x{:016x}", mapping.start, mapping.end)
            }
            PrivateMappingColumn::Permissions => mapping.permissions.clone(),
            PrivateMappingColumn::Unique => {
                format_memory_amount(mapping.private_bytes(), row.page_size)
            }
            PrivateMappingColumn::UniquePercent => format!(
                "{:.1}%",
                ratio(mapping.private_bytes(), row.total_unique) * 100.0
            ),
            PrivateMappingColumn::PrivateClean => {
                crate::kernel::format_bytes(mapping.private_clean)
            }
            PrivateMappingColumn::PrivateDirty => {
                crate::kernel::format_bytes(mapping.private_dirty)
            }
            PrivateMappingColumn::Rss => format_memory_amount(mapping.rss, row.page_size),
            PrivateMappingColumn::Virtual => format_memory_amount(mapping.size, row.page_size),
            PrivateMappingColumn::Anonymous => crate::kernel::format_bytes(mapping.anonymous),
            PrivateMappingColumn::Referenced => crate::kernel::format_bytes(mapping.referenced),
            PrivateMappingColumn::LazyFree => crate::kernel::format_bytes(mapping.lazy_free),
            PrivateMappingColumn::Huge => crate::kernel::format_bytes(mapping.huge_bytes()),
            PrivateMappingColumn::Path => mapping
                .path
                .clone()
                .unwrap_or_else(|| String::from("anonymous")),
        };
        if matches!(
            column,
            PrivateMappingColumn::Unique | PrivateMappingColumn::UniquePercent
        ) {
            label.add_css_class("kernel-memory-exclusive");
        }
        label.set_xalign(
            if matches!(
                column,
                PrivateMappingColumn::Address
                    | PrivateMappingColumn::Permissions
                    | PrivateMappingColumn::Path
            ) {
                0.0
            } else {
                1.0
            },
        );
        label.set_text(&text);
        label.set_tooltip_text(Some(&format!(
            "0x{:016x}–0x{:016x} · {} · device {} · inode {} · private RSS (USS) {} · clean {} · dirty {} · RSS {} · VSS {} · PSS {} · anonymous {} · referenced {} · lazy-free {} · huge/PMD {} · {}",
            mapping.start,
            mapping.end,
            mapping.permissions,
            mapping.device,
            mapping.inode,
            crate::kernel::format_bytes(mapping.private_bytes()),
            crate::kernel::format_bytes(mapping.private_clean),
            crate::kernel::format_bytes(mapping.private_dirty),
            crate::kernel::format_bytes(mapping.rss),
            crate::kernel::format_bytes(mapping.size),
            crate::kernel::format_bytes(mapping.pss),
            crate::kernel::format_bytes(mapping.anonymous),
            crate::kernel::format_bytes(mapping.referenced),
            crate::kernel::format_bytes(mapping.lazy_free),
            crate::kernel::format_bytes(mapping.huge_bytes()),
            mapping.path.as_deref().unwrap_or("anonymous"),
        )));
    })
}

fn private_mapping_search_text(row: &KernelPrivateMappingRow) -> String {
    let mapping = &row.mapping;
    format!(
        "0x{:x} 0x{:x} {} {} {} {}",
        mapping.start,
        mapping.end,
        mapping.permissions,
        mapping.device,
        mapping.inode,
        mapping.path.as_deref().unwrap_or("anonymous"),
    )
    .to_ascii_lowercase()
}

fn format_memory_amount(bytes: u64, page_size: u64) -> String {
    let pages = if page_size == 0 {
        0
    } else {
        bytes.div_ceil(page_size)
    };
    format!(
        "{} · {} pages",
        crate::kernel::format_bytes(bytes),
        format_grouped_count(pages)
    )
}

fn ratio(part: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        (part as f64 / total as f64).clamp(0.0, 1.0)
    }
}

fn mapping_page_size(mapping: &KernelMapping) -> u64 {
    if mapping.mmu_page_size == 0 {
        4096
    } else {
        mapping.mmu_page_size
    }
}

fn format_grouped_count(value: u64) -> String {
    let digits = value.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, character) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(character);
    }
    grouped
}

fn mapping_column(
    title: &str,
    width: i32,
    expand: bool,
    column: MappingColumn,
) -> gtk::ColumnViewColumn {
    table_column(title, width, expand, move |object, label| {
        let mapping = object.borrow::<KernelMapping>();
        reset_semantic_css(label);
        label.add_css_class(mapping_css(&mapping));
        let text = match column {
            MappingColumn::Address => format!(
                "0x{:016x}–0x{:016x}  +0x{:x}",
                mapping.start, mapping.end, mapping.offset
            ),
            MappingColumn::Permissions => mapping.permissions.clone(),
            MappingColumn::Size => format_memory_amount(mapping.size, mapping_page_size(&mapping)),
            MappingColumn::Rss => format_memory_amount(mapping.rss, mapping_page_size(&mapping)),
            MappingColumn::Pss => crate::kernel::format_bytes(mapping.pss),
            MappingColumn::Private => {
                format_memory_amount(mapping.private_bytes(), mapping_page_size(&mapping))
            }
            MappingColumn::PrivateDirty => crate::kernel::format_bytes(mapping.private_dirty),
            MappingColumn::Shared => {
                format_memory_amount(mapping.shared_bytes(), mapping_page_size(&mapping))
            }
            MappingColumn::Swap => crate::kernel::format_bytes(mapping.swap),
            MappingColumn::Huge => crate::kernel::format_bytes(mapping.huge_bytes()),
            MappingColumn::Anonymous => crate::kernel::format_bytes(mapping.anonymous),
            MappingColumn::Referenced => crate::kernel::format_bytes(mapping.referenced),
            MappingColumn::LazyFree => crate::kernel::format_bytes(mapping.lazy_free),
            MappingColumn::Locked => crate::kernel::format_bytes(mapping.locked),
            MappingColumn::Path => mapping
                .path
                .clone()
                .unwrap_or_else(|| String::from("anonymous")),
            MappingColumn::FileIdentity => format!("{} / {}", mapping.device, mapping.inode),
            MappingColumn::Numa => {
                if mapping.numa_policy.is_empty() {
                    mapping.numa_nodes.clone()
                } else if mapping.numa_nodes.is_empty() {
                    mapping.numa_policy.clone()
                } else {
                    format!("{} · {}", mapping.numa_policy, mapping.numa_nodes)
                }
            }
            MappingColumn::Page => mapping.page_sample.clone(),
            MappingColumn::Flags => {
                let mut flags = mapping.vm_flags.clone();
                if mapping.thp_eligible {
                    flags.push_str(" · THP eligible");
                }
                flags
            }
        };
        label.set_xalign(
            if matches!(
                column,
                MappingColumn::Address
                    | MappingColumn::Permissions
                    | MappingColumn::Path
                    | MappingColumn::FileIdentity
                    | MappingColumn::Numa
                    | MappingColumn::Page
                    | MappingColumn::Flags
            ) {
                0.0
            } else {
                1.0
            },
        );
        label.set_text(&text);
        label.set_tooltip_text(Some(&format!(
            "0x{:016x}–0x{:016x} · {} · device {} · inode {} · VSS {} · RSS {} · private RSS (USS) {} · shared RSS {} · PSS {} · anonymous {} · referenced {} · lazy-free {} · locked {} · huge/PMD {} · {}",
            mapping.start,
            mapping.end,
            mapping.permissions,
            mapping.device,
            mapping.inode,
            crate::kernel::format_bytes(mapping.size),
            crate::kernel::format_bytes(mapping.rss),
            crate::kernel::format_bytes(mapping.private_bytes()),
            crate::kernel::format_bytes(mapping.shared_bytes()),
            crate::kernel::format_bytes(mapping.pss),
            crate::kernel::format_bytes(mapping.anonymous),
            crate::kernel::format_bytes(mapping.referenced),
            crate::kernel::format_bytes(mapping.lazy_free),
            crate::kernel::format_bytes(mapping.locked),
            crate::kernel::format_bytes(mapping.huge_bytes()),
            mapping.path.as_deref().unwrap_or("anonymous")
        )));
    })
}

fn mapping_change_column(
    title: &str,
    width: i32,
    expand: bool,
    column: MappingChangeColumn,
) -> gtk::ColumnViewColumn {
    table_column(title, width, expand, move |object, label| {
        let change = object.borrow::<KernelMappingChange>();
        reset_mapping_change_css(label);
        let text = match column {
            MappingChangeColumn::Status => change.status.clone(),
            MappingChangeColumn::Address => {
                format!("0x{:016x}–0x{:016x}", change.start, change.end)
            }
            MappingChangeColumn::Permissions => change.permissions.clone(),
            MappingChangeColumn::Size => format_signed_bytes(change.size_delta),
            MappingChangeColumn::Rss => format_signed_bytes(change.rss_delta),
            MappingChangeColumn::Pss => format_signed_bytes(change.pss_delta),
            MappingChangeColumn::Private => format_signed_bytes(change.private_delta),
            MappingChangeColumn::Dirty => format_signed_bytes(change.dirty_delta),
            MappingChangeColumn::Referenced => format_signed_bytes(change.referenced_delta),
            MappingChangeColumn::Huge => format_signed_bytes(change.huge_delta),
            MappingChangeColumn::Swap => format_signed_bytes(change.swap_delta),
            MappingChangeColumn::Path => change
                .path
                .clone()
                .unwrap_or_else(|| String::from("anonymous")),
            MappingChangeColumn::FileIdentity => {
                format!("{} / {}", change.device, change.inode)
            }
        };
        let delta = match column {
            MappingChangeColumn::Size => Some(change.size_delta),
            MappingChangeColumn::Rss => Some(change.rss_delta),
            MappingChangeColumn::Pss => Some(change.pss_delta),
            MappingChangeColumn::Private => Some(change.private_delta),
            MappingChangeColumn::Dirty => Some(change.dirty_delta),
            MappingChangeColumn::Referenced => Some(change.referenced_delta),
            MappingChangeColumn::Huge => Some(change.huge_delta),
            MappingChangeColumn::Swap => Some(change.swap_delta),
            _ => None,
        };
        if let Some(delta) = delta {
            label.add_css_class(if delta > 0 {
                "kernel-change-growth"
            } else if delta < 0 {
                "kernel-change-release"
            } else {
                "kernel-change-idle"
            });
        }
        if matches!(column, MappingChangeColumn::Status) {
            label.add_css_class(match change.status.as_str() {
                "NEW" => "kernel-change-new",
                "UNMAPPED" => "kernel-change-removed",
                "PROTECTION" | "RESIZED / PROTECTION" => "kernel-change-protection",
                _ => "kernel-change-modified",
            });
        }
        label.set_xalign(
            if matches!(
                column,
                MappingChangeColumn::Status
                    | MappingChangeColumn::Address
                    | MappingChangeColumn::Permissions
                    | MappingChangeColumn::Path
                    | MappingChangeColumn::FileIdentity
            ) {
                0.0
            } else {
                1.0
            },
        );
        label.set_text(&text);
        label.set_tooltip_text(Some(&format!(
            "{} · 0x{:016x}–0x{:016x} · {} · device {} · inode {} · ΔVSS {} · ΔRSS {} · ΔPSS {} · ΔUSS {} · {}",
            change.status,
            change.start,
            change.end,
            change.permissions,
            change.device,
            change.inode,
            format_signed_bytes(change.size_delta),
            format_signed_bytes(change.rss_delta),
            format_signed_bytes(change.pss_delta),
            format_signed_bytes(change.private_delta),
            change.path.as_deref().unwrap_or("anonymous"),
        )));
    })
}

fn reset_mapping_change_css(label: &gtk::Label) {
    for class in [
        "kernel-change-growth",
        "kernel-change-release",
        "kernel-change-idle",
        "kernel-change-new",
        "kernel-change-removed",
        "kernel-change-protection",
        "kernel-change-modified",
    ] {
        label.remove_css_class(class);
    }
}

fn format_signed_bytes(delta: i128) -> String {
    match delta.cmp(&0) {
        std::cmp::Ordering::Greater => format!("+{}", crate::kernel::format_bytes(delta as u64)),
        std::cmp::Ordering::Less => format!(
            "−{}",
            crate::kernel::format_bytes(delta.unsigned_abs().min(u128::from(u64::MAX)) as u64)
        ),
        std::cmp::Ordering::Equal => String::from("—"),
    }
}

fn descriptor_column(
    title: &str,
    width: i32,
    expand: bool,
    column: DescriptorColumn,
) -> gtk::ColumnViewColumn {
    table_column(title, width, expand, move |object, label| {
        let descriptor = object.borrow::<KernelFileDescriptor>();
        label.set_text(&match column {
            DescriptorColumn::Number => descriptor.number.to_string(),
            DescriptorColumn::Kind => descriptor.kind.clone(),
            DescriptorColumn::Access => descriptor.access.clone(),
            DescriptorColumn::Flags => descriptor.flags.clone(),
            DescriptorColumn::Position => descriptor
                .position
                .map_or_else(String::new, |position| position.to_string()),
            DescriptorColumn::Target => descriptor.target.clone(),
            DescriptorColumn::Details => descriptor.details.clone(),
        });
        label.set_xalign(
            if matches!(
                column,
                DescriptorColumn::Number | DescriptorColumn::Position
            ) {
                1.0
            } else {
                0.0
            },
        );
        label.set_tooltip_text(Some(&format!(
            "{} · {} · {} · {}",
            descriptor.target, descriptor.access, descriptor.flags, descriptor.details
        )));
    })
}

fn limit_column(
    title: &str,
    width: i32,
    expand: bool,
    column: LimitColumn,
) -> gtk::ColumnViewColumn {
    table_column(title, width, expand, move |object, label| {
        let limit = object.borrow::<KernelLimit>();
        label.set_text(match column {
            LimitColumn::Resource => &limit.resource,
            LimitColumn::Soft => &limit.soft,
            LimitColumn::Hard => &limit.hard,
            LimitColumn::Units => &limit.units,
        });
        label.set_xalign(if matches!(column, LimitColumn::Soft | LimitColumn::Hard) {
            1.0
        } else {
            0.0
        });
    })
}

fn thread_column(
    title: &str,
    width: i32,
    expand: bool,
    column: ThreadColumn,
) -> gtk::ColumnViewColumn {
    table_column(title, width, expand, move |object, label| {
        let thread = object.borrow::<KernelThread>();
        reset_kernel_css(label);
        let text = match column {
            ThreadColumn::Tid => thread.tid.to_string(),
            ThreadColumn::Name => thread.name.clone(),
            ThreadColumn::State => thread.state.clone(),
            ThreadColumn::Cpu => thread.cpu.clone(),
            ThreadColumn::Policy => thread.policy.clone(),
            ThreadColumn::Priority => thread.priority.clone(),
            ThreadColumn::Affinity => thread.affinity.clone(),
            ThreadColumn::Wait => thread.wait_channel.clone(),
            ThreadColumn::Syscall => thread.syscall.clone(),
            ThreadColumn::Switches => thread.switches.clone(),
            ThreadColumn::Runtime => thread
                .runtime_ns
                .map_or_else(|| String::from("—"), crate::kernel::format_duration_ns),
            ThreadColumn::RunqueueWait => thread
                .runqueue_wait_ns
                .map_or_else(|| String::from("—"), crate::kernel::format_duration_ns),
            ThreadColumn::Timeslices => thread
                .timeslices
                .map_or_else(|| String::from("—"), |value| value.to_string()),
        };
        if matches!(column, ThreadColumn::State) {
            if thread.state.starts_with('R') {
                label.add_css_class("kernel-state-active");
            } else if thread.state.starts_with('D') {
                label.add_css_class("kernel-state-warning");
            }
        }
        if matches!(
            column,
            ThreadColumn::Tid
                | ThreadColumn::Cpu
                | ThreadColumn::Runtime
                | ThreadColumn::RunqueueWait
                | ThreadColumn::Timeslices
        ) {
            label.add_css_class("kernel-numeric");
        }
        label.set_xalign(
            if matches!(
                column,
                ThreadColumn::Tid
                    | ThreadColumn::Cpu
                    | ThreadColumn::Runtime
                    | ThreadColumn::RunqueueWait
                    | ThreadColumn::Timeslices
            ) {
                1.0
            } else {
                0.0
            },
        );
        label.set_text(&text);
        label.set_tooltip_text(Some(&text));
    })
}

fn signal_column(
    title: &str,
    width: i32,
    expand: bool,
    column: SignalColumn,
) -> gtk::ColumnViewColumn {
    table_column(title, width, expand, move |object, label| {
        let signal = object.borrow::<KernelSignal>();
        reset_kernel_css(label);
        let (text, active) = match column {
            SignalColumn::Number => (signal.number.to_string(), false),
            SignalColumn::Name => (signal.name.clone(), false),
            SignalColumn::ProcessPending => (mark(signal.pending_process), signal.pending_process),
            SignalColumn::ThreadPending => (
                if signal.pending_threads == 0 {
                    String::from("—")
                } else {
                    format!("{} thread(s)", signal.pending_threads)
                },
                signal.pending_threads > 0,
            ),
            SignalColumn::Blocked => (
                if signal.blocked_threads == 0 {
                    String::from("—")
                } else {
                    format!("{} thread(s)", signal.blocked_threads)
                },
                signal.blocked_threads > 0,
            ),
            SignalColumn::Ignored => (mark(signal.ignored), signal.ignored),
            SignalColumn::Caught => (mark(signal.caught), signal.caught),
        };
        if active {
            label.add_css_class(
                if matches!(
                    column,
                    SignalColumn::ProcessPending | SignalColumn::ThreadPending
                ) {
                    "kernel-state-warning"
                } else {
                    "kernel-state-active"
                },
            );
        }
        label.set_xalign(if matches!(column, SignalColumn::Name) {
            0.0
        } else {
            1.0
        });
        label.set_text(&text);
    })
}

fn process_column(
    title: &str,
    width: i32,
    expand: bool,
    column: ProcessColumn,
) -> gtk::ColumnViewColumn {
    table_column(title, width, expand, move |object, label| {
        let process = object.borrow::<KernelProcess>();
        reset_kernel_css(label);
        let text = match column {
            ProcessColumn::Pid => process.pid.to_string(),
            ProcessColumn::Parent => process.parent_pid.to_string(),
            ProcessColumn::Relation => process.relation.clone(),
            ProcessColumn::Name => format!("{}{}", "  ".repeat(process.depth.into()), process.name),
            ProcessColumn::State => process.state.clone(),
            ProcessColumn::Threads => process.threads.clone(),
        };
        if process.relation == "Target" {
            label.add_css_class("kernel-process-target");
        }
        if matches!(
            column,
            ProcessColumn::Pid | ProcessColumn::Parent | ProcessColumn::Threads
        ) {
            label.add_css_class("kernel-numeric");
        }
        label.set_xalign(
            if matches!(
                column,
                ProcessColumn::Pid | ProcessColumn::Parent | ProcessColumn::Threads
            ) {
                1.0
            } else {
                0.0
            },
        );
        label.set_text(&text);
        label.set_tooltip_text(Some(&text));
    })
}

fn mark(active: bool) -> String {
    if active {
        String::from("●")
    } else {
        String::from("—")
    }
}

fn reset_kernel_css(label: &gtk::Label) {
    for class in [
        "kernel-state-active",
        "kernel-state-warning",
        "kernel-process-target",
        "kernel-numeric",
    ] {
        label.remove_css_class(class);
    }
}

fn table_column(
    title: &str,
    width: i32,
    expand: bool,
    bind: impl Fn(&glib::BoxedAnyObject, &gtk::Label) + Copy + 'static,
) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(move |_, object| {
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
        bind(&data, &label);
    });
    let column = gtk::ColumnViewColumn::new(Some(title), Some(factory));
    column.set_fixed_width(width);
    column.set_resizable(true);
    column.set_expand(expand);
    column
}

fn mapping_search_text(mapping: &KernelMapping) -> String {
    format!(
        "0x{:x} 0x{:x} {} {} {} {} {} {} {} {}",
        mapping.start,
        mapping.end,
        mapping.permissions,
        mapping.device,
        mapping.inode,
        mapping.path.as_deref().unwrap_or("anonymous"),
        mapping.vm_flags,
        mapping.numa_policy,
        mapping.numa_nodes,
        mapping.page_sample
    )
    .to_ascii_lowercase()
}

fn mapping_change_search_text(change: &KernelMappingChange) -> String {
    format!(
        "{} 0x{:x} 0x{:x} {} {} {} {}",
        change.status,
        change.start,
        change.end,
        change.permissions,
        change.device,
        change.inode,
        change.path.as_deref().unwrap_or("anonymous"),
    )
    .to_ascii_lowercase()
}

fn mapping_css(mapping: &KernelMapping) -> &'static str {
    match mapping.path.as_deref() {
        Some("[stack]") => "memory-stack",
        Some("[heap]") => "memory-heap",
        _ if mapping.permissions.contains('x') && mapping.permissions.contains('w') => "memory-rwx",
        _ if mapping.permissions.contains('x') => "memory-code",
        _ if mapping.permissions.contains('w') => "memory-writable",
        _ if mapping.permissions.contains('r') => "memory-readonly",
        _ => "memory-none",
    }
}

impl KernelView {
    fn show_snapshot(&self, snapshot: &KernelSnapshot) {
        self.status.set_text(&format!(
            "PID {} · {} threads · {} VMAs · {} FDs",
            snapshot.pid,
            snapshot.threads.len(),
            snapshot.mappings.len(),
            snapshot.file_descriptors.len()
        ));
        self.status.set_tooltip_text(Some(&format!(
            "Linux procfs snapshot for PID {} · collected in {}. Changes compare this capture with the preceding successful snapshot.",
            snapshot.pid,
            crate::kernel::format_duration_ns(snapshot.capture_duration_micros.saturating_mul(1_000)),
        )));
        replace_boxed_store(&self.overview_store, overview_rows(snapshot));
        replace_boxed_store(&self.resource_store, resource_rows(snapshot));
        replace_boxed_store(&self.change_store, change_rows(snapshot));
        replace_boxed_store(
            &self.mapping_change_store,
            snapshot.mapping_changes.iter().cloned(),
        );
        if snapshot.comparison_ready {
            let summary = if snapshot.mapping_changes.is_empty() {
                String::from("No changes")
            } else {
                format!(
                    "{} changed mappings · largest first",
                    snapshot.mapping_changes.len()
                )
            };
            self.mapping_change_count.set_text(&summary);
            self.mapping_change_count.set_tooltip_text(Some(&summary));
            self.mapping_changes_empty
                .set_text(if snapshot.mapping_changes.is_empty() {
                    "No mapping changes since the previous snapshot"
                } else {
                    ""
                });
        } else {
            self.mapping_change_count
                .set_text("Capture another snapshot to compare mappings");
            self.mapping_change_count
                .set_tooltip_text(Some("Capture another snapshot to compare mappings"));
            self.mapping_changes_empty
                .set_text("Capture another snapshot to compare mappings");
        }
        self.mapping_changes_empty
            .set_visible(snapshot.mapping_changes.is_empty());
        if let Some(accounting) = snapshot.memory_accounting.as_ref() {
            let total_unique = accounting.unique_rss();
            let mut categories = accounting
                .categories
                .iter()
                .filter(|category| category.unique_rss() > 0)
                .cloned()
                .collect::<Vec<_>>();
            categories.sort_by_key(|category| Reverse(category.unique_rss()));
            replace_boxed_store(
                &self.memory_store,
                categories.into_iter().map(|category| KernelMemoryRow {
                    category,
                    page_size: accounting.page_size,
                    total_unique,
                }),
            );
            let mut private_mappings = snapshot
                .mappings
                .iter()
                .filter(|mapping| mapping.private_bytes() > 0)
                .cloned()
                .collect::<Vec<_>>();
            private_mappings.sort_by_key(|mapping| Reverse(mapping.private_bytes()));
            let private_mapping_count = private_mappings.len();
            let private_mappings_empty = private_mappings.is_empty();
            replace_boxed_store(
                &self.private_mapping_store,
                private_mappings
                    .into_iter()
                    .map(|mapping| KernelPrivateMappingRow {
                        page_size: mapping_page_size(&mapping),
                        mapping,
                        total_unique,
                    }),
            );
            update_memory_summary(
                &self.memory_summary,
                accounting,
                snapshot.mappings.len(),
                private_mapping_count,
            );
            self.memory_empty.set_visible(total_unique == 0);
            self.private_mapping_empty
                .set_visible(private_mappings_empty);
        } else {
            self.memory_store.remove_all();
            self.private_mapping_store.remove_all();
            clear_memory_summary(&self.memory_summary);
            self.memory_empty.set_visible(true);
            self.private_mapping_empty.set_visible(true);
        }
        populate_warnings(&self.warnings, &snapshot.warnings);
        replace_boxed_store(&self.thread_store, snapshot.threads.iter().cloned());
        replace_boxed_store(&self.signal_store, snapshot.signals.iter().cloned());
        replace_boxed_store(&self.mapping_store, snapshot.mappings.iter().cloned());
        replace_boxed_store(
            &self.descriptor_store,
            snapshot.file_descriptors.iter().cloned(),
        );
        replace_boxed_store(&self.limit_store, snapshot.limits.iter().cloned());
        replace_boxed_store(&self.process_store, snapshot.process_tree.iter().cloned());
        self.thread_count
            .set_text(&format!("{} kernel threads", snapshot.threads.len()));
        let active_signals = snapshot
            .signals
            .iter()
            .filter(|signal| {
                signal.pending_process
                    || signal.pending_threads > 0
                    || signal.blocked_threads > 0
                    || signal.ignored
                    || signal.caught
            })
            .count();
        self.signal_count.set_text(&format!(
            "{active_signals} active states · {} signals decoded",
            snapshot.signals.len()
        ));
        if let Some(accounting) = snapshot.memory_accounting.as_ref() {
            self.mapping_count.set_text(&format!(
                "{} VMAs · VSS {} · RSS {} · USS {}",
                snapshot.mappings.len(),
                crate::kernel::format_bytes(accounting.virtual_bytes),
                crate::kernel::format_bytes(accounting.rss),
                crate::kernel::format_bytes(accounting.unique_rss()),
            ));
        } else {
            self.mapping_count
                .set_text(&format!("{} mappings", snapshot.mappings.len()));
        }
        self.descriptor_count.set_text(&format!(
            "{} open file descriptors",
            snapshot.file_descriptors.len()
        ));
        self.limit_count
            .set_text(&format!("{} resource limits", snapshot.limits.len()));
        self.process_count.set_text(&format!(
            "{} related processes · ancestors and up to 256 descendants",
            snapshot.process_tree.len()
        ));
        self.threads_empty.set_visible(snapshot.threads.is_empty());
        self.signals_empty.set_visible(snapshot.signals.is_empty());
        self.mappings_empty
            .set_visible(snapshot.mappings.is_empty());
        self.descriptors_empty
            .set_visible(snapshot.file_descriptors.is_empty());
        self.limits_empty.set_visible(snapshot.limits.is_empty());
        self.processes_empty
            .set_visible(snapshot.process_tree.is_empty());
    }

    fn clear(&self, status: &str) {
        self.status.set_text(status);
        self.status.set_tooltip_text(None);
        clear_box(&self.warnings);
        self.warnings.set_visible(false);
        self.overview_store.remove_all();
        self.resource_store.remove_all();
        self.change_store.remove_all();
        self.mapping_change_store.remove_all();
        self.memory_store.remove_all();
        self.private_mapping_store.remove_all();
        self.thread_store.remove_all();
        self.signal_store.remove_all();
        self.mapping_store.remove_all();
        self.descriptor_store.remove_all();
        self.limit_store.remove_all();
        self.process_store.remove_all();
        self.previous_snapshot.replace(None);
        self.thread_count.set_text("No snapshot");
        self.mapping_change_count
            .set_text("Capture another snapshot to compare mappings");
        self.mapping_change_count.set_tooltip_text(None);
        clear_memory_summary(&self.memory_summary);
        self.signal_count.set_text("No snapshot");
        self.mapping_count.set_text("No snapshot");
        self.descriptor_count.set_text("No snapshot");
        self.limit_count.set_text("No snapshot");
        self.process_count.set_text("No snapshot");
        self.threads_empty.set_visible(true);
        self.mapping_changes_empty.set_visible(true);
        self.mapping_changes_empty
            .set_text("Capture another snapshot to compare mappings");
        self.memory_empty.set_visible(true);
        self.private_mapping_empty.set_visible(true);
        self.signals_empty.set_visible(true);
        self.mappings_empty.set_visible(true);
        self.descriptors_empty.set_visible(true);
        self.limits_empty.set_visible(true);
        self.processes_empty.set_visible(true);
    }
}

fn update_memory_summary(
    view: &KernelMemorySummaryView,
    accounting: &crate::kernel::KernelMemoryAccounting,
    mapping_count: usize,
    private_mapping_count: usize,
) {
    let not_resident = accounting.virtual_bytes.saturating_sub(accounting.rss);
    let values = [
        accounting.statm_virtual_bytes,
        accounting.statm_rss,
        Some(accounting.virtual_bytes),
        Some(accounting.rss),
        Some(not_resident),
        Some(accounting.pss),
        Some(accounting.unique_rss()),
        Some(accounting.private_clean),
        Some(accounting.private_dirty),
        Some(accounting.shared_rss()),
        Some(accounting.shared_clean),
        Some(accounting.shared_dirty),
        Some(accounting.swap),
        Some(accounting.anon_huge_pages),
        Some(accounting.anonymous),
        Some(accounting.referenced),
        Some(accounting.lazy_free),
        Some(accounting.locked),
        Some(accounting.ksm),
        Some(accounting.huge_bytes()),
        Some(accounting.page_tables),
        Some(accounting.pinned),
    ];
    debug_assert_eq!(view.rows.len(), values.len());
    for (row, value) in view.rows.iter().zip(values) {
        set_memory_unit_row(row, value, accounting.page_size);
    }
    view.meta.set_text(&format!(
        "{} VMAs  ·  base page {} ({} bytes)",
        format_grouped_count(mapping_count as u64),
        crate::kernel::format_bytes(accounting.page_size),
        format_grouped_count(accounting.page_size),
    ));
    let exclusive_categories = accounting
        .categories
        .iter()
        .filter(|category| category.unique_rss() > 0)
        .count();
    view.private_summary.total.set_text(&format_memory_amount(
        accounting.unique_rss(),
        accounting.page_size,
    ));
    view.private_summary.clean.set_text(&format_memory_amount(
        accounting.private_clean,
        accounting.page_size,
    ));
    view.private_summary.dirty.set_text(&format_memory_amount(
        accounting.private_dirty,
        accounting.page_size,
    ));
    view.private_summary.mappings.set_text(&format!(
        "{} mappings · {exclusive_categories} types",
        format_grouped_count(private_mapping_count as u64),
    ));
}

fn set_memory_unit_row(row: &KernelMemoryUnitRow, bytes: Option<u64>, page_size: u64) {
    let Some(bytes) = bytes else {
        for label in [&row.kib, &row.mib, &row.gib, &row.pages] {
            label.set_text("—");
            label.set_tooltip_text(None);
        }
        return;
    };
    row.kib.set_text(&format_scaled_binary(bytes, 1024, 2));
    row.mib
        .set_text(&format_scaled_binary(bytes, 1024 * 1024, 3));
    row.gib
        .set_text(&format_scaled_binary(bytes, 1024 * 1024 * 1024, 6));
    row.pages
        .set_text(&format_page_equivalents(bytes, page_size));
    let tooltip = format!(
        "{} bytes · {}",
        format_grouped_count(bytes),
        crate::kernel::format_bytes(bytes)
    );
    for label in [&row.kib, &row.mib, &row.gib, &row.pages] {
        label.set_tooltip_text(Some(&tooltip));
    }
}

fn format_scaled_binary(bytes: u64, unit: u64, decimals: usize) -> String {
    if bytes.is_multiple_of(unit) {
        format_grouped_count(bytes / unit)
    } else {
        format!("{:.*}", decimals, bytes as f64 / unit as f64)
    }
}

fn format_page_equivalents(bytes: u64, page_size: u64) -> String {
    if page_size == 0 {
        return String::from("—");
    }
    if bytes.is_multiple_of(page_size) {
        format_grouped_count(bytes / page_size)
    } else {
        format!("{:.2}", bytes as f64 / page_size as f64)
    }
}

fn clear_memory_summary(view: &KernelMemorySummaryView) {
    view.meta.set_text("No snapshot");
    for row in &view.rows {
        set_memory_unit_row(row, None, 0);
    }
    for label in [
        &view.private_summary.total,
        &view.private_summary.clean,
        &view.private_summary.dirty,
        &view.private_summary.mappings,
    ] {
        label.set_text("—");
    }
}

fn overview_rows(snapshot: &KernelSnapshot) -> Vec<KernelOverviewRow> {
    let mut rows = Vec::new();
    for (section, facts) in [
        ("PROCESS", &snapshot.process),
        ("MEMORY ACCOUNTING", &snapshot.memory),
        ("SCHEDULER", &snapshot.scheduler),
        ("SECURITY", &snapshot.security),
        ("I/O ACCOUNTING", &snapshot.io),
        ("NAMESPACES / CGROUPS", &snapshot.isolation),
        ("RUNTIME / ABI", &snapshot.runtime),
    ] {
        if facts.is_empty() {
            continue;
        }
        rows.push(KernelOverviewRow {
            section: true,
            section_key: section.to_owned(),
            label: section.to_owned(),
            value: String::new(),
        });
        rows.extend(facts.iter().map(|fact| KernelOverviewRow {
            section: false,
            section_key: section.to_owned(),
            label: fact.label.clone(),
            value: fact.value.clone(),
        }));
    }
    rows
}

fn resource_rows(snapshot: &KernelSnapshot) -> Vec<KernelOverviewRow> {
    let mut rows = Vec::new();
    for (section, facts) in [
        ("DEBUGGING SIGNALS", &snapshot.diagnostics),
        ("CGROUP / PROCESS CONSTRAINTS", &snapshot.constraints),
        ("NUMA / PAGE TABLE", &snapshot.advanced),
    ] {
        if facts.is_empty() {
            continue;
        }
        rows.push(KernelOverviewRow {
            section: true,
            section_key: section.to_owned(),
            label: section.to_owned(),
            value: String::new(),
        });
        rows.extend(facts.iter().map(|fact| KernelOverviewRow {
            section: false,
            section_key: section.to_owned(),
            label: fact.label.clone(),
            value: fact.value.clone(),
        }));
    }
    rows
}

fn facts_to_overview_rows(
    section: &str,
    facts: &[crate::kernel::KernelFact],
) -> Vec<KernelOverviewRow> {
    if facts.is_empty() {
        return Vec::new();
    }
    let mut rows = vec![KernelOverviewRow {
        section: true,
        section_key: String::from(section),
        label: String::from(section),
        value: String::new(),
    }];
    rows.extend(facts.iter().map(|fact| KernelOverviewRow {
        section: false,
        section_key: String::from(section),
        label: fact.label.clone(),
        value: fact.value.clone(),
    }));
    rows
}

fn change_rows(snapshot: &KernelSnapshot) -> Vec<KernelOverviewRow> {
    if !snapshot.comparison_ready {
        return vec![KernelOverviewRow {
            section: false,
            section_key: String::from("PROCESS TOTALS"),
            label: String::from("Baseline"),
            value: String::from("Refresh or stop again to compare with this snapshot"),
        }];
    }
    let mut rows = vec![KernelOverviewRow {
        section: true,
        section_key: String::from("PROCESS TOTALS"),
        label: String::from("PROCESS TOTALS"),
        value: String::new(),
    }];
    rows.extend(snapshot.changes.iter().map(|fact| KernelOverviewRow {
        section: false,
        section_key: String::from("PROCESS TOTALS"),
        label: fact.label.clone(),
        value: fact.value.clone(),
    }));
    rows.extend(facts_to_overview_rows(
        "ALLOCATION / MAPPING CHURN",
        &snapshot.mapping_summary,
    ));
    if !snapshot.cgroup_changes.is_empty() {
        rows.push(KernelOverviewRow {
            section: true,
            section_key: String::from("CGROUP DELTAS (GROUP-WIDE)"),
            label: String::from("CGROUP DELTAS (GROUP-WIDE)"),
            value: String::new(),
        });
        rows.push(KernelOverviewRow {
            section: false,
            section_key: String::from("CGROUP DELTAS (GROUP-WIDE)"),
            label: String::from("Scope"),
            value: String::from("Includes activity from every process in the target cgroup"),
        });
        rows.extend(
            snapshot
                .cgroup_changes
                .iter()
                .map(|fact| KernelOverviewRow {
                    section: false,
                    section_key: String::from("CGROUP DELTAS (GROUP-WIDE)"),
                    label: fact.label.clone(),
                    value: fact.value.clone(),
                }),
        );
    }
    rows
}

fn populate_warnings(container: &gtk::Box, warnings: &[String]) {
    clear_box(container);
    container.set_visible(!warnings.is_empty());
    for warning in warnings {
        let label = gtk::Label::new(Some(warning));
        label.add_css_class("kernel-warning");
        label.set_halign(gtk::Align::Start);
        label.set_xalign(0.0);
        label.set_wrap(true);
        enable_stable_text_selection(&label);
        container.append(&label);
    }
}

impl Ui {
    pub fn set_kernel_refresh_handler(&self, handler: impl Fn() + 'static) {
        self.kernel_refresh_handler.replace(Some(Rc::new(handler)));
    }

    pub fn begin_kernel_refresh(&self) -> Option<u64> {
        if !self.kernel_refresh_allowed() || self.kernel_view.in_flight.get() {
            return None;
        }
        let generation = self.kernel_refresh_generation.get().wrapping_add(1);
        self.kernel_refresh_generation.set(generation);
        self.kernel_view.in_flight.set(true);
        self.kernel_view.status.set_text("Reading /proc…");
        self.kernel_view.status.set_tooltip_text(Some(
            "Detailed smaps parsing runs outside the GTK main thread",
        ));
        self.update_control_sensitivity();
        Some(generation)
    }

    pub fn show_kernel_snapshot(&self, generation: u64, snapshot: &KernelSnapshot) {
        if generation != self.kernel_refresh_generation.get() {
            self.finish_stale_kernel_refresh();
            return;
        }
        self.kernel_view.in_flight.set(false);
        let mut snapshot = snapshot.clone();
        snapshot.compare_with(self.kernel_view.previous_snapshot.borrow().as_ref());
        self.kernel_view.show_snapshot(&snapshot);
        self.kernel_view.previous_snapshot.replace(Some(snapshot));
        self.kernel_view.needs_refresh.set(false);
        self.update_control_sensitivity();
    }

    pub fn show_kernel_error(&self, generation: u64, error: &str) {
        if generation != self.kernel_refresh_generation.get() {
            self.finish_stale_kernel_refresh();
            return;
        }
        self.kernel_view.in_flight.set(false);
        self.kernel_view.needs_refresh.set(true);
        if self.kernel_view.previous_snapshot.borrow().is_some() {
            self.kernel_view
                .status
                .set_text("Refresh failed · showing cached snapshot");
        } else {
            self.kernel_view.clear("Kernel data unavailable");
        }
        populate_warnings(&self.kernel_view.warnings, &[error.to_owned()]);
        self.update_control_sensitivity();
    }

    pub fn refresh_kernel_after_stop(&self) {
        if self.kernel_view.tracking_enabled.get()
            && self.kernel_view.needs_refresh.get()
            && self.kernel_refresh_allowed()
            && let Some(handler) = self.kernel_refresh_handler.borrow().as_ref()
        {
            handler();
        }
    }

    pub fn invalidate_kernel_refresh(&self) {
        self.kernel_refresh_generation
            .set(self.kernel_refresh_generation.get().wrapping_add(1));
        self.kernel_view.needs_refresh.set(true);
        self.update_control_sensitivity();
    }

    pub fn kernel_refresh_is_current(&self, generation: u64) -> bool {
        generation == self.kernel_refresh_generation.get()
    }

    pub fn finish_stale_kernel_refresh(&self) {
        self.kernel_view.in_flight.set(false);
        self.update_control_sensitivity();
        self.refresh_kernel_after_stop();
    }

    pub fn clear_kernel_snapshot(&self) {
        self.invalidate_kernel_refresh();
        self.kernel_view
            .clear("Start the inferior to inspect procfs");
    }

    fn kernel_refresh_allowed(&self) -> bool {
        self.debugger_ready.get()
            && self.inferior_started.get()
            && !self.inferior_running.get()
            && !self.command_pending.get()
    }
}

#[cfg(test)]
mod memory_view_tests {
    use super::*;

    #[test]
    fn overview_disclosures_default_to_process_only_and_restore_overrides() {
        let defaults = kernel_overview_collapsed(&HashMap::new());
        assert!(!defaults.contains("PROCESS"));
        assert_eq!(defaults.len(), KERNEL_OVERVIEW_DISCLOSURES.len() - 1);

        let remembered = HashMap::from([
            (String::from("kernel.overview.process"), false),
            (String::from("kernel.overview.scheduler"), true),
        ]);
        let restored = kernel_overview_collapsed(&remembered);
        assert!(restored.contains("PROCESS"));
        assert!(!restored.contains("SCHEDULER"));
        assert!(restored.contains("SECURITY"));
    }

    #[test]
    fn formats_page_counts_and_safe_memory_ratios() {
        assert_eq!(
            format_memory_amount(1024 * 1024, 4096),
            "1.00 MiB · 256 pages"
        );
        assert_eq!(format_scaled_binary(3_076_096, 1024, 2), "3,004");
        assert_eq!(format_scaled_binary(3_076_096, 1024 * 1024, 3), "2.934");
        assert_eq!(
            format_scaled_binary(3_076_096, 1024 * 1024 * 1024, 6),
            "0.002865"
        );
        assert_eq!(format_page_equivalents(3072, 4096), "0.75");
        assert_eq!(format_grouped_count(1_234_567), "1,234,567");
        assert_eq!(ratio(1, 4), 0.25);
        assert_eq!(ratio(4, 0), 0.0);
        assert_eq!(ratio(8, 4), 1.0);
    }
}
