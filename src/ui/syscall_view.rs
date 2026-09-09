use super::*;
use crate::syscalls::{Abi, Counts, Key, Metadata};
use std::{cell::OnceCell, sync::OnceLock};

#[derive(Clone, Copy)]
pub(crate) enum SyscallAction {
    Start,
    Stop,
    Reset,
}

struct RowData {
    key: Key,
    known: bool,
    name: Cow<'static, str>,
    category: &'static str,
    arguments: &'static str,
    search: String,
    count: Cell<u64>,
    share: Cell<f64>,
}

mod imp {
    use super::*;
    use glib::subclass::prelude::*;

    #[derive(Default)]
    pub struct Row {
        pub(super) data: OnceCell<RowData>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Row {
        const NAME: &'static str = "FgdbSyscallRow";
        type Type = super::Row;
    }

    impl ObjectImpl for Row {
        fn signals() -> &'static [glib::subclass::Signal] {
            static SIGNALS: OnceLock<Vec<glib::subclass::Signal>> = OnceLock::new();
            SIGNALS.get_or_init(|| vec![glib::subclass::Signal::builder("updated").build()])
        }
    }
}

glib::wrapper! {
    pub struct Row(ObjectSubclass<imp::Row>);
}

impl Row {
    fn new(key: Key, metadata: Option<&Metadata>) -> Self {
        use glib::subclass::types::ObjectSubclassIsExt;

        let row: Self = glib::Object::new();
        let name = metadata.map_or_else(
            || Cow::Owned(format!("Unknown syscall #{}", key.number_text())),
            |metadata| Cow::Borrowed(metadata.name),
        );

        let category = metadata.map_or("-", |metadata| metadata.category);
        let arguments = metadata.map_or("-", |metadata| metadata.arguments);

        let search = format!(
            "{} {} {} {category} {arguments}",
            name,
            key.number_text(),
            key.abi.label()
        )
        .to_lowercase();

        let data = RowData {
            key,
            known: metadata.is_some(),
            name,
            category,
            arguments,
            search,
            count: Cell::new(0),
            share: Cell::new(0.0),
        };

        assert!(row.imp().data.set(data).is_ok());
        row
    }

    fn data(&self) -> &RowData {
        use glib::subclass::types::ObjectSubclassIsExt;
        self.imp().data.get().expect("initialized syscall row")
    }
}

#[derive(Clone)]
pub(super) struct SyscallView {
    pub(super) root: gtk::Box,
    start: gtk::Button,
    stop: gtk::Button,
    reset: gtk::Button,
    status: gtk::Label,
    summary: gtk::Label,
    loss: gtk::Label,
    store: gio::ListStore,
    rows: Rc<RefCell<HashMap<Key, Row>>>,
    filter: gtk::CustomFilter,
    sorter: gtk::Sorter,
}

impl SyscallView {
    pub(super) fn new(layout: &TableLayout) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, components::CONTROL_GAP);
        root.set_size_request(0, 0);
        components::inset(&root, components::CONTENT_INSET);
        let header = components::control_row();
        header.append(&section_title("SYSCALL FREQUENCY"));
        let summary = gtk::Label::new(Some("0 entries   0 seen"));
        summary.set_hexpand(true);
        summary.set_xalign(0.0);
        summary.add_css_class("muted");
        header.append(&summary);
        let start = gtk::Button::with_label("Start");
        start.set_tooltip_text(Some("Start a new count for the selected local inferior, including all its threads. Child processes are not included"));
        let stop = gtk::Button::with_label("Stop");
        stop.set_sensitive(false);
        stop.set_tooltip_text(Some("Detach the counter and keep the final counts"));
        let reset = gtk::Button::with_label("Reset");
        reset.set_tooltip_text(Some("Reset counts without restarting the target"));
        header.append(&start);
        header.append(&stop);
        header.append(&reset);
        root.append(&header);
        let status = gtk::Label::new(Some("Inactive. Start collection for a live local inferior"));
        status.set_xalign(0.0);
        status.set_wrap(true);
        status.add_css_class("muted");
        enable_stable_text_selection(&status);
        root.append(&status);
        let note = gtk::Label::new(Some(
            "Entry counts only. Updates once per second. No GDB catchpoints or argument-value reads",
        ));

        note.set_xalign(0.0);
        note.set_wrap(true);
        note.add_css_class("muted");
        note.set_tooltip_text(Some("Collection has a small tracing cost. Counts cover observed syscall entries, including failed calls, and may count restarts again. Calls hidden by seccomp or tracing policy cannot be observed. ABI names and logical argument names are cached metadata, not proof that this kernel implements a syscall"));
        root.append(&note);
        let controls = components::control_row();
        let search = components::delayed_search_entry(
            "Filter syscall, number, ABI, category, or argument name",
        );

        search.set_hexpand(true);
        controls.append(&search);
        let seen = gtk::CheckButton::with_label("Seen only");
        seen.set_active(true);
        seen.set_tooltip_text(Some(
            "Hide zero counts. Turn off to browse the bundled catalog for this host's syscall ABIs",
        ));

        controls.append(&seen);
        root.append(&controls);
        let query = Rc::new(RefCell::new(String::new()));
        let only_seen = Rc::new(Cell::new(true));
        let filter_query = Rc::clone(&query);
        let filter_seen = Rc::clone(&only_seen);

        let filter = gtk::CustomFilter::new(move |object| {
            let Some(row) = object.downcast_ref::<Row>() else {
                return false;
            };

            let data = row.data();
            (!filter_seen.get() || data.count.get() > 0)
                && data.search.contains(filter_query.borrow().as_str())
        });

        let store = gio::ListStore::new::<Row>();
        let rows = Rc::new(RefCell::new(HashMap::new()));
        let catalog_rows = Rc::clone(&rows);
        let catalog_store = store.clone();

        root.connect_map(move |_| ensure_catalog(&catalog_store, &catalog_rows));
        let filtered = gtk::FilterListModel::new(Some(store.clone()), Some(filter.clone()));
        let sorted = gtk::SortListModel::new(Some(filtered.clone()), None::<gtk::Sorter>);
        let view = components::column_view(gtk::NoSelection::new(Some(sorted.clone())));
        view.add_css_class("debug-table");
        view.add_css_class("misc-data-table");
        view.set_reorderable(true);

        for column in Column::ALL {
            let widget = column.build();
            let key = match column {
                Column::Name => "name",
                Column::Count => "count",
                Column::Share => "share",
                Column::Number => "number",
                Column::Abi => "abi",
                Column::Category => "category",
                Column::Arguments => "arguments",
            };

            layout.append(&view, key, &widget);

            if matches!(column, Column::Count) {
                view.sort_by_column(Some(&widget), gtk::SortType::Descending);
            }
        }

        let sorter = view.sorter().expect("column view sorter");
        let sorters = gtk::MultiSorter::new();
        sorters.append(sorter.clone());

        sorters.append(gtk::CustomSorter::new(|left, right| {
            let left = left.downcast_ref::<Row>().unwrap().data();
            let right = right.downcast_ref::<Row>().unwrap().data();
            left.name
                .cmp(&right.name)
                .then_with(|| left.key.cmp(&right.key))
                .into()
        }));

        sorted.set_sorter(Some(&sorters));
        let empty = gtk::Label::new(Some("No syscalls match the current filters"));
        empty.add_css_class("muted");
        components::inset(&empty, components::CONTENT_INSET);
        let empty_weak = empty.downgrade();

        filtered.connect_items_changed(move |model, _, _, _| {
            if let Some(empty) = empty_weak.upgrade() {
                empty.set_visible(model.n_items() == 0);
            }
        });

        root.append(&empty);

        let scroll = gtk::ScrolledWindow::builder()
            .child(&view)
            .hexpand(true)
            .vexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .build();

        misc_view::configure_misc_scroller(&scroll);
        root.append(&scroll);
        let loss = gtk::Label::new(None);
        loss.set_xalign(0.0);
        loss.set_wrap(true);
        loss.add_css_class("status-warning");
        loss.set_visible(false);
        root.append(&loss);
        let search_filter = filter.clone();

        search.connect_search_changed(move |search| {
            let text = search.text().trim().to_lowercase();

            if *query.borrow() != text {
                query.replace(text);
                search_filter.changed(gtk::FilterChange::Different);
            }
        });

        let seen_filter = filter.clone();

        seen.connect_toggled(move |seen| {
            only_seen.set(seen.is_active());
            seen_filter.changed(gtk::FilterChange::Different);
        });

        Self {
            root,
            start,
            stop,
            reset,
            status,
            summary,
            loss,
            store,
            rows,
            filter,
            sorter,
        }
    }

    fn clear(&self) {
        self.render(&Counts::default());
        self.rows.borrow_mut().retain(|_, row| row.data().known);
        self.store.retain(|object| {
            object
                .downcast_ref::<Row>()
                .is_some_and(|row| row.data().known)
        });
    }

    fn render(&self, counts: &Counts) {
        ensure_catalog(&self.store, &self.rows);
        let incoming: HashMap<_, _> = counts.entries.iter().copied().collect();
        let total = counts
            .entries
            .iter()
            .fold(0_u128, |total, (_, count)| total + u128::from(*count));

        let seen = counts
            .entries
            .iter()
            .filter(|(_, count)| *count != 0)
            .count();

        set_cell(&self.summary, &format!("{total} entries   {seen} seen"));
        self.loss.set_visible(counts.lost != 0);

        if counts.lost != 0 {
            self.loss.set_text(&format!(
                "At least {} entries could not be counted. Counter capacity or range was exceeded",
                counts.lost
            ));
        }

        let mut changed = false;
        let mut refilter = false;
        let mut additions = Vec::new();

        // Release collection borrows before GTK can bind rows or emit signals.
        let rows: Vec<_> = {
            let mut rows = self.rows.borrow_mut();

            for key in incoming.keys() {
                rows.entry(*key).or_insert_with(|| {
                    let row = Row::new(*key, None);
                    additions.push(row.clone());
                    row
                });
            }

            rows.values().cloned().collect()
        };

        for row in rows {
            let data = row.data();
            let count = incoming.get(&data.key).copied().unwrap_or_default();
            let share = if total == 0 {
                0.0
            } else {
                count as f64 / total as f64 * 100.0
            };

            let old_count = data.count.replace(count);
            let old_share = data.share.replace(share);
            changed |= old_count != count;
            refilter |= (old_count == 0) != (count == 0);

            if old_count != count || old_share != share {
                row.emit_by_name::<()>("updated", &[]);
            }
        }

        self.store.splice(self.store.n_items(), 0, &additions);

        if refilter {
            self.filter.changed(gtk::FilterChange::Different);
        }

        if changed {
            self.sorter.changed(gtk::SorterChange::Different);
        }
    }
}

#[derive(Clone, Copy)]
enum Column {
    Name,
    Count,
    Share,
    Number,
    Abi,
    Category,
    Arguments,
}

impl Column {
    const ALL: [Self; 7] = [
        Self::Name,
        Self::Count,
        Self::Share,
        Self::Number,
        Self::Abi,
        Self::Category,
        Self::Arguments,
    ];

    fn text(self, row: &RowData) -> Cow<'_, str> {
        match self {
            Self::Name => Cow::Borrowed(&row.name),
            Self::Count => row.count.get().to_string().into(),
            Self::Share => format!("{:.1}%", row.share.get()).into(),
            Self::Number => row.key.number_text().into(),
            Self::Abi => row.key.abi.label().into(),
            Self::Category => row.category.into(),
            Self::Arguments => row.arguments.into(),
        }
    }

    fn build(self) -> gtk::ColumnViewColumn {
        let (title, width) = match self {
            Self::Name => ("Syscall", 200),
            Self::Count => ("Count", 100),
            Self::Share => ("Share", 75),
            Self::Number => ("Number", 115),
            Self::Abi => ("ABI", 85),
            Self::Category => ("Category", 100),
            Self::Arguments => ("Arguments", 280),
        };

        let factory = gtk::SignalListItemFactory::new();

        factory.connect_setup(move |_, object| {
            let item = object
                .downcast_ref::<gtk::ListItem>()
                .expect("syscall list item");

            let label = gtk::Label::new(None);
            label.add_css_class("debug-table-cell");
            label.set_xalign(
                if matches!(self, Self::Count | Self::Share | Self::Number) {
                    1.0
                } else {
                    0.0
                },
            );

            label.set_ellipsize(pango::EllipsizeMode::End);
            enable_stable_text_selection(&label);
            item.set_child(Some(&label));
            let subscription = RefCell::new(None::<lifecycle::SignalSubscription>);

            item.connect_item_notify(move |item| {
                subscription.borrow_mut().take();
                clear_label_selection(&label);

                let Some(row) = item.item().and_downcast::<Row>() else {
                    label.set_text("");
                    return;
                };

                set_cell(&label, &self.text(row.data()));

                if !matches!(self, Self::Count | Self::Share) {
                    return;
                }

                let label = label.downgrade();
                let item = item.downgrade();

                let handler = row.connect_local("updated", false, move |values| {
                    let row = values[0].get::<Row>().expect("syscall row signal");

                    if let (Some(item), Some(label)) = (item.upgrade(), label.upgrade())
                        && item.item().as_ref() == Some(row.upcast_ref())
                    {
                        set_cell(&label, &self.text(row.data()));
                    }

                    None
                });

                subscription.replace(Some(lifecycle::SignalSubscription::new(&row, handler)));
            });
        });

        let column = components::table_column(title, width, factory);

        column.set_sorter(Some(&gtk::CustomSorter::new(move |left, right| {
            let left = left.downcast_ref::<Row>().unwrap().data();
            let right = right.downcast_ref::<Row>().unwrap().data();

            let order = match self {
                Self::Name => left.name.cmp(&right.name),
                Self::Count | Self::Share => left.count.get().cmp(&right.count.get()),
                Self::Number => (left.key.number as i64).cmp(&(right.key.number as i64)),
                Self::Abi => left.key.abi.cmp(&right.key.abi),
                Self::Category => left.category.cmp(right.category),
                Self::Arguments => left.arguments.cmp(right.arguments),
            };

            order.into()
        })));

        column
    }
}

fn ensure_catalog(store: &gio::ListStore, rows: &RefCell<HashMap<Key, Row>>) {
    if !rows.borrow().is_empty() {
        return;
    }

    let catalog: HashMap<_, _> = crate::syscalls::catalog()
        .iter()
        .filter(|metadata| Abi::host_abis().contains(&metadata.key.abi))
        .map(|metadata| (metadata.key, Row::new(metadata.key, Some(metadata))))
        .collect();

    let additions: Vec<_> = catalog.values().cloned().collect();
    rows.replace(catalog);
    store.splice(0, 0, &additions);
}

fn set_cell(label: &gtk::Label, text: &str) {
    if label.text().as_str() != text {
        label.set_text(text);
        label.set_tooltip_text(Some(text));
    }
}

impl Ui {
    pub(crate) fn connect_syscall_actions(&self, handler: impl Fn(SyscallAction) + 'static) {
        let handler = Rc::new(handler);
        let view = &self.misc_view.syscalls;

        for (button, action) in [
            (&view.start, SyscallAction::Start),
            (&view.stop, SyscallAction::Stop),
            (&view.reset, SyscallAction::Reset),
        ] {
            let handler = Rc::clone(&handler);
            button.connect_clicked(move |_| handler(action));
        }
    }

    pub(crate) fn set_syscall_state(&self, text: &str, active: bool, stopping: bool) {
        let view = &self.misc_view.syscalls;
        set_cell(&view.status, text);
        view.start.set_sensitive(!active);
        view.stop.set_sensitive(active && !stopping);
        view.reset.set_sensitive(!stopping);
    }

    pub(crate) fn render_syscall_counts(&self, counts: &Counts) {
        self.misc_view.syscalls.render(counts);
    }

    pub(crate) fn clear_syscall_counts(&self) {
        self.misc_view.syscalls.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires a GTK display"]
    fn counts_keep_row_identity_and_filter_without_rebuilding_unchanged_cells() {
        gtk::init().unwrap();
        Theme::graphite().install();
        let view = SyscallView::new(
            &crate::ui::ColumnLayouts::default().table(crate::ui::TableId::Syscalls),
        );

        assert_eq!(view.store.n_items(), 0);
        let abi = Abi::host_abis()[0];
        let key = crate::syscalls::catalog()
            .iter()
            .find(|metadata| metadata.key.abi == abi && metadata.name == "read")
            .unwrap()
            .key;

        let unknown = Key {
            abi,
            number: u64::MAX,
        };

        let counts = Counts {
            entries: vec![(key, 9), (unknown, 3)],
            lost: 0,
        };

        view.render(&counts);
        let row = view.rows.borrow()[&key].clone();
        assert_eq!(row.data().name, "read");
        assert_eq!(row.data().share.get(), 75.0);
        let changes = Rc::new(Cell::new(0));
        let observed = Rc::clone(&changes);

        row.connect_local("updated", false, move |_| {
            observed.set(observed.get() + 1);
            None
        });

        view.render(&counts);
        assert_eq!(changes.get(), 0);
        assert_eq!(view.rows.borrow()[&key], row);
        let window = gtk::Window::builder()
            .child(&view.root)
            .default_width(1100)
            .default_height(600)
            .build();

        window.present();

        let context = glib::MainContext::default();
        while context.pending() {
            context.iteration(false);
        }

        fn find<T: IsA<gtk::Widget> + glib::types::StaticType>(root: &gtk::Widget) -> Option<T> {
            if let Ok(widget) = root.clone().downcast::<T>() {
                return Some(widget);
            }

            let mut child = root.first_child();

            while let Some(widget) = child {
                if let Some(found) = find::<T>(&widget) {
                    return Some(found);
                }

                child = widget.next_sibling();
            }

            None
        }

        let table = find::<gtk::ColumnView>(view.root.upcast_ref()).unwrap();
        let model = table.model().unwrap();
        assert_eq!(model.n_items(), 2);
        assert_eq!(model.item(0).and_downcast::<Row>().unwrap(), row);
        let search = find::<gtk::SearchEntry>(view.root.upcast_ref()).unwrap();
        search.set_text("unknown");
        search.emit_by_name::<()>("search-changed", &[]);
        assert_eq!(model.n_items(), 1);
        assert_eq!(
            model.item(0).and_downcast::<Row>().unwrap().data().key,
            unknown
        );

        search.set_text("");
        search.emit_by_name::<()>("search-changed", &[]);
        view.clear();
        assert_eq!(model.n_items(), 0);
        assert!(!view.rows.borrow().contains_key(&unknown));
        let seen = find::<gtk::CheckButton>(view.root.upcast_ref()).unwrap();
        seen.set_active(false);
        assert_eq!(model.n_items(), view.store.n_items());
        window.close();
    }
}
