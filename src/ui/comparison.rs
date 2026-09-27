//! Explicit, bounded comparisons of accepted UI data. No debugger polling.

use super::*;
use std::collections::BTreeMap;

const MAX_PINS: usize = 8;
const MAX_ROWS: usize = 256;
const MAX_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
enum Source {
    Local(Vec<(String, bool, Option<usize>)>),
    Watch(Vec<(String, bool, Option<usize>)>),
    Registers,
    Memory { id: u64, address: u64, bytes: usize },
}

impl Source {
    fn description(&self) -> String {
        match self {
            Self::Local(path) | Self::Watch(path) => format!(
                "{} · {}",
                if matches!(self, Self::Local(_)) {
                    "Local / argument"
                } else {
                    "Watch"
                },
                path.iter()
                    .map(|(name, _, _)| name.as_str())
                    .collect::<Vec<_>>()
                    .join(" / ")
            ),
            Self::Registers => "Registers".into(),
            Self::Memory { address, bytes, .. } => format!("Memory · 0x{address:x} · {bytes} B"),
        }
    }
}

#[derive(Clone)]
struct Origin {
    context: crate::debugger::StopContext,
    sequence: u64,
    symbols: u64,
    function: Option<String>,
}

impl Origin {
    fn compatible(&self, other: &Self) -> bool {
        self.context.transport_epoch() == other.context.transport_epoch()
            && self.context.inferior_id() == other.context.inferior_id()
            && self.context.thread_id() == other.context.thread_id()
            && self.context.frame_level() == other.context.frame_level()
            && self.symbols == other.symbols
            && self.function == other.function
    }

    fn description(&self) -> String {
        format!(
            "Stop {} · {} · thread {} · frame {} · {}",
            self.sequence,
            self.context.inferior_id().unwrap_or("inferior unknown"),
            self.context.thread_id(),
            self.context.frame_level(),
            self.function.as_deref().unwrap_or("function unknown")
        )
    }
}

#[derive(Clone)]
struct Baseline {
    source: Source,
    origin: Origin,
    values: BTreeMap<String, String>,
}

struct Pin {
    baseline: Baseline,
    compared: Option<Result<Baseline, String>>,
}

#[derive(Default)]
pub(super) struct Comparisons {
    pins: RefCell<Vec<Pin>>,
    window: glib::WeakRef<gtk::Window>,
    selected: Cell<u32>,
}

impl Comparisons {
    fn pin(&self, baseline: Baseline) -> Result<usize, &'static str> {
        let mut pins = self.pins.borrow_mut();

        let previous = pins.iter().position(|pin| {
            pin.baseline.source == baseline.source
                && pin.baseline.origin.compatible(&baseline.origin)
        });

        let pin = Pin {
            baseline,
            compared: None,
        };

        if let Some(index) = previous {
            pins[index] = pin;
            Ok(index)
        } else if pins.len() < MAX_PINS {
            pins.push(pin);
            Ok(pins.len() - 1)
        } else {
            Err("Eight pins retained. Clear pins before capturing another.")
        }
    }
}

fn insert(
    values: &mut BTreeMap<String, String>,
    name: String,
    value: String,
    bytes: &mut usize,
) -> Result<(), String> {
    *bytes = bytes.saturating_add(name.len()).saturating_add(value.len());

    if values.len() >= MAX_ROWS || *bytes > MAX_BYTES {
        return Err(
            "Capture exceeds 256 rows or 64 KiB. Select a smaller value or memory range.".into(),
        );
    }

    if values.insert(name, value).is_some() {
        return Err("Ambiguous variable paths cannot be compared safely".into());
    }

    Ok(())
}

fn variable_values(
    node: &VariableNode,
    values: &mut BTreeMap<String, String>,
    prefix: &str,
    bytes: &mut usize,
    depth: usize,
) -> Result<(), String> {
    if node.placeholder || depth > 16 {
        return Err("Expand a shallower, fully loaded value before capturing".into());
    }

    let variable = &node.variable;
    let name = format!(
        "{prefix}{} [{}]",
        variable.name,
        variable.type_name.as_deref().unwrap_or("?")
    );

    insert(values, name.clone(), variable.value.clone(), bytes)?;

    if node.children_loaded.get() && node.children.n_items() > 0 {
        for position in 0..node.children.n_items() {
            let item = node
                .children
                .item(position)
                .and_downcast::<SnapshotRow>()
                .ok_or("Variable row unavailable")?;

            let child = item.borrow::<VariableNode>();

            if !child.placeholder {
                variable_values(&child, values, &format!("{name} / "), bytes, depth + 1)?;
            }
        }
    }

    Ok(())
}

fn selection_path(tree: &VariableTree) -> Option<Vec<(String, bool, Option<usize>)>> {
    let (mut row, _) = variable_node_at(&tree.selection, tree.selection.selected())?;
    let mut path = Vec::new();

    loop {
        let item = row.item().and_downcast::<SnapshotRow>()?;
        let node = item.borrow::<VariableNode>();

        if node.placeholder || path.len() >= 16 {
            return None;
        }

        path.push((
            node.variable.name.clone(),
            node.variable.argument,
            node.variable.local_index,
        ));

        let Some(parent) = row.parent() else { break };
        row = parent;
    }

    path.reverse();

    Some(path)
}

fn find_path(tree: &VariableTree, path: &[(String, bool, Option<usize>)]) -> Option<VariableNode> {
    let mut store = tree.store.clone();
    let mut result = None;

    for (name, argument, index) in path {
        let mut found = None;

        for position in 0..store.n_items().min(4096) {
            let item = store.item(position).and_downcast::<SnapshotRow>()?;
            let node = item.borrow::<VariableNode>();

            if !node.placeholder
                && node.variable.name == *name
                && node.variable.argument == *argument
                && node.variable.local_index == *index
            {
                if found.is_some() {
                    return None;
                }

                found = Some(node.clone());
            }
        }

        let node = found?;
        store = node.children.clone();
        result = Some(node);
    }

    result
}

struct DifferenceRow {
    name: String,
    before: Option<String>,
    after: Option<String>,
    compared: bool,
}

impl DifferenceRow {
    fn changed(&self) -> bool {
        self.compared && self.before != self.after
    }

    fn cells(&self) -> [&str; 4] {
        let status = match (&self.before, &self.after) {
            _ if !self.compared => "Pinned",
            (None, _) => "Newly loaded",
            (_, None) => "Not loaded",
            _ if self.changed() => "Changed",
            _ => "Unchanged",
        };

        [
            &self.name,
            self.before.as_deref().unwrap_or("—"),
            self.after.as_deref().unwrap_or("—"),
            status,
        ]
    }
}

fn difference(
    before: &BTreeMap<String, String>,
    after: Option<&BTreeMap<String, String>>,
) -> Vec<DifferenceRow> {
    let names: std::collections::BTreeSet<_> = before
        .keys()
        .chain(after.into_iter().flat_map(BTreeMap::keys))
        .collect();

    names
        .into_iter()
        .map(|name| DifferenceRow {
            name: name.clone(),
            before: before.get(name).cloned(),
            after: after.and_then(|values| values.get(name)).cloned(),
            compared: after.is_some(),
        })
        .collect()
}

impl Ui {
    fn comparison_origin(&self) -> Result<Origin, String> {
        let generation = self.model.current_stop_refresh_generation();

        if !self.model.is_stop_refresh_current(generation)
            || !self.model.stopped_inspection_available()
        {
            return Err("Pause the target and wait for the current frame to load".into());
        }

        let context = self
            .model
            .stop_context(generation)
            .ok_or("No stopped context")?;

        let frames = self
            .model
            .frames_for_details(generation)
            .ok_or("Current call stack has not loaded")?;

        let function = frames
            .iter()
            .find(|frame| frame.level == context.frame_level())
            .map(|frame| frame.function.clone());

        Ok(Origin {
            context,
            function,
            sequence: self.model.observed_stop_sequence(),
            symbols: self.model.symbols.revision(),
        })
    }

    fn comparison_source(&self, selection: u32) -> Result<Source, String> {
        match selection {
            0 => selection_path(&self.locals_tree)
                .map(Source::Local)
                .ok_or_else(|| "Select a local or argument".into()),
            1 => selection_path(&self.watches_tree)
                .map(Source::Watch)
                .ok_or_else(|| "Select a watch".into()),
            2 => Ok(Source::Registers),
            3 => {
                let page = self
                    .memory_watch_container
                    .notebook
                    .current_page()
                    .ok_or("Open a memory inspector first")?;

                let watches = self.memory_watches.borrow();
                let watch = watches
                    .iter()
                    .find(|watch| {
                        self.memory_watch_container.notebook.page_num(&watch.page) == Some(page)
                    })
                    .ok_or("Memory inspector unavailable")?;

                Ok(Source::Memory {
                    id: watch.id,
                    address: watch.previous_begin.get().ok_or("Memory has not loaded")?,
                    bytes: watch.previous_bytes.borrow().len().min(4096),
                })
            }
            _ => Err("Select a capture source".into()),
        }
    }

    fn capture_comparison(&self, source: Source) -> Result<Baseline, String> {
        let origin = self.comparison_origin()?;
        let generation = origin.context.generation();
        let mut values = BTreeMap::new();
        let mut bytes = 0;

        match &source {
            Source::Local(path) | Source::Watch(path) => {
                if matches!(source, Source::Watch(_)) && !self.model.watches_are_current() {
                    return Err("Current watch values have not loaded".into());
                }

                let tree = if matches!(source, Source::Local(_)) {
                    &self.locals_tree
                } else {
                    &self.watches_tree
                };
                let node = find_path(tree, path)
                    .ok_or("Pinned variable is not loaded or is out of scope")?;

                if !self.variable_action_is_current(&node.variable) {
                    return Err("Variable data is stale or unavailable".into());
                }

                variable_values(&node, &mut values, "", &mut bytes, 0)?;
            }
            Source::Registers => {
                let registers = self
                    .model
                    .registers_for_details(generation)
                    .ok_or("Current register values have not loaded")?;

                for register in registers {
                    insert(&mut values, register.name, register.value, &mut bytes)?;
                }
            }
            Source::Memory {
                id,
                address,
                bytes: count,
            } => {
                let watches = self.memory_watches.borrow();
                let watch = watches
                    .iter()
                    .find(|watch| watch.id == *id)
                    .ok_or("Pinned memory inspector was closed")?;

                if watch.snapshot_generation.get() != Some(generation)
                    || watch.previous_begin.get() != Some(*address)
                {
                    return Err(
                        "Refresh the pinned memory range at this stop before comparing".into(),
                    );
                }

                let data = watch.previous_bytes.borrow();
                let data = data
                    .get(..*count)
                    .ok_or("Pinned memory range is no longer fully readable")?;

                for (offset, row) in data.chunks(16).enumerate() {
                    let address = address
                        .checked_add((offset * 16) as u64)
                        .ok_or("Memory address overflow")?;
                    insert(
                        &mut values,
                        format!("0x{address:x}"),
                        crate::hex::encode(row),
                        &mut bytes,
                    )?;
                }
            }
        }

        if values.is_empty() {
            return Err("No loaded values to capture".into());
        }

        Ok(Baseline {
            source,
            origin,
            values,
        })
    }

    pub(super) fn open_comparisons(self: &Rc<Self>) {
        if let Some(window) = self.comparisons.window.upgrade() {
            window.present();
            return;
        }

        let (window, view) = ComparisonView::new(&self.window, &self.column_layouts);
        self.layout.bind_window("comparisons", &window);
        self.comparisons.window.set(Some(&window));
        view.render(&self.comparisons, self.comparisons.selected.get());

        for (button, action) in [(&view.pin, 0), (&view.compare, 1), (&view.clear, 2)] {
            let weak = Rc::downgrade(self);
            let target = Rc::downgrade(&view);

            button.connect_clicked(move |_| {
                let (Some(ui), Some(view)) = (weak.upgrade(), target.upgrade()) else { return };
                let mut selected = view.pages.current_page().unwrap_or(0);
                view.status.set_visible(false);

                match action {
                    0 => {
                        let result = ui.comparison_source(view.source.selected())
                            .and_then(|source| ui.capture_comparison(source))
                            .and_then(|baseline| ui.comparisons.pin(baseline).map_err(str::to_owned));

                        match result {
                            Ok(index) => selected = index as u32,
                            Err(error) => {
                                view.status.set_text(&error);
                                view.status.set_visible(true);
                                return;
                            }
                        }
                    }
                    1 => {
                        for pin in ui.comparisons.pins.borrow_mut().iter_mut() {
                            pin.compared = Some(ui.capture_comparison(pin.baseline.source.clone()).and_then(|now| {
                                if pin.baseline.origin.compatible(&now.origin) {
                                    Ok(now)
                                } else {
                                    Err("Different target, thread, frame, function or symbol revision. Return to the baseline context before comparing.".into())
                                }
                            }));
                        }
                    }
                    _ => ui.comparisons.pins.borrow_mut().clear(),
                }

                view.render(&ui.comparisons, selected);
            });
        }

        let state = Rc::clone(&self.comparisons);

        window.connect_close_request(move |_| {
            state.selected.set(view.pages.current_page().unwrap_or(0));
            glib::Propagation::Proceed
        });

        window.present();
    }
}

struct ComparisonView {
    pages: gtk::Notebook,
    stack: gtk::Stack,
    source: gtk::DropDown,
    pin: gtk::Button,
    compare: gtk::Button,
    clear: gtk::Button,
    changed: gtk::CheckButton,
    filter: gtk::CustomFilter,
    count: gtk::Label,
    status: gtk::Label,
    columns: TableLayout,
}

impl ComparisonView {
    fn new(parent: &impl IsA<gtk::Window>, columns: &ColumnLayouts) -> (gtk::Window, Rc<Self>) {
        let (window, content, actions) = stop_info::inspection_window(parent, "Pinned comparisons");
        window.set_default_size(940, 500);
        let controls = components::control_row();

        let source = gtk::DropDown::from_strings(&[
            "Selected local / argument",
            "Selected watch",
            "Registers",
            "Active memory inspector",
        ]);

        source.set_hexpand(true);
        source.set_tooltip_text(Some("Choose the loaded data to capture as a baseline"));
        let pin = gtk::Button::with_label("Pin current");
        pin.set_tooltip_text(Some(
            "Capture loaded values. Pinning the same source and context replaces its baseline.",
        ));
        let compare = gtk::Button::with_label("Compare current");
        compare.set_tooltip_text(Some("Compare all pins with currently loaded data. Results stay fixed until you compare again."));
        let clear = gtk::Button::with_label("Clear pins");

        let help = empty_label(
            "Pin loaded data, step, then compare. Results are snapshots, not live values.\n\nUp to 8 pins, 256 rows / 64 KiB each and 4 KiB per memory range. Only loaded rows are captured. Comparisons require the same target, thread, frame, function and symbol revision, but do not prove the same call invocation.",
        );

        help.set_max_width_chars(54);
        components::inset(&help, components::CONTENT_INSET);
        let popover = gtk::Popover::builder().child(&help).build();

        let info = gtk::MenuButton::builder()
            .icon_name("dialog-information-symbolic")
            .tooltip_text("About pinned comparisons")
            .popover(&popover)
            .build();

        controls.append(&source);
        controls.append(&pin);
        controls.append(&compare);
        controls.append(&clear);
        controls.append(&info);
        content.append(&controls);
        let status = stop_info::inspection_text("");
        status.add_css_class("local-details-error");
        status.set_visible(false);
        content.append(&status);
        let pages = gtk::Notebook::new();
        pages.set_scrollable(true);
        pages.set_vexpand(true);
        let empty = gtk::Box::new(gtk::Orientation::Vertical, components::CONTENT_INSET);
        empty.set_valign(gtk::Align::Center);
        empty.set_halign(gtk::Align::Center);
        empty.append(&section_title("NO PINNED SNAPSHOTS"));

        empty.append(&empty_label(
            "Select a loaded value or memory inspector, then pin it before stepping.",
        ));

        let stack = gtk::Stack::new();
        stack.set_vexpand(true);
        stack.add_named(&empty, Some("empty"));
        stack.add_named(&pages, Some("pins"));
        content.append(&stack);
        let changed = gtk::CheckButton::with_label("Changed only");
        changed.set_active(true);
        let filter = difference_filter(&changed);
        let count = empty_label("0 / 8 pins");
        actions.prepend(&count);
        actions.prepend(&changed);

        let view = Rc::new(Self {
            pages,
            stack,
            source,
            pin,
            compare,
            clear,
            changed,
            filter,
            count,
            status,
            columns: columns.table(TableId::Comparisons),
        });

        (window, view)
    }

    fn render(&self, state: &Comparisons, selected: u32) {
        while self.pages.n_pages() > 0 {
            self.pages.remove_page(Some(0));
        }

        let pins = state.pins.borrow();

        for pin in pins.iter() {
            let page = gtk::Box::new(gtk::Orientation::Vertical, components::CONTROL_GAP);
            components::inset(&page, components::CONTENT_INSET);
            let context = components::control_row();
            context.set_homogeneous(true);
            context.append(&origin_label("Baseline", Some(&pin.baseline.origin)));

            let current = pin
                .compared
                .as_ref()
                .and_then(|result| result.as_ref().ok());

            context.append(&origin_label("Compared", current.map(|now| &now.origin)));
            page.append(&context);

            if let Some(Err(error)) = &pin.compared {
                let error = stop_info::inspection_text(error);
                error.add_css_class("local-details-error");
                page.append(&error);
            }

            page.append(&difference_table(
                difference(&pin.baseline.values, current.map(|now| &now.values)),
                &self.filter,
                &self.columns,
            ));

            let name = pin.baseline.source.description();
            let tab = gtk::Label::new(Some(&name));
            tab.set_ellipsize(pango::EllipsizeMode::End);
            tab.set_width_chars(name.chars().count().min(24) as i32);
            tab.set_max_width_chars(24);

            tab.set_tooltip_text(Some(&format!(
                "{name}\n{}",
                pin.baseline.origin.description()
            )));

            self.pages.append_page(&page, Some(&tab));
        }

        self.pages
            .set_current_page(Some(selected.min(pins.len().saturating_sub(1) as u32)));

        self.stack
            .set_visible_child_name(if pins.is_empty() { "empty" } else { "pins" });

        self.compare.set_sensitive(!pins.is_empty());
        self.clear.set_sensitive(!pins.is_empty());

        self.changed
            .set_sensitive(pins.iter().any(|pin| matches!(pin.compared, Some(Ok(_)))));

        self.count.set_text(&format!(
            "{} / {MAX_PINS} pins · results update on request",
            pins.len()
        ));
    }
}

fn origin_label(caption: &str, origin: Option<&Origin>) -> gtk::Label {
    let text = origin.map_or_else(|| "Not captured".into(), Origin::description);
    let label = stop_info::inspection_text(&format!("{caption} · {text}"));
    label.add_css_class("muted");
    label.set_hexpand(true);
    label.set_width_chars(1);
    label.set_lines(2);
    label.set_ellipsize(pango::EllipsizeMode::End);
    label.set_tooltip_text(Some(&text));
    label
}

fn difference_filter(changed: &gtk::CheckButton) -> gtk::CustomFilter {
    let button = changed.downgrade();

    let filter = gtk::CustomFilter::new(move |object| {
        !button.upgrade().is_some_and(|button| button.is_active())
            || object
                .downcast_ref::<glib::BoxedAnyObject>()
                .is_some_and(|object| {
                    let row = object.borrow::<DifferenceRow>();
                    !row.compared || row.changed()
                })
    });

    let updated = filter.clone();
    changed.connect_toggled(move |_| updated.changed(gtk::FilterChange::Different));
    filter
}

fn difference_table(
    rows: Vec<DifferenceRow>,
    filter: &gtk::CustomFilter,
    columns: &TableLayout,
) -> gtk::Box {
    let root = gtk::Box::new(gtk::Orientation::Vertical, components::CONTROL_GAP);
    root.set_vexpand(true);
    let total = rows.len();
    let changed = rows.iter().filter(|row| row.changed()).count();
    let compared = rows.iter().any(|row| row.compared);

    let summary = if compared {
        format!("{changed} changed · {total} captured entries")
    } else {
        format!("{total} values pinned · compare after stepping")
    };

    root.append(&empty_label(&summary));
    let mut store = gio::ListStore::new::<glib::BoxedAnyObject>();
    store.extend(rows.into_iter().map(glib::BoxedAnyObject::new));
    let model = gtk::FilterListModel::new(Some(store), Some(filter.clone()));
    let view = components::column_view(gtk::NoSelection::new(Some(model.clone())));
    view.add_css_class("debug-table");

    for (index, (key, title, width)) in [
        ("name", "NAME / ADDRESS", 280),
        ("baseline", "BASELINE", 220),
        ("compared", "COMPARED", 220),
        ("status", "STATUS", 120),
    ]
    .into_iter()
    .enumerate()
    {
        let column = components::label_column(title, width, move |object, label| {
            let row = object.borrow::<DifferenceRow>();
            let value = row.cells()[index];
            clear_label_selection(label);
            label.set_text(value);
            label.set_tooltip_text(Some(value));
            label.set_halign(gtk::Align::Fill);
            label.set_xalign(0.0);
            label.set_wrap(true);
            label.set_wrap_mode(pango::WrapMode::WordChar);
            label.set_lines(3);
            label.set_ellipsize(pango::EllipsizeMode::End);

            label.set_css_classes(&[
                "debug-table-cell",
                if index == 3
                    || (index == 1 && row.before.is_none())
                    || (index == 2 && row.after.is_none())
                {
                    "muted"
                } else if row.changed() && index == 2 {
                    "local-changed-value"
                } else {
                    "local-value"
                },
            ]);
        });

        columns.append(&view, key, &column);
    }

    let scroll = gtk::ScrolledWindow::builder()
        .child(&view)
        .vexpand(true)
        .build();

    let empty = empty_label("No differences in captured data");
    empty.set_valign(gtk::Align::Center);
    empty.set_halign(gtk::Align::Center);
    let stack = gtk::Stack::new();
    stack.set_vexpand(true);
    stack.add_named(&scroll, Some("table"));
    stack.add_named(&empty, Some("empty"));

    stack.set_visible_child_name(if model.n_items() > 0 {
        "table"
    } else {
        "empty"
    });

    root.append(&stack);
    let target = stack.downgrade();

    model.connect_items_changed(move |model, _, _, _| {
        if let Some(stack) = target.upgrade() {
            stack.set_visible_child_name(if model.n_items() > 0 {
                "table"
            } else {
                "empty"
            });
        }
    });

    root
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires a GTK display, run separately from other GTK tests"]
    fn comparison_tabs_keep_results_and_geometry_stable() {
        gtk::init().unwrap();
        Theme::graphite().install();
        let parent = gtk::Window::new();
        let (window, view) = ComparisonView::new(&parent, &ColumnLayouts::default());
        let state = Comparisons::default();
        view.render(&state, 0);
        assert!(!view.compare.is_sensitive());
        assert_eq!(view.stack.visible_child_name().as_deref(), Some("empty"));

        let origin = Origin {
            context: crate::debugger::StopContext::new(1, 1, Some("i1".into()), "1".into(), 0)
                .unwrap(),
            sequence: 1,
            symbols: 1,
            function: Some("main".into()),
        };

        let before = BTreeMap::from([("pair.x".into(), "1".into()), ("pair.y".into(), "2".into())]);

        let baseline = Baseline {
            source: Source::Registers,
            origin,
            values: before.clone(),
        };

        assert_eq!(state.pin(baseline.clone()), Ok(0));
        view.render(&state, 0);
        assert!(!view.changed.is_sensitive());
        window.present();
        let main = glib::MainContext::default();
        main.block_on(glib::timeout_future(Duration::from_millis(100)));
        let height = window.height();
        let page = view.pages.nth_page(Some(0)).unwrap();
        assert!(view.pages.tab_label(&page).unwrap().width() > 60);
        let mut current = baseline.clone();
        current.origin.sequence = 2;
        current.values.insert("pair.x".into(), "42".into());
        state.pins.borrow_mut()[0].compared = Some(Ok(current));
        view.render(&state, 0);
        main.block_on(glib::timeout_future(Duration::from_millis(100)));

        let table = super::super::tests::descendants::<gtk::ColumnView>(&view.pages)
            .pop()
            .unwrap();

        assert_eq!(table.columns().n_items(), 4);
        assert_eq!(table.model().unwrap().n_items(), 1);
        view.changed.set_active(false);
        assert_eq!(table.model().unwrap().n_items(), 2);
        view.changed.set_active(true);
        assert_eq!(table.model().unwrap().n_items(), 1);
        main.block_on(glib::timeout_future(Duration::from_millis(100)));
        assert_eq!(window.height(), height);

        if let Some(path) = std::env::var_os("FGDB_COMPARISON_CAPTURE") {
            let widget = window.child().unwrap();
            let snapshot = gtk::Snapshot::new();

            gtk::WidgetPaintable::new(Some(&widget)).snapshot(
                &snapshot,
                f64::from(widget.width()),
                f64::from(widget.height()),
            );

            window
                .renderer()
                .unwrap()
                .render_texture(snapshot.to_node().unwrap(), None)
                .save_to_png(path)
                .unwrap();
        }

        let mut large = baseline.clone();

        large.source = Source::Memory {
            id: 1,
            address: 0x1000,
            bytes: 4096,
        };

        large.values = (0..MAX_ROWS)
            .map(|index| (format!("field_{index}"), "long_value".repeat(40)))
            .collect();

        assert_eq!(state.pin(large), Ok(1));
        assert!(state.pins.borrow()[0].compared.is_some());
        view.render(&state, 1);
        main.block_on(glib::timeout_future(Duration::from_millis(100)));
        assert_eq!(view.pages.n_pages(), 2);
        assert_eq!(view.pages.current_page(), Some(1));
        let page = view.pages.nth_page(Some(1)).unwrap();
        let scrolls = super::super::tests::descendants::<gtk::ScrolledWindow>(&page);
        assert_eq!(scrolls.len(), 1);
        assert!(scrolls[0].vadjustment().upper() > scrolls[0].vadjustment().page_size());
        assert_eq!(window.height(), height);
        assert!(window.width() <= 960);
        state.pin(baseline.clone()).unwrap();
        state.pins.borrow_mut()[0].compared = Some(Ok(baseline));
        view.render(&state, 0);
        let page = view.pages.nth_page(Some(0)).unwrap();
        let stacks = super::super::tests::descendants::<gtk::Stack>(&page);
        assert_eq!(stacks[0].visible_child_name().as_deref(), Some("empty"));
        view.changed.set_active(false);
        assert_eq!(stacks[0].visible_child_name().as_deref(), Some("table"));
        state.pins.borrow_mut().clear();
        view.render(&state, 0);
        assert!(!view.compare.is_sensitive());
        assert!(!view.clear.is_sensitive());
        assert_eq!(view.stack.visible_child_name().as_deref(), Some("empty"));
        window.close();
        parent.close();
    }

    #[test]
    fn origins_allow_stepping_but_reject_context_and_symbol_changes() {
        let context = |epoch, generation, inferior: &str, thread: &str, frame| {
            crate::debugger::StopContext::new(
                epoch,
                generation,
                Some(inferior.into()),
                thread.into(),
                frame,
            )
            .unwrap()
        };
        let baseline = Origin {
            context: context(1, 1, "i1", "1", 0),
            sequence: 1,
            symbols: 1,
            function: Some("main".into()),
        };
        let mut now = baseline.clone();
        now.context = context(1, 2, "i1", "1", 0);
        now.sequence = 2;
        assert!(baseline.compatible(&now));

        for changed in [
            context(2, 2, "i1", "1", 0),
            context(1, 2, "i2", "1", 0),
            context(1, 2, "i1", "2", 0),
            context(1, 2, "i1", "1", 1),
        ] {
            now.context = changed;
            assert!(!baseline.compatible(&now));
        }

        now = baseline.clone();
        now.symbols += 1;
        assert!(!baseline.compatible(&now));
        now = baseline.clone();
        now.function = Some("other".into());
        assert!(!baseline.compatible(&now));
    }

    #[test]
    fn variable_capture_only_includes_loaded_children() {
        let record = crate::debugger::parse_record(
            r#"^done,name="root",numchild="2",value="{...}",type="Pair""#,
        )
        .unwrap();
        let node = VariableNode::new(crate::debugger::variable_object(&record, "pair").unwrap());
        let mut values = BTreeMap::new();
        variable_values(&node, &mut values, "", &mut 0, 0).unwrap();
        assert_eq!(values.len(), 1);
        let child_record = crate::debugger::parse_record(
            r#"^done,name="root.x",numchild="0",value="7",type="int""#,
        )
        .unwrap();
        let child = node.child(crate::debugger::variable_object(&child_record, "x").unwrap());
        node.children.append(&SnapshotRow::new(child));
        node.children_loaded.set(true);
        values.clear();
        variable_values(&node, &mut values, "", &mut 0, 0).unwrap();
        assert_eq!(
            values.get("pair [Pair] / x [int]").map(String::as_str),
            Some("7")
        );
    }

    #[test]
    fn comparisons_preserve_absence_and_do_not_compare_truncated_values() {
        let before = BTreeMap::from([("x".into(), "1".into()), ("gone".into(), "2".into())]);
        let after = BTreeMap::from([("x".into(), "3".into())]);
        let rows = difference(&before, Some(&after));
        assert_eq!(rows[0].cells(), ["gone", "2", "—", "Not loaded"]);
        assert_eq!(rows[1].cells(), ["x", "1", "3", "Changed"]);
        assert!(rows.iter().all(DifferenceRow::changed));
        assert!(
            difference(&before, Some(&before))
                .iter()
                .all(|row| !row.changed())
        );
        assert!(
            difference(&before, None)
                .iter()
                .all(|row| !row.compared && row.cells()[3] == "Pinned")
        );
        let mut values = BTreeMap::new();
        assert!(insert(&mut values, "x".into(), "x".repeat(MAX_BYTES), &mut 0).is_err());
        assert!(values.is_empty());
    }
}
