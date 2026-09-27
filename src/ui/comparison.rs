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
pub(super) struct Baseline {
    source: Source,
    origin: Origin,
    values: BTreeMap<String, String>,
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
    cells: [String; 3],
    changed: bool,
}

fn difference(
    before: &BTreeMap<String, String>,
    after: &BTreeMap<String, String>,
) -> Vec<DifferenceRow> {
    let names: std::collections::BTreeSet<_> = before.keys().chain(after.keys()).collect();

    names
        .into_iter()
        .map(|name| {
            let old = before.get(name);
            let new = after.get(name);

            DifferenceRow {
                cells: [
                    name.clone(),
                    old.map_or("<not captured>", String::as_str).into(),
                    new.map_or("<not loaded or out of scope>", String::as_str)
                        .into(),
                ],
                changed: old != new,
            }
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
        let (window, content, actions) =
            stop_info::inspection_window(&self.window, "Pinned comparisons");

        let help = empty_label(
            "Captures loaded rows only, not a full memory snapshot. Up to 8 pins, 256 rows / 64 KiB each and 4 KiB per memory range. Compare in the same target, thread, frame and function. Matching context does not prove the same call invocation.",
        );

        let (help, revealed) =
            build_disclosure_with_content("CAPTURE LIMITS", &help, false, "comparison-help");

        revealed.connect_visible_notify(stop_info::fit_inspection_window);
        content.append(&help);
        let controls = components::control_row();

        let source = gtk::DropDown::from_strings(&[
            "Selected local / argument",
            "Selected watch",
            "Registers",
            "Active memory inspector",
        ]);

        source.set_hexpand(true);
        let pin = gtk::Button::with_label("Pin current");
        let compare = gtk::Button::with_label("Compare current");
        let clear = gtk::Button::with_label("Clear pins");
        controls.append(&source);
        controls.append(&pin);
        content.append(&controls);
        let changed = gtk::CheckButton::with_label("Changed only");
        changed.set_active(true);
        let filter = difference_filter(&changed);
        actions.prepend(&changed);
        actions.prepend(&clear);
        actions.prepend(&compare);

        let status = stop_info::inspection_text(
            "Choose a loaded value or memory inspector, then pin it before stepping.",
        );

        content.append(&status);
        let results = gtk::Box::new(gtk::Orientation::Vertical, components::CONTENT_INSET);
        content.append(&stop_info::inspection_scroll(&results));
        self.render_comparisons(&results, false, &filter);

        for (button, action) in [(pin, 0), (compare, 1), (clear, 2)] {
            let weak = Rc::downgrade(self);
            let source = source.clone();
            let status = status.clone();
            let results = results.clone();
            let filter = filter.clone();

            button.connect_clicked(move |_| {
                let Some(ui) = weak.upgrade() else { return };

                if action == 2 {
                    ui.comparisons.borrow_mut().clear();
                } else if action == 0 {
                    let result = ui
                        .comparison_source(source.selected())
                        .and_then(|source| ui.capture_comparison(source));

                    match result {
                        Ok(baseline) => {
                            let mut pins = ui.comparisons.borrow_mut();

                            if let Some(previous) = pins.iter_mut().find(|pin| {
                                pin.source == baseline.source
                                    && pin.origin.compatible(&baseline.origin)
                            }) {
                                *previous = baseline;
                            } else if pins.len() < MAX_PINS {
                                pins.push(baseline);
                            } else {
                                status.set_text(
                                    "Eight pins retained. Clear pins before capturing another.",
                                );

                                status.add_css_class("local-details-error");
                                stop_info::fit_inspection_window(&results);
                                return;
                            }
                        }
                        Err(error) => {
                            status.set_text(&error);
                            status.add_css_class("local-details-error");
                            stop_info::fit_inspection_window(&results);
                            return;
                        }
                    }
                }

                let count = ui.comparisons.borrow().len();
                status.remove_css_class("local-details-error");

                status.set_text(&format!(
                    "{count} / {MAX_PINS} pins · updated only on request"
                ));

                ui.render_comparisons(&results, action == 1, &filter);
            });
        }

        window.present();
    }

    fn render_comparisons(&self, results: &gtk::Box, compare: bool, filter: &gtk::CustomFilter) {
        clear_box(results);

        for pin in self.comparisons.borrow().iter() {
            let card = components::card();
            let title = stop_info::inspection_text(&pin.source.description());
            title.add_css_class("field-label");
            title.set_lines(2);
            title.set_ellipsize(pango::EllipsizeMode::End);
            title.set_tooltip_text(Some(&pin.source.description()));
            card.append(&title);

            let origin = empty_label(&format!("Baseline · {}", pin.origin.description()));

            origin.set_wrap_mode(pango::WrapMode::WordChar);
            card.append(&origin);

            if compare {
                match self.capture_comparison(pin.source.clone()) {
                    Ok(now) if pin.origin.compatible(&now.origin) => {
                        let origin =
                            empty_label(&format!("Current · {}", now.origin.description()));

                        origin.set_wrap_mode(pango::WrapMode::WordChar);
                        card.append(&origin);

                        card.append(&difference_table(
                            difference(&pin.values, &now.values),
                            filter,
                        ));
                    }
                    result => {
                        let error = result.err().unwrap_or_else(|| "Different target, thread, frame, function or symbol revision. Return to the captured context before comparing.".into());
                        let label = stop_info::inspection_text(&error);
                        label.add_css_class("local-details-error");
                        card.append(&label);
                    }
                }
            } else {
                card.append(&empty_label(&format!("{} values pinned", pin.values.len())));
            }

            results.append(&card);
        }

        stop_info::fit_inspection_window(results);
    }
}

fn difference_filter(changed: &gtk::CheckButton) -> gtk::CustomFilter {
    let button = changed.downgrade();

    let filter = gtk::CustomFilter::new(move |object| {
        !button.upgrade().is_some_and(|button| button.is_active())
            || object
                .downcast_ref::<glib::BoxedAnyObject>()
                .is_some_and(|object| object.borrow::<DifferenceRow>().changed)
    });

    let updated = filter.clone();
    changed.connect_toggled(move |_| updated.changed(gtk::FilterChange::Different));
    filter
}

fn difference_table(rows: Vec<DifferenceRow>, filter: &gtk::CustomFilter) -> gtk::Box {
    let root = gtk::Box::new(gtk::Orientation::Vertical, components::CONTROL_GAP);
    let total = rows.len();
    let changed = rows.iter().filter(|row| row.changed).count();

    root.append(&empty_label(&format!(
        "{changed} changed · {total} captured entries"
    )));

    let mut store = gio::ListStore::new::<glib::BoxedAnyObject>();
    store.extend(rows.into_iter().map(glib::BoxedAnyObject::new));
    let model = gtk::FilterListModel::new(Some(store), Some(filter.clone()));
    let view = components::column_view(gtk::NoSelection::new(Some(model.clone())));
    view.add_css_class("debug-table");

    for (index, title) in ["NAME / ADDRESS", "BASELINE", "CURRENT"]
        .into_iter()
        .enumerate()
    {
        let column = components::label_column(title, 200, move |object, label| {
            let row = object.borrow::<DifferenceRow>();
            let value = &row.cells[index];
            clear_label_selection(label);
            label.set_text(value);
            label.set_tooltip_text(Some(value));
            label.set_halign(gtk::Align::Fill);
            label.set_xalign(0.0);

            label.set_css_classes(&[
                "debug-table-cell",
                if row.changed && index == 2 {
                    "local-changed-value"
                } else {
                    "local-value"
                },
            ]);
        });

        view.append_column(&column);
    }

    let scroll = gtk::ScrolledWindow::builder()
        .child(&view)
        .propagate_natural_height(true)
        .max_content_height(240)
        .build();

    let empty = empty_label("No differences in captured data");
    scroll.set_visible(model.n_items() > 0);
    empty.set_visible(model.n_items() == 0);
    root.append(&scroll);
    root.append(&empty);
    let scroll = scroll.downgrade();
    let empty = empty.downgrade();
    let target = root.downgrade();

    model.connect_items_changed(move |model, _, _, _| {
        if let (Some(scroll), Some(empty), Some(target)) =
            (scroll.upgrade(), empty.upgrade(), target.upgrade())
        {
            scroll.set_visible(model.n_items() > 0);
            empty.set_visible(model.n_items() == 0);
            stop_info::fit_inspection_window(&target);
        }
    });

    root
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires a GTK display, run separately from other GTK tests"]
    fn comparison_filter_updates_captured_rows_and_compact_layout() {
        gtk::init().unwrap();
        Theme::graphite().install();
        let parent = gtk::Window::new();
        let (window, content, actions) =
            stop_info::inspection_window(&parent, "Pinned comparisons");

        let changed = gtk::CheckButton::with_label("Changed only");
        changed.set_active(true);
        actions.prepend(&changed);
        let filter = difference_filter(&changed);

        let before = BTreeMap::from([
            ("pair.x [int]".into(), "1".into()),
            ("pair.y [int]".into(), "2".into()),
        ]);

        let after = BTreeMap::from([
            ("pair.x [int]".into(), "42".into()),
            ("pair.y [int]".into(), "2".into()),
        ]);

        let results = components::card();
        content.append(&stop_info::inspection_scroll(&results));
        let table = difference_table(difference(&before, &after), &filter);
        results.append(&table);

        let view = super::super::tests::descendants::<gtk::ColumnView>(&table)
            .pop()
            .unwrap();

        let model = view.model().unwrap();
        window.present();
        let main = glib::MainContext::default();
        main.block_on(glib::timeout_future(Duration::from_millis(100)));
        assert_eq!(model.n_items(), 1);
        assert_eq!(view.columns().n_items(), 3);
        assert!(window.height() < 320, "compact height {}", window.height());
        changed.set_active(false);
        assert_eq!(model.n_items(), 2);
        changed.set_active(true);
        assert_eq!(model.n_items(), 1);
        main.block_on(glib::timeout_future(Duration::from_millis(100)));

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

        clear_box(&results);
        let unchanged = difference_table(difference(&before, &before), &filter);
        results.append(&unchanged);
        let empty = unchanged.last_child().unwrap();

        let scroll = super::super::tests::descendants::<gtk::ScrolledWindow>(&unchanged)
            .pop()
            .unwrap();

        assert!(empty.is_visible());
        assert!(!scroll.is_visible());
        changed.set_active(false);
        assert!(!empty.is_visible());
        assert!(scroll.is_visible());
        changed.set_active(true);
        assert!(empty.is_visible());
        assert!(!scroll.is_visible());

        let large = (0..MAX_ROWS)
            .map(|index| (format!("field_{index}"), "v".repeat(256)))
            .collect();

        clear_box(&results);
        let table = difference_table(difference(&large, &BTreeMap::new()), &filter);
        results.append(&table);
        stop_info::fit_inspection_window(&results);
        main.block_on(glib::timeout_future(Duration::from_millis(100)));

        let scroll = super::super::tests::descendants::<gtk::ScrolledWindow>(&table)
            .pop()
            .unwrap();

        assert!(scroll.vadjustment().upper() > scroll.vadjustment().page_size());
        assert!(window.height() <= 520, "bounded height {}", window.height());
        assert!(window.width() <= 740, "bounded width {}", window.width());
        assert!(actions.is_mapped());
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
        let rows = difference(&before, &after);
        assert_eq!(rows[0].cells, ["gone", "2", "<not loaded or out of scope>"]);
        assert_eq!(rows[1].cells, ["x", "1", "3"]);
        assert!(rows.iter().all(|row| row.changed));
        assert!(difference(&before, &before).iter().all(|row| !row.changed));
        let mut values = BTreeMap::new();
        assert!(insert(&mut values, "x".into(), "x".repeat(MAX_BYTES), &mut 0).is_err());
        assert!(values.is_empty());
    }
}
