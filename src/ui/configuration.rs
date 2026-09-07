use super::*;

impl Ui {
    pub(crate) fn preferred_assembly_syntax(&self) -> crate::config::settings::AssemblySyntax {
        self.settings.assembly_syntax()
    }

    pub(super) fn apply_instruction_preferences(
        &self,
        previous: &crate::config::settings::Preferences,
        preferences: &crate::config::settings::Preferences,
        initial: bool,
    ) {
        let controls = &self.disassembly_controls;

        if initial || previous.instruction_bytes != preferences.instruction_bytes {
            controls
                .columns
                .bytes
                .set_visible(preferences.instruction_bytes);
        }

        if initial || previous.instruction_symbols != preferences.instruction_symbols {
            controls
                .columns
                .symbols
                .set_visible(preferences.instruction_symbols);
        }

        if initial || previous.instruction_source != preferences.instruction_source {
            controls.mixed.set_active(preferences.instruction_source);
        }
    }

    pub(crate) fn breakpoint_auto_relocate(&self) -> bool {
        self.settings.breakpoint_auto_relocate.get()
    }

    pub fn connect_configuration_actions(self: &Rc<Self>) {
        self.settings.bind(self);
        let weak_ui = Rc::downgrade(self);

        self.configuration_button.connect_clicked(move |_| {
            if let Some(ui) = weak_ui.upgrade() {
                ui.session_popover.popdown();
                ui.settings.present(&ui, false);
            }
        });
    }

    pub(crate) fn has_configuration_issues(&self) -> bool {
        !self.configuration_report.issues().is_empty()
    }

    pub(crate) fn present_configuration_diagnostics(&self) {
        self.settings.present(self, true);
    }
}

pub(super) fn launch_diagnostics(ui: &Ui, current_issues: &gtk::Label) -> (gtk::Box, gtk::Grid) {
    let root = gtk::Box::new(gtk::Orientation::Vertical, components::CONTROL_GAP);
    root.append(&components::section_title("LAUNCH SNAPSHOT"));

    let note = gtk::Label::builder()
        .label("Effective configuration at app launch, including profile, environment, and command-line overrides")
        .xalign(0.0).wrap(true).css_classes(["muted"]).build();

    root.append(&note);

    for path in ui.configuration_report.loaded_paths() {
        let label = gtk::Label::builder()
            .label(format!(
                "{}: {}",
                if ui.configuration_report.created() {
                    "Created at launch"
                } else {
                    "Loaded at launch"
                },
                path.display()
            ))
            .xalign(0.0)
            .wrap(true)
            .wrap_mode(pango::WrapMode::WordChar)
            .css_classes(["muted"])
            .build();

        enable_stable_text_selection(&label);
        root.append(&label);
    }

    let grid = gtk::Grid::builder()
        .column_spacing(12)
        .row_spacing(0)
        .hexpand(true)
        .build();

    grid.add_css_class("configuration-grid");
    grid.attach(current_issues, 0, 0, 2, 1);

    if !ui.configuration_report.issues().is_empty() {
        let summary = gtk::Label::builder()
            .label(format!(
                "{} configuration issues at launch. Details are in the table below",
                ui.configuration_report.issues().len(),
            ))
            .xalign(0.0)
            .wrap(true)
            .css_classes(["configuration-error"])
            .build();

        root.append(&summary);

        let details = gtk::Label::builder()
            .label(issue_details(
                "Launch issues",
                ui.configuration_report.issues(),
            ))
            .xalign(0.0)
            .wrap(true)
            .wrap_mode(pango::WrapMode::WordChar)
            .css_classes(["configuration-error"])
            .build();

        enable_stable_text_selection(&details);
        grid.attach(&details, 0, 1, 2, 1);
    }

    for (index, entry) in ui.configuration_report.effective().iter().enumerate() {
        let name = gtk::Label::builder()
            .label(entry.name())
            .xalign(0.0)
            .valign(gtk::Align::Start)
            .css_classes(["configuration-setting"])
            .build();

        let value = gtk::Label::builder()
            .label(entry.value())
            .xalign(0.0)
            .hexpand(true)
            .wrap(true)
            .wrap_mode(pango::WrapMode::WordChar)
            .css_classes(["configuration-value"])
            .build();

        enable_stable_text_selection(&value);
        grid.attach(&name, 0, index as i32 + 2, 1, 1);
        grid.attach(&value, 1, index as i32 + 2, 1, 1);
    }

    (root, grid)
}

pub(super) fn issue_details(title: &str, issues: &[crate::config::ConfigurationIssue]) -> String {
    use std::fmt::Write;

    if issues.is_empty() {
        return String::new();
    }

    let mut details = title.to_owned();

    for issue in issues {
        let _ = write!(details, "\n\n{}\n{}", issue.location(), issue.message());
    }

    details
}

pub(super) fn open_configuration_file(parent: &gtk::Window, path: &Path) {
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
