//! Two FD presentations share one snapshot; socket details never widen the file table.

use super::*;
use crate::kernel::sockets::{Protocol, Queue};
use crate::kernel::{KernelFact, fact};
use std::fmt::Write as _;

#[cfg(test)]
mod tests;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Row {
    fd: Rc<KernelFileDescriptor>,
    search: String,
    change: String,
    receive_changed: bool,
    send_changed: bool,
    state_changed: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Column {
    Number,
    Kind,
    Access,
    Flags,
    SocketFlags,
    Position,
    Target,
    Details,
    Protocol,
    State,
    Local,
    Peer,
    Receive,
    Send,
    Change,
}

struct Table {
    selection: gtk::SingleSelection,
    filter: gtk::CustomFilter,
    view: gtk::ColumnView,
    scroll: gtk::ScrolledWindow,
    empty: gtk::Label,
}

#[derive(Default)]
struct Query {
    text: String,
    protocol: String,
    state: String,
}

struct DetailSection {
    root: gtk::Box,
    grid: gtk::Grid,
    title: &'static str,
    facts: RefCell<Vec<KernelFact>>,
}

impl DetailSection {
    fn new(title: &'static str) -> Self {
        let root = components::card();
        root.append(&section_title(title));

        let grid = gtk::Grid::builder()
            .column_spacing(16)
            .row_spacing(components::CONTROL_GAP)
            .hexpand(true)
            .build();

        root.append(&grid);
        root.set_visible(false);
        Self {
            root,
            grid,
            title,
            facts: RefCell::new(Vec::new()),
        }
    }

    fn set(&self, facts: Vec<KernelFact>) {
        self.root.set_visible(!facts.is_empty());
        let mut previous = self.facts.borrow_mut();

        if *previous == facts {
            return;
        }

        for (row, fact) in facts.iter().enumerate() {
            for (column, text) in [(0, &fact.label), (1, &fact.value)] {
                let label = self
                    .grid
                    .child_at(column, row as i32)
                    .and_downcast::<gtk::Label>()
                    .unwrap_or_else(|| {
                        let label = detail_label();

                        if column == 0 {
                            label.add_css_class("muted");
                            label.set_width_chars(18);
                            label.set_hexpand(false);
                        }

                        self.grid.attach(&label, column, row as i32, 1, 1);
                        label
                    });

                set_label_text(&label, text);
            }
        }

        for row in (facts.len()..previous.len()).rev() {
            self.grid.remove_row(row as i32);
        }

        *previous = facts;
    }

    fn text(&self) -> String {
        let facts = self.facts.borrow();

        if facts.is_empty() {
            return String::new();
        }

        let mut text = format!("{}\n", self.title);

        for fact in facts.iter() {
            let _ = writeln!(text, "{}: {}", fact.label, fact.value);
        }

        text
    }
}

pub(in crate::ui) struct DescriptorView {
    pub(super) root: gtk::Box,
    store: gio::ListStore,
    rows: RefCell<Vec<Row>>,
    tables: [Table; 2],
    pages: gtk::Stack,
    mode: Cell<usize>,
    all: gtk::ToggleButton,
    sockets: gtk::ToggleButton,
    query: Rc<RefCell<Query>>,
    search: gtk::SearchEntry,
    protocol: gtk::DropDown,
    state: gtk::DropDown,
    summary: gtk::Label,
    changes: gtk::Label,
    details: gtk::ToggleButton,
    detail_pages: gtk::Stack,
    detail_empty: gtk::Label,
    descriptor: DetailSection,
    socket: DetailSection,
    socket_note: gtk::Label,
    raw_info: gtk::Label,
    raw_socket: gtk::Label,
    links: gtk::Box,
    link_entries: RefCell<Vec<(String, Option<Rc<KernelFileDescriptor>>)>>,
    details_dirty: Cell<bool>,
    copy: gtk::Button,
    copy_local: gtk::Button,
    copy_peer: gtk::Button,
    inspect: gtk::Button,
    diagnostics: DetailSection,
    pub(super) diagnostic_handler: RefCell<Option<Rc<dyn Fn()>>>,
    identity: Cell<Option<(u32, u64)>>,
    captured_at: Cell<u64>,
    complete: Cell<bool>,
    comparison_stop: Cell<Option<u64>>,
    comparison_ready: Cell<bool>,
    previous: RefCell<HashMap<u32, Rc<KernelFileDescriptor>>>,
    updating: Cell<bool>,
    fresh: Cell<bool>,
    stamp: Cell<u64>,
    pending: Cell<bool>,
    commands_allowed: Cell<bool>,
}

impl DescriptorView {
    pub(super) fn build(columns: &ColumnLayouts) -> Rc<Self> {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let controls = components::control_row();
        controls.add_css_class("kernel-table-controls");
        let all = gtk::ToggleButton::with_label("All FDs");
        let sockets = gtk::ToggleButton::with_label("Sockets");
        sockets.set_group(Some(&all));
        all.set_active(true);

        for button in [&all, &sockets] {
            button.add_css_class("inline-action");
            controls.append(button);
        }

        let search =
            components::delayed_search_entry("Filter FD, path, inode, flags, address, or state");
        search.set_hexpand(true);
        search.set_width_chars(8);
        search.add_css_class("kernel-table-search");
        controls.append(&search);
        let details = gtk::ToggleButton::with_label("Details");
        details.add_css_class("inline-action");
        details.set_tooltip_text(Some("Show selected descriptor details"));
        controls.append(&details);
        root.append(&controls);
        let filters = components::control_row();
        filters.add_css_class("kernel-table-controls");
        let protocol = gtk::DropDown::from_strings(&[
            "All protocols",
            "TCP",
            "TCP6",
            "UDP",
            "UDP6",
            "UNIX",
            "Unknown",
        ]);
        protocol.set_tooltip_text(Some("Filter socket protocol"));
        let state = gtk::DropDown::from_strings(&["All states"]);
        state.set_tooltip_text(Some("Filter socket state"));
        filters.append(&protocol);
        filters.append(&state);
        filters.set_visible(false);
        root.append(&filters);
        let summary = components::empty_label("No snapshot");
        root.append(&summary);
        let changes = components::empty_label("");
        changes.set_visible(false);
        root.append(&changes);
        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        let query = Rc::new(RefCell::new(Query::default()));
        let tables = [
            build_table(&store, &query, columns, false),
            build_table(&store, &query, columns, true),
        ];
        let pages = gtk::Stack::new();
        pages.set_hhomogeneous(false);
        pages.set_vhomogeneous(false);
        pages.set_vexpand(true);

        for (index, table) in tables.iter().enumerate() {
            let page = gtk::Box::new(gtk::Orientation::Vertical, 3);
            page.append(&table.empty);
            page.append(&table.scroll);
            pages.add_named(&page, Some(if index == 0 { "all" } else { "sockets" }));
        }

        let detail_panel = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let header = components::control_row();
        header.add_css_class("kernel-memory-subtitle");
        let title = section_title("SELECTED FD");
        title.set_hexpand(true);
        header.append(&title);
        let detail_pages = gtk::Stack::new();
        detail_pages.set_hhomogeneous(false);
        detail_pages.set_vhomogeneous(false);
        detail_pages.set_vexpand(true);
        let switcher = gtk::StackSwitcher::new();
        switcher.set_stack(Some(&detail_pages));
        header.append(&switcher);
        detail_panel.append(&header);
        let actions = components::control_row();
        components::inset(&actions, components::CONTENT_INSET);
        let copy = gtk::Button::with_label("Copy details");
        let copy_local = gtk::Button::with_label("Copy local");
        let copy_peer = gtk::Button::with_label("Copy peer");
        let inspect = gtk::Button::with_label("Inspect TCP");

        for button in [&copy, &copy_local, &copy_peer, &inspect] {
            button.add_css_class("inline-action");
            button.set_sensitive(false);
            actions.append(button);
        }

        detail_panel.append(&actions);
        let content = gtk::Box::new(gtk::Orientation::Vertical, components::CONTROL_GAP);
        components::inset(&content, components::CONTENT_INSET);
        let detail_empty = components::empty_label("Select a descriptor to inspect it");
        content.append(&detail_empty);

        let cards = gtk::FlowBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .min_children_per_line(1)
            .max_children_per_line(2)
            .column_spacing(components::CONTROL_GAP as u32)
            .row_spacing(components::CONTROL_GAP as u32)
            .homogeneous(true)
            .build();

        cards.add_css_class("ui-action-flow");
        let descriptor = DetailSection::new("DESCRIPTOR");
        let socket = DetailSection::new("SOCKET");

        for section in [&descriptor, &socket] {
            section.root.set_size_request(340, -1);
            cards.insert(&section.root, -1);
            let child = section.root.parent().unwrap();
            section
                .root
                .bind_property("visible", &child, "visible")
                .sync_create()
                .build();
        }

        descriptor
            .root
            .bind_property("visible", &cards, "visible")
            .sync_create()
            .build();

        socket
            .root
            .bind_property("visible", &cards, "max-children-per-line")
            .transform_to(|_, visible: bool| Some(if visible { 2_u32 } else { 1 }))
            .sync_create()
            .build();

        let socket_note = detail_label();
        socket_note.add_css_class("muted");
        socket_note.set_visible(false);
        socket.root.append(&socket_note);
        content.append(&cards);
        let diagnostics = DetailSection::new("TCP DIAGNOSTICS");
        content.append(&diagnostics.root);
        content.reorder_child_after(&diagnostics.root, Some(&detail_empty));
        let links = components::card();
        links.set_visible(false);
        content.append(&links);
        let raw = gtk::Box::new(gtk::Orientation::Vertical, components::CONTROL_GAP);
        components::inset(&raw, components::CONTENT_INSET);
        let raw_info = raw_section(&raw, "FDINFO");
        let raw_socket = raw_section(&raw, "RAW SOCKET ENTRY");

        for (name, title, child) in [("overview", "Overview", &content), ("raw", "Raw", &raw)] {
            child.set_margin_top(0);

            let scroll = gtk::ScrolledWindow::builder()
                .child(child)
                .min_content_height(260)
                .vexpand(true)
                .build();

            configure_content_scroller(&scroll);
            detail_pages.add_titled(&scroll, Some(name), title);
        }

        detail_panel.append(&detail_pages);
        detail_panel.set_visible(false);
        details
            .bind_property("active", &detail_panel, "visible")
            .sync_create()
            .build();
        let split = gtk::Paned::new(gtk::Orientation::Vertical);
        split.add_css_class("misc-data-split");
        split.set_vexpand(true);
        split.set_resize_start_child(true);
        split.set_resize_end_child(false);
        split.set_shrink_start_child(true);
        split.set_shrink_end_child(true);
        split.set_start_child(Some(&pages));
        split.set_end_child(Some(&detail_panel));
        root.append(&split);

        let view = Rc::new(Self {
            root,
            store,
            rows: RefCell::new(Vec::new()),
            tables,
            pages,
            mode: Cell::new(0),
            all,
            sockets,
            query,
            search,
            protocol,
            state,
            summary,
            changes,
            details,
            detail_pages,
            detail_empty,
            descriptor,
            socket,
            socket_note,
            raw_info,
            raw_socket,
            links,
            link_entries: RefCell::new(Vec::new()),
            details_dirty: Cell::new(true),
            copy,
            copy_local,
            copy_peer,
            inspect,
            diagnostics,
            diagnostic_handler: RefCell::new(None),
            identity: Cell::new(None),
            captured_at: Cell::new(0),
            complete: Cell::new(false),
            comparison_stop: Cell::new(None),
            comparison_ready: Cell::new(false),
            previous: RefCell::new(HashMap::new()),
            updating: Cell::new(false),
            fresh: Cell::new(false),
            stamp: Cell::new(0),
            pending: Cell::new(false),
            commands_allowed: Cell::new(false),
        });

        let weak = Rc::downgrade(&view);
        view.sockets.connect_toggled(move |button| {
            if let Some(view) = weak.upgrade() {
                let mode = usize::from(button.is_active());
                view.mode.set(mode);
                filters.set_visible(mode == 1);
                view.pages
                    .set_visible_child_name(if mode == 0 { "all" } else { "sockets" });
                view.selection_changed();
                view.update_count();
            }
        });

        let weak = Rc::downgrade(&view);
        view.search.connect_search_changed(move |search| {
            if let Some(view) = weak.upgrade() {
                view.query.borrow_mut().text = search.text().trim().to_ascii_lowercase();
                view.refilter();
            }
        });

        for (is_protocol, dropdown) in [(true, &view.protocol), (false, &view.state)] {
            let weak = Rc::downgrade(&view);
            dropdown.connect_selected_item_notify(move |dropdown| {
                if let Some(view) = weak.upgrade()
                    && !view.updating.get()
                {
                    let value = if dropdown.selected() == 0 {
                        String::new()
                    } else {
                        selected_string(dropdown)
                    };

                    if is_protocol {
                        view.query.borrow_mut().protocol = value;
                    } else {
                        view.query.borrow_mut().state = value;
                    }

                    view.refilter();
                }
            });
        }

        for table in &view.tables {
            let weak = Rc::downgrade(&view);
            table.selection.connect_selected_item_notify(move |_| {
                if let Some(view) = weak.upgrade()
                    && !view.updating.get()
                {
                    view.selection_changed();
                }
            });
        }

        let weak = Rc::downgrade(&view);
        view.details.connect_toggled(move |_| {
            if let Some(view) = weak.upgrade() {
                view.render_details();
            }
        });

        for (field, button) in [(0, &view.copy), (1, &view.copy_local), (2, &view.copy_peer)] {
            let weak = Rc::downgrade(&view);
            button.connect_clicked(move |button| {
                let Some(view) = weak.upgrade() else {
                    return;
                };
                let Some(row) = view.selected() else {
                    return;
                };

                let text = match field {
                    1 => row
                        .fd
                        .socket
                        .as_ref()
                        .map(|socket| socket.local_name())
                        .unwrap_or_default(),
                    2 => row
                        .fd
                        .socket
                        .as_ref()
                        .map(|socket| socket.peer_name())
                        .unwrap_or_default(),
                    _ => view.details_text(),
                };

                button.clipboard().set_text(&text);
            });
        }

        let weak = Rc::downgrade(&view);
        view.inspect.connect_clicked(move |_| {
            if let Some(view) = weak.upgrade() {
                let handler = view.diagnostic_handler.borrow().clone();

                if let Some(handler) = handler {
                    handler();
                }
            }
        });

        view
    }

    pub(super) fn show(
        self: &Rc<Self>,
        identity: Option<(u32, u64)>,
        captured_at: u64,
        stop: u64,
        complete: bool,
        descriptors: Vec<KernelFileDescriptor>,
    ) {
        let same_process = identity.is_some() && self.identity.get() == identity;
        let unchanged = same_process
            && self.comparison_stop.get() == Some(stop)
            && self.complete.get() == complete
            && self
                .rows
                .borrow()
                .iter()
                .map(|row| row.fd.as_ref())
                .eq(&descriptors);
        self.invalidate();
        self.captured_at.set(captured_at);

        if unchanged {
            self.fresh.set(true);
            self.update_count();
            self.render_details();
            return;
        }

        let selections = self
            .tables
            .each_ref()
            .map(|table| selected_row(&table.selection).map(|row| row.fd));
        self.updating.set(true);
        self.details_dirty.set(true);
        self.identity.set(identity);

        if !same_process {
            self.previous.borrow_mut().clear();
            self.comparison_ready.set(false);
        } else if self.comparison_stop.get() != Some(stop) {
            // Repeated refreshes at one stop must not erase its visible changes.
            self.previous.replace(
                self.rows
                    .borrow()
                    .iter()
                    .map(|row| (row.fd.number, Rc::clone(&row.fd)))
                    .collect(),
            );
            self.comparison_ready.set(self.complete.get());
        }

        self.comparison_stop.set(Some(stop));
        self.complete.set(complete);
        let comparing = complete && self.comparison_ready.get();
        let previous = self.previous.borrow();
        let current_numbers = descriptors
            .iter()
            .map(|fd| fd.number)
            .collect::<HashSet<_>>();
        let closed = if comparing {
            previous
                .values()
                .filter(|fd| !current_numbers.contains(&fd.number))
                .count()
        } else {
            0
        };

        let rows = descriptors
            .into_iter()
            .map(|fd| {
                let old = comparing
                    .then(|| previous.get(&fd.number))
                    .flatten()
                    .map(Rc::as_ref);
                make_row(fd, old, comparing)
            })
            .collect::<Vec<_>>();

        let changed = rows.iter().filter(|row| !row.change.is_empty()).count();
        self.changes.set_text(&format!(
            "{changed} changed or new · {closed} closed since the previous stop"
        ));
        self.changes
            .set_visible(comparing && (changed != 0 || closed != 0));

        let closed_details = previous
            .values()
            .filter(|fd| !current_numbers.contains(&fd.number))
            .take(256)
            .map(|fd| format!("FD {}: {}", fd.number, fd.target))
            .collect::<Vec<_>>()
            .join("\n");

        self.changes
            .set_tooltip_text((closed > 0).then_some(closed_details.as_str()));
        let mut states = rows
            .iter()
            .filter(|row| row.fd.kind == "socket")
            .map(|row| state_name(&row.fd))
            .collect::<Vec<_>>();
        states.sort_unstable();
        states.dedup();
        let selected_state = self.query.borrow().state.clone();

        if !selected_state.is_empty() && !states.contains(&selected_state.as_str()) {
            states.push(&selected_state);
        }

        states.insert(0, "All states");
        let states_unchanged = self.state.model().is_some_and(|model| {
            model.n_items() as usize == states.len()
                && states.iter().enumerate().all(|(index, state)| {
                    model
                        .item(index as u32)
                        .and_downcast::<gtk::StringObject>()
                        .is_some_and(|item| item.string() == *state)
                })
        });

        if !states_unchanged {
            self.state.set_model(Some(&gtk::StringList::new(&states)));
            self.state.set_selected(
                states
                    .iter()
                    .position(|state| *state == selected_state)
                    .unwrap_or(0) as u32,
            );
        }

        replace_boxed_store_if_changed(&self.store, rows.iter().cloned());
        self.rows.replace(rows);

        for (table, selected) in self.tables.iter().zip(&selections) {
            restore_selection(
                &table.selection,
                same_process.then_some(selected.as_deref()).flatten(),
            );
        }

        self.fresh.set(true);
        self.updating.set(false);
        self.update_count();
        self.render_details();
    }

    pub(super) fn clear(self: &Rc<Self>) {
        self.invalidate();
        self.identity.set(None);
        self.complete.set(false);
        self.details_dirty.set(true);
        self.rows.borrow_mut().clear();
        self.previous.borrow_mut().clear();
        self.comparison_stop.set(None);
        self.comparison_ready.set(false);
        self.store.remove_all();
        self.summary.set_text("No snapshot");
        self.changes.set_visible(false);
        self.render_details();
        self.update_count();
    }

    pub(super) fn invalidate(&self) {
        self.fresh.set(false);
        self.stamp.set(self.stamp.get().wrapping_add(1));
        self.pending.set(false);
        self.inspect.set_sensitive(false);
        self.diagnostics.set(Vec::new());
    }

    fn begin_diagnostics(&self) -> u64 {
        self.stamp.set(self.stamp.get().wrapping_add(1));
        self.pending.set(true);
        self.inspect.set_sensitive(false);
        self.detail_pages.set_visible_child_name("overview");

        if let Some(scroll) = self
            .detail_pages
            .visible_child()
            .and_downcast::<gtk::ScrolledWindow>()
        {
            scroll.vadjustment().set_value(scroll.vadjustment().lower());
        }

        self.diagnostics
            .set(vec![fact("Status", "Collecting TCP diagnostics…")]);
        self.stamp.get()
    }

    fn cancel_diagnostics(self: &Rc<Self>, stamp: u64) {
        if self.stamp.get() == stamp {
            self.pending.set(false);
            self.diagnostics.set(Vec::new());
            self.render_details();
        }
    }

    fn selected(&self) -> Option<Row> {
        selected_row(&self.tables[self.mode.get()].selection)
    }

    fn selection_changed(self: &Rc<Self>) {
        self.details_dirty.set(true);
        self.stamp.set(self.stamp.get().wrapping_add(1));
        self.pending.set(false);
        self.diagnostics.set(Vec::new());
        self.render_details();
    }

    fn refilter(self: &Rc<Self>) {
        self.updating.set(true);

        for table in &self.tables {
            let selected = selected_row(&table.selection).map(|row| row.fd);
            table.filter.changed(gtk::FilterChange::Different);
            restore_selection(&table.selection, selected.as_deref());
        }

        self.updating.set(false);
        self.selection_changed();
        self.update_count();
    }

    fn update_count(&self) {
        let rows = self.rows.borrow();
        let sockets = rows.iter().filter(|row| row.fd.kind == "socket").count();
        let unknown = rows
            .iter()
            .filter(|row| row.fd.kind == "socket" && row.fd.socket.is_none())
            .count();
        let shown = self.tables[self.mode.get()].selection.n_items();

        if self.identity.get().is_some() {
            let mut summary = format!("{} FDs · {sockets} sockets", rows.len());
            let total = if self.mode.get() == 0 {
                rows.len()
            } else {
                sockets
            };

            if shown as usize != total {
                let _ = write!(summary, " · {shown} matching");
            }

            if !self.complete.get() {
                summary.push_str(" · partial snapshot");
            }

            if unknown != 0 {
                let _ = write!(summary, " · {unknown} socket details unavailable");
            }

            if let Ok(time) =
                glib::DateTime::from_unix_local((self.captured_at.get() / 1000) as i64)
                && let Ok(time) = time.format("%H:%M:%S")
            {
                let _ = write!(summary, " · captured {time}");
            }

            self.summary.set_text(&summary);
            self.summary.set_tooltip_text(Some("Queues and states describe this snapshot. Inspect TCP captures additional diagnostics on request."));
        }

        for (index, table) in self.tables.iter().enumerate() {
            let total = if index == 0 { rows.len() } else { sockets };
            table.empty.set_text(if total == 0 {
                if index == 0 {
                    "No open file descriptors available"
                } else {
                    "No sockets in this snapshot"
                }
            } else {
                "No descriptors match the filters"
            });
            table.empty.set_visible(table.selection.n_items() == 0);
        }
    }

    fn render_details(self: &Rc<Self>) {
        let row = self.selected();
        self.copy.set_sensitive(row.is_some());
        let socket = row.as_ref().and_then(|row| row.fd.socket.as_ref());
        self.copy_local.set_visible(socket.is_some());
        self.copy_peer
            .set_visible(socket.is_some_and(|socket| socket.peer.is_some()));
        self.inspect
            .set_visible(socket.is_some_and(|socket| socket.protocol.is_tcp()));
        self.copy_local.set_sensitive(socket.is_some());
        self.copy_peer
            .set_sensitive(socket.is_some_and(|socket| socket.peer.is_some()));
        self.inspect.set_sensitive(
            self.commands_allowed.get()
                && self.fresh.get()
                && !self.pending.get()
                && socket.is_some_and(|socket| socket.protocol.is_tcp()),
        );

        if !self.details.is_active() || !self.details_dirty.replace(false) {
            return;
        }

        self.detail_empty.set_visible(row.is_none());
        let Some(row) = row else {
            self.set_links(Vec::new());
            self.descriptor.set(Vec::new());
            self.socket.set(Vec::new());
            set_raw(&self.raw_info, "Select a descriptor to inspect it");
            set_raw(&self.raw_socket, "");
            return;
        };
        let fd = &row.fd;
        let mut facts = vec![
            fact("FD", fd.number.to_string()),
            fact("Kind", &fd.kind),
            fact("Access", &fd.access),
            fact("Target", &fd.target),
            fact("Flags", &fd.flags),
        ];

        if let Some(position) = fd.position {
            facts.push(fact("Position", position.to_string()));
        }

        if let Some(inode) = fd.inode {
            facts.push(fact("Inode", inode.to_string()));
        }

        if !row.change.is_empty() {
            facts.push(fact("Changes", &row.change));
        }

        self.descriptor.set(facts);

        if let Some(socket) = &fd.socket {
            let mut facts = vec![
                fact("Protocol", socket.protocol.name()),
                fact("Type", socket.type_name()),
                fact(
                    "State",
                    format!("{} (0x{:02x})", socket.state_name(), socket.state),
                ),
                fact("Local", socket.local_name()),
            ];

            if socket.peer.is_some() {
                facts.push(fact("Peer", socket.peer_name()));
            }

            for (name, queue) in [
                ("Receive queue", socket.receive),
                ("Send queue", socket.send),
            ] {
                if let Some(queue) = queue {
                    facts.push(fact(name, queue.to_string()));
                }
            }

            self.socket.set(facts);

            let explanation = if socket.protocol.is_tcp() {
                if socket.listening {
                    "Receive queue counts pending connections. Inspect TCP for maximum backlog."
                } else {
                    match socket.state {
                        8 => "CLOSE_WAIT: the peer has ended its sending direction.",
                        5 => {
                            "FIN_WAIT2: the local sending direction has closed and its FIN was acknowledged."
                        }
                        _ => "TCP queues count sequence bytes.",
                    }
                }
            } else if socket.protocol == Protocol::Unix {
                "Queue sizes and peer endpoints are unavailable from procfs."
            } else {
                "UDP queues count kernel memory including overhead. Connected means a default peer is configured."
            };

            set_label_text(&self.socket_note, explanation);
            self.socket_note.set_visible(true);
            set_raw(&self.raw_socket, &socket.raw);
        } else if fd.kind == "socket" {
            self.socket.set(vec![fact("Status", "Details unavailable: unsupported protocol, restricted procfs access, or socket changed during capture.")]);
            self.socket_note.set_visible(false);
            set_raw(&self.raw_socket, "");
        } else {
            self.socket.set(Vec::new());
            self.socket_note.set_visible(false);
            set_raw(&self.raw_socket, "");
        }

        set_raw(
            &self.raw_info,
            if fd.raw_info.is_empty() {
                "Unavailable"
            } else {
                &fd.raw_info
            },
        );

        let rows = self.rows.borrow();

        let mut links = Vec::new();

        if !fd.watches.is_empty() {
            let by_number = rows
                .iter()
                .map(|row| (row.fd.number, &row.fd))
                .collect::<HashMap<_, _>>();

            for watch in &fd.watches {
                let target = by_number.get(&watch.fd).filter(|fd| watch.matches(fd));
                let status = if target.is_some() {
                    ""
                } else {
                    " · identity unverified"
                };
                links.push((
                    format!(
                        "Watches FD {} · {} · data 0x{:x}{status}",
                        watch.fd,
                        watch.interests(),
                        watch.data
                    ),
                    target.map(|fd| Rc::clone(fd)),
                ));
            }
        }

        for parent in rows.iter() {
            for watch in parent.fd.watches.iter().filter(|watch| watch.matches(fd)) {
                links.push((
                    format!("Watched by FD {} · {}", parent.fd.number, watch.interests()),
                    Some(Rc::clone(&parent.fd)),
                ));
            }
        }

        self.set_links(links);
    }

    fn set_links(self: &Rc<Self>, links: Vec<(String, Option<Rc<KernelFileDescriptor>>)>) {
        let mut previous = self.link_entries.borrow_mut();

        if *previous == links {
            return;
        }

        clear_box(&self.links);
        self.links.set_visible(!links.is_empty());

        if !links.is_empty() {
            self.links.append(&section_title("EPOLL"));

            for (text, target) in &links {
                self.add_link(text, target.clone());
            }

            let note = detail_label();
            note.add_css_class("muted");
            note.set_text("Registered interests, not current readiness");
            self.links.append(&note);
        }

        *previous = links;
    }

    fn details_text(&self) -> String {
        let mut sections = vec![self.descriptor.text(), self.socket.text()];

        if self.socket_note.is_visible() && self.socket.root.is_visible() {
            sections.push(self.socket_note.text().to_string());
        }

        sections.push(self.diagnostics.text());
        sections.push(format!("FDINFO\n{}", self.raw_info.text()));

        if !self.raw_socket.text().is_empty() {
            sections.push(format!("RAW SOCKET ENTRY\n{}", self.raw_socket.text()));
        }

        sections.retain(|text| !text.is_empty());
        sections.join("\n\n")
    }

    fn add_link(self: &Rc<Self>, text: &str, key: Option<Rc<KernelFileDescriptor>>) {
        let button = gtk::Button::with_label(text);
        button.add_css_class("inline-action");
        button.set_halign(gtk::Align::Fill);
        button.set_sensitive(key.is_some());

        if let Some(label) = button.child().and_downcast::<gtk::Label>() {
            label.set_xalign(0.0);
            label.set_wrap(true);
            label.set_wrap_mode(pango::WrapMode::WordChar);
            label.set_width_chars(1);
        }

        let weak = Rc::downgrade(self);

        button.connect_clicked(move |_| {
            if let Some(view) = weak.upgrade() {
                view.all.set_active(true);
                view.search.set_text("");
                view.query.borrow_mut().text.clear();
                view.refilter();
                restore_selection(&view.tables[0].selection, key.as_deref());
                view.tables[0].view.grab_focus();
                let position = view.tables[0].selection.selected();

                if position != gtk::INVALID_LIST_POSITION {
                    view.tables[0].view.scroll_to(
                        position,
                        None,
                        gtk::ListScrollFlags::FOCUS,
                        None,
                    );
                }

                view.render_details();
            }
        });

        self.links.append(&button);
    }
}

fn detail_label() -> gtk::Label {
    let label = gtk::Label::new(None);
    label.set_xalign(0.0);
    label.set_wrap(true);
    label.set_wrap_mode(pango::WrapMode::WordChar);
    label.set_width_chars(1);
    label.set_max_width_chars(32);
    label.set_hexpand(true);
    label.set_valign(gtk::Align::Start);
    enable_stable_text_selection(&label);
    label
}

fn raw_section(parent: &gtk::Box, title: &str) -> gtk::Label {
    let card = components::card();
    card.append(&section_title(title));
    let label = detail_label();
    card.append(&label);
    parent.append(&card);
    card.set_visible(false);
    label
}

fn set_raw(label: &gtk::Label, text: &str) {
    set_label_text(label, text.trim_end_matches('\n'));
    label.parent().unwrap().set_visible(!text.is_empty());
}

fn selected_string(dropdown: &gtk::DropDown) -> String {
    dropdown
        .selected_item()
        .and_downcast::<gtk::StringObject>()
        .map(|value| value.string().to_string())
        .unwrap_or_default()
}

fn state_name(fd: &KernelFileDescriptor) -> &'static str {
    fd.socket
        .as_ref()
        .map_or("Unknown", |socket| socket.state_name())
}

fn queue_text(queue: Option<Queue>) -> String {
    queue.map_or_else(|| "Unavailable".into(), |queue| queue.to_string())
}

fn selected_row(selection: &gtk::SingleSelection) -> Option<Row> {
    selection
        .selected_item()
        .and_downcast::<glib::BoxedAnyObject>()
        .map(|object| object.borrow::<Row>().clone())
}

fn restore_selection(selection: &gtk::SingleSelection, key: Option<&KernelFileDescriptor>) {
    let index = key.and_then(|key| {
        (0..selection.n_items()).find(|index| {
            selection
                .item(*index)
                .and_downcast::<glib::BoxedAnyObject>()
                .is_some_and(|object| object.borrow::<Row>().fd.same_identity(key))
        })
    });

    selection.set_selected(index.unwrap_or(gtk::INVALID_LIST_POSITION));
}

fn make_row(fd: KernelFileDescriptor, old: Option<&KernelFileDescriptor>, comparing: bool) -> Row {
    let same = old.filter(|old| old.same_identity(&fd));
    let before = same.and_then(|old| old.socket.as_deref());
    let after = fd.socket.as_deref();
    let mut changes = Vec::new();
    let mut receive_changed = false;
    let mut send_changed = false;
    let mut state_changed = false;

    if comparing && same.is_none() {
        changes.push(if old.is_some() { "FD reused" } else { "New" }.to_owned());
    }

    if let (Some(before), Some(after)) = (before, after) {
        state_changed = before.state != after.state || before.listening != after.listening;

        if state_changed {
            changes.push(format!("{} → {}", before.state_name(), after.state_name()));
        }

        for (label, old, new, changed) in [
            ("RX", before.receive, after.receive, &mut receive_changed),
            ("TX", before.send, after.send, &mut send_changed),
        ] {
            if let (Some(old), Some(new)) = (old, new)
                && old.unit == new.unit
                && old.value != new.value
            {
                *changed = true;
                changes.push(format!(
                    "{label} {:+} {}",
                    i128::from(new.value) - i128::from(old.value),
                    new.unit.label()
                ));
            }
        }
    }

    if let Some(old) = same
        && (old.flags != fd.flags || old.position != fd.position || old.target != fd.target)
    {
        changes.push("Descriptor changed".into());
    }

    let search = format!(
        "{} {} {} {} {} {} {}",
        fd.number,
        fd.kind,
        fd.access,
        fd.flags,
        fd.target,
        fd.details,
        fd.inode.map_or_else(String::new, |inode| inode.to_string())
    )
    .to_ascii_lowercase();

    Row {
        fd: Rc::new(fd),
        search,
        change: changes.join(" · "),
        receive_changed,
        send_changed,
        state_changed,
    }
}

fn build_table(
    store: &gio::ListStore,
    query: &Rc<RefCell<Query>>,
    columns: &ColumnLayouts,
    sockets: bool,
) -> Table {
    let query = Rc::clone(query);
    let filter = gtk::CustomFilter::new(move |object| {
        let Some(object) = object.downcast_ref::<glib::BoxedAnyObject>() else {
            return false;
        };
        let row = object.borrow::<Row>();
        let query = query.borrow();
        let protocol = row
            .fd
            .socket
            .as_ref()
            .map_or("Unknown", |socket| socket.protocol.name());

        row.search.contains(&query.text)
            && (!sockets
                || row.fd.kind == "socket"
                    && (query.protocol.is_empty() || query.protocol == protocol)
                    && (query.state.is_empty() || query.state == state_name(&row.fd)))
    });

    let filtered = gtk::FilterListModel::new(Some(store.clone()), Some(filter.clone()));
    let sorted = gtk::SortListModel::new(Some(filtered), None::<gtk::Sorter>);
    let selection = gtk::SingleSelection::new(Some(sorted.clone()));
    selection.set_autoselect(false);
    selection.set_can_unselect(true);
    let view = components::column_view(selection.clone());
    view.add_css_class("debug-table");
    view.set_vexpand(true);
    view.set_reorderable(true);
    sorted.set_sorter(view.sorter().as_ref());
    let layout = columns.table(if sockets {
        TableId::Sockets
    } else {
        TableId::FileDescriptors
    });

    let definitions = if sockets {
        vec![
            ("number", "FD", 55, Column::Number),
            ("protocol", "PROTOCOL", 85, Column::Protocol),
            ("state", "STATE", 125, Column::State),
            ("local", "LOCAL", 200, Column::Local),
            ("peer", "PEER", 200, Column::Peer),
            ("receive", "RECEIVE QUEUE", 130, Column::Receive),
            ("send", "SEND QUEUE", 130, Column::Send),
            ("flags", "FLAGS", 180, Column::SocketFlags),
            ("change", "CHANGE", 230, Column::Change),
        ]
    } else {
        vec![
            ("number", "FD", 55, Column::Number),
            ("kind", "KIND", 90, Column::Kind),
            ("access", "ACCESS", 100, Column::Access),
            ("flags", "FLAGS", 220, Column::Flags),
            ("position", "POSITION", 110, Column::Position),
            ("target", "TARGET", 360, Column::Target),
            ("details", "FDINFO", 280, Column::Details),
        ]
    };

    for (key, title, width, field) in definitions {
        let column = super::table_column(title, width, move |object, label| {
            let row = object.borrow::<Row>();
            let text = cell_text(&row, field);
            label.set_text(&text);
            let tooltip = if sockets {
                format!("{text}\n{}", row.change)
            } else {
                format!(
                    "{}  {}  {}  {}\n{}",
                    row.fd.target, row.fd.access, row.fd.flags, row.fd.details, row.change
                )
            };

            label.set_tooltip_text(Some(&tooltip));
            label.set_xalign(
                if matches!(
                    field,
                    Column::Number | Column::Position | Column::Receive | Column::Send
                ) {
                    1.0
                } else {
                    0.0
                },
            );
            let changed = match field {
                Column::Receive => row.receive_changed,
                Column::Send => row.send_changed,
                Column::State => row.state_changed,
                Column::Change => !row.change.is_empty(),
                _ => false,
            };

            if changed {
                label.add_css_class("kernel-change-modified");
            } else {
                label.remove_css_class("kernel-change-modified");
            }
        });

        let selection_for_factory = selection.clone();
        let factory = column
            .factory()
            .unwrap()
            .downcast::<gtk::SignalListItemFactory>()
            .unwrap();

        factory.connect_setup(move |_, object| {
            let Some(item) = object.downcast_ref::<gtk::ListItem>() else {
                return;
            };
            let Some(label) = item.child() else {
                return;
            };
            let item = item.downgrade();
            let selection = selection_for_factory.clone();
            let click = gtk::GestureClick::new();
            click.set_button(gtk::gdk::BUTTON_PRIMARY);
            click.set_propagation_phase(gtk::PropagationPhase::Capture);

            click.connect_pressed(move |_, _, _, _| {
                if let Some(item) = item.upgrade()
                    && item.item().is_some()
                    && selection.item(item.position()) == item.item()
                {
                    selection.set_selected(item.position());
                }
            });

            label.add_controller(click);
        });

        column.set_sorter(Some(&gtk::CustomSorter::new(move |left, right| {
            let left = left
                .downcast_ref::<glib::BoxedAnyObject>()
                .unwrap()
                .borrow::<Row>();
            let right = right
                .downcast_ref::<glib::BoxedAnyObject>()
                .unwrap()
                .borrow::<Row>();

            let order = match field {
                Column::Number => left.fd.number.cmp(&right.fd.number),
                Column::Position => left.fd.position.cmp(&right.fd.position),
                Column::Receive => left
                    .fd
                    .socket
                    .as_ref()
                    .and_then(|socket| socket.receive)
                    .cmp(&right.fd.socket.as_ref().and_then(|socket| socket.receive)),
                Column::Send => left
                    .fd
                    .socket
                    .as_ref()
                    .and_then(|socket| socket.send)
                    .cmp(&right.fd.socket.as_ref().and_then(|socket| socket.send)),
                _ => cell_text(&left, field).cmp(&cell_text(&right, field)),
            };

            match order.then_with(|| left.fd.number.cmp(&right.fd.number)) {
                std::cmp::Ordering::Less => gtk::Ordering::Smaller,
                std::cmp::Ordering::Equal => gtk::Ordering::Equal,
                std::cmp::Ordering::Greater => gtk::Ordering::Larger,
            }
        })));

        layout.append(&view, key, &column);
    }

    let empty = components::empty_label("No snapshot");
    let scroll = gtk::ScrolledWindow::builder()
        .child(&view)
        .min_content_height(1)
        .vexpand(true)
        .build();
    configure_content_scroller(&scroll);
    Table {
        selection,
        filter,
        view,
        scroll,
        empty,
    }
}

fn cell_text(row: &Row, column: Column) -> String {
    let fd = &row.fd;
    let socket = fd.socket.as_deref();

    match column {
        Column::Number => fd.number.to_string(),
        Column::Kind => fd.kind.clone(),
        Column::Access => fd.access.clone(),
        Column::Flags => fd.flags.clone(),
        Column::SocketFlags => fd
            .flags
            .split_once("  ")
            .map_or(fd.flags.as_str(), |(_, names)| names)
            .to_owned(),
        Column::Position => fd
            .position
            .map_or_else(String::new, |value| value.to_string()),
        Column::Target => fd.target.clone(),
        Column::Details => fd.details.clone(),
        Column::Protocol => socket
            .map_or("Unknown", |socket| socket.protocol.name())
            .into(),
        Column::State => state_name(fd).into(),
        Column::Local => socket.map_or_else(|| "Unavailable".into(), |socket| socket.local_name()),
        Column::Peer => socket.map_or_else(|| "Unavailable".into(), |socket| socket.peer_name()),
        Column::Receive => queue_text(socket.and_then(|socket| socket.receive)),
        Column::Send => queue_text(socket.and_then(|socket| socket.send)),
        Column::Change => row.change.clone(),
    }
}

impl Ui {
    pub(in crate::ui) fn update_socket_controls(&self) {
        let view = &self.kernel_view.descriptors;

        if view.commands_allowed.replace(self.kernel_refresh_allowed())
            != self.kernel_refresh_allowed()
        {
            view.render_details();
        }
    }

    pub(crate) fn cancel_socket_diagnostics(&self, stamp: u64) {
        let view = &self.kernel_view.descriptors;

        view.commands_allowed.set(self.kernel_refresh_allowed());
        view.cancel_diagnostics(stamp);
    }

    pub(crate) fn set_socket_diagnostics_handler(&self, handler: impl Fn() + 'static) {
        self.kernel_view
            .descriptors
            .diagnostic_handler
            .replace(Some(Rc::new(handler)));
    }

    pub(crate) fn begin_socket_diagnostics(
        &self,
    ) -> Option<(u64, u64, crate::kernel::sockets::diagnostics::Request)> {
        let view = &self.kernel_view.descriptors;

        if !self.kernel_refresh_allowed() || !view.fresh.get() || view.pending.get() {
            return None;
        }

        let (pid, start_time) = view.identity.get()?;

        if self.model.inferior_pid() != Some(pid) {
            return None;
        }

        let row = view.selected()?;
        let socket = row.fd.socket.clone()?;

        if !socket.protocol.is_tcp() {
            return None;
        }

        let request = crate::kernel::sockets::diagnostics::Request {
            pid,
            debugger_pid: self.model.debugger_pid()?,
            start_time,
            fd: row.fd.number,
            inode: row.fd.inode?,
            socket,
        };

        let stamp = view.begin_diagnostics();
        Some((self.kernel_refresh_generation.get(), stamp, request))
    }

    pub(crate) fn socket_diagnostics_current(&self, generation: u64, stamp: u64) -> bool {
        self.kernel_refresh_generation.get() == generation
            && self.kernel_refresh_allowed()
            && self.kernel_view.descriptors.fresh.get()
            && self.kernel_view.descriptors.stamp.get() == stamp
    }

    pub(crate) fn show_socket_diagnostics(
        &self,
        generation: u64,
        stamp: u64,
        result: Result<Vec<crate::kernel::KernelFact>, String>,
    ) {
        if !self.socket_diagnostics_current(generation, stamp) {
            return;
        }

        let view = &self.kernel_view.descriptors;
        view.pending.set(false);
        view.render_details();

        view.diagnostics.set(match result {
            Ok(facts) => facts,
            Err(error) => vec![fact("Unavailable", error)],
        });
    }
}
