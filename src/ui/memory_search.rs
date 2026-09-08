use super::*;
use crate::memory_search::{Action, Hit, MAX_RANGES, Progress, Query, SearchKind, parse_address};

type Handler = Rc<dyn Fn(Action)>;

struct ResultRow {
    address: u64,
    cells: [String; 4],
    filter: String,
}

pub(super) struct MemorySearchView {
    pub root: gtk::Box,
    pages: gtk::Stack,
    kind: gtk::DropDown,
    value: gtk::Entry,
    scope: gtk::DropDown,
    mappings: gtk::Box,
    mapping_body: gtk::Box,
    mapping_header: gtk::Box,
    collapsed_header: gtk::Box,
    mapping_toggle: gtk::ToggleButton,
    mapping_clear: gtk::Button,
    mapping_search: gtk::Button,
    mapping_selection: gtk::SelectionModel,
    mapping_summary: gtk::Label,
    range_row: gtk::Box,
    begin: gtk::Entry,
    end: gtk::Entry,
    aligned: gtk::CheckButton,
    max_results: gtk::SpinButton,
    max_mib: gtk::SpinButton,
    start: gtk::Button,
    cancel: gtk::Button,
    inspect: gtk::Button,
    status: gtk::Label,
    origin: gtk::Label,
    issues: gtk::Expander,
    issue_buffer: gtk::TextBuffer,
    issue_revision: Cell<(usize, u64, u64)>,
    store: gio::ListStore,
    selection: gtk::SingleSelection,
    results: gtk::ColumnView,
    generation: Cell<Option<u64>>,
    running: Cell<bool>,
    handler: RefCell<Option<Handler>>,
}

impl MemorySearchView {
    pub(super) fn new(
        inspector: &gtk::Box,
        split: &gtk::Paned,
        mappings: &gtk::Box,
        map_body: &gtk::Box,
        map_header: &gtk::Box,
        map_view: &gtk::ColumnView,
    ) -> Rc<Self> {
        let root = components::panel();
        let pages = gtk::Stack::builder()
            .vexpand(true)
            .hhomogeneous(false)
            .vhomogeneous(false)
            .transition_type(gtk::StackTransitionType::None)
            .build();

        pages.add_titled(inspector, Some("inspect"), "Inspect");
        let switcher = gtk::StackSwitcher::builder()
            .stack(&pages)
            .halign(gtk::Align::Start)
            .build();

        let mapping_toggle = gtk::ToggleButton::builder()
            .icon_name("pan-down-symbolic")
            .active(true)
            .valign(gtk::Align::Center)
            .css_classes(["icon-action"])
            .build();

        mapping_toggle.update_property(&[gtk::accessible::Property::Label("Memory mappings")]);
        map_header.prepend(&mapping_toggle);
        root.append(&switcher);
        split.set_start_child(Some(&pages));
        root.append(split);
        let collapsed_header = components::panel();
        collapsed_header.add_css_class("memory-map-section");
        collapsed_header.set_visible(false);
        root.append(&collapsed_header);
        let search = components::panel();
        pages.add_titled(&search, Some("search"), "Search");
        let controls = gtk::Box::new(gtk::Orientation::Vertical, components::CONTROL_GAP);
        components::inset(&controls, components::CONTENT_INSET);
        search.append(&controls);
        let header = components::control_row();
        let title = section_title("MEMORY SEARCH");
        title.set_hexpand(true);
        let start = gtk::Button::with_label("Search");
        start.add_css_class("inline-action");
        start.set_sensitive(false);
        let cancel = gtk::Button::with_label("Cancel");
        cancel.add_css_class("inline-action");
        cancel.set_sensitive(false);
        header.append(&title);
        header.append(&start);
        header.append(&cancel);
        controls.append(&header);
        let form = gtk::Box::new(gtk::Orientation::Vertical, components::CONTROL_GAP);
        let pattern_row = components::control_row();
        let kinds: Vec<_> = SearchKind::ALL.iter().map(|(_, label)| *label).collect();
        let kind = gtk::DropDown::from_strings(&kinds);
        let value = search_entry("Text to find, without quotes");
        value.set_max_length(768);
        pattern_row.append(&kind);
        pattern_row.append(&value);
        form.append(&pattern_row);
        let scope_row = components::control_row();
        scope_row.append(&section_title("SCOPE"));
        let scope = gtk::DropDown::from_strings(&["Address range", "Selected mappings"]);
        scope.set_selected(1);
        scope.set_hexpand(true);
        scope_row.append(&scope);
        let mapping_clear =
            components::icon_button("edit-clear-symbolic", "Clear selected mappings");
        mapping_clear
            .update_property(&[gtk::accessible::Property::Label("Clear selected mappings")]);
        mapping_clear.set_sensitive(false);
        scope_row.append(&mapping_clear);
        form.append(&scope_row);
        let range_row = components::control_row();
        let begin = search_entry("0x1000");
        let end = search_entry("0x2000");
        begin.set_max_length(20);
        end.set_max_length(20);
        range_row.append(&field("Start", &begin));
        range_row.append(&field("End (exclusive)", &end));
        form.append(&range_row);
        let mapping_summary = note("Select mappings in the table below");
        mapping_summary.set_lines(3);
        mapping_summary.set_ellipsize(pango::EllipsizeMode::End);
        mapping_summary.set_visible(false);
        enable_stable_text_selection(&mapping_summary);
        form.append(&mapping_summary);
        let limits = gtk::Grid::builder()
            .column_spacing(components::CONTROL_GAP)
            .row_spacing(components::CONTROL_GAP)
            .build();

        let aligned = gtk::CheckButton::with_label("Aligned");
        aligned.set_tooltip_text(Some("Only naturally aligned pointer and numeric matches. Text and byte patterns always search at every address"));
        aligned.set_sensitive(false);
        aligned.set_valign(gtk::Align::Center);
        let max_results = gtk::SpinButton::with_range(1.0, 10_000.0, 100.0);
        max_results.set_value(1000.0);
        max_results.set_width_chars(5);
        let max_mib = gtk::SpinButton::with_range(1.0, 4096.0, 16.0);
        max_mib.set_value(64.0);
        max_mib.set_width_chars(4);
        max_mib.set_tooltip_text(Some(
            "Maximum address-space bytes to examine. Unreadable bytes count toward this limit",
        ));

        for (column, label, input) in [
            (0, "Result limit", &max_results),
            (1, "Scan limit (MiB)", &max_mib),
        ] {
            let label = gtk::Label::builder().label(label).xalign(0.0).build();
            input.set_hexpand(true);
            limits.attach(&label, column, 0, 1, 1);
            limits.attach(input, column, 1, 1, 1);
        }

        limits.attach(&aligned, 2, 1, 1, 1);
        form.append(&limits);
        controls.append(&form);
        let origin = note("Reads through GDB, never the inspector cache");
        origin.set_tooltip_text(Some("Search reads only. GDB's own target cache and executable-backed core sections may still apply. Shared memory may change during a live scan. No target expressions are evaluated. Floating-point searches compare exact stored bits, including the sign of zero"));
        controls.append(&origin);
        let status = note("Choose a pattern and search the selected scope");
        enable_stable_text_selection(&status);
        controls.append(&status);
        let filter_row = components::control_row();
        components::inset(&filter_row, components::CONTROL_GAP);
        filter_row.set_margin_start(components::CONTENT_INSET);
        filter_row.set_margin_end(components::CONTENT_INSET);
        let filter_entry =
            components::delayed_search_entry("Filter results by address, bytes, text, or mapping");
        filter_entry.set_hexpand(true);
        let inspect = gtk::Button::with_label("Inspect");
        inspect.add_css_class("inline-action");
        inspect.set_sensitive(false);
        filter_row.append(&filter_entry);
        filter_row.append(&inspect);
        search.append(&filter_row);
        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        let query = Rc::new(RefCell::new(String::new()));
        let filter_query = Rc::clone(&query);

        let filter = gtk::CustomFilter::new(move |object| {
            object
                .downcast_ref::<glib::BoxedAnyObject>()
                .is_some_and(|object| {
                    object
                        .borrow::<ResultRow>()
                        .filter
                        .contains(filter_query.borrow().as_str())
                })
        });

        let filtered = gtk::FilterListModel::new(Some(store.clone()), Some(filter.clone()));
        let selection = gtk::SingleSelection::new(Some(filtered));
        selection.set_autoselect(false);
        selection.set_can_unselect(true);
        let results = gtk::ColumnView::new(Some(selection.clone()));
        results.add_css_class("debug-table");
        results.set_single_click_activate(false);

        for (index, title, width) in [
            (0, "ADDRESS", 175),
            (1, "MATCHED BYTES", 220),
            (2, "TEXT", 180),
            (3, "MAPPING", 220),
        ] {
            results.append_column(&result_column(index, title, width, &selection));
        }

        let scroll = gtk::ScrolledWindow::builder()
            .child(&results)
            .vexpand(true)
            .margin_start(components::CONTENT_INSET)
            .margin_end(components::CONTENT_INSET)
            .min_content_height(144)
            .overlay_scrolling(false)
            .build();

        search.append(&scroll);
        let issue_buffer = gtk::TextBuffer::new(None);
        let issue_view = gtk::TextView::builder()
            .buffer(&issue_buffer)
            .editable(false)
            .cursor_visible(false)
            .monospace(true)
            .wrap_mode(gtk::WrapMode::WordChar)
            .top_margin(components::CONTENT_INSET)
            .bottom_margin(components::CONTENT_INSET)
            .left_margin(components::CONTENT_INSET)
            .right_margin(components::CONTENT_INSET)
            .build();

        let issue_scroll = gtk::ScrolledWindow::builder()
            .child(&issue_view)
            .min_content_height(96)
            .max_content_height(96)
            .propagate_natural_height(true)
            .overlay_scrolling(false)
            .build();

        let issues = gtk::Expander::builder()
            .label("Unreadable / skipped ranges")
            .child(&issue_scroll)
            .visible(false)
            .build();

        issues.set_tooltip_text(Some("These bytes were not searched. GDB may skip readable islands inside a failed range. Failed large reads are retried in 4 KiB pieces, not byte by byte"));
        search.append(&issues);
        let mapping_search = gtk::Button::with_label("Search selected");
        mapping_search.add_css_class("inline-action");
        mapping_search.set_tooltip_text(Some("Search selected mappings. Ctrl-click or Shift-click to select multiple mappings, double-click to inspect one"));
        map_header.append(&mapping_search);

        let view = Rc::new(Self {
            root,
            pages,
            kind,
            value,
            scope,
            mappings: mappings.clone(),
            mapping_body: map_body.clone(),
            mapping_header: map_header.clone(),
            collapsed_header,
            mapping_toggle,
            mapping_clear,
            mapping_search: mapping_search.clone(),
            mapping_selection: map_view
                .model()
                .expect("memory mappings have a selection model"),
            mapping_summary,
            range_row,
            begin,
            end,
            aligned,
            max_results,
            max_mib,
            start,
            cancel,
            inspect,
            status,
            origin,
            issues,
            issue_buffer,
            issue_revision: Cell::new((0, 0, 0)),
            store,
            selection,
            results,
            generation: Cell::new(None),
            running: Cell::new(false),
            handler: RefCell::new(None),
        });

        filter_entry.connect_search_changed(move |entry| {
            query.replace(entry.text().to_ascii_lowercase());
            filter.changed(gtk::FilterChange::Different);
        });

        let weak = Rc::downgrade(&view);
        mapping_search.connect_clicked(move |_| {
            if let Some(view) = weak.upgrade() {
                view.mapping_toggle.set_active(true);
                view.scope.set_selected(1);
                view.pages.set_visible_child_name("search");
                view.value.grab_focus();
            }
        });

        let weak = Rc::downgrade(&view);
        view.mapping_toggle.connect_toggled(move |_| {
            if let Some(view) = weak.upgrade() {
                view.update_scope();
            }
        });

        let weak = Rc::downgrade(&view);
        view.mapping_clear.connect_clicked(move |_| {
            if let Some(view) = weak.upgrade().filter(|view| !view.running.get()) {
                view.mapping_selection.unselect_all();
            }
        });

        let weak = Rc::downgrade(&view);
        view.scope.connect_selected_notify(move |_| {
            if let Some(view) = weak.upgrade() {
                view.update_scope();
            }
        });

        let weak = Rc::downgrade(&view);
        view.pages.connect_visible_child_name_notify(move |_| {
            if let Some(view) = weak.upgrade() {
                view.update_scope();
            }
        });

        let weak = Rc::downgrade(&view);
        view.mapping_selection
            .connect_selection_changed(move |_, _, _| {
                if let Some(view) = weak.upgrade() {
                    view.update_mapping_summary();
                }
            });

        let weak = Rc::downgrade(&view);
        view.mapping_selection
            .connect_items_changed(move |_, _, _, _| {
                if let Some(view) = weak.upgrade() {
                    view.update_mapping_summary();
                }
            });

        let weak = Rc::downgrade(&view);
        view.kind.connect_selected_notify(move |_| {
            if let Some(view) = weak.upgrade() {
                let kind = view.selected_kind();
                view.update_alignment();

                view.value.set_placeholder_text(Some(match kind {
                    SearchKind::Text => "Text to find, without quotes",
                    SearchKind::Bytes => "48 65 ?? 6c 6f",
                    SearchKind::Pointer => "Pointer value, such as 0x7fffffffe000",
                    _ => "Numeric value",
                }));
            }
        });

        let weak = Rc::downgrade(&view);
        view.selection.connect_selected_notify(move |_| {
            if let Some(view) = weak.upgrade() {
                view.inspect.set_sensitive(
                    view.generation.get().is_some() && view.selection.selected_item().is_some(),
                );
            }
        });

        view.update_scope();
        view
    }

    fn update_scope(&self) {
        let selected_mappings = self.scope.selected() == 1;
        let available =
            selected_mappings || self.pages.visible_child_name().as_deref() != Some("search");

        let expanded = self.mapping_toggle.is_active();
        let restore_focus = self.mapping_toggle.is_focus();
        self.range_row.set_visible(!selected_mappings);
        self.mapping_summary.set_visible(selected_mappings);
        self.mapping_clear.set_visible(selected_mappings);
        self.mapping_toggle.set_visible(available);

        // Keep the same header accessible while hiding the paned child. This
        // gives its space to results without changing the saved divider position.
        let header_parent = if expanded {
            &self.mappings
        } else {
            &self.collapsed_header
        };

        if self.mapping_header.parent().as_ref() != Some(header_parent.upcast_ref()) {
            if expanded {
                self.collapsed_header.remove(&self.mapping_header);
                self.mappings.prepend(&self.mapping_header);
            } else {
                self.mappings.remove(&self.mapping_header);
                self.collapsed_header.append(&self.mapping_header);
            }
        }

        self.mappings.set_visible(available && expanded);
        self.collapsed_header.set_visible(available && !expanded);
        self.mapping_search.set_visible(expanded);

        if available && restore_focus {
            self.mapping_toggle.grab_focus();
        }

        self.mapping_toggle.set_icon_name(if expanded {
            "pan-down-symbolic"
        } else {
            "pan-end-symbolic"
        });

        self.mapping_toggle.set_tooltip_text(Some(if expanded {
            "Collapse memory mappings"
        } else {
            "Expand memory mappings"
        }));

        self.mapping_toggle
            .update_state(&[gtk::accessible::State::Expanded(Some(expanded))]);

        self.update_mapping_summary();
    }

    fn update_mapping_summary(&self) {
        let selected = !self.mapping_selection.selection().is_empty();
        let running = self.running.get();
        set_execution_sensitive(
            &self.mapping_clear,
            selected && !running,
            selected && running,
        );

        if self.running.get() {
            return;
        }

        let (mut summary, details) = mapping_selection_summary(&self.mapping_selection);

        if !self.mapping_toggle.is_active() && self.mapping_selection.selection().is_empty() {
            summary = String::from("Expand memory mappings to select a search scope");
        }

        self.mapping_summary.set_text(&summary);
        self.mapping_summary.set_tooltip_text(details.as_deref());
    }

    fn selected_kind(&self) -> SearchKind {
        SearchKind::ALL
            .get(self.kind.selected() as usize)
            .map_or(SearchKind::Text, |(kind, _)| *kind)
    }

    fn can_start(&self) -> bool {
        self.start.is_sensitive() && !self.running.get()
    }

    fn update_alignment(&self) {
        let available = !matches!(self.selected_kind(), SearchKind::Text | SearchKind::Bytes);
        let running = self.running.get();
        set_execution_sensitive(&self.aligned, available && !running, available && running);
    }

    fn set_running(&self, running: bool) {
        self.running.set(running);

        // Lock the query without repainting its chrome. Apply the lock to
        // controls individually so unavailable options keep their disabled paint.
        for widget in [
            self.kind.upcast_ref::<gtk::Widget>(),
            self.value.upcast_ref(),
            self.scope.upcast_ref(),
            self.begin.upcast_ref(),
            self.end.upcast_ref(),
            self.max_results.upcast_ref(),
            self.max_mib.upcast_ref(),
            self.mapping_body.upcast_ref(),
            self.mapping_search.upcast_ref(),
        ] {
            set_execution_sensitive(widget, !running, running);
        }

        self.update_alignment();
        self.update_mapping_summary();
        self.cancel.set_sensitive(running);
    }

    pub(super) fn show_inspector(&self) {
        self.pages.set_visible_child_name("inspect");
    }

    fn emit(&self, action: Action) {
        let handler = self.handler.borrow().clone();

        if let Some(handler) = handler {
            handler(action);
        }
    }

    fn query(&self, ui: &Ui) -> Result<Query, String> {
        let ranges = if self.scope.selected() == 0 {
            std::iter::once(parse_address(&self.begin.text())?..parse_address(&self.end.text())?)
                .collect()
        } else {
            if !ui
                .model
                .memory_regions_are_current(ui.model.current_stop_refresh_generation())
            {
                return Err(String::from(
                    "Wait for current mappings, or enter an explicit address range",
                ));
            }

            let model = &self.mapping_selection;
            let selection = model.selection();

            if selection.size() as usize > MAX_RANGES {
                return Err(String::from("Select at most 4096 mappings"));
            }

            let mut ranges = Vec::new();

            for index in selected_mapping_indices(&selection) {
                if let Some(object) = model.item(index).and_downcast::<glib::BoxedAnyObject>() {
                    let region = object.borrow::<MemoryRegion>();
                    ranges.push(region.start..region.end);
                }
            }

            if ranges.is_empty() {
                self.mapping_toggle.set_active(true);
                return Err(String::from(
                    "Select mappings in the table below. Use Ctrl-click or Shift-click for multiple mappings",
                ));
            }

            ranges
        };

        Ok(Query {
            kind: self.selected_kind(),
            value: self.value.text().to_string(),
            ranges,
            aligned: self.aligned.is_active(),
            max_results: self.max_results.value_as_int() as usize,
            max_bytes: self.max_mib.value_as_int() as u64 * 1024 * 1024,
        })
    }

    pub(super) fn update_state(&self, ui: &Ui) {
        let generation = ui.model.current_stop_refresh_generation();
        let current = ui
            .model
            .stop_context(generation)
            .is_some_and(|context| ui.model.is_stop_context_current(&context));

        set_transient_execution_sensitive(
            &self.start,
            current && ui.model.debugger_synchronization_available() && !self.running.get(),
            current && self.running.get(),
        );

        if self
            .generation
            .get()
            .is_some_and(|saved| saved != generation || !current)
        {
            self.generation.set(None);
            self.inspect.set_sensitive(false);
            self.status
                .set_text(&format!("Earlier stopped context - {}", self.status.text()));
        }
    }

    fn begin(&self, generation: u64, origin: &str) {
        self.store.remove_all();
        self.issue_buffer.set_text("");
        self.issue_revision.set((0, 0, 0));
        self.issues.set_visible(false);
        self.generation.set(Some(generation));
        self.origin.set_text(origin);
        self.set_running(true);
        // Preserve the initiating button's hover and focus. Both mouse and
        // keyboard submission use can_start to reject another active search.
        set_transient_execution_sensitive(&self.start, false, true);
        self.status.set_text("Searching…");
    }

    fn render(&self, ui: &Ui, progress: &Progress, phase: &str, finished: bool) {
        let count = self.store.n_items() as usize;
        let regions = ui.model.memory_regions();
        let rows: Vec<_> = progress.hits[count..]
            .iter()
            .map(|hit| glib::BoxedAnyObject::new(result_row(hit, &regions)))
            .collect();
        drop(regions);

        if !rows.is_empty() {
            self.store.splice(count as u32, 0, &rows);
        }

        self.status.set_text(&format!(
            "{phase}  {} matches  {} searched  {} skipped  {} remaining",
            progress.hits.len(),
            format_memory_size(progress.searched),
            format_memory_size(progress.skipped),
            format_memory_size(progress.remaining()),
        ));

        let revision = (
            progress.issues.len(),
            progress.issues.last().map_or(0, |issue| issue.range.end),
            progress.omitted_issues,
        );

        if self.issue_revision.replace(revision) != revision {
            use std::fmt::Write as _;
            let mut text = String::new();

            for issue in &progress.issues {
                let _ = writeln!(
                    text,
                    "0x{:x}-0x{:x} (end exclusive)  {}",
                    issue.range.start, issue.range.end, issue.reason
                );
            }

            if progress.omitted_issues != 0 {
                let _ = write!(
                    text,
                    "{} additional skipped spans. Their bytes are included in the totals",
                    progress.omitted_issues
                );
            }

            self.issue_buffer.set_text(&text);
            self.issues.set_visible(!text.is_empty());
        }

        if finished {
            self.set_running(false);
        }

        self.update_state(ui);
    }
}

impl Ui {
    pub(crate) fn connect_memory_search_actions(
        self: &Rc<Self>,
        handler: impl Fn(Action) + 'static,
    ) {
        let view = &self.memory_search;
        view.handler.replace(Some(Rc::new(handler)));
        let weak = Rc::downgrade(self);

        view.start.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade().filter(|ui| ui.memory_search.can_start()) {
                match ui.memory_search.query(&ui) {
                    Ok(query) => ui.memory_search.emit(Action::Start(query)),
                    Err(error) => ui.memory_search.status.set_text(&error),
                }
            }
        });

        let weak = Rc::downgrade(view);
        view.cancel.connect_clicked(move |_| {
            if let Some(view) = weak.upgrade() {
                view.emit(Action::Cancel);
            }
        });

        let weak = Rc::downgrade(view);
        view.value.connect_activate(move |_| {
            if let Some(view) = weak.upgrade().filter(|view| view.can_start()) {
                view.start.emit_clicked();
            }
        });

        let weak = Rc::downgrade(view);
        view.inspect.connect_clicked(move |_| {
            if let Some(view) = weak.upgrade()
                && view.generation.get().is_some()
                && let Some(object) = view
                    .selection
                    .selected_item()
                    .and_downcast::<glib::BoxedAnyObject>()
            {
                let address = object.borrow::<ResultRow>().address;
                view.emit(Action::Inspect(address));
            }
        });

        let button = view.inspect.downgrade();
        view.results.connect_activate(move |_, _| {
            if let Some(button) = button.upgrade().filter(|button| button.is_sensitive()) {
                button.emit_clicked();
            }
        });
    }

    pub(crate) fn begin_memory_search(&self, generation: u64, origin: &str) {
        self.memory_search.begin(generation, origin);
    }

    pub(crate) fn show_memory_search(&self, progress: &Progress, phase: &str, finished: bool) {
        self.memory_search.render(self, progress, phase, finished);
    }

    pub(crate) fn memory_search_error(&self, message: &str) {
        self.memory_search.status.set_text(message);
    }

    pub(crate) fn inspect_memory_search_hit(&self, address: u64) {
        if self.memory_search.generation.get() != Some(self.model.current_stop_refresh_generation())
        {
            return;
        }

        if add_memory_watch(
            &self.memory_watch_container,
            &self.memory_watches,
            &self.memory_watch_handler,
            format!("0x{address:x}"),
            256,
            MemoryWatchFormat::Bytes,
        ) {
            self.memory_search.show_inspector();
        } else {
            self.memory_search
                .status
                .set_text("Close an inspector before adding another (limit 256)");
        }
    }
}

fn selected_mapping_indices(selection: &gtk::Bitset) -> impl Iterator<Item = u32> + '_ {
    gtk::BitsetIter::init_first(selection)
        .into_iter()
        .flat_map(|(iter, first)| std::iter::once(first).chain(iter))
}

fn mapping_selection_summary(model: &gtk::SelectionModel) -> (String, Option<String>) {
    use std::fmt::Write as _;
    let selection = model.selection();
    let count = selection.size();

    if count == 0 {
        return (String::from("Select mappings in the table below"), None);
    }

    if count > MAX_RANGES as u64 {
        return (
            format!("{count} mappings selected - select at most {MAX_RANGES}"),
            None,
        );
    }

    let mut bytes = 0_u64;
    let mut details = String::new();
    let mut first = String::new();

    for (ordinal, index) in selected_mapping_indices(&selection).enumerate() {
        let Some(object) = model.item(index).and_downcast::<glib::BoxedAnyObject>() else {
            continue;
        };

        let region = object.borrow::<MemoryRegion>();
        bytes = bytes.saturating_add(region.end.saturating_sub(region.start));

        if ordinal < 16 {
            let line = format!(
                "0x{:x}-0x{:x}  {}  {}",
                region.start,
                region.end,
                region.permissions,
                region
                    .path
                    .as_deref()
                    .filter(|path| !path.is_empty())
                    .unwrap_or("anonymous"),
            );

            if ordinal == 0 {
                first.clone_from(&line);
            }

            let _ = writeln!(details, "{line}");
        }
    }

    if count > 16 {
        let _ = writeln!(
            details,
            "{} more selected mappings in the table below",
            count - 16
        );
    }

    details.push_str(
        "End addresses are exclusive. Ctrl-click or Shift-click to select multiple mappings",
    );
    let noun = if count == 1 { "mapping" } else { "mappings" };
    let mut summary = format!(
        "{count} {noun} selected  {}\n{first}",
        format_memory_size(bytes)
    );

    if count > 1 {
        let _ = write!(summary, "\n{} more selected mappings below", count - 1);
    }

    (summary, Some(details))
}

fn search_entry(placeholder: &str) -> gtk::Entry {
    gtk::Entry::builder()
        .placeholder_text(placeholder)
        .hexpand(true)
        .width_chars(1)
        .build()
}

fn field(label: &str, widget: &impl IsA<gtk::Widget>) -> gtk::Box {
    let field = gtk::Box::new(gtk::Orientation::Vertical, components::CONTROL_GAP);
    field.set_hexpand(true);
    field.append(&gtk::Label::builder().label(label).xalign(0.0).build());
    field.append(widget);

    field
}

fn note(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .wrap(true)
        .css_classes(["muted"])
        .build()
}

fn result_row(hit: &Hit, regions: &[MemoryRegion]) -> ResultRow {
    use std::fmt::Write as _;
    let mut bytes = String::with_capacity(hit.bytes.len() * 3);

    for (index, byte) in hit.bytes.iter().enumerate() {
        if index != 0 {
            bytes.push(' ');
        }

        let _ = write!(bytes, "{byte:02x}");
    }

    let text: String = String::from_utf8_lossy(&hit.bytes)
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect();

    let cells = [
        format!("0x{:016x}", hit.address),
        bytes,
        text,
        memory_region_for_address(regions, hit.address)
            .map(MemoryRegion::description)
            .unwrap_or_default(),
    ];

    ResultRow {
        address: hit.address,
        filter: cells.join(" ").to_ascii_lowercase(),
        cells,
    }
}

fn result_column(
    index: usize,
    title: &str,
    width: i32,
    selection: &gtk::SingleSelection,
) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    let selection = selection.clone();

    factory.connect_setup(move |_, object| {
        if let Some(item) = object.downcast_ref::<gtk::ListItem>() {
            let label = gtk::Label::builder()
                .xalign(0.0)
                .ellipsize(pango::EllipsizeMode::End)
                .css_classes(["debug-table-cell"])
                .build();

            enable_recycled_text_selection(&label);
            let click = gtk::GestureClick::new();
            click.set_propagation_phase(gtk::PropagationPhase::Capture);
            let weak_item = item.downgrade();
            let selection = selection.clone();

            click.connect_pressed(move |_, _, _, _| {
                if let Some(item) = weak_item.upgrade()
                    && item.position() != gtk::INVALID_LIST_POSITION
                    && item.item() == selection.item(item.position())
                {
                    selection.set_selected(item.position());
                }
            });

            label.add_controller(click);
            item.set_child(Some(&label));
        }
    });

    factory.connect_bind(move |_, object| {
        let Some(item) = object.downcast_ref::<gtk::ListItem>() else {
            return;
        };

        if let (Some(label), Some(data)) = (
            item.child().and_downcast::<gtk::Label>(),
            item.item().and_downcast::<glib::BoxedAnyObject>(),
        ) {
            clear_label_selection(&label);
            let data = data.borrow::<ResultRow>();
            label.set_text(&data.cells[index]);
            label.set_tooltip_text(Some(&data.cells[index]));
        }
    });

    factory.connect_unbind(|_, object| {
        if let Some(item) = object.downcast_ref::<gtk::ListItem>()
            && let Some(label) = item.child().and_downcast::<gtk::Label>()
        {
            clear_label_selection(&label);
        }
    });

    gtk::ColumnViewColumn::builder()
        .title(title)
        .factory(&factory)
        .fixed_width(width)
        .resizable(true)
        .expand(index == 3)
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires a GTK display, run separately from other GTK tests"]
    fn mapping_scope_shares_selection_and_stays_visible_in_search() {
        gtk::init().unwrap();
        Theme::graphite().install();
        let filter = components::delayed_search_entry("Filter mappings");
        let (table, store) = build_memory_region_view(&Rc::new(Cell::new(64)), &filter);

        replace_boxed_store(
            &store,
            (0..20).map(|index| MemoryRegion {
                start: 0x1000 + index * 0x1000,
                end: 0x2000 + index * 0x1000,
                permissions: String::from("rw-p"),
                path: Some(format!("/tmp/mapping-{index}")),
                kind: MemoryKind::Writable,
                referenced_by: Vec::new(),
            }),
        );

        let mappings = components::panel();
        mappings.add_css_class("memory-map-section");
        let header = components::control_row();
        header.add_css_class("subpanel-header");
        header.append(&section_title("VIRTUAL MEMORY MAP"));
        mappings.append(&header);
        let body = components::panel();
        body.append(&filter);

        let scroll = gtk::ScrolledWindow::builder()
            .child(&table)
            .min_content_height(48)
            .vexpand(true)
            .overlay_scrolling(false)
            .build();

        body.append(&scroll);
        mappings.append(&body);
        let split = gtk::Paned::new(gtk::Orientation::Vertical);
        split.set_end_child(Some(&mappings));
        split.set_shrink_start_child(false);
        let view = MemorySearchView::new(
            &components::panel(),
            &split,
            &mappings,
            &body,
            &header,
            &table,
        );
        let selection = table.model().unwrap();
        selection.select_item(0, true);
        assert!(view.mapping_summary.text().contains("1 mapping selected"));
        assert!(view.mapping_summary.text().contains("0x1000-0x2000"));
        assert!(view.mapping_summary.text().contains("/tmp/mapping-0"));
        view.pages.set_visible_child_name("search");
        assert!(mappings.get_visible());
        view.scope.set_selected(0);
        assert!(!mappings.get_visible());
        assert!(!view.mapping_toggle.get_visible());
        view.scope.set_selected(1);
        assert!(mappings.get_visible());
        assert!(view.mapping_toggle.get_visible());
        assert!(view.mapping_summary.get_visible());
        assert!(!view.range_row.get_visible());
        assert_eq!(table.model().unwrap(), view.mapping_selection);
        selection.select_item(3, false);
        assert_eq!(
            selected_mapping_indices(&selection.selection()).collect::<Vec<_>>(),
            [0, 3]
        );

        assert!(view.mapping_summary.text().contains("2 mappings selected"));
        assert!(view.mapping_summary.text().contains("8.0 KiB"));
        assert!(
            view.mapping_summary
                .tooltip_text()
                .unwrap()
                .contains("0x4000-0x5000")
        );

        view.show_inspector();
        view.pages.set_visible_child_name("search");
        assert_eq!(selection.selection().size(), 2);

        let window = gtk::Window::builder()
            .default_width(500)
            .default_height(800)
            .child(&view.root)
            .build();

        window.present();
        let icons = gtk::IconTheme::for_display(&gtk::prelude::WidgetExt::display(&window));
        assert!(icons.has_icon("pan-down-symbolic"));
        assert!(icons.has_icon("pan-end-symbolic"));
        assert!(icons.has_icon("edit-clear-symbolic"));
        let main = glib::MainContext::default();
        main.block_on(glib::timeout_future(Duration::from_millis(50)));
        assert!(table.is_mapped());
        assert!(view.results.is_mapped());
        assert!(scroll.height() >= 48);
        assert!(view.results.parent().unwrap().height() >= 144);
        let limit_bounds = view.max_mib.compute_bounds(&view.root).unwrap();

        let checkbox_bounds = view
            .aligned
            .first_child()
            .unwrap()
            .compute_bounds(&view.root)
            .unwrap();

        assert_eq!(
            checkbox_bounds.y() - limit_bounds.y(),
            limit_bounds.y() + limit_bounds.height()
                - checkbox_bounds.y()
                - checkbox_bounds.height(),
        );

        assert_mapping_toggle_spacing(&view);
        let expanded_position = split.position();
        let expanded_height = view.results.parent().unwrap().height();
        view.mapping_toggle.grab_focus();
        view.mapping_toggle.emit_clicked();
        main.block_on(glib::timeout_future(Duration::from_millis(50)));
        assert!(!table.is_mapped());
        assert!(header.is_mapped());
        assert!(view.mapping_toggle.is_focus());
        assert_mapping_toggle_spacing(&view);
        assert_eq!(
            view.collapsed_header.height() as f32,
            header
                .compute_bounds(&view.collapsed_header)
                .unwrap()
                .height()
        );

        assert!(view.mapping_toggle.is_mapped());
        assert!(view.mapping_summary.is_mapped());
        assert!(view.results.parent().unwrap().height() > expanded_height);
        assert_eq!(
            view.mapping_toggle.icon_name().as_deref(),
            Some("pan-end-symbolic")
        );

        assert_eq!(selection.selection().size(), 2);
        assert!(view.mapping_clear.is_sensitive());
        view.mapping_clear.emit_clicked();
        assert!(selection.selection().is_empty());
        assert!(!view.mapping_clear.is_sensitive());
        assert_eq!(store.n_items(), 20);
        selection.select_item(0, true);
        selection.select_item(3, false);
        view.show_inspector();
        assert!(!mappings.get_visible());
        view.pages.set_visible_child_name("search");
        assert!(!mappings.get_visible());
        view.mapping_toggle.set_active(true);
        main.block_on(glib::timeout_future(Duration::from_millis(50)));
        assert!(table.is_mapped());
        assert_eq!(split.position(), expanded_position);
        assert_eq!(
            view.mapping_toggle.icon_name().as_deref(),
            Some("pan-down-symbolic")
        );

        let hit = Hit {
            address: 0x1010,
            bytes: vec![0x41, 0x42],
        };

        let row = result_row(&hit, &[]);
        view.store.append(&glib::BoxedAnyObject::new(row));
        view.selection.set_selected(0);
        main.block_on(glib::timeout_future(Duration::from_millis(50)));
        let address = find_result_label(view.results.upcast_ref(), "0x0000000000001010").unwrap();
        let clipboard = address.display().clipboard();
        assert!(address.is_selectable());
        let cell_bounds = address
            .parent()
            .unwrap()
            .compute_bounds(&view.results)
            .unwrap();

        let layout_origin = address.layout_offsets();

        let origin = address
            .compute_point(
                &view.results,
                &gtk::graphene::Point::new(layout_origin.0 as f32, layout_origin.1 as f32),
            )
            .unwrap();

        assert_eq!(origin.x() - cell_bounds.x(), 4.0);
        address.select_region(0, -1);
        address.emit_by_name::<()>("copy-clipboard", &[]);
        assert_eq!(
            main.block_on(clipboard.read_text_future())
                .unwrap()
                .as_deref(),
            Some("0x0000000000001010")
        );

        view.store
            .splice(0, 1, &[glib::BoxedAnyObject::new(result_row(&hit, &[]))]);

        main.block_on(glib::timeout_future(Duration::from_millis(50)));
        assert!(address.selection_bounds().is_none());
        view.store.remove_all();

        // Compare actual paint as well as sensitivity. Ancestor locks can
        // otherwise change dropdown surfaces and table-header colors unnoticed.
        gtk::prelude::GtkWindowExt::set_focus(&window, None::<&gtk::Widget>);

        for (kind, hovered) in [(0, false), (2, false), (2, true)] {
            view.kind.set_selected(kind);
            view.value
                .set_text(if kind == 0 { "fgdb" } else { "0x410000" });

            view.max_results.set_value(1.0);
            view.start.set_sensitive(true);

            if hovered {
                view.start.set_state_flags(gtk::StateFlags::PRELIGHT, false);
            }

            main.block_on(glib::timeout_future(Duration::from_millis(50)));

            let widgets = [
                view.start.upcast_ref::<gtk::Widget>(),
                view.kind.upcast_ref(),
                view.value.upcast_ref(),
                view.scope.upcast_ref(),
                view.aligned.upcast_ref(),
                view.max_results.upcast_ref(),
                view.max_mib.upcast_ref(),
                view.mapping_clear.upcast_ref(),
                view.mapping_search.upcast_ref(),
                &table.first_child().unwrap(),
            ];

            let before: Vec<_> = widgets
                .iter()
                .map(|widget| paint(&window, widget))
                .collect();

            let flags: Vec<_> = widgets.iter().map(|widget| widget.state_flags()).collect();
            view.begin(1, "Target memory through GDB");
            main.block_on(glib::timeout_future(Duration::from_millis(50)));

            for (index, (widget, expected)) in widgets.iter().zip(&before).enumerate() {
                assert_eq!(
                    widget.is_sensitive(),
                    index == 0,
                    "{} lock state",
                    widget.type_()
                );

                let actual = paint(&window, widget);

                assert!(
                    same_paint(expected, &actual),
                    "{} changed paint during search (kind {kind}, index {index}, flags {:?} -> {:?}, classes {:?})",
                    widget.type_(),
                    flags[index],
                    widget.state_flags(),
                    widget.css_classes()
                );
            }

            assert!(!view.can_start());
            assert!(view.cancel.is_sensitive());
            view.set_running(false);
            set_transient_execution_sensitive(&view.start, true, false);
            main.block_on(glib::timeout_future(Duration::from_millis(50)));

            for (widget, expected) in widgets.iter().zip(&before) {
                assert!(
                    same_paint(expected, &paint(&window, widget)),
                    "{} changed paint after search (kind {kind})",
                    widget.type_()
                );
            }

            assert!(!view.cancel.is_sensitive());
            assert!(view.can_start());
            assert_eq!(view.aligned.is_sensitive(), kind == 2);

            view.start.unset_state_flags(gtk::StateFlags::PRELIGHT);
        }

        let summary = view.mapping_summary.text();
        view.begin(1, "Target memory through GDB");
        assert!(!body.is_sensitive());
        assert!(!view.mapping_clear.is_sensitive());
        view.mapping_clear.emit_clicked();
        assert_eq!(selection.selection().size(), 2);
        assert!(view.mapping_toggle.is_sensitive());
        view.mapping_toggle.set_active(false);
        assert!(!mappings.get_visible());
        view.mapping_toggle.set_active(true);
        assert!(mappings.get_visible());
        assert!(!body.is_sensitive());
        selection.unselect_all();
        assert_eq!(view.mapping_summary.text(), summary);
        view.set_running(false);
        assert_eq!(
            view.mapping_summary.text(),
            "Select mappings in the table below"
        );

        selection.select_all();
        let (_, details) = mapping_selection_summary(&selection);
        assert!(details.unwrap().contains("4 more selected mappings"));
        store.remove_all();
        assert_eq!(
            view.mapping_summary.text(),
            "Select mappings in the table below"
        );

        view.mapping_toggle.set_active(false);
        assert_eq!(
            view.mapping_summary.text(),
            "Expand memory mappings to select a search scope"
        );

        window.close();
    }

    fn paint(window: &gtk::Window, widget: &gtk::Widget) -> Vec<u8> {
        use gtk::gsk::prelude::*;

        let snapshot = gtk::Snapshot::new();
        let paintable = gtk::WidgetPaintable::new(Some(widget));
        paintable.snapshot(
            &snapshot,
            f64::from(widget.width()),
            f64::from(widget.height()),
        );

        let node = snapshot
            .to_node()
            .expect("mapped control has visible paint");

        let texture = window.renderer().unwrap().render_texture(&node, None);
        let stride = texture.width() as usize * 4;
        let mut pixels = vec![0; stride * texture.height() as usize];
        texture.download(&mut pixels, stride);
        pixels
    }

    fn same_paint(expected: &[u8], actual: &[u8]) -> bool {
        // Allow one channel level for rasterization rounding, not dimmed text
        // or changed backgrounds and borders.
        expected.len() == actual.len()
            && expected
                .iter()
                .zip(actual)
                .all(|(before, after)| before.abs_diff(*after) <= 1)
    }

    fn assert_mapping_toggle_spacing(view: &MemorySearchView) {
        let header = view.mapping_header.compute_bounds(&view.root).unwrap();
        let button = view.mapping_toggle.compute_bounds(&view.root).unwrap();
        assert_eq!(button.width(), button.height());
        assert_eq!(button.x() - header.x(), 2.0);
        assert_eq!(button.y() - header.y(), 2.0);
        // The header's bottom divider is outside the button's content inset.
        assert_eq!(
            header.y() + header.height() - 1.0 - button.y() - button.height(),
            2.0
        );

        view.mapping_toggle
            .set_state_flags(gtk::StateFlags::PRELIGHT, false);

        glib::MainContext::default().block_on(glib::timeout_future(Duration::from_millis(50)));
        assert_eq!(
            view.mapping_toggle.compute_bounds(&view.root).unwrap(),
            button
        );

        view.mapping_toggle
            .unset_state_flags(gtk::StateFlags::PRELIGHT);
    }

    fn find_result_label(widget: &gtk::Widget, text: &str) -> Option<gtk::Label> {
        if let Some(label) = widget.downcast_ref::<gtk::Label>()
            && label.has_css_class("debug-table-cell")
            && label.text() == text
        {
            return Some(label.clone());
        }

        let mut child = widget.first_child();

        while let Some(current) = child {
            if let Some(label) = find_result_label(&current, text) {
                return Some(label);
            }

            child = current.next_sibling();
        }

        None
    }
}
