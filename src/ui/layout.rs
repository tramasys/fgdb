use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    fmt::Write as _,
    path::PathBuf,
    rc::Rc,
    time::Duration,
};

use gtk::{glib, prelude::*};

use super::workspace::console::Selection as ConsoleView;
use super::{KernelSectionHandler, PanelId, workspace::Panels};

mod columns;
mod writer;
pub(super) use columns::{ColumnLayouts, TableId, TableLayout};

const SAVE_DELAY: Duration = Duration::from_millis(350);
const FINAL_SAVE_GRACE: Duration = Duration::from_secs(2);
const MIN_WINDOW_WIDTH: i32 = 320;
const MIN_WINDOW_HEIGHT: i32 = 200;
const MAX_WINDOW_DIMENSION: i32 = 32_768;
const DISCLOSURE_PREFIX: &str = "disclosure.";
const NOTEBOOK_PREFIX: &str = "notebook.";
const STACK_PREFIX: &str = "stack.";
const TERMINAL_VISIBLE_KEY: &str = "terminal.visible";
const CONSOLE_VIEW_KEY: &str = "console.view";
const PANEL_PREFIX: &str = "panel.";
const MAX_LAYOUT_BYTES: usize = 1024 * 1024;
const MAX_NOTEBOOK_PAGE: u32 = 1024;

fn layout_path() -> PathBuf {
    glib::user_config_dir().join("fgdb/layout.conf")
}

pub(super) fn remembered_disclosures() -> HashMap<String, bool> {
    crate::bounded::read_string(&layout_path(), MAX_LAYOUT_BYTES)
        .map(|contents| parse_layout(&contents).disclosures)
        .unwrap_or_default()
}

#[derive(Clone)]
pub(super) struct Pane {
    key: &'static str,
    widget: gtk::Paned,
    default_fraction: Option<f64>,
}

impl Pane {
    pub(super) fn new(key: &'static str, widget: &gtk::Paned) -> Self {
        Self {
            key,
            widget: widget.clone(),
            default_fraction: None,
        }
    }

    pub(super) fn with_default_fraction(
        key: &'static str,
        widget: &gtk::Paned,
        default_fraction: f64,
    ) -> Self {
        debug_assert!((0.0..=1.0).contains(&default_fraction));

        Self {
            key,
            widget: widget.clone(),
            default_fraction: Some(default_fraction.clamp(0.0, 1.0)),
        }
    }
}

#[derive(Clone)]
pub(super) struct Persistence(Rc<State>);

impl Persistence {
    pub(super) fn install(
        window: &gtk::ApplicationWindow,
        panes: Vec<Pane>,
        columns: &ColumnLayouts,
    ) -> Self {
        Self::install_at(window, panes, layout_path(), columns)
    }

    pub(super) fn install_at(
        window: &gtk::ApplicationWindow,
        panes: Vec<Pane>,
        path: PathBuf,
        columns: &ColumnLayouts,
    ) -> Self {
        let mut remembered = crate::bounded::read_string(&path, MAX_LAYOUT_BYTES)
            .map(|contents| parse_layout(&contents))
            .unwrap_or_default();

        if let Some(geometry) = remembered.window {
            geometry.apply(window);
        }

        columns.restore(std::mem::take(&mut remembered.column_widths));

        let state = Rc::new(State {
            columns: columns.clone(),
            writer: std::sync::Arc::new(writer::Writer::new(path)),
            window: window.clone(),
            panes,
            remembered: RefCell::new(remembered),
            ready_handler: RefCell::new(None),
            error_handler: RefCell::new(None),
            error: RefCell::new(None),
            status_labels: RefCell::new(Vec::new()),
            read_only: Cell::new(false),
            writing: Cell::new(false),
            pending_write: RefCell::new(None),
            finished: Cell::new(false),
            #[cfg(test)]
            final_save_done: Cell::new(false),
            pending_save: RefCell::new(None),
            pending_pane_restores: RefCell::new(HashMap::new()),
            restore_started: Cell::new(false),
            restoring_position: Cell::new(false),
            ready_to_save: Cell::new(false),
        });

        let weak = Rc::downgrade(&state);
        columns.on_changed(move || {
            if let Some(state) = weak.upgrade()
                && state.ready_to_save.get()
            {
                state.schedule_save();
            }
        });

        for pane in &state.panes {
            let weak_state = Rc::downgrade(&state);
            let key = pane.key;

            pane.widget.connect_position_notify(move |widget| {
                let Some(state) = weak_state.upgrade() else {
                    return;
                };

                if state.ready_to_save.get() && !state.restoring_position.get() {
                    state.cancel_pane_restore(key);
                    state.remember_position(key, widget);
                    state.schedule_save();
                }
            });

            let weak_state = Rc::downgrade(&state);
            let key = pane.key;
            let default_fraction = pane.default_fraction;

            pane.widget.connect_map(move |widget| {
                let Some(state) = weak_state.upgrade() else {
                    return;
                };

                if !state.ready_to_save.get() || state.finished.get() {
                    return;
                }

                state.cancel_pane_restore(key);
                let weak_state = Rc::downgrade(&state);
                let measured = Cell::new(false);

                // A map signal and an idle callback can both precede allocation.
                // Let one layout frame finish before restoring the new pane.
                let source = widget.add_tick_callback(move |widget, _| {
                    if !measured.replace(true) {
                        return glib::ControlFlow::Continue;
                    }

                    let Some(state) = weak_state.upgrade() else {
                        return glib::ControlFlow::Break;
                    };

                    state.pending_pane_restores.borrow_mut().remove(key);

                    if !state.finished.get() && widget.is_mapped() {
                        state.restore_position(key, default_fraction, widget);
                    }

                    glib::ControlFlow::Break
                });

                state.pending_pane_restores.borrow_mut().insert(key, source);
            });

            let weak_state = Rc::downgrade(&state);

            pane.widget.connect_unmap(move |_| {
                if let Some(state) = weak_state.upgrade() {
                    state.cancel_pane_restore(key);
                }
            });
        }

        let weak_state = Rc::downgrade(&state);

        window.connect_map(move |_| {
            if let Some(state) = weak_state.upgrade() {
                state.start_restore();
            }
        });

        for property in ["default-width", "default-height", "maximized"] {
            let weak_state = Rc::downgrade(&state);

            window.connect_notify_local(Some(property), move |_, _| {
                if let Some(state) = weak_state.upgrade()
                    && state.ready_to_save.get()
                {
                    state.schedule_save();
                }
            });
        }

        state.write(None);

        Self(state)
    }

    pub(super) fn save(&self) {
        self.0.save_now();
    }

    pub(super) fn finish(&self) {
        if self.0.finished.replace(true) {
            return;
        }

        self.0.cancel_save();
        self.0.columns.finish();
        self.0.pending_write.borrow_mut().take();
        let contents = self.0.snapshot();

        for (_, source) in self.0.pending_pane_restores.borrow_mut().drain() {
            source.remove();
        }

        // Freeze the snapshot before hosts are dismantled. Neither acquiring
        // the writer gate nor flushing storage may block debugger shutdown.
        self.0.writer.retire();
        let writer = std::sync::Arc::clone(&self.0.writer);
        let work = gtk::gio::spawn_blocking(move || writer.finish(&contents));
        let hold = self.0.window.application().map(|app| app.hold());
        let weak = Rc::downgrade(&self.0);

        glib::spawn_future_local(async move {
            let result = match glib::future_with_timeout(FINAL_SAVE_GRACE, work).await {
                Ok(Ok(result)) => result,
                Ok(Err(_)) => Err(std::io::Error::other("The layout writer stopped")),
                Err(_) => Err(std::io::Error::other(
                    "Storage is still busy. The final layout save may not finish before exit",
                )),
            };

            if let Some(state) = weak.upgrade() {
                state.report_write(result);
                #[cfg(test)]
                state.final_save_done.set(true);
            }

            drop(hold);
        });
    }

    pub(super) fn on_ready(&self, callback: impl FnOnce() + 'static) {
        if self.0.ready_to_save.get() {
            callback();
        } else {
            self.0.ready_handler.replace(Some(Box::new(callback)));
        }
    }

    #[cfg(test)]
    pub(super) fn final_save_done(&self) -> bool {
        self.0.final_save_done.get()
    }

    pub(super) fn on_error(&self, callback: impl Fn(&str) + 'static) {
        if let Some(error) = self.0.error.borrow().as_deref() {
            callback(error);
        }

        self.0.error_handler.replace(Some(Rc::new(callback)));
    }

    pub(super) fn status_note(&self) -> gtk::Label {
        let error = self.0.error.borrow();
        let label = super::components::empty_label(error.as_deref().unwrap_or(""));
        label.set_visible(error.is_some());
        let mut labels = self.0.status_labels.borrow_mut();
        labels.retain(|label| label.upgrade().is_some());
        labels.push(label.downgrade());

        label
    }

    pub(super) fn panel(&self, id: PanelId) -> Option<PanelPlacement> {
        self.0.remembered.borrow().panels.get(&id).copied()
    }

    pub(super) fn remember_panel(&self, id: PanelId, placement: PanelPlacement) {
        if self.0.finished.get() || self.panel(id) == Some(placement) {
            return;
        }

        self.0.remembered.borrow_mut().panels.insert(id, placement);

        if self.0.ready_to_save.get() {
            self.0.schedule_save();
        }
    }

    pub(super) fn console_view(&self) -> ConsoleView {
        self.0.remembered.borrow().console_view()
    }

    pub(super) fn console_handler(&self) -> impl Fn(ConsoleView) + 'static {
        let weak = Rc::downgrade(&self.0);

        move |selected| {
            let Some(state) = weak.upgrade().filter(|state| !state.finished.get()) else {
                return;
            };

            let mut remembered = state.remembered.borrow_mut();
            remembered.console_view = Some(selected);
            remembered.terminal_visible = Some(selected == ConsoleView::Terminal);
            drop(remembered);

            if state.ready_to_save.get() {
                state.schedule_save();
            }
        }
    }

    pub(super) fn bind_notebook(
        &self,
        key: &'static str,
        notebook: &gtk::Notebook,
        panels: &Panels,
    ) {
        let page = self.0.remembered.borrow().notebooks.get(key).copied();

        let page = page.and_then(|page| match page {
            SavedPage::Panel(id) => panels.slot(id).and_then(|slot| notebook.page_num(slot)),
            SavedPage::Legacy(index) => Some(index),
        });

        if let Some(page) = page.filter(|page| *page < notebook.n_pages()) {
            notebook.set_current_page(Some(page));
        }

        let identities = (0..notebook.n_pages())
            .filter_map(|position| {
                let root = notebook.nth_page(Some(position))?;
                Some((panels.id(&root)?, root.downgrade()))
            })
            .collect::<Vec<_>>();

        if let Some(root) = notebook
            .current_page()
            .and_then(|page| notebook.nth_page(Some(page)))
            && let Some(id) = panels.id(&root)
        {
            self.0
                .remembered
                .borrow_mut()
                .notebooks
                .insert(key.to_owned(), SavedPage::Panel(id));
        }

        let weak_state = Rc::downgrade(&self.0);

        notebook.connect_switch_page(move |_, root, _| {
            let Some(state) = weak_state.upgrade() else {
                return;
            };

            let Some((id, _)) = identities
                .iter()
                .find(|(_, widget)| widget.upgrade().as_ref() == Some(root))
            else {
                return;
            };

            let page = SavedPage::Panel(*id);
            let changed = state.remembered.borrow().notebooks.get(key).copied() != Some(page);

            if !changed {
                return;
            }

            state
                .remembered
                .borrow_mut()
                .notebooks
                .insert(key.to_owned(), page);

            if state.ready_to_save.get() {
                state.schedule_save();
            }
        });
    }

    pub(super) fn disclosure_handler(&self) -> KernelSectionHandler {
        let weak_state = Rc::downgrade(&self.0);

        Rc::new(move |key, expanded| {
            if let Some(state) = weak_state.upgrade() {
                state.set_disclosure(key, expanded);
            }
        })
    }

    pub(super) fn bind_stack(&self, key: &'static str, stack: &gtk::Stack) {
        let selected = self.0.remembered.borrow().stacks.get(key).cloned();

        if let Some(selected) = selected
            && stack.child_by_name(&selected).is_some()
        {
            stack.set_visible_child_name(&selected);
        }

        let weak = Rc::downgrade(&self.0);

        stack.connect_visible_child_name_notify(move |stack| {
            if let Some(state) = weak.upgrade()
                && let Some(name) = stack.visible_child_name()
            {
                let changed = state
                    .remembered
                    .borrow()
                    .stacks
                    .get(key)
                    .map(String::as_str)
                    != Some(name.as_str());

                if changed {
                    state
                        .remembered
                        .borrow_mut()
                        .stacks
                        .insert(key.to_owned(), name.to_string());

                    if state.ready_to_save.get() {
                        state.schedule_save();
                    }
                }
            }
        });
    }
}

type ReadyHandler = Box<dyn FnOnce()>;
type ErrorHandler = Rc<dyn Fn(&str)>;

struct State {
    columns: ColumnLayouts,
    writer: std::sync::Arc<writer::Writer>,
    window: gtk::ApplicationWindow,
    panes: Vec<Pane>,
    remembered: RefCell<RememberedLayout>,
    ready_handler: RefCell<Option<ReadyHandler>>,
    error_handler: RefCell<Option<ErrorHandler>>,
    error: RefCell<Option<String>>,
    status_labels: RefCell<Vec<glib::WeakRef<gtk::Label>>>,
    read_only: Cell<bool>,
    writing: Cell<bool>,
    pending_write: RefCell<Option<String>>,
    finished: Cell<bool>,
    #[cfg(test)]
    final_save_done: Cell<bool>,
    pending_save: RefCell<Option<glib::SourceId>>,
    pending_pane_restores: RefCell<HashMap<&'static str, gtk::TickCallbackId>>,
    restore_started: Cell<bool>,
    restoring_position: Cell<bool>,
    ready_to_save: Cell<bool>,
}

impl State {
    fn cancel_pane_restore(&self, key: &str) {
        if let Some(source) = self.pending_pane_restores.borrow_mut().remove(key) {
            source.remove();
        }
    }

    fn set_disclosure(self: &Rc<Self>, key: &str, expanded: bool) {
        self.remembered
            .borrow_mut()
            .disclosures
            .insert(key.to_owned(), expanded);

        if self.ready_to_save.get() {
            self.schedule_save();
        }
    }

    fn start_restore(self: &Rc<Self>) {
        if self.finished.get() || self.restore_started.replace(true) {
            return;
        }

        let weak_state = Rc::downgrade(self);

        glib::idle_add_local_once(move || {
            let Some(state) = weak_state.upgrade() else {
                return;
            };

            if state.finished.get() {
                return;
            }

            state.restore_positions();

            // Restoring an outer split changes the allocation available to its
            // nested splits. A second pass after one frame gives those panes
            // their final proportional positions.
            let weak_state = Rc::downgrade(&state);

            glib::timeout_add_local_once(Duration::from_millis(16), move || {
                let Some(state) = weak_state.upgrade() else {
                    return;
                };

                if state.finished.get() {
                    return;
                }

                state.restore_positions();
                state.ready_to_save.set(true);
                let callback = state.ready_handler.borrow_mut().take();

                if let Some(callback) = callback {
                    callback();
                }
            });
        });
    }

    fn restore_positions(&self) {
        for pane in &self.panes {
            if pane.widget.is_mapped() {
                self.restore_position(pane.key, pane.default_fraction, &pane.widget);
            }
        }
    }

    fn restore_position(
        &self,
        key: &'static str,
        default_fraction: Option<f64>,
        widget: &gtk::Paned,
    ) {
        let maximum = widget.max_position();
        let minimum = widget.min_position();

        if !pane_children_visible(widget) || !valid_pane_range(minimum, maximum) {
            return;
        }

        // `set_position` emits `position-notify` synchronously. Drop the
        // immutable RefCell borrow before entering GTK so that callback can
        // record the applied position without tripping a re-entrant borrow.
        let saved = self.remembered.borrow().panes.get(key).copied();

        if let Some(saved) = saved {
            self.set_position(widget, scale_position(saved, minimum, maximum));
            return;
        }

        let Some(fraction) = default_fraction else {
            return;
        };

        let extent = match widget.orientation() {
            gtk::Orientation::Horizontal => widget.width(),
            gtk::Orientation::Vertical => widget.height(),
            _ => 0,
        };

        if extent > 0 {
            self.set_position(
                widget,
                fractional_position(fraction, extent, minimum, maximum),
            );
        }
    }

    fn set_position(&self, widget: &gtk::Paned, position: i32) {
        self.restoring_position.set(true);
        widget.set_position(position);
        self.restoring_position.set(false);
    }

    fn remember_position(&self, key: &'static str, widget: &gtk::Paned) {
        if !widget.is_mapped() || !pane_children_visible(widget) {
            return;
        }

        let minimum = widget.min_position();
        let maximum = widget.max_position();
        let position = widget.position();

        if valid_live_pane_position(position, minimum, maximum) {
            self.remembered.borrow_mut().panes.insert(
                key.to_owned(),
                PanePosition {
                    position,
                    extent: maximum,
                },
            );
        }
    }

    fn schedule_save(self: &Rc<Self>) {
        if self.finished.get() || self.read_only.get() {
            return;
        }

        self.cancel_save();
        let weak_state = Rc::downgrade(self);

        let source = glib::timeout_add_local_once(SAVE_DELAY, move || {
            let Some(state) = weak_state.upgrade() else {
                return;
            };

            state.pending_save.borrow_mut().take();
            state.save_now();
        });

        self.pending_save.replace(Some(source));
    }

    fn cancel_save(&self) {
        if let Some(source) = self.pending_save.borrow_mut().take() {
            source.remove();
        }
    }

    fn save_now(self: &Rc<Self>) {
        if self.finished.get() || self.read_only.get() {
            return;
        }

        self.cancel_save();
        self.write(Some(self.snapshot()));
    }

    fn write(self: &Rc<Self>, contents: Option<String>) {
        if self.writing.replace(true) {
            if let Some(contents) = contents {
                self.pending_write.replace(Some(contents));
            }

            return;
        }

        let writer = std::sync::Arc::clone(&self.writer);
        let weak = Rc::downgrade(self);

        let work = gtk::gio::spawn_blocking(move || match contents {
            Some(contents) => writer.write(&contents),
            None => writer.initialize(),
        });

        glib::spawn_future_local(async move {
            let result = work
                .await
                .unwrap_or_else(|_| Err(std::io::Error::other("The layout writer stopped")));

            if let Some(state) = weak.upgrade() {
                state.writing.set(false);

                if state.finished.get() {
                    return;
                }

                state.report_write(result);
                let pending = state.pending_write.borrow_mut().take();

                if let Some(contents) = pending
                    && !state.read_only.get()
                {
                    state.write(Some(contents));
                }
            }
        });
    }

    fn report_write(&self, result: std::io::Result<writer::Outcome>) {
        let error = match result {
            Ok(writer::Outcome::Saved) => None,
            Ok(writer::Outcome::ReadOnly) => {
                self.read_only.set(true);
                Some("Another fgdb instance owns the shared layout. Layout changes in this instance are not saved.".to_owned())
            }
            Err(error) => Some(format!("Could not save layout: {error}")),
        };

        if *self.error.borrow() == error {
            return;
        }

        self.error.replace(error.clone());
        self.status_labels.borrow_mut().retain(|label| {
            let Some(label) = label.upgrade() else {
                return false;
            };

            label.set_label(error.as_deref().unwrap_or(""));
            label.set_visible(error.is_some());
            true
        });

        let callback = self.error_handler.borrow().clone();

        if let (Some(error), Some(callback)) = (error, callback) {
            callback(&error);
        }
    }

    fn snapshot(&self) -> String {
        let mut remembered = self.remembered.borrow_mut();

        if let Some(geometry) = WindowGeometry::capture(&self.window) {
            remembered.window = Some(geometry);
        }

        for pane in &self.panes {
            if !self.ready_to_save.get()
                || !pane.widget.is_mapped()
                || !pane_children_visible(&pane.widget)
                || self.pending_pane_restores.borrow().contains_key(pane.key)
            {
                continue;
            }

            let maximum = pane.widget.max_position();
            let minimum = pane.widget.min_position();
            let position = pane.widget.position();

            if valid_live_pane_position(position, minimum, maximum) {
                remembered.panes.insert(
                    pane.key.to_owned(),
                    PanePosition {
                        position,
                        extent: maximum,
                    },
                );
            }
        }

        let mut contents = serialize_layout(&self.panes, &remembered);
        self.columns.write(&mut contents);
        contents
    }
}

fn pane_children_visible(pane: &gtk::Paned) -> bool {
    // A detached pane must not replace the remembered docked proportions.
    pane.start_child().is_some_and(|child| child.get_visible())
        && pane.end_child().is_some_and(|child| child.get_visible())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PanePosition {
    position: i32,
    extent: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct WindowSize {
    width: i32,
    height: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct WindowGeometry {
    size: WindowSize,
    maximized: bool,
}

impl WindowGeometry {
    pub(super) fn capture(window: &impl IsA<gtk::Window>) -> Option<Self> {
        let (width, height) = window.default_size();

        valid_window_size(width, height).then(|| Self {
            size: WindowSize { width, height },
            maximized: window.is_maximized(),
        })
    }

    pub(super) fn apply(self, window: &impl IsA<gtk::Window>) {
        // GTK tracks the normal size even while maximized. Widget allocations
        // include decoration differences and can drift across launches.
        window.set_default_size(self.size.width, self.size.height);

        if self.maximized {
            window.maximize();
        }
    }

    fn parse(value: &str) -> Option<Self> {
        let mut values = value.split(',').map(str::trim);
        let width = values.next()?.parse().ok()?;
        let height = values.next()?.parse().ok()?;
        let maximized = parse_bool(values.next()?)?;

        (values.next().is_none() && valid_window_size(width, height)).then_some(Self {
            size: WindowSize { width, height },
            maximized,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct PanelPlacement {
    pub floating: bool,
    pub geometry: WindowGeometry,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct RememberedLayout {
    column_widths: columns::Widths,
    window: Option<WindowGeometry>,
    terminal_visible: Option<bool>,
    console_view: Option<ConsoleView>,
    panes: HashMap<String, PanePosition>,
    notebooks: HashMap<String, SavedPage>,
    stacks: HashMap<String, String>,
    disclosures: HashMap<String, bool>,
    panels: HashMap<PanelId, PanelPlacement>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SavedPage {
    Panel(PanelId),
    Legacy(u32),
}

impl RememberedLayout {
    fn console_view(&self) -> ConsoleView {
        self.console_view.unwrap_or_else(|| {
            if self.terminal_visible.unwrap_or(true) {
                ConsoleView::Terminal
            } else {
                ConsoleView::Hidden
            }
        })
    }
}

fn scale_position(saved: PanePosition, minimum: i32, maximum: i32) -> i32 {
    let scaled = if saved.extent > 0 {
        (i64::from(saved.position) * i64::from(maximum) + i64::from(saved.extent) / 2)
            / i64::from(saved.extent)
    } else {
        i64::from(saved.position)
    };

    scaled.clamp(i64::from(minimum), i64::from(maximum)) as i32
}

fn fractional_position(fraction: f64, extent: i32, minimum: i32, maximum: i32) -> i32 {
    ((f64::from(extent) * fraction).round() as i32).clamp(minimum, maximum)
}

fn valid_pane_range(minimum: i32, maximum: i32) -> bool {
    maximum > minimum && maximum <= MAX_WINDOW_DIMENSION
}

fn valid_live_pane_position(position: i32, minimum: i32, maximum: i32) -> bool {
    valid_pane_range(minimum, maximum) && position > minimum && position < maximum
}

fn valid_window_size(width: i32, height: i32) -> bool {
    (MIN_WINDOW_WIDTH..=MAX_WINDOW_DIMENSION).contains(&width)
        && (MIN_WINDOW_HEIGHT..=MAX_WINDOW_DIMENSION).contains(&height)
}

fn parse_layout(contents: &str) -> RememberedLayout {
    let mut remembered = RememberedLayout::default();

    for line in contents.lines() {
        let line = line.trim();

        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let Some((key, geometry)) = line.split_once('=') else {
            continue;
        };

        if remembered.column_widths.parse(key.trim(), geometry) {
            continue;
        }

        if key.trim() == "window" {
            if let Some(geometry) = WindowGeometry::parse(geometry) {
                remembered.window = Some(geometry);
            }

            continue;
        }

        if let Some(key) = key.trim().strip_prefix(PANEL_PREFIX) {
            if let Some(id) = PanelId::from_key(key)
                && let Some((floating, geometry)) = geometry.split_once(',')
                && let Some(floating) = parse_bool(floating.trim())
                && let Some(geometry) = WindowGeometry::parse(geometry)
            {
                remembered
                    .panels
                    .insert(id, PanelPlacement { floating, geometry });
            }

            continue;
        }

        if key.trim() == TERMINAL_VISIBLE_KEY {
            remembered.terminal_visible = parse_bool(geometry.trim());
            continue;
        }

        if key.trim() == CONSOLE_VIEW_KEY {
            remembered.console_view = ConsoleView::parse(geometry.trim());
            continue;
        }

        if let Some(key) = key.trim().strip_prefix(DISCLOSURE_PREFIX) {
            if !key.is_empty()
                && let Some(expanded) = parse_bool(geometry.trim())
            {
                remembered.disclosures.insert(key.to_owned(), expanded);
            }

            continue;
        }

        if let Some(key) = key.trim().strip_prefix(NOTEBOOK_PREFIX) {
            let value = geometry.trim();
            let page = PanelId::from_key(value).map(SavedPage::Panel).or_else(|| {
                value
                    .parse::<u32>()
                    .ok()
                    .filter(|page| *page <= MAX_NOTEBOOK_PAGE)
                    .map(SavedPage::Legacy)
            });

            if !key.is_empty()
                && let Some(page) = page
            {
                remembered.notebooks.insert(key.to_owned(), page);
            }

            continue;
        }

        if let Some(key) = key.trim().strip_prefix(STACK_PREFIX) {
            let page = geometry.trim();

            if !key.is_empty() && !page.is_empty() && page.len() <= 128 {
                remembered.stacks.insert(key.to_owned(), page.to_owned());
            }

            continue;
        }

        let Some((position, extent)) = geometry.split_once(',') else {
            continue;
        };

        let (Ok(position), Ok(extent)) =
            (position.trim().parse::<i32>(), extent.trim().parse::<i32>())
        else {
            continue;
        };

        if valid_live_pane_position(position, 0, extent) {
            remembered
                .panes
                .insert(key.trim().to_owned(), PanePosition { position, extent });
        }
    }

    remembered
}

fn parse_bool(value: &str) -> Option<bool> {
    match value {
        "1" | "true" => Some(true),
        "0" | "false" => Some(false),
        _ => None,
    }
}

fn serialize_layout(panes: &[Pane], remembered: &RememberedLayout) -> String {
    let mut contents = String::from("# fgdb layout v9\n");
    remembered.column_widths.write(&mut contents);

    if let Some(window) = remembered.window {
        writeln!(
            contents,
            "window={},{},{}",
            window.size.width,
            window.size.height,
            u8::from(window.maximized)
        )
        .expect("writing to a String cannot fail");
    }

    if let Some(visible) = remembered.terminal_visible {
        writeln!(contents, "{TERMINAL_VISIBLE_KEY}={}", u8::from(visible))
            .expect("writing to a String cannot fail");
    }

    if let Some(view) = remembered.console_view {
        writeln!(contents, "{CONSOLE_VIEW_KEY}={}", view.name())
            .expect("writing to a String cannot fail");
    }

    for id in PanelId::ALL {
        if let Some(placement) = remembered.panels.get(&id) {
            let window = placement.geometry;

            writeln!(
                contents,
                "{PANEL_PREFIX}{}={},{},{},{}",
                id.key(),
                u8::from(placement.floating),
                window.size.width,
                window.size.height,
                u8::from(window.maximized)
            )
            .expect("writing to a String cannot fail");
        }
    }

    for pane in panes {
        let Some(position) = remembered.panes.get(pane.key) else {
            continue;
        };

        writeln!(
            contents,
            "{}={},{}",
            pane.key, position.position, position.extent
        )
        .expect("writing to a String cannot fail");
    }

    let mut notebooks = remembered.notebooks.iter().collect::<Vec<_>>();
    notebooks.sort_unstable_by_key(|(key, _)| *key);

    for (key, page) in notebooks {
        match page {
            SavedPage::Panel(id) => writeln!(contents, "{NOTEBOOK_PREFIX}{key}={}", id.key()),
            SavedPage::Legacy(index) => writeln!(contents, "{NOTEBOOK_PREFIX}{key}={index}"),
        }
        .expect("writing to a String cannot fail");
    }

    let mut disclosures = remembered.disclosures.iter().collect::<Vec<_>>();
    disclosures.sort_unstable_by_key(|(key, _)| *key);

    for (key, expanded) in disclosures {
        writeln!(contents, "{DISCLOSURE_PREFIX}{key}={}", u8::from(*expanded))
            .expect("writing to a String cannot fail");
    }

    let mut stacks = remembered.stacks.iter().collect::<Vec<_>>();
    stacks.sort_unstable_by_key(|(key, _)| *key);

    for (key, page) in stacks {
        writeln!(contents, "{STACK_PREFIX}{key}={page}").expect("writing to a String cannot fail");
    }

    contents
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_layout_entries_and_ignores_malformed_ones() {
        for (index, panel) in PanelId::ALL.into_iter().enumerate() {
            assert_eq!(panel as usize, index);
            assert_eq!(PanelId::from_key(panel.key()), Some(panel));
        }

        let parsed = parse_layout(
            "# layout\nwindow=1440,900,1\nterminal.visible=0\nworkspace_inspector=980,1375\nnotebook.left_sidebar=4\nnotebook.invalid=2048\ndisclosure.kernel.overview.process=1\ndisclosure.kernel.overview.scheduler=0\ndisclosure.invalid=maybe\nbroken=nope\nnegative=-1,100\ncollapsed=100,100\nzero=0,100\noversized=1507950899,2147483647\n",
        );

        assert_eq!(
            parsed.panes.get("workspace_inspector"),
            Some(&PanePosition {
                position: 980,
                extent: 1375,
            })
        );

        assert_eq!(
            parsed.window,
            Some(WindowGeometry {
                size: WindowSize {
                    width: 1440,
                    height: 900,
                },
                maximized: true,
            })
        );

        assert_eq!(parsed.terminal_visible, Some(false));
        assert_eq!(
            parsed.notebooks.get("left_sidebar"),
            Some(&SavedPage::Legacy(4))
        );
        assert!(!parsed.notebooks.contains_key("invalid"));
        assert!(!parsed.panes.contains_key("broken"));
        assert!(!parsed.panes.contains_key("negative"));
        assert!(!parsed.panes.contains_key("collapsed"));
        assert!(!parsed.panes.contains_key("zero"));
        assert!(!parsed.panes.contains_key("oversized"));

        assert_eq!(
            parsed.disclosures.get("kernel.overview.process"),
            Some(&true)
        );

        assert_eq!(
            parsed.disclosures.get("kernel.overview.scheduler"),
            Some(&false)
        );

        assert!(!parsed.disclosures.contains_key("invalid"));

        let named = parse_layout("notebook.left_sidebar=threads\nnotebook.future=unknown-panel\n");

        assert_eq!(
            named.notebooks.get("left_sidebar"),
            Some(&SavedPage::Panel(PanelId::Threads))
        );

        assert!(!named.notebooks.contains_key("future"));

        let panels = parse_layout(
            "panel.right-pane=1,960,700,1\npanel.registers=0,640,480,0\npanel.console=1,800,300,0\nconsole.view=hidden\nstack.misc=cfg\nnotebook.inspector=misc\npanel.unknown=1,800,600,0\npanel.stack=1,1,1,0\npanel.memory=maybe,800,600,0\npanel.threads=1,800,600,0,extra\n",
        );

        assert_eq!(panels.panels.len(), 3);
        let right = panels.panels[&PanelId::RightPane];
        assert!(right.floating && right.geometry.maximized);
        assert!(!panels.panels[&PanelId::Registers].floating);
        assert_eq!(panels.console_view(), ConsoleView::Hidden);
        assert_eq!(panels.stacks.get("misc").map(String::as_str), Some("cfg"));
        assert_eq!(parse_layout(&serialize_layout(&[], &panels)), panels);
    }

    #[test]
    fn rejects_implausible_window_sizes_and_invalid_states() {
        assert_eq!(parse_layout("window=10,10,0\n").window, None);
        assert_eq!(parse_layout("window=1200,800,maybe\n").window, None);

        assert_eq!(
            parse_layout("terminal.visible=maybe\n").terminal_visible,
            None
        );
        assert_eq!(parse_layout("console.view=invalid\n").console_view, None);
        assert_eq!(parse_layout("").console_view(), ConsoleView::Terminal);
        assert_eq!(
            parse_layout("terminal.visible=0\n").console_view(),
            ConsoleView::Hidden
        );
        for view in [ConsoleView::Hidden, ConsoleView::Terminal, ConsoleView::Log] {
            let text = format!("console.view={}\nterminal.visible=0\n", view.name());
            assert_eq!(parse_layout(&text).console_view(), view);
        }
    }

    #[test]
    fn scales_and_clamps_remembered_positions() {
        let saved = PanePosition {
            position: 300,
            extent: 1_000,
        };

        assert_eq!(scale_position(saved, 0, 2_000), 600);
        assert_eq!(scale_position(saved, 700, 2_000), 700);
        assert_eq!(fractional_position(0.5, 1_000, 0, 900), 500);
        assert_eq!(fractional_position(0.5, 1_000, 600, 900), 600);
    }

    #[test]
    fn final_layout_write_retires_older_work_and_preserves_the_file_on_failure() {
        let temporary =
            glib::mkdtemp(std::env::temp_dir().join("fgdb-layout-writer-XXXXXX")).unwrap();
        let path = temporary.join("layout.conf");
        let writer = writer::Writer::new(path.clone());
        assert_eq!(writer.initialize().unwrap(), writer::Outcome::Saved);
        let secondary = writer::Writer::new(path.clone());
        assert_eq!(secondary.initialize().unwrap(), writer::Outcome::ReadOnly);
        writer.write("panel.stack=1,640,480,0\n").unwrap();
        assert_eq!(
            secondary.write("stale instance").unwrap(),
            writer::Outcome::ReadOnly
        );
        writer.finish("panel.stack=0,640,480,0\n").unwrap();
        writer.write("stale snapshot").unwrap();
        assert_eq!(
            secondary.finish("stale final save").unwrap(),
            writer::Outcome::ReadOnly
        );

        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "panel.stack=0,640,480,0\n"
        );
        let invalid = writer::Writer::new(path.join("layout.conf"));
        assert!(invalid.write("not a directory").is_err());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "panel.stack=0,640,480,0\n"
        );
        assert_eq!(std::fs::read_dir(&temporary).unwrap().count(), 2);
        let next = writer::Writer::new(path.clone());
        assert_eq!(next.initialize().unwrap(), writer::Outcome::Saved);
        drop(next);
        std::fs::remove_file(path.with_extension("lock")).unwrap();
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(temporary).unwrap();
    }
}
