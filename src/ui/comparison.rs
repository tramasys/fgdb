//! Explicit, bounded snapshots. Variable capture is on demand, never stop polling.

use super::*;
use crate::debugger::comparison::{
    CapturedValue, MAX_BYTES, MAX_ROWS, SnapshotReply, ValueSnapshot,
};
use crate::debugger::vector::VectorValue;
use std::collections::BTreeMap;

const MAX_PINS: usize = 8;
type CaptureHandler = Rc<dyn Fn(Variable, Rc<dyn Fn() -> bool>, SnapshotReply)>;
type CaptureReply = Box<dyn FnOnce(Result<Baseline, String>)>;

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
    values: BTreeMap<String, CapturedValue>,
    complete: bool,
}

struct Pin {
    baseline: Baseline,
    compared: Option<Result<Baseline, String>>,
    vector_display: Rc<Cell<VectorDisplay>>,
}

#[derive(Default)]
pub(super) struct Comparisons {
    pins: RefCell<Vec<Pin>>,
    window: glib::WeakRef<gtk::Window>,
    selected: Cell<u32>,
    request: Cell<u64>,
    capture: RefCell<Option<CaptureHandler>>,
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
            vector_display: Rc::default(),
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
    values: &mut BTreeMap<String, CapturedValue>,
    name: String,
    value: CapturedValue,
    bytes: &mut usize,
) -> Result<(), String> {
    *bytes = bytes
        .saturating_add(name.len())
        .saturating_add(value.text.len());

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

fn register_snapshot(
    registers: Vec<Register>,
    architecture: TargetArchitecture,
    endian: Option<TargetEndian>,
    pointer_width: PointerWidth,
) -> Result<ValueSnapshot, String> {
    let mut values = BTreeMap::new();
    let mut bytes = 0;

    for register in registers {
        let value = if let Some(vector) = VectorValue::parse(&register.name, &register.value) {
            // Retain exact bits, not GDB's redundant union interpretations.
            vector.hex().into()
        } else {
            let complete = !register.value.trim().is_empty()
                && crate::debugger::vector::register_bytes(&register.name).is_none()
                && !register.value.contains(['{', '…'])
                && !["...", "<unavailable", "<not available", "<optimized out"]
                    .iter()
                    .any(|marker| register.value.contains(marker));

            let text = if complete {
                format_register_value_for_target(
                    &register.name,
                    &register.value,
                    false,
                    architecture,
                    endian,
                    pointer_width,
                )
            } else {
                register.value
            };

            CapturedValue { text, complete }
        };

        insert(&mut values, register.name, value, &mut bytes)?;
    }

    Ok(ValueSnapshot {
        complete: values.values().all(|value| value.complete),
        values,
    })
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
    before: Option<CapturedValue>,
    after: Option<CapturedValue>,
    compared: bool,
    bits: Option<[Option<u64>; 2]>,
}

impl DifferenceRow {
    fn changed(&self) -> bool {
        self.compared
            && !self.uncertain()
            && self.bits.map_or_else(
                || self.before != self.after,
                |[before, after]| before != after,
            )
    }

    fn uncertain(&self) -> bool {
        self.before
            .iter()
            .chain(self.after.iter())
            .any(|value| !value.complete)
    }

    fn cells(&self) -> [&str; 4] {
        let status = match (&self.before, &self.after) {
            _ if self.uncertain() => "Incomplete",
            _ if !self.compared => "Pinned",
            (None, _) => "Newly captured",
            (_, None) => "Not captured",
            _ if self.changed() => "Changed",
            _ => "Unchanged",
        };

        [
            &self.name,
            self.before
                .as_ref()
                .map_or("—", |value| value.text.as_str()),
            self.after.as_ref().map_or("—", |value| value.text.as_str()),
            status,
        ]
    }
}

fn difference(
    before: &BTreeMap<String, CapturedValue>,
    after: Option<&BTreeMap<String, CapturedValue>>,
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
            bits: None,
        })
        .collect()
}

fn register_difference(
    before: &BTreeMap<String, CapturedValue>,
    after: Option<&BTreeMap<String, CapturedValue>>,
    display: VectorDisplay,
) -> Vec<DifferenceRow> {
    let mut result = Vec::new();
    let width = display.format.lane_bytes();
    let mut registers = difference(before, after);

    registers.sort_by(|left, right| {
        let key = |name: &str| {
            let prefix = name.trim_end_matches(|ch: char| ch.is_ascii_digit());
            (
                prefix.len(),
                name[prefix.len()..].parse::<u32>().unwrap_or(0),
            )
        };

        let (left_prefix, left_index) = key(&left.name);
        let (right_prefix, right_index) = key(&right.name);
        left.name[..left_prefix]
            .cmp(&right.name[..right_prefix])
            .then(left_index.cmp(&right_index))
    });

    for mut row in registers {
        let values = [row.before.as_ref(), row.after.as_ref()];

        let vectors = values.map(|value| {
            value
                .filter(|value| value.complete)
                .and_then(|value| VectorValue::parse(&row.name, &value.text))
        });

        let count = vectors
            .iter()
            .flatten()
            .map(|value| value.bytes() / width)
            .max();

        let Some(count) = count else {
            row.name.insert(0, '$');
            result.push(row);
            continue;
        };

        for index in 0..count {
            let bits = vectors
                .each_ref()
                .map(|value| value.as_ref().and_then(|value| value.lane(index, width)));

            let cells = std::array::from_fn::<_, 2, _>(|side| {
                bits[side]
                    .map(|raw| display.text(raw).into())
                    .or_else(|| values[side].cloned())
            });

            let [before, after] = cells;

            result.push(DifferenceRow {
                name: format!("${} [{index}]", row.name),
                before,
                after,
                compared: row.compared,
                bits: Some(bits),
            });
        }
    }

    result
}

impl Ui {
    pub(crate) fn connect_comparisons(
        &self,
        capture: impl Fn(Variable, Rc<dyn Fn() -> bool>, SnapshotReply) + 'static,
    ) {
        self.comparisons.capture.replace(Some(Rc::new(capture)));
    }

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

    fn capture_loaded_comparison(&self, source: Source) -> Result<Baseline, String> {
        let origin = self.comparison_origin()?;
        let generation = origin.context.generation();
        let mut values = BTreeMap::new();
        let mut bytes = 0;
        let mut complete = true;

        match &source {
            Source::Local(_) | Source::Watch(_) => {
                return Err("Variable snapshots require on-demand capture".into());
            }
            Source::Registers => {
                let registers = self
                    .model
                    .registers_for_details(generation)
                    .ok_or("Current register values have not loaded")?;

                let snapshot = register_snapshot(
                    registers,
                    self.model.target_architecture(),
                    self.model.target_endian(),
                    self.model.target_pointer_width(),
                )?;

                values = snapshot.values;
                complete = snapshot.complete;
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
                        crate::hex::encode(row).into(),
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
            complete,
        })
    }

    fn capture_comparison(
        self: &Rc<Self>,
        source: Source,
        current: Rc<dyn Fn() -> bool>,
        reply: CaptureReply,
    ) {
        let (path, tree) = match &source {
            Source::Local(path) => (path, &self.locals_tree),
            Source::Watch(path) => (path, &self.watches_tree),
            _ => {
                reply(self.capture_loaded_comparison(source));
                return;
            }
        };

        let prepared = self.comparison_origin().and_then(|origin| {
            let node =
                find_path(tree, path).ok_or("Pinned variable is not loaded or is out of scope")?;

            if !self.variable_action_is_current(&node.variable) {
                return Err("Variable data is stale or unavailable".into());
            }

            Ok((origin, node.variable))
        });

        let (origin, variable) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                reply(Err(error));
                return;
            }
        };

        let Some(capture) = self.comparisons.capture.borrow().clone() else {
            reply(Err("Variable snapshot capture is unavailable".into()));
            return;
        };

        let weak = Rc::downgrade(self);
        let context = origin.context.clone();
        let symbols = origin.symbols;
        let target = variable.clone();

        let guard: Rc<dyn Fn() -> bool> = Rc::new(move || {
            current()
                && weak.upgrade().is_some_and(|ui| {
                    ui.model.is_stop_context_current(&context)
                        && ui.model.symbols.revision() == symbols
                        && ui.variable_action_is_current(&target)
                })
        });

        let response = Rc::clone(&guard);

        capture(
            variable,
            guard,
            Box::new(move |result| {
                reply(if response() {
                    result.map(|snapshot| Baseline {
                        source,
                        origin,
                        values: snapshot.values,
                        complete: snapshot.complete,
                    })
                } else {
                    Err("Snapshot cancelled because the stop or selection changed".into())
                });
            }),
        );
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

                if view.pending.get() != 0 {
                    return;
                }

                view.status.set_visible(false);

                let sources = match action {
                    0 => match ui.comparison_source(view.source.selected()) {
                        Ok(source) => vec![(None, source)],
                        Err(error) => {
                            view.status.set_text(&error);
                            view.status.set_visible(true);
                            return;
                        }
                    },
                    1 => ui.comparisons.pins.borrow().iter().enumerate()
                        .map(|(index, pin)| (Some(index), pin.baseline.source.clone())).collect(),
                    _ => {
                        ui.comparisons.pins.borrow_mut().clear();
                        view.render(&ui.comparisons, 0);
                        return;
                    }
                };

                let request = ui.comparisons.request.get().wrapping_add(1);
                ui.comparisons.request.set(request);
                view.pending.set(sources.len());
                view.update_actions(&ui.comparisons);
                view.status.set_text("Capturing fields and elements…");
                view.status.set_visible(true);

                for (index, source) in sources {
                    let weak = Rc::downgrade(&ui);
                    let target = Rc::downgrade(&view);

                    let current: Rc<dyn Fn() -> bool> = Rc::new(move || {
                        target.upgrade().is_some() && weak.upgrade().is_some_and(|ui| ui.comparisons.request.get() == request)
                    });

                    let weak = Rc::downgrade(&ui);
                    let target = Rc::downgrade(&view);

                    let reply: CaptureReply = Box::new(move |result| {
                        let (Some(ui), Some(view)) = (weak.upgrade(), target.upgrade()) else { return };

                        if ui.comparisons.request.get() != request {
                            return;
                        }

                        let mut selected = view.pages.current_page().unwrap_or(0);
                        view.pending.set(view.pending.get().saturating_sub(1));

                        if view.pending.get() == 0 {
                            view.status.set_visible(false);
                        }

                        if let Some(index) = index {
                            ui.comparisons.pins.borrow_mut()[index].compared = Some(result);
                        } else {
                            match result.and_then(|baseline| ui.comparisons.pin(baseline).map_err(str::to_owned)) {
                                Ok(index) => selected = index as u32,
                                Err(error) => {
                                    view.status.set_text(&error);
                                    view.status.set_visible(true);
                                }
                            }
                        }

                        if view.pending.get() == 0 {
                            view.render(&ui.comparisons, selected);
                        }
                    });

                    let compatible = index.is_none_or(|index| {
                        ui.comparison_origin().is_ok_and(|origin| ui.comparisons.pins.borrow()[index].baseline.origin.compatible(&origin))
                    });

                    if compatible {
                        ui.capture_comparison(source, current, reply);
                    } else {
                        reply(Err("Different target, thread, frame, function or symbol revision. Return to the baseline context before comparing.".into()));
                    }
                }
            });
        }

        let state = Rc::clone(&self.comparisons);

        window.connect_close_request(move |_| {
            state.request.set(state.request.get().wrapping_add(1));
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
    pending: Cell<usize>,
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
        source.set_tooltip_text(Some(
            "Choose a variable, registers, or memory range to capture",
        ));
        let pin = gtk::Button::with_label("Pin current");
        pin.set_tooltip_text(Some(
            "Capture fields and collection elements. Pinning the same source and context replaces its baseline.",
        ));
        let compare = gtk::Button::with_label("Compare current");
        compare.set_tooltip_text(Some("Capture all pinned variables again and compare. Results stay fixed until you compare again."));
        let clear = gtk::Button::with_label("Clear pins");

        let help = empty_label(
            "Pin a value, step, then compare. Fields and collection elements are captured on demand, even when collapsed. Pointers retain their addresses without following object graphs. Raw union members do not identify the active member.\n\nUp to 8 pins, 256 captured values / 64 KiB each and 8 nested levels. Partial captures are labelled. Registers and memory use loaded data, with up to 4 KiB per memory range. Supported SIMD values are shown by lane and compared bit for bit, independently of their display format. Comparisons require the same target, thread, frame, function and symbol revision, but do not prove the same call invocation.",
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
            pending: Cell::new(0),
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

            if !pin.baseline.complete || current.is_some_and(|now| !now.complete) {
                page.append(&empty_label("Partial snapshot · unreadable values, abbreviated printers or capture limits. Matching captured fields do not establish equality of the entire object."));
            }

            if pin.baseline.source == Source::Registers {
                page.append(&register_difference_table(
                    pin,
                    current,
                    &self.filter,
                    &self.columns,
                ));
            } else {
                page.append(&difference_table(
                    difference(&pin.baseline.values, current.map(|now| &now.values)),
                    &self.filter,
                    &self.columns,
                ));
            }

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

        drop(pins);
        self.update_actions(state);
    }

    fn update_actions(&self, state: &Comparisons) {
        let pins = state.pins.borrow();
        let idle = self.pending.get() == 0;
        self.pin.set_sensitive(idle);
        self.source.set_sensitive(idle);
        self.compare.set_sensitive(idle && !pins.is_empty());
        self.clear.set_sensitive(idle && !pins.is_empty());

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
                    !row.compared || row.changed() || row.uncertain()
                })
    });

    let updated = filter.clone();
    changed.connect_toggled(move |_| updated.changed(gtk::FilterChange::Different));
    filter
}

fn register_difference_table(
    pin: &Pin,
    current: Option<&Baseline>,
    filter: &gtk::CustomFilter,
    columns: &TableLayout,
) -> gtk::Box {
    let display = Rc::clone(&pin.vector_display);
    let before = pin.baseline.values.clone();
    let after = current.map(|now| now.values.clone());
    let root = gtk::Box::new(gtk::Orientation::Vertical, components::CONTROL_GAP);
    root.set_vexpand(true);
    let table = gtk::Box::new(gtk::Orientation::Vertical, 0);
    table.set_vexpand(true);

    table.append(&difference_table(
        register_difference(&before, after.as_ref(), display.get()),
        filter,
        columns,
    ));

    let has_vectors = before
        .iter()
        .chain(after.iter().flat_map(|values| values.iter()))
        .any(|(name, value)| value.complete && VectorValue::parse(name, &value.text).is_some());

    if has_vectors {
        let controls = VectorControls::new();
        controls.set_display(display.get());
        controls.root.prepend(&empty_label("SIMD lanes"));
        controls
            .root
            .append(&empty_label("Low → high · changes compare bits"));
        root.append(&controls.root);
        let target = table.downgrade();
        let filter = filter.clone();
        let columns = columns.clone();

        controls.connect_changed(move |selected| {
            display.set(selected);

            if let Some(table) = target.upgrade() {
                while let Some(child) = table.first_child() {
                    table.remove(&child);
                }

                table.append(&difference_table(
                    register_difference(&before, after.as_ref(), selected),
                    &filter,
                    &columns,
                ));
            }
        });
    }

    root.append(&table);
    root
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
    let uncertain = rows.iter().filter(|row| row.uncertain()).count();
    let compared = rows.iter().any(|row| row.compared);

    let summary = if compared {
        format!("{changed} changed · {uncertain} incomplete · {total} captured entries")
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

            let bits = row.bits.and_then(|bits| {
                index
                    .checked_sub(1)
                    .and_then(|side| bits.get(side).copied().flatten())
            });

            if let Some(raw) = bits
                && row.changed()
                && row.before.as_ref().map(|value| &value.text)
                    == row.after.as_ref().map(|value| &value.text)
            {
                // Equal float text can hide a different NaN payload or rounding.
                label.set_text(&format!("{value}\n0x{raw:x}"));
            } else {
                label.set_text(value);
            }

            label.set_tooltip_text(Some(&bits.map_or_else(
                || value.to_owned(),
                |raw| format!("{value}\nBits 0x{raw:x}"),
            )));
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

    let empty = empty_label(if total == 0 {
        "No captured values"
    } else {
        "No differences in captured data"
    });

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
            complete: true,
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
        view.pending.set(1);
        view.update_actions(&state);

        assert!(
            !view.pin.is_sensitive() && !view.compare.is_sensitive() && !view.clear.is_sensitive()
        );

        view.pending.set(0);
        view.update_actions(&state);

        assert!(
            view.pin.is_sensitive() && view.compare.is_sensitive() && view.clear.is_sensitive()
        );

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

        let mut large = baseline.clone();

        large.source = Source::Memory {
            id: 1,
            address: 0x1000,
            bytes: 4096,
        };

        large.values = (0..MAX_ROWS)
            .map(|index| (format!("field_{index}"), "long_value".repeat(40).into()))
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
        let mut partial = state.pins.borrow()[0].baseline.clone();
        partial.complete = false;
        partial.values.values_mut().next().unwrap().complete = false;
        state.pins.borrow_mut()[0].compared = Some(Ok(partial));
        view.changed.set_active(true);
        view.render(&state, 0);
        let page = view.pages.nth_page(Some(0)).unwrap();

        let table = super::super::tests::descendants::<gtk::ColumnView>(&page)
            .pop()
            .unwrap();

        assert_eq!(
            table.model().unwrap().n_items(),
            1,
            "incomplete rows must remain visible under Changed only"
        );

        let mut registers = state.pins.borrow()[0].baseline.clone();

        registers.values = BTreeMap::from([
            ("rax".into(), "0x0000000000000001".into()),
            ("xmm0".into(), "0x7fc0004280000000c02000003f800000".into()),
            ("zmm2".into(), "0x0".into()),
        ]);

        state.pin(registers.clone()).unwrap();
        registers
            .values
            .insert("rax".into(), "0x0000000000000002".into());

        registers
            .values
            .insert("xmm0".into(), "0x7fc0004380000000c02000003fc00000".into());

        state.pins.borrow_mut()[0].compared = Some(Ok(registers));
        view.render(&state, 0);
        let page = view.pages.nth_page(Some(0)).unwrap();
        let controls = super::super::tests::descendants::<gtk::DropDown>(&page);
        assert_eq!(controls.len(), 2);
        controls[0].set_selected(4);
        assert!(!controls[1].is_sensitive());

        let table = super::super::tests::descendants::<gtk::ColumnView>(&page)
            .pop()
            .unwrap();

        let model = table.model().unwrap();
        assert_eq!(model.n_items(), 3);

        let nan = model
            .item(2)
            .and_downcast::<glib::BoxedAnyObject>()
            .unwrap();

        assert_eq!(nan.borrow::<DifferenceRow>().name, "$xmm0 [3]");
        assert!(nan.borrow::<DifferenceRow>().changed());
        let previous = table.downgrade();
        drop(table);
        controls[0].set_selected(0);
        main.block_on(glib::timeout_future(Duration::from_millis(100)));

        assert!(
            previous.upgrade().is_none(),
            "reformatting must release the old table"
        );

        assert!(controls[1].is_sensitive());
        controls[1].set_selected(2);
        view.changed.set_active(false);
        main.block_on(glib::timeout_future(Duration::from_millis(100)));
        assert_eq!(window.height(), height);
        assert!(window.width() <= 960);
        controls[0].set_selected(4);
        view.changed.set_active(true);
        view.render(&state, 0);
        let page = view.pages.nth_page(Some(0)).unwrap();
        let controls = super::super::tests::descendants::<gtk::DropDown>(&page);

        assert_eq!(
            controls[0].selected(),
            4,
            "comparison refresh retains the lane format"
        );

        view.source.set_selected(2);
        main.block_on(glib::timeout_future(Duration::from_millis(100)));

        assert!(
            super::super::tests::descendants::<gtk::Label>(&page)
                .iter()
                .any(|label| label.text().contains("0x7fc00043"))
        );

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
    fn incomplete_values_never_establish_equality() {
        let values = BTreeMap::from([(
            "pair".into(),
            CapturedValue {
                text: "{...}".into(),
                complete: false,
            },
        )]);

        let rows = difference(&values, Some(&values));
        assert!(rows[0].uncertain());
        assert!(!rows[0].changed());
        assert_eq!(rows[0].cells()[3], "Incomplete");
    }

    #[test]
    fn comparisons_preserve_absence_and_do_not_compare_truncated_values() {
        let before = BTreeMap::from([("x".into(), "1".into()), ("gone".into(), "2".into())]);
        let after = BTreeMap::from([("x".into(), "3".into())]);
        let rows = difference(&before, Some(&after));
        assert_eq!(rows[0].cells(), ["gone", "2", "—", "Not captured"]);
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
        assert!(
            insert(
                &mut values,
                "x".into(),
                "x".repeat(MAX_BYTES).into(),
                &mut 0
            )
            .is_err()
        );
        assert!(values.is_empty());
    }

    #[test]
    fn register_snapshots_compare_exact_lanes_not_printer_or_float_text() {
        let capture = |entries: &[(&str, &str)]| {
            register_snapshot(
                entries
                    .iter()
                    .map(|(name, value)| Register {
                        name: (*name).into(),
                        value: (*value).into(),
                        pointer_chain: Vec::new(),
                    })
                    .collect(),
                TargetArchitecture::X86_64,
                Some(TargetEndian::Little),
                PointerWidth::Bits64,
            )
            .unwrap()
        };

        let before = capture(&[
            ("rax", "0x1"),
            (
                "xmm0",
                "{v4_float = {1, -2.5, -0, nan}, v2_int64 = {0xc02000003f800000, 0x7fc0004280000000}}",
            ),
            ("xmm2", "{v2_int64 = {0 <repeats 2 times>}}"),
            ("xmm10", "0x0"),
        ]);

        assert!(before.complete);
        assert_eq!(before.values["rax"].text, "0x0000000000000001");

        assert_eq!(
            before.values["xmm0"].text,
            "0x7fc0004280000000c02000003f800000"
        );

        let mut display = VectorDisplay::default();
        display.format = VectorLaneFormat::Float32;
        let mut after = before.values.clone();
        after.insert("xmm0".into(), "0x7fc0004380000000c02000003f800000".into());
        let rows = register_difference(&before.values, Some(&after), display);
        let changed = rows.iter().filter(|row| row.changed()).collect::<Vec<_>>();
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0].name, "$xmm0 [3]");

        assert_eq!(
            changed[0].before, changed[0].after,
            "NaN payload changes must survive identical displayed text"
        );

        assert_eq!(changed[0].bits, Some([Some(0x7fc00042), Some(0x7fc00043)]));
        assert_eq!(rows[5].name, "$xmm2 [0]");
        assert_eq!(rows[9].name, "$xmm10 [0]");

        assert!(
            rows.iter()
                .any(|row| row.before.as_ref().is_some_and(|value| value.text == "1"))
        );

        after.insert("xmm0".into(), "0x7fc0004200000000c02000003f800000".into());
        let zeros = register_difference(&before.values, Some(&after), display);
        assert_eq!(zeros.iter().filter(|row| row.changed()).count(), 1);
        assert!(zeros[3].changed(), "negative zero has different bits");
        let unavailable = capture(&[("xmm0", "<unavailable>")]);
        assert!(!unavailable.complete);
        assert!(!capture(&[("xmm0", "0xinvalid")]).complete);
        let rows = register_difference(&before.values, Some(&unavailable.values), display);
        assert!(rows[1..5].iter().all(DifferenceRow::uncertain));

        let large = (0..32)
            .map(|index| Register {
                name: format!("zmm{index}"),
                value: format!(
                    "{{ignored = {{{}}}, v8_int64 = {{0 <repeats 8 times>}}}}",
                    "0, ".repeat(1024)
                ),
                pointer_chain: Vec::new(),
            })
            .collect();

        let snapshot = register_snapshot(
            large,
            TargetArchitecture::X86_64,
            Some(TargetEndian::Little),
            PointerWidth::Bits64,
        )
        .unwrap();

        assert!(snapshot.complete);
        assert_eq!(snapshot.values.len(), 32);

        assert!(
            snapshot
                .values
                .values()
                .all(|value| value.text.len() == 130)
        );

        display.format = VectorLaneFormat::Int8;

        assert_eq!(
            register_difference(&snapshot.values, None, display).len(),
            2048
        );
    }

    #[test]
    #[ignore = "requires GDB, an x86 host and the c-simd-target fixture"]
    fn live_register_comparisons_detect_changed_simd_lanes() {
        use crate::app::test_support::{open_debugger, request};

        let (_debugger, client) = open_debugger("c-simd-target", "simd_sse_checkpoint");
        let names = crate::debugger::register_names(&request(&client, "-data-list-register-names"));
        let numbers = crate::debugger::compact_register_numbers(&names, TargetArchitecture::X86_64);

        let command = format!(
            "-data-list-register-values x {}",
            numbers
                .iter()
                .map(usize::to_string)
                .collect::<Vec<_>>()
                .join(" ")
        );

        let capture = || {
            register_snapshot(
                crate::debugger::registers(&request(&client, &command), &names),
                TargetArchitecture::X86_64,
                Some(TargetEndian::Little),
                PointerWidth::Bits64,
            )
            .unwrap()
        };

        let before = capture();
        assert!(before.complete, "{before:?}");

        assert!(
            request(
                &client,
                "-data-evaluate-expression \"$xmm0.v4_int32[3] = 0x7fc00043\""
            )
            .is_done()
        );

        let after = capture();
        assert!(after.complete);
        let mut display = VectorDisplay::default();
        display.format = VectorLaneFormat::Float32;
        let rows = register_difference(&before.values, Some(&after.values), display);
        let changed = rows.iter().filter(|row| row.changed()).collect::<Vec<_>>();
        assert_eq!(changed.len(), 1);
        assert!(changed[0].name.ends_with("0 [3]"));
        assert_eq!(changed[0].bits, Some([Some(0x7fc00042), Some(0x7fc00043)]));
        assert_eq!(changed[0].before, changed[0].after);
    }
}
