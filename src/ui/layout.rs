use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet},
    fmt::Write as _,
    fs, io,
    path::{Path, PathBuf},
    rc::Rc,
    time::Duration,
};

use gtk::{glib, prelude::*};

use super::KernelSectionHandler;

const SAVE_DELAY: Duration = Duration::from_millis(350);
const MAPPED_PANE_RESTORE_DELAY: Duration = Duration::from_millis(100);
const MIN_WINDOW_WIDTH: i32 = 320;
const MIN_WINDOW_HEIGHT: i32 = 200;
const MAX_WINDOW_DIMENSION: i32 = 32_768;
const DISCLOSURE_PREFIX: &str = "disclosure.";
const NOTEBOOK_PREFIX: &str = "notebook.";
const TERMINAL_VISIBLE_KEY: &str = "terminal.visible";
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
    pub(super) fn install(window: &gtk::ApplicationWindow, panes: Vec<Pane>) -> Self {
        let path = layout_path();

        let remembered = crate::bounded::read_string(&path, MAX_LAYOUT_BYTES)
            .map(|contents| parse_layout(&contents))
            .unwrap_or_default();

        let normal_window_size = remembered
            .window
            .map(|geometry| geometry.size)
            .unwrap_or_else(|| WindowSize {
                width: window.default_width(),
                height: window.default_height(),
            });

        if let Some(geometry) = remembered.window {
            window.set_default_size(geometry.size.width, geometry.size.height);

            if geometry.maximized {
                window.maximize();
            }
        }

        let state = Rc::new(State {
            path,
            window: window.clone(),
            panes,
            remembered: RefCell::new(remembered),
            normal_window_size: Cell::new(normal_window_size),
            pending_save: RefCell::new(None),
            pending_pane_restores: RefCell::new(HashSet::new()),
            restore_started: Cell::new(false),
            restoring_position: Cell::new(false),
            surface_connected: Cell::new(false),
            ready_to_save: Cell::new(false),
        });

        for pane in &state.panes {
            let weak_state = Rc::downgrade(&state);
            let key = pane.key;

            pane.widget.connect_position_notify(move |widget| {
                let Some(state) = weak_state.upgrade() else {
                    return;
                };

                let restore_pending = state.pending_pane_restores.borrow().contains(key);

                if state.ready_to_save.get() && !state.restoring_position.get() && !restore_pending
                {
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

                if !state.ready_to_save.get() {
                    return;
                }

                state.pending_pane_restores.borrow_mut().insert(key);
                let widget = widget.clone();
                let weak_state = Rc::downgrade(&state);

                glib::timeout_add_local_once(MAPPED_PANE_RESTORE_DELAY, move || {
                    let Some(state) = weak_state.upgrade() else {
                        return;
                    };

                    if state.ready_to_save.get() && widget.is_mapped() {
                        state.restore_position(key, default_fraction, &widget);
                    }

                    state.pending_pane_restores.borrow_mut().remove(key);
                });
            });
        }

        let weak_state = Rc::downgrade(&state);

        window.connect_map(move |_| {
            if let Some(state) = weak_state.upgrade() {
                state.connect_surface_size_tracking();
                state.start_restore();
            }
        });

        let weak_state = Rc::downgrade(&state);

        window.connect_maximized_notify(move |_| {
            let Some(state) = weak_state.upgrade() else {
                return;
            };

            if state.ready_to_save.get() {
                state.schedule_save();
            }
        });

        Self(state)
    }

    pub(super) fn save(&self) {
        self.0.save_now();
    }

    pub(super) fn terminal_visible(&self) -> bool {
        self.0.remembered.borrow().terminal_visible.unwrap_or(true)
    }

    pub(super) fn set_terminal_visible(&self, visible: bool) {
        let changed = self.0.remembered.borrow().terminal_visible != Some(visible);

        if !changed {
            return;
        }

        self.0.remembered.borrow_mut().terminal_visible = Some(visible);

        if self.0.ready_to_save.get() {
            self.0.schedule_save();
        }
    }

    pub(super) fn bind_notebook(&self, key: &'static str, notebook: &gtk::Notebook) {
        let page = self.0.remembered.borrow().notebooks.get(key).copied();

        if let Some(page) = page.filter(|page| *page < notebook.n_pages()) {
            notebook.set_current_page(Some(page));
        }

        let weak_state = Rc::downgrade(&self.0);

        notebook.connect_switch_page(move |notebook, _, page| {
            let Some(state) = weak_state.upgrade() else {
                return;
            };

            if page >= notebook.n_pages() || page > MAX_NOTEBOOK_PAGE {
                return;
            }

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
}

struct State {
    path: PathBuf,
    window: gtk::ApplicationWindow,
    panes: Vec<Pane>,
    remembered: RefCell<RememberedLayout>,
    normal_window_size: Cell<WindowSize>,
    pending_save: RefCell<Option<glib::SourceId>>,
    pending_pane_restores: RefCell<HashSet<&'static str>>,
    restore_started: Cell<bool>,
    restoring_position: Cell<bool>,
    surface_connected: Cell<bool>,
    ready_to_save: Cell<bool>,
}

impl State {
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
        if self.restore_started.replace(true) {
            return;
        }

        let weak_state = Rc::downgrade(self);

        glib::idle_add_local_once(move || {
            let Some(state) = weak_state.upgrade() else {
                return;
            };

            state.restore_positions();

            // Restoring an outer split changes the allocation available to its
            // nested splits. A second pass after one frame gives those panes
            // their final proportional positions.
            let weak_state = Rc::downgrade(&state);

            glib::timeout_add_local_once(Duration::from_millis(16), move || {
                let Some(state) = weak_state.upgrade() else {
                    return;
                };

                state.restore_positions();
                state.ready_to_save.set(true);
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

        if !valid_pane_range(minimum, maximum) {
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
        if !widget.is_mapped() {
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

    fn connect_surface_size_tracking(self: &Rc<Self>) {
        if self.surface_connected.replace(true) {
            return;
        }

        let Some(surface) = self.window.surface() else {
            self.surface_connected.set(false);
            return;
        };

        let weak_state = Rc::downgrade(self);

        surface.connect_width_notify(move |surface| {
            if let Some(state) = weak_state.upgrade() {
                state.window_size_changed(surface.width(), surface.height());
            }
        });

        let weak_state = Rc::downgrade(self);

        surface.connect_height_notify(move |surface| {
            if let Some(state) = weak_state.upgrade() {
                state.window_size_changed(surface.width(), surface.height());
            }
        });
    }

    fn window_size_changed(self: &Rc<Self>, width: i32, height: i32) {
        if self.window.is_maximized() || !valid_window_size(width, height) {
            return;
        }

        self.normal_window_size.set(WindowSize { width, height });

        if self.ready_to_save.get() {
            self.schedule_save();
        }
    }

    fn schedule_save(self: &Rc<Self>) {
        if let Some(source) = self.pending_save.borrow_mut().take() {
            source.remove();
        }

        let weak_state = Rc::downgrade(self);

        let source = glib::timeout_add_local_once(SAVE_DELAY, move || {
            let Some(state) = weak_state.upgrade() else {
                return;
            };

            state.pending_save.borrow_mut().take();
            state.write_current_layout();
        });

        self.pending_save.replace(Some(source));
    }

    fn save_now(&self) {
        if let Some(source) = self.pending_save.borrow_mut().take() {
            source.remove();
        }

        self.write_current_layout();
    }

    fn write_current_layout(&self) {
        let mut remembered = self.remembered.borrow().clone();

        if !self.window.is_maximized() {
            let size = WindowSize {
                width: self.window.width(),
                height: self.window.height(),
            };

            if valid_window_size(size.width, size.height) {
                self.normal_window_size.set(size);
            }
        }

        remembered.window = Some(WindowGeometry {
            size: self.normal_window_size.get(),
            maximized: self.window.is_maximized(),
        });

        for pane in &self.panes {
            if !pane.widget.is_mapped() {
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

        if write_layout(&self.path, &self.panes, &remembered).is_ok() {
            *self.remembered.borrow_mut() = remembered;
        }
    }
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
struct WindowGeometry {
    size: WindowSize,
    maximized: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct RememberedLayout {
    window: Option<WindowGeometry>,
    terminal_visible: Option<bool>,
    panes: HashMap<String, PanePosition>,
    notebooks: HashMap<String, u32>,
    disclosures: HashMap<String, bool>,
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

        if key.trim() == "window" {
            let values = geometry.split(',').map(str::trim).collect::<Vec<_>>();

            if let [width, height, maximized] = values.as_slice()
                && let (Ok(width), Ok(height), Some(maximized)) = (
                    width.parse::<i32>(),
                    height.parse::<i32>(),
                    parse_bool(maximized),
                )
                && valid_window_size(width, height)
            {
                remembered.window = Some(WindowGeometry {
                    size: WindowSize { width, height },
                    maximized,
                });
            }

            continue;
        }

        if key.trim() == TERMINAL_VISIBLE_KEY {
            remembered.terminal_visible = parse_bool(geometry.trim());
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
            if !key.is_empty()
                && let Ok(page) = geometry.trim().parse::<u32>()
                && page <= MAX_NOTEBOOK_PAGE
            {
                remembered.notebooks.insert(key.to_owned(), page);
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

fn write_layout(path: &Path, panes: &[Pane], remembered: &RememberedLayout) -> io::Result<()> {
    let mut contents = String::from("# fgdb layout v5\n");

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
        writeln!(contents, "{NOTEBOOK_PREFIX}{key}={page}")
            .expect("writing to a String cannot fail");
    }

    let mut disclosures = remembered.disclosures.iter().collect::<Vec<_>>();
    disclosures.sort_unstable_by_key(|(key, _)| *key);

    for (key, expanded) in disclosures {
        writeln!(contents, "{DISCLOSURE_PREFIX}{key}={}", u8::from(*expanded))
            .expect("writing to a String cannot fail");
    }

    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "the layout path does not have a parent directory",
        )
    })?;

    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(".layout.{}.tmp", std::process::id()));
    fs::write(&temporary, contents)?;

    fs::rename(temporary, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_layout_entries_and_ignores_malformed_ones() {
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
        assert_eq!(parsed.notebooks.get("left_sidebar"), Some(&4));
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
    }

    #[test]
    fn rejects_implausible_window_sizes_and_invalid_states() {
        assert_eq!(parse_layout("window=10,10,0\n").window, None);
        assert_eq!(parse_layout("window=1200,800,maybe\n").window, None);

        assert_eq!(
            parse_layout("terminal.visible=maybe\n").terminal_visible,
            None
        );
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
}
