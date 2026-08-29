use super::*;

const MISC_PAGES: [(&str, &str); 1] = [("startup-vectors", "Args / Env")];

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

pub(super) fn build_misc_view(bindings: &MiscViewBindings<'_>) -> MiscView {
    let active = Rc::new(Cell::new(false));
    let tracking_enabled = Rc::new(Cell::new(false));
    let in_flight = Rc::new(Cell::new(false));
    let needs_refresh = Rc::new(Cell::new(true));
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.set_size_request(0, 0);
    root.add_css_class("sidebar");
    root.add_css_class("kernel-page");
    root.add_css_class("misc-page");

    let status = gtk::Label::new(Some("Start and pause a local inferior"));
    status.add_css_class("kernel-status");
    status.add_css_class("muted");
    status.set_ellipsize(pango::EllipsizeMode::Middle);
    let refresh_button = gtk::Button::with_label("Refresh");
    refresh_button.add_css_class("inline-action");
    refresh_button.set_sensitive(false);
    refresh_button.set_tooltip_text(Some("Read argc, argv and envp again from procfs"));

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
    navigation.root.append(&status);
    navigation.root.append(&refresh_button);
    let compact_refresh_button = gtk::Button::with_label("Refresh");
    compact_refresh_button.add_css_class("inline-action");
    compact_refresh_button.set_tooltip_text(refresh_button.tooltip_text().as_deref());
    compact_refresh_button.set_sensitive(refresh_button.is_sensitive());
    let compact_refresh_for_sensitivity = compact_refresh_button.clone();
    refresh_button.connect_sensitive_notify(move |button| {
        compact_refresh_for_sensitivity.set_sensitive(button.is_sensitive());
    });
    navigation.compact_root.append(&compact_refresh_button);
    root.append(&navigation.root);
    root.append(&navigation.compact_root);

    let startup = build_startup_page();
    pages.add_titled(&startup.root, Some("startup-vectors"), "Args / Env");
    root.append(&pages);

    let handler = Rc::clone(bindings.refresh_handler);
    refresh_button.connect_clicked(move |_| {
        if let Some(handler) = handler.borrow().as_ref() {
            handler();
        }
    });
    let handler = Rc::clone(bindings.refresh_handler);
    compact_refresh_button.connect_clicked(move |_| {
        if let Some(handler) = handler.borrow().as_ref() {
            handler();
        }
    });

    MiscView {
        root,
        wide_subtabs: navigation.root,
        compact_subtabs: navigation.compact_root,
        active,
        tracking_enabled,
        in_flight,
        needs_refresh,
        refresh_button,
        status,
        summary: startup.summary,
        warning: startup.warning,
        arguments_store: startup.arguments_store,
        arguments_empty: startup.arguments_empty,
        environment_store: startup.environment_store,
        environment_empty: startup.environment_empty,
        startup_split: startup.split,
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
}

impl MiscView {
    fn show_snapshot(&self, snapshot: ProcessStartupSnapshot) {
        let argument_count = snapshot.arguments.len();
        let environment_count = snapshot.environment.len();
        self.status.set_text(&format!("PID {}", snapshot.pid));
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

    fn clear(&self, message: &str) {
        self.status.set_text(message);
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
    }
}

impl Ui {
    pub fn set_misc_refresh_handler(&self, handler: impl Fn() + 'static) {
        self.misc_refresh_handler.replace(Some(Rc::new(handler)));
    }

    pub fn begin_misc_refresh(&self) -> Option<u64> {
        if !self.misc_refresh_allowed() || self.misc_view.in_flight.get() {
            return None;
        }
        let generation = self.misc_refresh_generation.get().wrapping_add(1);
        self.misc_refresh_generation.set(generation);
        self.misc_view.in_flight.set(true);
        self.misc_view.status.set_text("Reading process vectors…");
        self.update_control_sensitivity();
        Some(generation)
    }

    pub fn show_misc_snapshot(&self, generation: u64, snapshot: ProcessStartupSnapshot) {
        if generation != self.misc_refresh_generation.get() {
            self.finish_stale_misc_refresh();
            return;
        }
        self.misc_view.in_flight.set(false);
        self.misc_view.needs_refresh.set(false);
        self.misc_view.show_snapshot(snapshot);
        self.update_control_sensitivity();
    }

    pub fn show_misc_error(&self, generation: u64, error: &str) {
        if generation != self.misc_refresh_generation.get() {
            self.finish_stale_misc_refresh();
            return;
        }
        self.misc_view.in_flight.set(false);
        // Keep the diagnostic stable while the target is stopped. A new run
        // invalidates the view, and Refresh remains available for a manual
        // retry without repeatedly probing unsupported remote/core sessions.
        self.misc_view.needs_refresh.set(false);
        self.misc_view.clear("Process vectors unavailable");
        self.misc_view.warning.set_text(error);
        self.misc_view.warning.set_visible(true);
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
        self.misc_view.clear("Start and pause a local inferior");
    }

    fn misc_refresh_allowed(&self) -> bool {
        self.debugger_ready.get()
            && self.inferior_started.get()
            && !self.inferior_running.get()
            && !self.command_pending.get()
    }
}
