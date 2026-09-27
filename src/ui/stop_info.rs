//! Compact stop evidence and on-demand history, separate from live variables.

use super::*;
use crate::model::stop_info::StopEntry;

type SignalHandler = Rc<dyn Fn(Rc<StopEntry>, glib::WeakRef<gtk::Box>)>;
type InspectionAction = (glib::WeakRef<gtk::Button>, u64);

#[derive(Clone)]
pub(super) struct StopInfoView {
    pub root: gtk::Box,
    summary: gtk::Label,
    details: gtk::Button,
    history: gtk::Button,
    comparison: gtk::Button,
    signal: Rc<RefCell<Option<SignalHandler>>>,
    rendered: Rc<Cell<Option<(u64, u64, bool)>>>,
    live_actions: Rc<RefCell<Vec<InspectionAction>>>,
}

impl StopInfoView {
    pub fn new() -> Self {
        let root = components::control_row();
        root.add_css_class("subpanel-header");
        let summary = gtk::Label::new(Some("No stop recorded"));
        summary.set_xalign(0.0);
        summary.set_hexpand(true);
        summary.set_ellipsize(pango::EllipsizeMode::End);
        let details = gtk::Button::with_label("Stop details");
        details.add_css_class("inline-action");
        details.set_sensitive(false);
        let history = gtk::Button::with_label("History");
        history.add_css_class("inline-action");
        let comparison = gtk::Button::with_label("Compare snapshots");
        let (popover, menu) = build_context_menu();
        menu.append(&history);
        menu.append(&comparison);

        let more = gtk::MenuButton::builder()
            .icon_name("view-more-symbolic")
            .tooltip_text("Stop history and pinned comparisons")
            .popover(&popover)
            .build();

        for button in [&history, &comparison] {
            let popover = popover.downgrade();

            button.connect_clicked(move |_| {
                if let Some(popover) = popover.upgrade() {
                    popover.popdown();
                }
            });
        }

        root.append(&summary);
        root.append(&details);
        root.append(&more);

        Self {
            root,
            summary,
            details,
            history,
            comparison,
            signal: Rc::default(),
            rendered: Rc::default(),
            live_actions: Rc::default(),
        }
    }
}

impl Ui {
    pub(crate) fn connect_stop_info(
        self: &Rc<Self>,
        signal: impl Fn(Rc<StopEntry>, glib::WeakRef<gtk::Box>) + 'static,
    ) {
        self.stop_info.signal.replace(Some(Rc::new(signal)));
        let weak = Rc::downgrade(self);

        self.stop_info.comparison.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.open_comparisons();
            }
        });

        let weak = Rc::downgrade(self);

        self.stop_info.details.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                let entry = ui.model.stop_info.borrow().current.clone();

                if let Some(entry) = entry {
                    ui.open_stop_details(entry);
                }
            }
        });

        let weak = Rc::downgrade(self);

        self.stop_info.history.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.open_stop_history();
            }
        });
    }

    pub(crate) fn render_stop_info(&self) {
        let history = self.model.stop_info.borrow();
        let key = history.current.as_ref().map(|entry| {
            (
                entry.epoch,
                entry.sequence,
                self.stop_entry_is_current(entry),
            )
        });

        self.stop_info
            .live_actions
            .borrow_mut()
            .retain(|(button, generation)| {
                let Some(button) = button.upgrade() else {
                    return false;
                };

                if !self.model.is_stop_refresh_current(*generation) {
                    button.set_sensitive(false);
                }

                true
            });

        if self.stop_info.rendered.replace(key) == key {
            return;
        }

        let summary = history.current.as_ref().map_or_else(
            || "No stop recorded".into(),
            |entry| {
                if key.is_some_and(|(_, _, current)| current) {
                    entry.summary()
                } else {
                    format!("Last stop · {}", entry.summary())
                }
            },
        );

        if self.stop_info.summary.text() != summary {
            self.stop_info.summary.set_text(&summary);
        }

        self.stop_info
            .details
            .set_sensitive(history.current.is_some());
    }

    pub(super) fn track_inspection_action(&self, button: &gtk::Button, generation: u64) {
        self.stop_info
            .live_actions
            .borrow_mut()
            .push((button.downgrade(), generation));
    }

    pub(crate) fn stop_entry_is_current(&self, entry: &StopEntry) -> bool {
        self.model
            .stop_info
            .borrow()
            .current
            .as_ref()
            .is_some_and(|current| {
                current.sequence == entry.sequence && current.epoch == entry.epoch
            })
            && self.model.observed_stop_sequence() == entry.sequence
            && self.model.current_thread_id() == entry.thread
            && self.model.selected_inferior_id() == entry.inferior
            && self
                .model
                .is_stop_refresh_current(self.model.current_stop_refresh_generation())
            && !self.model.inferior_is_running()
    }

    fn open_stop_details(self: &Rc<Self>, entry: Rc<StopEntry>) {
        let (window, content, actions) = inspection_window(&self.window, "Stop details");
        let body = components::card();
        let heading = inspection_text(&format!("Stop {} · {}", entry.sequence, entry.summary()));
        heading.add_css_class("field-label");
        body.append(&heading);
        body.append(&inspection_fields(entry.fields()));
        content.append(&inspection_scroll(&body));

        content.append(&empty_label(
            "Captured evidence · source navigation does not restore program state",
        ));

        self.append_stop_source(&actions, &entry);
        let copy = gtk::Button::with_label("Copy details");
        let text = entry.description();
        copy.connect_clicked(move |button| button.display().clipboard().set_text(&text));
        actions.prepend(&copy);

        if entry.signal.is_some() {
            let load = gtk::Button::with_label("Inspect signal");
            load.set_sensitive(self.stop_entry_is_current(&entry));
            self.track_inspection_action(&load, self.model.current_stop_refresh_generation());
            load.set_tooltip_text(Some(
                "Read signal information for this stop on demand. Select its stopped thread first.",
            ));

            actions.prepend(&load);
            let details = gtk::Box::new(gtk::Orientation::Vertical, components::CONTROL_GAP);
            body.append(&details);
            let weak = Rc::downgrade(self);
            let target = details.downgrade();

            load.connect_clicked(move |button| {
                let Some(ui) = weak.upgrade() else { return };

                if !ui.stop_entry_is_current(&entry) {
                    if let Some(details) = target.upgrade() {
                        clear_box(&details);
                        details.append(&empty_label(
                            "This stop is no longer current. Signal details were not read.",
                        ));

                        fit_inspection_window(&details);
                    }

                    return;
                }

                let handler = ui.stop_info.signal.borrow().clone();

                if let Some(handler) = handler {
                    button.set_sensitive(false);
                    handler(Rc::clone(&entry), target.clone());
                }
            });
        }

        window.present();
    }

    fn append_stop_source(self: &Rc<Self>, actions: &gtk::Box, entry: &StopEntry) {
        if let (Some(path), Some(line)) = (&entry.details.source, entry.details.line) {
            let button = gtk::Button::with_label("Open source");
            let path = PathBuf::from(path);
            let weak = Rc::downgrade(self);

            button.connect_clicked(move |_| {
                if let Some(ui) = weak.upgrade() {
                    ui.navigate_to_source(&path, line, true);
                }
            });

            actions.prepend(&button);
        }
    }

    fn open_stop_history(self: &Rc<Self>) {
        let (window, content, actions) = inspection_window(&self.window, "Stop history");

        content.append(&empty_label(
            "Last 64 significant stops. Entries retain evidence, not executable snapshots.",
        ));

        let controls = components::control_row();
        let include = gtk::CheckButton::with_label("Include future steps");
        include.set_hexpand(true);
        include.set_active(self.model.stop_info.borrow().include_steps);
        let model = Rc::downgrade(&self.model);

        include.connect_toggled(move |button| {
            if let Some(model) = model.upgrade() {
                model.stop_info.borrow_mut().include_steps = button.is_active();
            }
        });

        let refresh = gtk::Button::with_label("Refresh");
        let clear = gtk::Button::with_label("Clear");
        controls.append(&include);
        actions.prepend(&clear);
        actions.prepend(&refresh);
        content.append(&controls);
        let list = gtk::Box::new(gtk::Orientation::Vertical, components::CONTROL_GAP);
        content.append(&inspection_scroll(&list));
        self.render_stop_history(&list);

        for (button, erase) in [(refresh, false), (clear, true)] {
            let weak = Rc::downgrade(self);
            let list = list.downgrade();

            button.connect_clicked(move |_| {
                if let (Some(ui), Some(list)) = (weak.upgrade(), list.upgrade()) {
                    if erase {
                        ui.model.stop_info.borrow_mut().entries.clear();
                    }

                    ui.render_stop_history(&list);
                }
            });
        }

        window.present();
    }

    fn render_stop_history(self: &Rc<Self>, list: &gtk::Box) {
        clear_box(list);
        let entries: Vec<_> = self
            .model
            .stop_info
            .borrow()
            .entries
            .iter()
            .rev()
            .cloned()
            .collect();

        if entries.is_empty() {
            list.append(&empty_label("No retained stops"));
        }

        for (index, entry) in entries.into_iter().enumerate() {
            let body = components::card();
            let weak = Rc::downgrade(self);
            let title = format!("Stop {} · {}", entry.sequence, entry.summary());

            body.connect_map(move |body| {
                if body.first_child().is_none() {
                    body.append(&inspection_fields(entry.fields()));

                    if let Some(ui) = weak.upgrade() {
                        let actions = components::control_row();
                        ui.append_stop_source(&actions, &entry);
                        body.append(&actions);
                    }

                    fit_inspection_window(body);
                }
            });

            let (row, revealed) =
                build_disclosure_with_content(&title, &body, index == 0, "stop-history-entry");

            revealed.connect_visible_notify(fit_inspection_window);
            list.append(&row);
        }

        fit_inspection_window(list);
    }

    pub(crate) fn show_stop_signal(
        self: &Rc<Self>,
        entry: Rc<StopEntry>,
        target: &gtk::Box,
        result: Result<(i64, Option<u64>), String>,
    ) {
        clear_box(target);

        if !self.stop_entry_is_current(&entry) {
            target.append(&empty_label(
                "Stop changed before signal inspection completed",
            ));

            fit_inspection_window(target);
            return;
        }

        let (code, address) = match result {
            Ok(value) => value,
            Err(error) => {
                target.append(&inspection_text(&error));
                fit_inspection_window(target);
                return;
            }
        };

        target.append(&section_title("SIGNAL INFORMATION"));
        let mut fields = vec![("Signal code", code.to_string())];
        let actions = components::control_row();

        if let Some(address) = address {
            let generation = self.model.current_stop_refresh_generation();
            let mapping = if self.model.memory_regions_are_current(generation) {
                memory_region_for_address(&self.model.memory_regions(), address)
                    .map(MemoryRegion::description)
                    .unwrap_or_else(|| "Not in the current mapping snapshot".into())
            } else {
                "Mapping information unavailable for this stop".into()
            };

            fields.push(("Fault address", format!("0x{address:x}")));
            fields.push(("Mapping", mapping));
            let memory = gtk::Button::with_label("Inspect fault address");
            self.track_inspection_action(&memory, generation);
            let weak = Rc::downgrade(self);
            let captured = Rc::clone(&entry);

            memory.connect_clicked(move |_| {
                if let Some(ui) = weak.upgrade()
                    && ui.stop_entry_is_current(&captured)
                {
                    ui.inspect_address(address, 256);
                }
            });

            actions.append(&memory);
        } else {
            fields.push((
                "Fault address",
                "Unavailable or not applicable to this signal".into(),
            ));
        }

        target.append(&inspection_fields(fields));

        if let Some(address) = entry
            .address
            .as_deref()
            .and_then(crate::debugger::context::pointer_address)
        {
            let instruction = gtk::Button::with_label("Inspect stopped instruction");
            self.track_inspection_action(
                &instruction,
                self.model.current_stop_refresh_generation(),
            );
            let weak = Rc::downgrade(self);

            instruction.connect_clicked(move |_| {
                if let Some(ui) = weak.upgrade()
                    && ui.stop_entry_is_current(&entry)
                {
                    ui.panels.reveal(PanelId::Context);
                    ui.request_disassembly_for_stop(format!("0x{address:x}"), None);
                }
            });

            actions.append(&instruction);
        }

        target.append(&actions);
        fit_inspection_window(target);
    }

    pub(crate) fn inspect_address(&self, address: u64, bytes: usize) {
        if add_memory_watch(
            &self.memory_watch_container,
            &self.memory_watches,
            &self.memory_watch_handler,
            format!("0x{address:x}"),
            bytes,
            MemoryWatchFormat::Bytes,
        ) {
            self.panels.reveal(PanelId::Memory);
            self.memory_search.show_inspector();
        } else {
            self.set_status(
                "Memory inspector",
                "Close an inspector before adding another (limit 256)",
                None,
            );
        }
    }
}

pub(super) fn inspection_window(
    parent: &impl IsA<gtk::Window>,
    title: &str,
) -> (gtk::Window, gtk::Box, gtk::Box) {
    let window = gtk::Window::builder()
        .title(title)
        .transient_for(parent)
        .default_width(720)
        .destroy_with_parent(true)
        .build();

    window.add_css_class("value-editor");
    let root = gtk::Box::new(gtk::Orientation::Vertical, components::CONTENT_INSET);
    components::inset(&root, components::DIALOG_INSET);
    let content = gtk::Box::new(gtk::Orientation::Vertical, components::CONTROL_GAP);
    content.set_vexpand(true);
    root.append(&content);
    let actions = components::control_row();
    let close = gtk::Button::with_label("Close");
    close.set_hexpand(true);
    close.set_halign(gtk::Align::End);

    close.connect_clicked(|button| {
        if let Some(window) = button.root().and_downcast::<gtk::Window>() {
            window.close();
        }
    });

    actions.append(&close);
    root.append(&actions);
    window.set_child(Some(&root));
    gtk::prelude::GtkWindowExt::set_focus(&window, Some(&close));
    connect_escape_to_close(&window);

    (window, content, actions)
}

pub(super) fn inspection_scroll(child: &impl IsA<gtk::Widget>) -> gtk::ScrolledWindow {
    gtk::ScrolledWindow::builder()
        .child(child)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(true)
        .max_content_height(400)
        .vexpand(true)
        .build()
}

pub(super) fn fit_inspection_window(widget: &impl IsA<gtk::Widget>) {
    if let Some(window) = widget.root().and_downcast::<gtk::Window>()
        && let Some(content) = window.child()
    {
        let width = if window.width() > 0 {
            window.width()
        } else {
            window.default_width()
        };

        window.set_default_height(content.measure(gtk::Orientation::Vertical, width).1);
    }
}

fn inspection_fields(fields: impl IntoIterator<Item = (&'static str, String)>) -> gtk::Grid {
    let grid = gtk::Grid::builder()
        .column_spacing(components::DIALOG_INSET)
        .row_spacing(components::CONTROL_GAP)
        .build();

    for (row, (name, value)) in fields.into_iter().enumerate() {
        let label = gtk::Label::new(Some(name));
        label.add_css_class("muted");
        label.set_xalign(0.0);
        label.set_valign(gtk::Align::Start);
        let value = inspection_text(&value);
        value.set_hexpand(true);
        value.set_max_width_chars(60);
        grid.attach(&label, 0, row as i32, 1, 1);
        grid.attach(&value, 1, row as i32, 1, 1);
    }

    grid
}

pub(super) fn inspection_text(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .selectable(true)
        .wrap(true)
        .wrap_mode(pango::WrapMode::WordChar)
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires a GTK display, run separately from other GTK tests"]
    fn inspection_dialogs_fit_details_and_bound_history() {
        gtk::init().unwrap();
        Theme::graphite().install();
        let parent = gtk::Window::new();
        let (window, content, actions) = inspection_window(&parent, "Stop details");
        let body = components::card();

        body.append(&inspection_fields([
            ("Function", "main".into()),
            ("Instruction", "0x1234".into()),
            ("Source", "/src/example.c:12".into()),
        ]));

        let scroll = inspection_scroll(&body);
        content.append(&scroll);
        window.present();
        let main = glib::MainContext::default();
        main.block_on(glib::timeout_future(Duration::from_millis(100)));
        assert!(window.height() < 260, "compact height {}", window.height());
        assert!(actions.is_mapped());
        clear_box(&body);
        let fields = inspection_fields([("Expression", "long value ".repeat(500))]);

        let (row, revealed) = build_disclosure_with_content(
            &"long reason ".repeat(200),
            &fields,
            true,
            "stop-history-entry",
        );

        body.append(&row);
        revealed.connect_visible_notify(fit_inspection_window);
        fit_inspection_window(&body);
        main.block_on(glib::timeout_future(Duration::from_millis(100)));
        assert!(scroll.vadjustment().upper() > scroll.vadjustment().page_size());
        assert!(window.height() <= 500, "bounded height {}", window.height());
        assert!(window.width() <= 740, "bounded width {}", window.width());
        let header = row.first_child().and_downcast::<gtk::Button>().unwrap();
        header.emit_clicked();
        main.block_on(glib::timeout_future(Duration::from_millis(100)));
        assert!(!revealed.is_visible());

        assert!(
            window.height() < 200,
            "collapsed height {}",
            window.height()
        );

        actions
            .last_child()
            .and_downcast::<gtk::Button>()
            .unwrap()
            .emit_clicked();

        assert!(!window.is_visible());
        parent.close();

        let released = {
            let row = build_disclosure(
                "History entry",
                &gtk::Label::new(None),
                false,
                "stop-history-entry",
            );

            row.first_child().unwrap().downgrade()
        };

        assert!(
            released.upgrade().is_none(),
            "disclosure must not retain its own button"
        );
    }

    #[test]
    #[ignore = "requires a GTK display, run separately from other GTK tests"]
    fn stop_menu_exposes_history_and_comparison_actions() {
        gtk::init().unwrap();
        let view = StopInfoView::new();
        let window = gtk::Window::builder()
            .child(&view.root)
            .default_width(700)
            .build();
        window.present();
        let context = glib::MainContext::default();
        context.block_on(glib::timeout_future(Duration::from_millis(50)));
        let menu = view
            .root
            .last_child()
            .and_downcast::<gtk::MenuButton>()
            .unwrap();
        menu.popup();
        context.block_on(glib::timeout_future(Duration::from_millis(50)));
        assert!(menu.popover().unwrap().is_visible());
        assert!(view.history.is_mapped());
        assert!(view.comparison.is_mapped());
        view.history.emit_clicked();
        assert!(!menu.popover().unwrap().is_visible());
        window.close();
    }
}
