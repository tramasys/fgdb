//! Compact, read-only compiler layout using the shared table presentation.

use super::*;
use crate::debugger::type_layout::{LAYOUT_UNITS, LayoutRow, TypeLayout};

impl Ui {
    pub(crate) fn connect_type_layout(
        self: &Rc<Self>,
        inspect: impl Fn(Variable, glib::WeakRef<gtk::Box>) + 'static,
    ) {
        let weak = Rc::downgrade(self);

        self.variable_presentation
            .inspect
            .replace(Some(Rc::new(move |variable| {
                let Some(ui) = weak
                    .upgrade()
                    .filter(|ui| ui.variable_action_is_current(&variable))
                else {
                    return;
                };

                let (window, content) = layout_window(&ui.window, &variable.name);
                inspect(variable, content.downgrade());
                window.present();
            })));
    }

    pub(crate) fn show_type_layout(
        self: &Rc<Self>,
        generation: u64,
        content: &gtk::Box,
        result: Result<TypeLayout, String>,
    ) {
        clear_box(content);

        let result = if self.model.is_stop_refresh_current(generation) {
            result
        } else {
            Err("Stop changed before type inspection completed".into())
        };

        let layout = match result {
            Ok(layout) => layout,
            Err(error) => {
                Self::show_type_layout_error(content, &error);
                return;
            }
        };

        append_layout(content, &layout);
        let actions = append_actions(content);

        if let Some(address) = layout.address {
            let memory = gtk::Button::with_label("Inspect object storage");
            memory.set_tooltip_text(Some("Read object storage while this stop remains current"));
            self.track_inspection_action(&memory, generation);
            let weak = Rc::downgrade(self);
            let bytes = layout.bytes.clamp(1, 4096);

            memory.connect_clicked(move |_| {
                if let Some(ui) = weak.upgrade()
                    && ui.model.is_stop_refresh_current(generation)
                {
                    ui.inspect_address(address, bytes);
                }
            });

            actions.prepend(&memory);
        }

        let copy = gtk::Button::with_label("Copy layout");
        let text = layout.text();
        copy.connect_clicked(move |button| button.display().clipboard().set_text(&text));
        actions.prepend(&copy);
    }

    pub(crate) fn show_type_layout_error(content: &gtk::Box, error: &str) {
        clear_box(content);
        content.append(&stop_info::inspection_text(error));
        append_actions(content);
    }
}

fn layout_window(parent: &impl IsA<gtk::Window>, name: &str) -> (gtk::Window, gtk::Box) {
    let window = gtk::Window::builder()
        .title(format!("Type layout — {name}"))
        .transient_for(parent)
        .default_width(680)
        .destroy_with_parent(true)
        .build();

    window.add_css_class("value-editor");
    let content = gtk::Box::new(gtk::Orientation::Vertical, components::CONTENT_INSET);
    components::inset(&content, components::DIALOG_INSET);
    window.set_child(Some(&content));
    content.append(&empty_label("Reading compiler-described layout…"));
    append_actions(&content);
    connect_escape_to_close(&window);

    (window, content)
}

fn append_actions(content: &gtk::Box) -> gtk::Box {
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
    content.append(&actions);
    close.grab_focus();

    stop_info::fit_inspection_window(content);

    actions
}

fn append_layout(content: &gtk::Box, layout: &TypeLayout) {
    let body = gtk::Box::new(gtk::Orientation::Vertical, components::CONTENT_INSET);
    let mut table: Option<(gio::ListStore, gtk::ScrolledWindow)> = None;

    for row in &layout.rows {
        match row {
            LayoutRow::Type {
                name,
                size,
                alignment,
            } => {
                let summary = components::card();

                if alignment.is_none() {
                    summary.append(&section_title("POINTEE TYPE"));
                }

                let title = stop_info::inspection_text(name);
                title.add_css_class("local-type");
                title.set_lines(2);
                title.set_ellipsize(pango::EllipsizeMode::End);
                title.set_tooltip_text(Some(name));
                summary.append(&title);

                let detail = match alignment {
                    Some(alignment) => format!("Size {size} B  ·  Alignment {alignment} B"),
                    None => format!("Size {size} B"),
                };

                let detail = stop_info::inspection_text(&detail);
                detail.add_css_class("muted");
                summary.append(&detail);
                body.append(&summary);
                let (store, scroll) = field_table();
                scroll.set_visible(false);
                body.append(&scroll);
                table = Some((store, scroll));
            }
            LayoutRow::Field(cells) => {
                if let Some((store, scroll)) = &table {
                    store.append(&glib::BoxedAnyObject::new(cells.clone()));
                    scroll.set_visible(true);
                }
            }
            LayoutRow::Note(note) => body.append(&empty_label(note)),
        }
    }

    let note = stop_info::inspection_text(LAYOUT_UNITS);
    note.add_css_class("muted");
    body.append(&note);

    let scroll = gtk::ScrolledWindow::builder()
        .child(&body)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(true)
        .max_content_height(480)
        .vexpand(true)
        .build();

    content.append(&scroll);
}

fn field_table() -> (gio::ListStore, gtk::ScrolledWindow) {
    let store = gio::ListStore::new::<glib::BoxedAnyObject>();
    let view = components::column_view(gtk::NoSelection::new(Some(store.clone())));
    view.add_css_class("debug-table");

    for (index, (title, width)) in [("OFFSET", 72), ("SIZE", 72), ("FIELD", 190), ("TYPE", 260)]
        .into_iter()
        .enumerate()
    {
        let column = components::label_column(title, width, move |data, label| {
            let cells = data.borrow::<[String; 4]>();
            let text = &cells[index];
            clear_label_selection(label);
            label.set_text(text);
            label.set_tooltip_text(Some(text));
            label.set_halign(gtk::Align::Fill);
            label.set_xalign(if index < 2 { 1.0 } else { 0.0 });
            label.set_css_classes(&[
                "debug-table-cell",
                if matches!(cells[2].as_str(), "<padding>" | "<tail padding>") {
                    "muted"
                } else {
                    "local-value"
                },
            ]);
        });

        view.append_column(&column);
    }

    let scroll = gtk::ScrolledWindow::builder()
        .child(&view)
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .propagate_natural_height(true)
        .max_content_height(240)
        .vexpand(true)
        .build();

    (store, scroll)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires a GTK display, run separately from other GTK tests"]
    fn layout_dialog_fits_small_types_and_scrolls_large_types() {
        gtk::init().unwrap();
        Theme::graphite().install();
        let parent = gtk::Window::new();
        let (window, content) = layout_window(&parent, "floats");
        window.present();
        let main = glib::MainContext::default();
        main.block_on(glib::timeout_future(Duration::from_millis(100)));

        let first_table = |content: &gtk::Box| {
            content
                .first_child()
                .and_downcast::<gtk::ScrolledWindow>()
                .unwrap()
                .child()
                .and_downcast::<gtk::Viewport>()
                .unwrap()
                .child()
                .unwrap()
                .first_child()
                .unwrap()
                .next_sibling()
                .and_downcast::<gtk::ScrolledWindow>()
                .unwrap()
        };

        let layout = TypeLayout::parse(
            "type\tvolatile Floats\t16\t8\nfield\t0\t8\tx\tdouble\nfield\t8\t8\ty\tdouble",
            Some(0x1000),
            16,
        )
        .unwrap();

        clear_box(&content);
        append_layout(&content, &layout);
        let actions = append_actions(&content);
        actions.prepend(&gtk::Button::with_label("Inspect object storage"));
        actions.prepend(&gtk::Button::with_label("Copy layout"));
        main.block_on(glib::timeout_future(Duration::from_millis(100)));

        let table = first_table(&content);
        let view = table.child().and_downcast::<gtk::ColumnView>().unwrap();
        assert_eq!(view.columns().n_items(), 4);
        assert_eq!(view.model().unwrap().n_items(), 2);

        assert!(
            table.height() >= 70,
            "small table height {}",
            table.height()
        );

        assert!(
            window.height() < 320,
            "small window height {}",
            window.height()
        );

        if let Some(path) = std::env::var_os("FGDB_TYPE_LAYOUT_CAPTURE") {
            let paintable = gtk::WidgetPaintable::new(Some(&content));
            let snapshot = gtk::Snapshot::new();

            paintable.snapshot(
                &snapshot,
                f64::from(content.width()),
                f64::from(content.height()),
            );

            window
                .renderer()
                .unwrap()
                .render_texture(snapshot.to_node().unwrap(), None)
                .save_to_png(path)
                .unwrap();
        }

        let mut large = format!("type\tLargeRecord<{}>\t960\t8\n", "NestedType, ".repeat(70));

        for index in 0..120 {
            large.push_str(&format!("field\t{}\t8\tfield_{index}\tdouble\n", index * 8));
        }

        clear_box(&content);
        append_layout(&content, &TypeLayout::parse(&large, None, 960).unwrap());
        let actions = append_actions(&content);
        main.block_on(glib::timeout_future(Duration::from_millis(100)));

        let table = first_table(&content);
        assert!(table.vadjustment().upper() > table.vadjustment().page_size());

        assert!(
            window.height() <= 480,
            "large window height {}",
            window.height()
        );

        assert!(actions.is_mapped());
        large.push_str(&format!("note\t{}\n", "Layout depth limit. ".repeat(100)));
        clear_box(&content);
        append_layout(&content, &TypeLayout::parse(&large, None, 960).unwrap());
        append_actions(&content);
        main.block_on(glib::timeout_future(Duration::from_millis(100)));

        assert!(
            window.height() <= 560,
            "bounded window height {}",
            window.height()
        );

        Ui::show_type_layout_error(&content, "Layout unavailable");

        let close = content
            .last_child()
            .unwrap()
            .last_child()
            .and_downcast::<gtk::Button>()
            .unwrap();

        close.emit_clicked();
        assert!(!window.is_visible());
        parent.close();
    }
}
