use super::*;
use crate::config::settings::{Control, Document, Values, settings as settings_catalog};

enum Input {
    Text(gtk::Entry),
    Font(gtk::FontDialogButton),
    Toggle(gtk::CheckButton),
    Number(gtk::SpinButton),
    Shortcut(shortcut::ShortcutInput),
    Choice(gtk::DropDown, &'static [crate::config::settings::Choice]),
}

impl Input {
    fn new(control: Control) -> Self {
        match control {
            Control::Choice(choices) => Self::Choice(
                gtk::DropDown::from_strings(
                    &choices
                        .iter()
                        .map(|choice| choice.label)
                        .collect::<Vec<_>>(),
                ),
                choices,
            ),
            Control::Shortcut(action) => Self::Shortcut(shortcut::ShortcutInput::new(action)),
            Control::Text => Self::Text(gtk::Entry::new()),
            Control::Font => {
                let dialog = gtk::FontDialog::builder().title("Choose font").build();
                let button = gtk::FontDialogButton::new(Some(dialog));
                button.set_use_font(false);
                button.set_use_size(false);

                Self::Font(button)
            }
            Control::Toggle => Self::Toggle(gtk::CheckButton::new()),
            Control::Number { min, max } => Self::Number(gtk::SpinButton::with_range(
                f64::from(min),
                f64::from(max),
                1.0,
            )),
        }
    }

    fn widget(&self) -> &gtk::Widget {
        match self {
            Self::Choice(widget, _) => widget.upcast_ref(),
            Self::Shortcut(widget) => widget.root.upcast_ref(),
            Self::Text(widget) => widget.upcast_ref(),
            Self::Font(widget) => widget.upcast_ref(),
            Self::Toggle(widget) => widget.upcast_ref(),
            Self::Number(widget) => widget.upcast_ref(),
        }
    }

    fn set(&self, value: &str) {
        match self {
            Self::Choice(widget, choices) => widget.set_selected(
                choices
                    .iter()
                    .position(|choice| choice.value == value)
                    .map_or(gtk::INVALID_LIST_POSITION, |index| index as u32),
            ),
            Self::Shortcut(widget) => widget.button.set_label(value),
            Self::Text(widget) => widget.set_text(value),
            Self::Font(widget) => widget.set_font_desc(&pango::FontDescription::from_string(value)),
            Self::Toggle(widget) => widget.set_active(value == "true"),
            Self::Number(widget) => widget.set_value(value.parse().unwrap_or_default()),
        }
    }

    fn value(&self) -> String {
        match self {
            Self::Choice(widget, choices) => choices
                .get(widget.selected() as usize)
                .map_or("", |choice| choice.value)
                .to_owned(),
            Self::Shortcut(widget) => widget.button.label().unwrap_or_default().to_string(),
            Self::Text(widget) => widget.text().trim().to_owned(),
            Self::Font(widget) => widget
                .font_desc()
                .map(|font| font.to_string())
                .unwrap_or_default(),
            Self::Toggle(widget) => widget.is_active().to_string(),
            // Keep typed input intact for validation. Do not silently save a stale spin value.
            Self::Number(widget) => widget.text().trim().to_owned(),
        }
    }

    fn connect(&self, editor: &Rc<Editor>, index: usize) {
        let weak = Rc::downgrade(editor);
        let changed = move || {
            if let Some(editor) = weak.upgrade()
                && !editor.rendering.get()
            {
                editor.input_changed(index);
            }
        };

        match self {
            Self::Choice(widget, _) => {
                widget.connect_selected_notify(move |_| changed());
            }
            Self::Shortcut(widget) => {
                widget.button.connect_label_notify(move |_| changed());
            }
            Self::Text(widget) => {
                widget.connect_changed(move |_| changed());
            }
            Self::Font(widget) => {
                widget.connect_font_desc_notify(move |_| changed());
            }
            Self::Toggle(widget) => {
                widget.connect_active_notify(move |_| changed());
            }
            Self::Number(widget) => {
                widget.connect_changed(move |_| changed());
            }
        }
    }
}

pub(super) struct Editor {
    window: gtk::Window,
    stack: gtk::Stack,
    form: gtk::Box,
    scope: gtk::DropDown,
    profiles: RefCell<Vec<String>>,
    selected_profile: RefCell<Option<String>>,
    active_profile: Option<String>,
    context: gtk::Label,
    inputs: Vec<(&'static str, Input)>,
    baseline: RefCell<Values>,
    changes: RefCell<Values>,
    revision: Cell<u64>,
    base_text: RefCell<Option<String>>,
    message: gtk::Label,
    issues: gtk::Label,
    issue_details: gtk::Label,
    apply: gtk::Button,
    reload: gtk::Button,
    rendering: Cell<bool>,
    saving: Cell<bool>,
}

impl Editor {
    pub(super) fn new(settings: &Rc<Settings>, ui: &Ui) -> Rc<Self> {
        let window = gtk::Window::builder()
            .title("fgdb settings")
            .transient_for(&ui.action_window())
            .destroy_with_parent(true)
            .default_width(960)
            .default_height(700)
            .css_classes(["settings-dialog"])
            .build();

        let root = gtk::Box::new(gtk::Orientation::Vertical, components::CONTENT_INSET);
        components::inset(&root, components::DIALOG_INSET);
        let header = components::control_row();
        let title = gtk::Label::new(Some("Settings"));
        title.add_css_class("title-2");
        title.set_xalign(0.0);
        title.set_hexpand(true);
        header.append(&title);
        let scope_label = gtk::Label::new(Some("Edit"));
        header.append(&scope_label);
        let scope = gtk::DropDown::from_strings(&["Defaults"]);
        scope.set_tooltip_text(Some(
            "Choose defaults or a named config profile. Only edited values are written",
        ));

        scope.set_size_request(170, -1);
        header.append(&scope);
        root.append(&header);

        let context = text("Loading preferences", "muted");
        root.append(&context);
        let form = gtk::Box::new(gtk::Orientation::Horizontal, components::CONTENT_INSET);
        form.set_vexpand(true);
        let stack = gtk::Stack::builder().hexpand(true).vexpand(true).build();
        stack.set_transition_type(gtk::StackTransitionType::None);
        let sidebar = gtk::StackSidebar::builder().stack(&stack).build();
        sidebar.add_css_class("settings-sidebar");
        form.append(&sidebar);
        form.append(&stack);
        root.append(&form);
        let mut inputs = Vec::with_capacity(settings_catalog().count());

        for page in [
            "Appearance",
            "Layout",
            "Keybindings",
            "Debugging",
            "Instructions",
            "Terminal",
            "Log",
            "Backend",
            "Recording",
        ] {
            let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
            let heading = gtk::Box::new(gtk::Orientation::Vertical, components::CONTROL_GAP);
            heading.add_css_class("settings-heading");
            heading.append(&components::section_title(&page.to_ascii_uppercase()));

            let help = match page {
                "Keybindings" => {
                    "Click a shortcut to record it. Clear disables it and Reset restores its built-in binding. Saved bindings apply immediately to the active configuration"
                }
                "Appearance" => {
                    "Changes to the active configuration apply immediately to source views"
                }
                "Layout" => {
                    "Panel placement, window sizes, selected tabs, and column widths are saved automatically. Workspace controls below apply immediately across config profiles. Window positions are managed by your desktop"
                }
                "Instructions" => {
                    "Column preferences apply live. Source annotations refresh when inspection is available. Assembly syntax is a default for new debugger backends"
                }
                "Terminal" => {
                    "Changes to the active configuration apply immediately to the terminal"
                }
                "Log" => {
                    "Saved changes apply live. Toolbar choices are temporary. Filters only affect display, not message capture"
                }
                "Debugging" => {
                    "Changes to the active configuration apply immediately. Execution controls remain in Session"
                }
                "Backend" => {
                    "Startup settings apply on the next app launch. The running debugger is never restarted automatically"
                }
                _ => {
                    "Defaults apply on the next app launch. The execution history menu controls recording for this session"
                }
            };

            heading.append(&text(help, "muted"));
            content.append(&heading);

            let filter = (page == "Keybindings").then(|| {
                let entry = components::search_entry("Filter actions");
                components::inset(&entry, components::CONTENT_INSET);
                content.append(&entry);
                entry
            });

            let mut shortcut_rows = Vec::new();
            let rows = gtk::Box::new(gtk::Orientation::Vertical, 0);

            for setting in settings_catalog().filter(|setting| setting.page == page) {
                let row = gtk::Box::new(gtk::Orientation::Vertical, components::CONTROL_GAP);
                row.add_css_class("settings-row");
                let top = components::control_row();
                let label = text(setting.title, "settings-name");
                label.set_hexpand(true);
                top.append(&label);
                let input = Input::new(setting.control);
                input.widget().set_valign(gtk::Align::Center);
                input.widget().set_sensitive(false);
                input.widget().set_tooltip_text(Some(setting.key));

                if matches!(setting.control, Control::Text) {
                    row.append(&top);
                    input.widget().set_hexpand(true);
                    row.append(input.widget());
                } else {
                    if !matches!(setting.control, Control::Toggle | Control::Shortcut(_)) {
                        input.widget().set_size_request(200, -1);
                    }

                    top.append(input.widget());
                    row.append(&top);
                }

                let focus_widget = match &input {
                    Input::Shortcut(shortcut) => shortcut.button.upcast_ref(),
                    _ => input.widget(),
                };

                label.set_mnemonic_widget(Some(focus_widget));
                row.append(&text(setting.help, "muted"));

                if filter.is_some() {
                    shortcut_rows.push((
                        row.clone(),
                        format!("{} {} {}", setting.title, setting.help, setting.key)
                            .to_lowercase(),
                    ));
                }

                rows.append(&row);
                inputs.push((setting.key, input));
            }

            if let Some(filter) = filter {
                let empty = text("No matching actions", "muted");
                components::inset(&empty, components::CONTENT_INSET);
                empty.set_visible(false);
                rows.append(&empty);
                // Filter metadata without rebuilding controls or discarding hidden edits.
                filter.connect_changed(move |entry| {
                    let query = entry.text().trim().to_lowercase();
                    let mut any_visible = false;

                    for (row, text) in &shortcut_rows {
                        let visible = text.contains(&query);
                        row.set_visible(visible);
                        any_visible |= visible;
                    }

                    empty.set_visible(!any_visible);
                });
            }

            if page == "Layout" {
                content.append(&rows);
                let columns = gtk::Box::new(gtk::Orientation::Vertical, components::CONTROL_GAP);
                columns.add_css_class("settings-row");
                let controls = components::control_row();
                let title = text("Table columns", "settings-name");
                title.set_hexpand(true);
                controls.append(&title);
                let reset = gtk::Button::with_label("Reset column widths");
                reset.add_css_class("inline-action");
                reset.set_tooltip_text(Some("Restore built-in column widths in all tables and clear saved widths, including closed viewers"));
                controls.append(&reset);
                let layouts = ui.column_layouts.clone();
                reset.connect_clicked(move |_| layouts.reset());
                columns.append(&controls);
                columns.append(&text("Dragging a column divider saves its width. Tables remember their own widths, including hidden columns. Viewer windows of the same kind share widths", "muted"));
                content.append(&columns);
                content.append(&ui.panel_hosts.settings_controls());
                stack.add_titled(&content, Some(page), page);
            } else if page == "Keybindings" {
                content.append(&scroll(&rows));
                stack.add_titled(&content, Some(page), page);
            } else {
                content.append(&rows);
                stack.add_titled(&scroll(&content), Some(page), page);
            }
        }

        let diagnostics = gtk::Box::new(gtk::Orientation::Vertical, components::CONTENT_INSET);
        components::inset(&diagnostics, components::CONTENT_INSET);
        diagnostics.append(&components::section_title("CURRENT FILE"));
        let path_label = text(&settings.path.display().to_string(), "muted");
        enable_stable_text_selection(&path_label);
        diagnostics.append(&path_label);
        let issues = text("Loading configuration", "muted");
        enable_stable_text_selection(&issues);
        diagnostics.append(&issues);
        let issue_details = text("", "configuration-error");
        issue_details.set_visible(false);
        enable_stable_text_selection(&issue_details);
        let (launch_summary, values) =
            super::super::configuration::launch_diagnostics(ui, &issue_details);

        diagnostics.append(&launch_summary);
        diagnostics.append(&scroll(&values));
        let open = gtk::Button::with_label("Open config file");
        open.add_css_class("inline-action");
        open.set_halign(gtk::Align::Start);
        let parent = window.downgrade();
        let path = settings.path.clone();

        open.connect_clicked(move |_| {
            if let Some(parent) = parent.upgrade() {
                super::super::configuration::open_configuration_file(&parent, &path);
            }
        });

        diagnostics.append(&open);
        stack.add_titled(&diagnostics, Some("Configuration"), "Configuration");

        let footer = components::control_row();
        let message = text("Loading configuration", "muted");
        message.set_hexpand(true);
        message.set_size_request(-1, 48);
        footer.append(&message);
        let reload = gtk::Button::with_label("Reload");
        reload.add_css_class("inline-action");
        reload.set_valign(gtk::Align::Center);
        reload.set_tooltip_text(Some("Read the file again and discard unsaved edits"));
        footer.append(&reload);
        let apply = gtk::Button::with_label("Apply");
        apply.add_css_class("inline-action");
        apply.set_valign(gtk::Align::Center);
        apply.set_sensitive(false);
        footer.append(&apply);
        root.append(&footer);
        window.set_child(Some(&root));

        let editor = Rc::new(Self {
            window,
            stack,
            form,
            scope,
            profiles: RefCell::new(Vec::new()),
            selected_profile: RefCell::new(None),
            active_profile: ui
                .configuration_report
                .selected_profile()
                .map(str::to_owned),
            context,
            inputs,
            baseline: RefCell::new(Values::new()),
            changes: RefCell::new(Values::new()),
            revision: Cell::new(0),
            base_text: RefCell::new(None),
            message,
            issues,
            issue_details,
            apply,
            reload,
            rendering: Cell::new(false),
            saving: Cell::new(false),
        });

        for (index, (_, input)) in editor.inputs.iter().enumerate() {
            input.connect(&editor, index);
        }

        Self::connect(&editor, settings);
        editor
    }

    fn connect(editor: &Rc<Self>, settings: &Rc<Settings>) {
        let weak = Rc::downgrade(editor);
        let owner = Rc::downgrade(settings);

        editor.apply.connect_clicked(move |_| {
            if let (Some(editor), Some(settings)) = (weak.upgrade(), owner.upgrade()) {
                settings.save(&editor);
            }
        });

        let weak = Rc::downgrade(editor);
        let owner = Rc::downgrade(settings);

        editor.reload.connect_clicked(move |_| {
            if let (Some(editor), Some(settings)) = (weak.upgrade(), owner.upgrade()) {
                editor.status("Reloading configuration", false);
                settings
                    .discard_on_reload
                    .replace(Some((Rc::downgrade(&editor), editor.revision())));
                settings.reload();
            }
        });

        let weak = Rc::downgrade(editor);
        let owner = Rc::downgrade(settings);

        editor.scope.connect_selected_notify(move |scope| {
            let (Some(editor), Some(settings)) = (weak.upgrade(), owner.upgrade()) else {
                return;
            };

            if editor.rendering.get() {
                return;
            }

            if editor.is_dirty() {
                editor.rendering.set(true);
                scope.set_selected(editor.profile_index());
                editor.rendering.set(false);
                editor.status("Apply or reload your edits before changing profile", true);
                return;
            }

            let profile = scope
                .selected()
                .checked_sub(1)
                .and_then(|index| editor.profiles.borrow().get(index as usize).cloned());

            if let Some(snapshot) = settings.snapshot.borrow().as_ref() {
                editor.populate(snapshot, profile.as_deref());
            }
        });

        let weak = Rc::downgrade(editor);
        let owner = Rc::downgrade(settings);

        editor.window.connect_close_request(move |_| {
            let (Some(editor), Some(settings)) = (weak.upgrade(), owner.upgrade()) else {
                return glib::Propagation::Proceed;
            };

            if editor.saving.get() {
                editor.status("Wait for the current save to finish before closing", false);
                return glib::Propagation::Stop;
            }

            if editor.is_dirty() {
                let dialog = gtk::AlertDialog::builder()
                    .message("Discard unsaved settings?")
                    .detail("Your configuration file has not been changed")
                    .buttons(["Keep editing", "Discard"])
                    .cancel_button(0)
                    .default_button(0)
                    .modal(true)
                    .build();

                let weak = Rc::downgrade(&editor);

                dialog.choose(
                    Some(&editor.window),
                    None::<&gio::Cancellable>,
                    move |result| {
                        if result == Ok(1)
                            && let Some(editor) = weak.upgrade()
                        {
                            editor.mark_saved();
                            editor.window.close();
                        }
                    },
                );

                return glib::Propagation::Stop;
            }

            settings.editor.borrow_mut().take();
            glib::Propagation::Proceed
        });
    }

    pub(super) fn present(&self, parent: &gtk::Window, diagnostics: bool) {
        if diagnostics {
            self.stack.set_visible_child_name("Configuration");
        }

        self.window.set_transient_for(Some(parent));
        self.window.present();
    }

    pub(super) fn populate(&self, snapshot: &Snapshot, profile: Option<&str>) {
        let document = &snapshot.document;
        let profile = profile.filter(|profile| document.profiles().any(|name| name == *profile));
        let values = match document.values(profile) {
            Ok(values) => values,
            Err(error) => {
                self.status(&error, true);
                return;
            }
        };

        self.rendering.set(true);
        let profiles: Vec<_> = document.profiles().map(str::to_owned).collect();
        let profiles_changed = *self.profiles.borrow() != profiles;
        self.profiles.replace(profiles);
        self.selected_profile.replace(profile.map(str::to_owned));
        self.context.set_text(&match (profile, self.active_profile.as_deref()) {
            (Some(profile), active) if active != Some(profile) => format!("Editing inactive profile '{profile}'. These preferences apply when fgdb is launched with --profile {profile}"),
            (Some(profile), _) => format!("Editing active profile '{profile}'. Unchanged fields inherit defaults. Environment and command-line overrides remain in effect"),
            (None, Some(active)) => format!("Editing defaults. Active profile '{active}', environment, and command-line settings may override these values"),
            (None, None) => String::from("Editing persistent preferences. Environment and command-line overrides remain in effect"),
        });

        if profiles_changed {
            let names = gtk::StringList::new(&["Defaults"]);

            for profile in self.profiles.borrow().iter() {
                names.append(profile);
            }

            self.scope.set_model(Some(&names));
        }

        self.scope.set_selected(self.profile_index());

        for (key, input) in &self.inputs {
            if let Some(value) = values.get(key) {
                input.set(value);
                input.widget().set_sensitive(true);
            }
        }

        self.base_text.replace(Some(document.text.clone()));
        self.mark_saved();
        self.rendering.set(false);
        self.form.set_sensitive(!self.saving.get());
        self.status(if !document.issues.is_empty() {
            "Configuration has issues. See Configuration. Live preferences keep their last valid values"
        } else if profile.is_some() && profile != self.active_profile.as_deref() {
            "Up to date. This profile applies when selected on a future app launch"
        } else {
            "Up to date. Display preferences apply live. Startup defaults take effect as described above"
        }, !document.issues.is_empty());
    }

    pub(super) fn update_issues(&self, document: &Document) {
        self.issues.set_text(&match document.issues.len() {
            0 => String::from("No configuration issues. External edits reload automatically"),
            1 => String::from("1 configuration issue. Details are in the table below"),
            count => format!("{count} configuration issues. Details are in the table below"),
        });

        self.issue_details
            .set_text(&super::super::configuration::issue_details(
                "Current file issues",
                &document.issues,
            ));

        self.issue_details.set_visible(!document.issues.is_empty());
    }

    pub(super) fn profile(&self) -> Option<String> {
        self.selected_profile.borrow().clone()
    }

    fn profile_index(&self) -> u32 {
        self.selected_profile
            .borrow()
            .as_ref()
            .and_then(|name| {
                self.profiles
                    .borrow()
                    .iter()
                    .position(|profile| profile == name)
            })
            .map_or(0, |index| index as u32 + 1)
    }

    fn values(&self) -> Values {
        self.inputs
            .iter()
            .map(|(key, input)| (*key, input.value()))
            .collect()
    }

    fn input_changed(&self, index: usize) {
        let (key, input) = &self.inputs[index];
        let value = input.value();

        if self.baseline.borrow().get(key) == Some(&value) {
            self.changes.borrow_mut().remove(key);
        } else {
            self.changes.borrow_mut().insert(key, value);
        }

        self.revision.set(self.revision.get().wrapping_add(1));
        self.update_actions();
        self.status(
            if self.is_dirty() {
                "Unsaved changes. Apply writes only the settings you changed"
            } else {
                "No unsaved changes"
            },
            false,
        );
    }

    pub(super) fn revision(&self) -> u64 {
        self.revision.get()
    }

    pub(super) fn is_current(&self, document: &Document) -> bool {
        self.base_text.borrow().as_deref() == Some(&document.text)
    }

    pub(super) fn is_dirty(&self) -> bool {
        self.base_text.borrow().is_some() && !self.changes.borrow().is_empty()
    }

    pub(super) fn mark_saved(&self) {
        self.baseline.replace(self.values());
        self.changes.borrow_mut().clear();
        self.revision.set(self.revision.get().wrapping_add(1));
        self.update_actions();
    }

    fn update_actions(&self) {
        self.apply
            .set_sensitive(self.is_dirty() && !self.saving.get());

        self.reload.set_sensitive(!self.saving.get());
        self.scope.set_sensitive(!self.saving.get());
    }

    pub(super) fn set_saving(&self, saving: bool) {
        self.saving.set(saving);
        self.form.set_sensitive(!saving);
        self.update_actions();

        if saving {
            self.status("Saving configuration", false);
        }
    }

    pub(super) fn patch(&self, document: &Document) -> Result<Option<String>, String> {
        if !self.is_current(document) {
            return Err(String::from(
                "The file changed outside this editor. Reload before applying your changes",
            ));
        }

        let changes = self.changes.borrow();

        if changes.is_empty() {
            return Ok(None);
        }

        document
            .patch(self.selected_profile.borrow().as_deref(), &changes)
            .map(Some)
    }

    pub(super) fn status(&self, message: &str, error: bool) {
        self.message.set_text(message);

        if error {
            self.message.add_css_class("configuration-error");
        } else {
            self.message.remove_css_class("configuration-error");
        }
    }
}

fn text(value: &str, class: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(value)
        .xalign(0.0)
        .wrap(true)
        .wrap_mode(pango::WrapMode::WordChar)
        .css_classes([class])
        .build()
}

fn scroll(child: &impl IsA<gtk::Widget>) -> gtk::ScrolledWindow {
    gtk::ScrolledWindow::builder()
        .child(child)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .overlay_scrolling(false)
        .hexpand(true)
        .vexpand(true)
        .build()
}
