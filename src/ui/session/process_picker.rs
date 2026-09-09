use super::*;
use crate::{
    local_process::{Process, ProcessIdentity, Snapshot},
    session_request::SessionRequest,
};

use std::sync::mpsc;

#[cfg(test)]
mod tests;

pub(super) struct ProcessPicker {
    pub root: gtk::Box,
    pid: gtk::Entry,
    store: gio::ListStore,
    selection: gtk::SingleSelection,
    filtered: gtk::FilterListModel,
    refresh: gtk::Button,
    status: gtk::Label,
    detail: gtk::Label,
    selected: RefCell<Option<ProcessIdentity>>,
    syncing: Cell<bool>,
    loading: Cell<bool>,
    loaded: Cell<bool>,
    cancelled: Arc<AtomicBool>,
    excluded: Vec<u32>,
    debugger_pid: Option<u32>,
    skipped: Cell<usize>,
    limited: Cell<bool>,
}

impl ProcessPicker {
    pub(super) fn new(pid: &gtk::Entry, debugger_pid: Option<u32>) -> Rc<Self> {
        let root = gtk::Box::new(gtk::Orientation::Vertical, components::CONTROL_GAP);
        root.set_vexpand(true);
        let controls = components::control_row();
        let search =
            components::delayed_search_entry("Filter PID, executable, command line, or owner");

        search.set_hexpand(true);
        let own = gtk::CheckButton::with_label("Own processes");
        own.set_active(true);
        own.set_valign(gtk::Align::Center);
        own.set_tooltip_text(Some("Show processes with your real user ID. Names are resolved from local accounts, with a numeric UID fallback"));
        let refresh = gtk::Button::with_label("Refresh");
        refresh.set_tooltip_text(Some("Read a fresh local process snapshot"));
        controls.append(&search);
        controls.append(&own);
        controls.append(&refresh);
        root.append(&controls);
        let query = Rc::new(RefCell::new(String::new()));
        let filter_query = Rc::clone(&query);
        let own_only = Rc::new(Cell::new(true));
        let filter_own = Rc::clone(&own_only);
        let uid = rustix::process::getuid().as_raw();

        let filter = gtk::CustomFilter::new(move |object| {
            let process = object
                .downcast_ref::<glib::BoxedAnyObject>()
                .unwrap()
                .borrow::<Process>();

            (!filter_own.get() || process.uid == Some(uid))
                && filter_query
                    .borrow()
                    .split_whitespace()
                    .all(|term| process.search.contains(term))
        });

        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        let filtered = gtk::FilterListModel::new(Some(store.clone()), Some(filter.clone()));
        let sorted = gtk::SortListModel::new(Some(filtered.clone()), None::<gtk::Sorter>);
        let selection = gtk::SingleSelection::new(Some(sorted.clone()));
        selection.set_autoselect(false);
        selection.set_can_unselect(true);
        let table = components::column_view(selection.clone());
        table.add_css_class("debug-table");
        table.set_vexpand(true);

        for column in Column::ALL {
            let widget = column.build();
            table.append_column(&widget);

            if matches!(column, Column::Pid) {
                table.sort_by_column(Some(&widget), gtk::SortType::Ascending);
            }
        }

        sorted.set_sorter(table.sorter().as_ref());

        let scroll = gtk::ScrolledWindow::builder()
            .child(&table)
            .min_content_height(200)
            .vexpand(true)
            .hexpand(true)
            .overlay_scrolling(false)
            .build();

        root.append(&scroll);
        let status = components::empty_label("Processes are loaded when you open Attach");
        root.append(&status);
        let detail = components::empty_label(
            "Select a process above, or enter a PID below. Refresh updates the snapshot",
        );

        detail.set_lines(3);
        detail.set_ellipsize(pango::EllipsizeMode::End);
        enable_stable_text_selection(&detail);
        root.append(&detail);
        let mut excluded = vec![std::process::id()];
        excluded.extend(debugger_pid);

        let picker = Rc::new(Self {
            root,
            pid: pid.clone(),
            store,
            selection,
            filtered,
            refresh,
            status,
            detail,
            selected: RefCell::new(None),
            syncing: Cell::new(false),
            loading: Cell::new(false),
            loaded: Cell::new(false),
            cancelled: Arc::new(AtomicBool::new(false)),
            excluded,
            debugger_pid,
            skipped: Cell::new(0),
            limited: Cell::new(false),
        });

        let search_filter = filter.clone();

        search.connect_search_changed(move |search| {
            query.replace(search.text().to_lowercase());
            search_filter.changed(gtk::FilterChange::Different);
        });

        own.connect_toggled(move |button| {
            own_only.set(button.is_active());
            filter.changed(gtk::FilterChange::Different);
        });

        let weak = Rc::downgrade(&picker);

        picker.root.connect_map(move |_| {
            if let Some(picker) = weak.upgrade()
                && !picker.loaded.get()
            {
                picker.refresh();
            }
        });

        let weak = Rc::downgrade(&picker);

        picker.refresh.connect_clicked(move |_| {
            if let Some(picker) = weak.upgrade() {
                picker.refresh();
            }
        });

        let weak = Rc::downgrade(&picker);

        picker.filtered.connect_items_changed(move |_, _, _, _| {
            if let Some(picker) = weak.upgrade() {
                picker.update_count();
            }
        });

        let weak = Rc::downgrade(&picker);

        picker.selection.connect_selected_notify(move |_| {
            if let Some(picker) = weak.upgrade() {
                picker.choose_selected();
            }
        });

        let weak = Rc::downgrade(&picker);

        pid.connect_changed(move |_| {
            if let Some(picker) = weak.upgrade()
                && !picker.syncing.get()
            {
                picker.selected.borrow_mut().take();
                picker.syncing.set(true);
                picker.selection.unselect_all();
                picker.syncing.set(false);
                picker
                    .detail
                    .set_text("Manual PID. The process identity will be verified before attaching");

                picker.detail.set_tooltip_text(None);
            }
        });

        picker
    }

    pub(super) fn request(&self, session: DebugSession) -> Result<SessionRequest, String> {
        let mut request = SessionRequest {
            session,
            attach_identity: *self.selected.borrow(),
        };

        request.attach_identity = request.validate_attach(self.debugger_pid)?;
        Ok(request)
    }

    fn refresh(self: &Rc<Self>) {
        if self.loading.replace(true) {
            return;
        }

        self.refresh.set_sensitive(false);
        self.status.set_text("Reading local processes…");
        let cancelled = Arc::clone(&self.cancelled);
        let current = Arc::clone(&cancelled);
        let excluded = self.excluded.clone();
        let (send, receive) = mpsc::sync_channel(1);

        let result = crate::background::submit_cancellable_with_priority(
            crate::background::Priority::Interactive,
            move || !current.load(Ordering::Relaxed),
            move || {
                let _ = send.send(crate::local_process::scan(&cancelled, &excluded));
            },
        );

        if let Err(error) = result {
            self.finish_refresh(Err(format!("Process refresh unavailable: {error}")));
            return;
        }

        let weak = Rc::downgrade(self);

        glib::timeout_add_local(Duration::from_millis(20), move || {
            let Some(picker) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };

            match receive.try_recv() {
                Ok(result) => picker.finish_refresh(result),
                Err(mpsc::TryRecvError::Empty) => return glib::ControlFlow::Continue,
                Err(mpsc::TryRecvError::Disconnected) => picker.finish_refresh(Err(String::from(
                    "Process discovery stopped before completing. Refresh to retry",
                ))),
            }

            glib::ControlFlow::Break
        });
    }

    fn finish_refresh(&self, result: Result<Snapshot, String>) {
        self.loading.set(false);
        self.refresh.set_sensitive(true);

        match result {
            Ok(snapshot) => {
                self.loaded.set(true);
                self.skipped.set(snapshot.skipped);
                self.limited.set(snapshot.limited);
                let expected = *self.selected.borrow();
                self.syncing.set(true);
                components::replace_boxed_store_if_changed(&self.store, snapshot.processes);

                let position = expected.and_then(|identity| {
                    (0..self.selection.n_items()).find(|index| {
                        self.selection
                            .item(*index)
                            .and_downcast::<glib::BoxedAnyObject>()
                            .is_some_and(|row| row.borrow::<Process>().identity == identity)
                    })
                });

                self.selection
                    .set_selected(position.unwrap_or(gtk::INVALID_LIST_POSITION));
                self.syncing.set(false);
                self.update_count();

                if expected.is_some() && position.is_none() {
                    self.detail.set_text("The selected process is no longer visible. It may have exited or be hidden by a filter. Its original identity is retained for attach validation");
                } else {
                    self.choose_selected();
                }
            }
            Err(error) => self.status.set_text(&error),
        }
    }

    fn update_count(&self) {
        if self.loading.get() {
            return;
        }
        let mut message = format!(
            "{} shown / {} local processes",
            self.filtered.n_items(),
            self.store.n_items()
        );

        if self.skipped.get() > 0 {
            message.push_str(&format!("  {} unavailable or exited", self.skipped.get()));
        }

        if self.limited.get() {
            message.push_str("  Collection limit reached, snapshot is partial");
        }

        self.status.set_text(&message);
        self.status.set_tooltip_text(Some("Only processes visible in fgdb's PID namespace and /proc mount are listed. Permission-restricted metadata may be unavailable. No continuous polling"));
    }

    fn choose_selected(&self) {
        if self.syncing.get() {
            return;
        }
        let Some(row) = self
            .selection
            .selected_item()
            .and_downcast::<glib::BoxedAnyObject>()
        else {
            return;
        };

        let process = row.borrow::<Process>();
        self.selected.replace(Some(process.identity));
        self.syncing.set(true);
        self.pid.set_text(&process.identity.pid.to_string());
        self.syncing.set(false);
        let mut detail = format!("{}\n{}", process.executable, process.command);

        if !process.detail.is_empty() {
            detail.push_str(&format!("\n{}", process.detail));
        }

        self.detail.set_text(&detail);
        self.detail.set_tooltip_text(Some(&detail));
    }
}

impl Drop for ProcessPicker {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }
}

#[derive(Clone, Copy)]
enum Column {
    Pid,
    Executable,
    Owner,
    Command,
}

impl Column {
    const ALL: [Self; 4] = [Self::Pid, Self::Executable, Self::Owner, Self::Command];

    fn text(self, process: &Process) -> Cow<'_, str> {
        match self {
            Self::Pid => Cow::Owned(process.identity.pid.to_string()),
            Self::Executable => Cow::Borrowed(process.executable_name()),
            Self::Owner => Cow::Borrowed(&process.owner),
            Self::Command => Cow::Borrowed(&process.command),
        }
    }

    fn build(self) -> gtk::ColumnViewColumn {
        let factory = gtk::SignalListItemFactory::new();

        factory.connect_setup(move |_, object| {
            let item = object.downcast_ref::<gtk::ListItem>().unwrap();
            let label = gtk::Label::new(None);
            label.set_xalign(0.0);
            label.set_ellipsize(pango::EllipsizeMode::End);
            label.add_css_class("debug-table-cell");
            item.set_child(Some(&label));
        });

        factory.connect_bind(move |_, object| {
            let item = object.downcast_ref::<gtk::ListItem>().unwrap();
            let row = item.item().and_downcast::<glib::BoxedAnyObject>().unwrap();
            let label = item.child().and_downcast::<gtk::Label>().unwrap();
            let process = row.borrow::<Process>();
            let text = self.text(&process);
            label.set_text(&text);
            label.set_tooltip_text(Some(if matches!(self, Self::Executable) {
                &process.executable
            } else {
                &text
            }));
        });

        let (title, width) = match self {
            Self::Pid => ("PID", 75),
            Self::Executable => ("EXECUTABLE", 195),
            Self::Owner => ("OWNER", 130),
            Self::Command => ("COMMAND LINE", 280),
        };

        let column = components::table_column(title, width, factory);

        column.set_sorter(Some(&gtk::CustomSorter::new(move |left, right| {
            let left = left
                .downcast_ref::<glib::BoxedAnyObject>()
                .unwrap()
                .borrow::<Process>();

            let right = right
                .downcast_ref::<glib::BoxedAnyObject>()
                .unwrap()
                .borrow::<Process>();

            let order = match self {
                Self::Pid => left.identity.pid.cmp(&right.identity.pid),
                Self::Executable => left.executable_name().cmp(right.executable_name()),
                Self::Owner => left.owner.cmp(&right.owner),
                Self::Command => left.command.cmp(&right.command),
            };

            order
                .then_with(|| left.identity.pid.cmp(&right.identity.pid))
                .into()
        })));

        column
    }
}
