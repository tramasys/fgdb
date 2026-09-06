use gtk::gdk;
use vte4::prelude::*;

#[derive(Clone, Debug)]
pub struct Theme {
    pub source_scheme: &'static str,
    pub colors: Colors,
}

#[derive(Clone, Debug)]
pub struct Colors {
    pub background: &'static str,
    pub surface: &'static str,
    pub raised: &'static str,
    pub border: &'static str,
    pub foreground: &'static str,
    pub muted: &'static str,
    pub accent: &'static str,
    pub accent_hover: &'static str,
    pub success: &'static str,
    pub warning: &'static str,
    pub danger: &'static str,
    pub terminal_background: &'static str,
}

impl Theme {
    pub const fn graphite() -> Self {
        Self {
            source_scheme: "carbon",
            colors: Colors {
                background: "#000000",
                surface: "#0e0e0e",
                raised: "#202020",
                border: "#2b2b2b",
                foreground: "#dedede",
                muted: "#8f8f8f",
                accent: "#84a9ce",
                accent_hover: "#a8c8e8",
                success: "#88b89a",
                warning: "#d0b56f",
                danger: "#cf7777",
                terminal_background: "#000000",
            },
        }
    }

    pub fn install(&self) {
        let provider = gtk::CssProvider::new();
        provider.load_from_string(&self.stylesheet());

        if let Some(display) = gdk::Display::default() {
            gtk::style_context_add_provider_for_display(
                &display,
                &provider,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        }
    }

    pub fn style_terminal(&self, terminal: &vte4::Terminal) {
        let foreground = rgba(self.colors.foreground);
        let background = rgba(self.colors.terminal_background);

        let palette = [
            rgba("#111111"),
            rgba("#b86f6f"),
            rgba("#7dac8d"),
            rgba("#b8a06c"),
            rgba("#7f91ad"),
            rgba("#a17fa8"),
            rgba("#739da3"),
            rgba("#c5c5c5"),
            rgba("#5d5d5d"),
            rgba("#d18484"),
            rgba("#93c3a4"),
            rgba("#d0b77c"),
            rgba("#96a8c3"),
            rgba("#b996c0"),
            rgba("#8bb5bb"),
            rgba("#eeeeee"),
        ];

        let palette_refs = palette.each_ref();
        terminal.set_colors(Some(&foreground), Some(&background), &palette_refs);
    }

    pub fn source_style_scheme(&self) -> Option<sourceview5::StyleScheme> {
        let manager = sourceview5::StyleSchemeManager::default();
        manager.prepend_search_path(&format!("resource://{}/themes", crate::RESOURCE_PREFIX));
        manager.force_rescan();

        manager
            .scheme(self.source_scheme)
            .or_else(|| manager.scheme("Adwaita-dark"))
    }

    fn stylesheet(&self) -> String {
        let colors = &self.colors;

        let palette = format!(
            "@define-color app_bg {};\n\
             @define-color app_surface {};\n\
             @define-color app_raised {};\n\
             @define-color app_border {};\n\
             @define-color app_fg {};\n\
             @define-color app_muted {};\n\
             @define-color app_accent {};\n\
             @define-color app_accent_hover {};\n\
             @define-color app_success {};\n\
             @define-color app_warning {};\n\
             @define-color app_danger {};\n",
            colors.background,
            colors.surface,
            colors.raised,
            colors.border,
            colors.foreground,
            colors.muted,
            colors.accent,
            colors.accent_hover,
            colors.success,
            colors.warning,
            colors.danger,
        );

        // Shared controls own interaction geometry. Panes only specialize their
        // layout and debugger data, and execution locks preserve those styles.
        [
            palette.as_str(),
            include_str!("theme/base.css"),
            include_str!("theme/controls.css"),
            include_str!("theme/panes.css"),
            include_str!("theme/typography.css"),
            include_str!("theme/execution.css"),
        ]
        .concat()
    }
}

fn rgba(value: &str) -> gdk::RGBA {
    gdk::RGBA::parse(value).expect("built-in theme colors must be valid")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, rc::Rc, time::Duration};

    #[test]
    #[ignore = "requires a GTK display"]
    fn shared_controls_parse_and_keep_geometry_across_states() {
        gtk::init().unwrap();
        let provider = gtk::CssProvider::new();
        let errors = Rc::new(RefCell::new(Vec::new()));
        let captured = Rc::clone(&errors);
        provider.connect_parsing_error(move |_, section, error| {
            captured.borrow_mut().push(format!("{section:?}: {error}"));
        });
        provider.load_from_string(&Theme::graphite().stylesheet());
        assert!(errors.borrow().is_empty(), "{:?}", errors.borrow());
        let display = gdk::Display::default().unwrap();
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );

        let root = gtk::Box::new(gtk::Orientation::Vertical, 6);
        let mut controls = Vec::new();
        for (container_class, control_class) in [
            ("thread-workspace", ""),
            ("inferior-policy", "inferior-policy-choice"),
            ("debug-data-window", "inline-action"),
            ("sidebar", "catchpoint-action"),
            ("cfg-page", "cfg-follow"),
            ("value-editor", "primary-control"),
        ] {
            let container = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            container.add_css_class(container_class);
            let button = gtk::ToggleButton::with_label("Action");
            if !control_class.is_empty() {
                button.add_css_class(control_class);
            }

            container.append(&button);
            root.append(&container);
            controls.push(button);
        }

        let entry = gtk::Entry::new();
        root.append(&entry);
        let dropdown = gtk::DropDown::from_strings(&["Parent", "Child"]);
        root.append(&dropdown);
        let checkbox = gtk::CheckButton::with_label("Enabled");
        root.append(&checkbox);
        let switcher = gtk::StackSwitcher::new();
        switcher.add_css_class("allocator-view-tabs");
        let stack = gtk::Stack::new();
        stack.add_titled(&gtk::Label::new(Some("First")), Some("first"), "First");
        stack.add_titled(&gtk::Label::new(Some("Second")), Some("second"), "Second");
        switcher.set_stack(Some(&stack));
        root.append(&switcher);
        root.append(&stack);

        let cells = Rc::new(RefCell::new(Vec::<gtk::Label>::new()));
        let captured_cells = Rc::clone(&cells);
        let factory = gtk::SignalListItemFactory::new();
        factory.connect_setup(move |_, object| {
            let item = object.downcast_ref::<gtk::ListItem>().unwrap();
            let label = gtk::Label::new(Some("Value"));
            label.add_css_class("debug-table-cell");
            item.set_child(Some(&label));
            captured_cells.borrow_mut().push(label);
        });
        let table =
            gtk::ColumnView::new(Some(gtk::NoSelection::new(Some(gtk::StringList::new(&[
                "first", "second",
            ])))));
        table.add_css_class("debug-table");
        table.append_column(&gtk::ColumnViewColumn::new(Some("VALUE"), Some(factory)));
        root.append(&table);
        let window = gtk::Window::builder().child(&root).build();
        window.present();

        let settle = || {
            let context = gtk::glib::MainContext::default();
            for _ in 0..8 {
                while context.pending() {
                    context.iteration(false);
                }
                std::thread::sleep(Duration::from_millis(8));
            }
        };
        let geometry = |widget: &gtk::Widget| {
            (
                widget.measure(gtk::Orientation::Horizontal, -1),
                widget.measure(gtk::Orientation::Vertical, -1),
            )
        };
        settle();
        assert_eq!(cells.borrow().len(), 2);
        for cell in cells.borrow().iter() {
            assert_eq!(cell.color(), rgba(Theme::graphite().colors.foreground));
            let parent = cell.parent().unwrap();
            assert!(
                parent.height() <= 30,
                "Table cell height: {}",
                parent.height()
            );
        }

        let expected = geometry(controls[0].upcast_ref());
        let tabs_before = geometry(switcher.upcast_ref());
        stack.set_visible_child_name("second");
        settle();
        assert_eq!(geometry(switcher.upcast_ref()), tabs_before);

        for button in &controls {
            assert_eq!(
                geometry(button.upcast_ref()),
                expected,
                "{:?}",
                button.css_classes()
            );
            button.set_state_flags(gtk::StateFlags::PRELIGHT, false);
            settle();
            assert_eq!(geometry(button.upcast_ref()), expected);
            button.set_active(true);
            settle();
            assert_eq!(geometry(button.upcast_ref()), expected);
            button.set_sensitive(false);
            settle();
            assert_eq!(geometry(button.upcast_ref()), expected);
        }

        assert_eq!(
            entry.measure(gtk::Orientation::Vertical, -1).0,
            expected.1.0
        );
        assert_eq!(
            dropdown.measure(gtk::Orientation::Vertical, -1).0,
            expected.1.0
        );

        let input_colors = (entry.color(), checkbox.color());
        entry.add_css_class("execution-interlocked");
        entry.set_sensitive(false);
        checkbox.add_css_class("execution-interlocked");
        checkbox.set_sensitive(false);
        settle();
        assert_eq!((entry.color(), checkbox.color()), input_colors);
        assert!(errors.borrow().is_empty(), "{:?}", errors.borrow());
        window.close();
        gtk::style_context_remove_provider_for_display(&display, &provider);
    }
}
