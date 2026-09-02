use super::*;

impl Ui {
    pub fn connect_configuration_actions(self: &Rc<Self>) {
        let weak_ui = Rc::downgrade(self);

        self.configuration_button.connect_clicked(move |_| {
            let Some(ui) = weak_ui.upgrade() else {
                return;
            };

            ui.session_popover.popdown();
            ui.present_configuration_diagnostics();
        });
    }

    pub(crate) fn has_configuration_issues(&self) -> bool {
        !self.configuration_report.issues().is_empty()
    }

    pub(crate) fn present_configuration_diagnostics(&self) {
        if let Some(dialog) = self.configuration_dialog.borrow().as_ref() {
            dialog.present();
            return;
        }

        let dialog = gtk::Window::builder()
            .title("fgdb configuration")
            .transient_for(&self.window)
            .modal(false)
            .default_width(780)
            .default_height(620)
            .build();

        dialog.add_css_class("configuration-dialog");
        let root = gtk::Box::new(gtk::Orientation::Vertical, 10);
        root.set_margin_top(12);
        root.set_margin_bottom(12);
        root.set_margin_start(12);
        root.set_margin_end(12);
        let header = gtk::Box::new(gtk::Orientation::Vertical, 3);
        let title = gtk::Label::new(Some("Configuration diagnostics"));
        title.add_css_class("title-2");
        title.set_halign(gtk::Align::Start);
        header.append(&title);
        let issue_count = self.configuration_report.issues().len();

        let summary = gtk::Label::new(Some(&match issue_count {
            0 => String::from("No configuration issues were found"),
            1 => String::from("1 configuration issue needs attention"),
            count => format!("{count} configuration issues need attention"),
        }));

        summary.set_halign(gtk::Align::Start);

        summary.add_css_class(if issue_count == 0 {
            "configuration-ok"
        } else {
            "configuration-error"
        });

        header.append(&summary);
        root.append(&header);
        root.append(&configuration_section_title("FILES"));
        let files = gtk::Box::new(gtk::Orientation::Vertical, 4);
        files.add_css_class("configuration-files");

        files.append(&configuration_fact(
            "Active file",
            &self
                .configuration_report
                .active_path()
                .display()
                .to_string(),
        ));

        if self.configuration_report.loaded_paths().is_empty() {
            files.append(&configuration_fact("Loaded files", "None"));
        } else {
            for (index, path) in self.configuration_report.loaded_paths().iter().enumerate() {
                let label = if self.configuration_report.created() && index == 0 {
                    "Loaded and created"
                } else {
                    "Loaded"
                };

                files.append(&configuration_fact(label, &path.display().to_string()));
            }
        }

        if let Some(profile) = self.configuration_report.selected_profile() {
            files.append(&configuration_fact("Selected profile", profile));
        }

        root.append(&files);
        root.append(&configuration_section_title("ISSUES"));
        let issues = gtk::Box::new(gtk::Orientation::Vertical, 4);
        issues.add_css_class("configuration-issues");

        if self.configuration_report.issues().is_empty() {
            let empty = gtk::Label::new(Some("No unknown keys, invalid values, or file errors"));
            empty.add_css_class("muted");
            empty.set_halign(gtk::Align::Start);
            issues.append(&empty);
        } else {
            for issue in self.configuration_report.issues() {
                let row = gtk::Box::new(gtk::Orientation::Vertical, 2);
                row.add_css_class("configuration-issue");
                let location = gtk::Label::new(Some(&issue.location()));
                location.add_css_class("configuration-issue-location");
                location.set_halign(gtk::Align::Start);
                enable_stable_text_selection(&location);
                location.set_ellipsize(pango::EllipsizeMode::Middle);
                location.set_tooltip_text(Some(&issue.location()));
                row.append(&location);
                let message = gtk::Label::new(Some(issue.message()));
                message.set_halign(gtk::Align::Start);
                enable_stable_text_selection(&message);
                message.set_wrap(true);
                message.set_wrap_mode(pango::WrapMode::WordChar);
                row.append(&message);
                issues.append(&row);
            }
        }

        root.append(&issues);
        root.append(&configuration_section_title("EFFECTIVE CONFIGURATION"));

        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .vexpand(true)
            .build();

        let grid = gtk::Grid::new();
        grid.add_css_class("configuration-grid");
        grid.set_column_spacing(12);
        grid.set_row_spacing(1);
        grid.set_hexpand(true);
        let setting_heading = configuration_grid_label("SETTING", "configuration-grid-heading");
        let value_heading = configuration_grid_label("VALUE", "configuration-grid-heading");
        grid.attach(&setting_heading, 0, 0, 1, 1);
        grid.attach(&value_heading, 1, 0, 1, 1);

        for (index, entry) in self.configuration_report.effective().iter().enumerate() {
            let row = i32::try_from(index + 1).unwrap_or(i32::MAX);
            let name = configuration_grid_label(entry.name(), "configuration-setting");
            let value = configuration_grid_label(entry.value(), "configuration-value");
            enable_stable_text_selection(&value);
            value.set_wrap(true);
            value.set_wrap_mode(pango::WrapMode::WordChar);
            value.set_hexpand(true);
            grid.attach(&name, 0, row, 1, 1);
            grid.attach(&value, 1, row, 1, 1);
        }

        scrolled.set_child(Some(&grid));
        root.append(&scrolled);
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        actions.set_halign(gtk::Align::End);
        let open = gtk::Button::with_label("Open active config");
        open.add_css_class("inline-action");
        let dialog_for_open = dialog.clone();
        let path = self.configuration_report.active_path().to_path_buf();
        open.connect_clicked(move |_| open_configuration_file(&dialog_for_open, &path));
        actions.append(&open);
        let close = gtk::Button::with_label("Close");
        let dialog_for_close = dialog.clone();
        close.connect_clicked(move |_| dialog_for_close.close());
        actions.append(&close);
        root.append(&actions);
        dialog.set_child(Some(&root));
        self.configuration_dialog.replace(Some(dialog.clone()));
        let weak_dialog_state = Rc::downgrade(&self.configuration_dialog);

        dialog.connect_close_request(move |_| {
            if let Some(dialog_state) = weak_dialog_state.upgrade() {
                dialog_state.borrow_mut().take();
            }

            glib::Propagation::Proceed
        });

        dialog.present();
    }
}

fn configuration_section_title(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.add_css_class("session-caption");
    label.set_halign(gtk::Align::Start);

    label
}

fn configuration_fact(name: &str, value: &str) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    row.add_css_class("configuration-fact");
    let name = gtk::Label::new(Some(name));
    name.add_css_class("configuration-fact-name");
    name.set_halign(gtk::Align::Start);
    let value = gtk::Label::new(Some(value));
    value.add_css_class("configuration-fact-value");
    value.set_halign(gtk::Align::Start);
    enable_stable_text_selection(&value);
    value.set_ellipsize(pango::EllipsizeMode::Middle);
    value.set_hexpand(true);
    value.set_tooltip_text(Some(value.text().as_str()));
    row.append(&name);
    row.append(&value);

    row
}

fn configuration_grid_label(text: &str, class: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.add_css_class(class);
    label.set_halign(gtk::Align::Start);
    label.set_valign(gtk::Align::Start);
    label.set_xalign(0.0);

    label
}

fn open_configuration_file(parent: &gtk::Window, path: &Path) {
    let file = gio::File::for_path(path);
    let launcher = gtk::FileLauncher::new(Some(&file));
    launcher.set_writable(true);
    let parent_for_error = parent.clone();

    launcher.launch(Some(parent), None::<&gio::Cancellable>, move |result| {
        if let Err(error) = result {
            gtk::AlertDialog::builder()
                .message("Could not open the configuration file")
                .detail(error.to_string())
                .modal(true)
                .build()
                .show(Some(&parent_for_error));
        }
    });
}
