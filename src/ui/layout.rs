use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    fmt::Write as _,
    fs, io,
    path::{Path, PathBuf},
    rc::Rc,
    time::Duration,
};

use gtk::{glib, prelude::*};

const SAVE_DELAY: Duration = Duration::from_millis(350);
const MIN_WINDOW_WIDTH: i32 = 320;
const MIN_WINDOW_HEIGHT: i32 = 200;
const MAX_WINDOW_DIMENSION: i32 = 32_768;

#[derive(Clone)]
pub(super) struct Pane {
    key: &'static str,
    widget: gtk::Paned,
}

impl Pane {
    pub(super) fn new(key: &'static str, widget: &gtk::Paned) -> Self {
        Self {
            key,
            widget: widget.clone(),
        }
    }
}

#[derive(Clone)]
pub(super) struct Persistence(Rc<State>);

impl Persistence {
    pub(super) fn install(window: &gtk::ApplicationWindow, panes: Vec<Pane>) -> Self {
        let path = glib::user_config_dir().join("fgdb/layout.conf");
        let remembered = fs::read_to_string(&path)
            .map(|contents| parse_layout(&contents))
            .unwrap_or_default();
        let normal_window_size =
            remembered
                .window
                .map(|geometry| geometry.size)
                .unwrap_or(WindowSize {
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
            restore_started: Cell::new(false),
            surface_connected: Cell::new(false),
            ready_to_save: Cell::new(false),
        });

        for pane in &state.panes {
            let weak_state = Rc::downgrade(&state);
            pane.widget.connect_position_notify(move |_| {
                let Some(state) = weak_state.upgrade() else {
                    return;
                };
                if state.ready_to_save.get() {
                    state.schedule_save();
                }
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
}

struct State {
    path: PathBuf,
    window: gtk::ApplicationWindow,
    panes: Vec<Pane>,
    remembered: RefCell<RememberedLayout>,
    normal_window_size: Cell<WindowSize>,
    pending_save: RefCell<Option<glib::SourceId>>,
    restore_started: Cell<bool>,
    surface_connected: Cell<bool>,
    ready_to_save: Cell<bool>,
}

impl State {
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
        let remembered = self.remembered.borrow();
        for pane in &self.panes {
            let Some(saved) = remembered.panes.get(pane.key) else {
                continue;
            };
            let maximum = pane.widget.max_position();
            if maximum <= 0 {
                continue;
            }
            pane.widget
                .set_position(scale_position(*saved, pane.widget.min_position(), maximum));
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
            let maximum = pane.widget.max_position();
            if maximum > pane.widget.min_position() {
                remembered.panes.insert(
                    pane.key.to_owned(),
                    PanePosition {
                        position: pane.widget.position(),
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
    panes: HashMap<String, PanePosition>,
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
        let Some((position, extent)) = geometry.split_once(',') else {
            continue;
        };
        let (Ok(position), Ok(extent)) =
            (position.trim().parse::<i32>(), extent.trim().parse::<i32>())
        else {
            continue;
        };
        if position >= 0 && extent > 0 {
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
    let mut contents = String::from("# fgdb layout v2\n");
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

    let parent = path.parent().expect("the layout path has a parent");
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
            "# layout\nwindow=1440,900,1\nworkspace_inspector=980,1375\nbroken=nope\nnegative=-1,100\n",
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
        assert!(!parsed.panes.contains_key("broken"));
        assert!(!parsed.panes.contains_key("negative"));
    }

    #[test]
    fn rejects_implausible_window_sizes_and_invalid_states() {
        assert_eq!(parse_layout("window=10,10,0\n").window, None);
        assert_eq!(parse_layout("window=1200,800,maybe\n").window, None);
    }

    #[test]
    fn scales_and_clamps_remembered_positions() {
        let saved = PanePosition {
            position: 300,
            extent: 1_000,
        };

        assert_eq!(scale_position(saved, 0, 2_000), 600);
        assert_eq!(scale_position(saved, 700, 2_000), 700);
    }
}
