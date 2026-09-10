//! Selected-lock inspection and bounded wait-chain navigation.

use super::*;

mod actions;
mod controls;
mod details;
mod selection;
#[cfg(test)]
mod tests;

pub(crate) use actions::LockSymbolRequest;
pub(in crate::ui) use controls::process_running;

const LOCKS_NOTE: &str = "Stopped-process futex snapshot. Owner edges can be inferred, and an incomplete graph cannot rule out deadlock.";
type ActionHandler = Rc<dyn Fn(LockAction)>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LockAction {
    Selection,
    Waiter,
    Owner,
    Memory,
    Copy,
    Follow,
    Back,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct LockContext {
    generation: u64,
    inferior: String,
    pid: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct WaitKey {
    tid: u32,
    address: Option<u64>,
}

impl From<&LockWait> for WaitKey {
    fn from(wait: &LockWait) -> Self {
        Self {
            tid: wait.tid,
            address: wait.address,
        }
    }
}

pub(in crate::ui) struct LocksView {
    pub(in crate::ui) root: gtk::Paned,
    summary: gtk::Label,
    note: gtk::Label,
    store: gio::ListStore,
    empty: gtk::Label,
    graph_summary: gtk::Label,
    dependency_store: gio::ListStore,
    graph_empty: gtk::Label,
    selection: gtk::SingleSelection,
    dependency_selection: gtk::SingleSelection,
    waiters: gtk::ColumnView,
    chain_root: Cell<Option<WaitKey>>,
    detail: gtk::Label,
    word: gtk::Label,
    mapping: gtk::Label,
    symbol: gtk::Label,
    evidence: gtk::Label,
    actions: [(LockAction, gtk::Button); 6],
    snapshot: RefCell<Option<LockSnapshot>>,
    context: RefCell<Option<LockContext>>,
    fresh: Cell<bool>,
    stamp: Cell<u64>,
    symbols: RefCell<HashMap<u64, String>>,
    symbol_pending: Cell<Option<LockSymbolRequest>>,
    symbol_scheduled: Cell<bool>,
    history: RefCell<Vec<selection::SelectionState>>,
    updating: Cell<bool>,
    handler: RefCell<Option<ActionHandler>>,
}

pub(in crate::ui) fn build_locks_page(columns: &ColumnLayouts) -> Rc<LocksView> {
    let page = build_misc_table_page("Open this tab to inspect kernel-visible futex waits");
    page.note.set_text(LOCKS_NOTE);
    let layout = columns.table(TableId::LockWaits);

    layout.append(
        &page.view,
        "tid",
        &selection::column::<LockWait>("TID", 90, |row| row.tid.to_string()),
    );

    layout.append(
        &page.view,
        "thread",
        &selection::column::<LockWait>("THREAD", 180, |row| row.thread.clone()),
    );

    layout.append(
        &page.view,
        "state",
        &selection::column::<LockWait>("STATE", 150, |row| row.state.clone()),
    );

    layout.append(
        &page.view,
        "wait-address",
        &selection::column::<LockWait>("WAIT ADDRESS", 190, |row| {
            row.address
                .map_or_else(|| String::from("-"), |value| format!("0x{value:016x}"))
        }),
    );

    layout.append(
        &page.view,
        "operation",
        &selection::column::<LockWait>("OPERATION", 190, |row| row.operation.clone()),
    );

    layout.append(&page.view, "inferred-owner", &details::owner_column());

    layout.append(
        &page.view,
        "current-word",
        &selection::column::<LockWait>("CURRENT WORD", 140, |row| {
            row.observation
                .word
                .map_or_else(|| "unavailable".into(), |word| format!("0x{word:08x}"))
        }),
    );

    layout.append(
        &page.view,
        "expected-count",
        &selection::column::<LockWait>("EXPECTED / COUNT", 140, |row| {
            row.expected
                .map_or_else(|| String::from("-"), |value| format!("0x{value:x}"))
        }),
    );

    layout.append(
        &page.view,
        "details",
        &selection::column::<LockWait>("DETAILS", 360, |row| row.details.clone()),
    );

    let selection = page
        .view
        .model()
        .and_downcast::<gtk::SingleSelection>()
        .unwrap();

    selection.set_can_unselect(false);
    page.view.set_single_click_activate(false);
    let graph = gtk::Box::new(gtk::Orientation::Vertical, 4);
    graph.add_css_class("lock-graph");
    graph.append(&section_title("SELECTED LOCK"));
    let detail = misc_summary_label();
    let word = misc_note_label();
    let mapping = misc_note_label();
    let symbol = misc_note_label();
    let evidence = misc_note_label();

    for label in [&detail, &word, &mapping, &symbol, &evidence] {
        graph.append(label);
    }

    let toolbar = gtk::FlowBox::new();
    toolbar.set_selection_mode(gtk::SelectionMode::None);
    toolbar.set_min_children_per_line(1);
    toolbar.set_max_children_per_line(6);

    let actions = [
        (LockAction::Waiter, "Waiter stack"),
        (LockAction::Owner, "Owner stack"),
        (LockAction::Memory, "Show memory"),
        (LockAction::Copy, "Copy address"),
        (LockAction::Follow, "Follow owner"),
        (LockAction::Back, "Back"),
    ]
    .map(|(action, label)| {
        let button = gtk::Button::with_label(label);

        button.set_tooltip_text(Some(match action {
            LockAction::Waiter => "Select the stopped GDB thread matching this waiter and open its call stack",
            LockAction::Owner => "Open the inferred owner's call stack when it maps to a stopped thread in this inferior",
            LockAction::Memory => "Inspect 32 bytes beginning at the selected wait address",
            LockAction::Copy => "Copy the selected wait address",
            LockAction::Follow => "Follow the owner's observed wait. Disabled when no further wait is known",
            LockAction::Back => "Return to the previous waiter in this snapshot",
            LockAction::Selection => "",
        }));

        button.set_sensitive(false);
        toolbar.insert(&button, -1);

        (action, button)
    });

    graph.append(&toolbar);
    graph.append(&section_title("WAIT CHAIN"));
    let graph_summary = misc_note_label();

    graph_summary
        .set_text("Owner edges appear only when the live futex word identifies a scanned thread");

    graph.append(&graph_summary);
    let dependency_store = gio::ListStore::new::<glib::BoxedAnyObject>();
    let dependency_selection = gtk::SingleSelection::new(Some(dependency_store.clone()));
    dependency_selection.set_autoselect(false);
    dependency_selection.set_can_unselect(false);
    let dependency_view = components::column_view(dependency_selection.clone());
    dependency_view.set_single_click_activate(false);
    dependency_view.add_css_class("debug-table");
    dependency_view.set_vexpand(true);
    dependency_view.set_reorderable(true);
    let layout = columns.table(TableId::LockDependencies);

    layout.append(
        &dependency_view,
        "waiter",
        &selection::column::<details::ChainRow>("WAITER", 200, |row| {
            format!("{}  {}", row.edge.waiter_tid, row.edge.waiter)
        }),
    );

    layout.append(&dependency_view, "relation", &details::relation_column());

    layout.append(
        &dependency_view,
        "owner",
        &selection::column::<details::ChainRow>("OWNER", 200, |row| {
            format!("{}  {}", row.edge.owner_tid, row.edge.owner)
        }),
    );

    layout.append(
        &dependency_view,
        "address",
        &selection::column::<details::ChainRow>("ADDRESS", 190, |row| {
            format!("0x{:016x}", row.edge.address)
        }),
    );

    layout.append(
        &dependency_view,
        "futex-word",
        &selection::column::<details::ChainRow>("FUTEX WORD", 130, |row| {
            format!("0x{:08x}", row.edge.futex_value)
        }),
    );

    let graph_empty = empty_label("Select a waiter to follow its inferred dependencies");
    graph.append(&graph_empty);

    let dependency_scrolled = gtk::ScrolledWindow::builder()
        .child(&dependency_view)
        .min_content_height(160)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .build();

    configure_misc_scroller(&dependency_scrolled);
    graph.append(&dependency_scrolled);
    let root = gtk::Paned::new(gtk::Orientation::Vertical);
    root.set_wide_handle(true);
    root.set_shrink_start_child(true);
    root.set_shrink_end_child(true);
    root.set_resize_start_child(true);
    root.set_resize_end_child(true);
    root.set_start_child(Some(&page.root));

    let details_scroll = gtk::ScrolledWindow::builder()
        .child(&graph)
        .vexpand(true)
        .build();

    configure_misc_scroller(&details_scroll);
    root.set_end_child(Some(&details_scroll));
    root.set_position(390);

    let locks = Rc::new(LocksView {
        root,
        summary: page.summary,
        note: page.note,
        store: page.store,
        empty: page.empty,
        graph_summary,
        dependency_store,
        graph_empty,
        selection,
        dependency_selection,
        waiters: page.view.clone(),
        chain_root: Cell::new(None),
        detail,
        word,
        mapping,
        symbol,
        evidence,
        actions,
        snapshot: RefCell::new(None),
        context: RefCell::new(None),
        fresh: Cell::new(false),
        stamp: Cell::new(0),
        symbols: RefCell::new(HashMap::new()),
        symbol_pending: Cell::new(None),
        symbol_scheduled: Cell::new(false),
        history: RefCell::new(Vec::new()),
        updating: Cell::new(false),
        handler: RefCell::new(None),
    });

    locks.connect_selection(&dependency_view);

    locks
}

impl LocksView {
    fn emit(&self, action: LockAction) {
        let handler = self.handler.borrow().clone();

        if let Some(handler) = handler {
            handler(action);
        }
    }

    pub(in crate::ui) fn show(&self, snapshot: LockSnapshot) {
        let selected = self.selection_state();
        self.updating.set(true);
        self.fresh.set(true);
        self.stamp.set(self.stamp.get().wrapping_add(1));
        self.symbols.borrow_mut().clear();
        self.symbol_pending.set(None);
        self.history.borrow_mut().clear();

        let addresses = snapshot
            .waits
            .iter()
            .filter_map(|wait| wait.address)
            .collect::<HashSet<_>>()
            .len();

        self.summary.set_text(&format!(
            "{} threads scanned  {} waiters  {addresses} wait addresses  {} inferred cycles",
            snapshot.threads_scanned,
            snapshot.waits.len(),
            snapshot.deadlocks.len(),
        ));

        self.note
            .set_text(&format!("{LOCKS_NOTE}\n{}", snapshot.warnings.join("\n")));

        self.empty
            .set_text("No waits were observed. Unreadable or userspace-only waits may be absent");

        self.empty.set_visible(snapshot.waits.is_empty());
        replace_boxed_store_if_changed(&self.store, snapshot.waits.clone());
        self.snapshot.replace(Some(snapshot));
        self.restore_selection(selected);
        self.updating.set(false);
        self.emit(LockAction::Selection);
    }

    pub(in crate::ui) fn clear(&self) {
        self.updating.set(true);
        self.context.borrow_mut().take();
        self.fresh.set(false);
        self.snapshot.borrow_mut().take();
        self.stamp.set(self.stamp.get().wrapping_add(1));
        self.symbols.borrow_mut().clear();
        self.symbol_pending.set(None);
        self.history.borrow_mut().clear();
        self.chain_root.set(None);
        self.store.remove_all();
        self.dependency_store.remove_all();
        self.summary.set_text("No current lock snapshot");
        self.note.set_text(LOCKS_NOTE);
        self.empty.set_visible(true);
        self.updating.set(false);
        self.render_chain();
        self.disable_actions();
    }

    fn selected_wait(&self) -> Option<LockWait> {
        self.selection
            .selected_item()
            .and_downcast::<glib::BoxedAnyObject>()
            .map(|row| row.borrow::<LockWait>().clone())
    }

    fn disable_actions(&self) {
        for (_, button) in &self.actions {
            button.set_sensitive(false);
        }
    }
}
