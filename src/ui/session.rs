use super::*;

impl Ui {
    pub fn set_session_handler(&self, handler: impl Fn(DebugSession) + 'static) {
        self.session_handler.replace(Some(Rc::new(handler)));
    }

    pub fn set_session_action_handler(&self, handler: impl Fn(SessionAction) + 'static) {
        self.session_action_handler.replace(Some(Rc::new(handler)));
    }

    pub fn current_session(&self) -> Option<DebugSession> {
        self.current_session.borrow().clone()
    }

    /// Store the desired session and update configuration-derived UI only.
    /// Live target and inferior state is updated from successful GDB commands
    /// and events, so a configured session may legitimately be disconnected.
    pub fn set_current_session(&self, session: DebugSession) {
        self.close_source_palette();
        self.source_loaded_cache.borrow_mut().take();
        self.source_loaded_generation
            .fetch_add(1, Ordering::Relaxed);
        self.source_tree_render_generation
            .fetch_add(1, Ordering::Relaxed);
        let session_directory = session.working_directory().filter(|path| path.is_dir());
        let mut resolution_roots = self.source_base_roots.clone();
        prioritize_source_root(&mut resolution_roots, session_directory);
        if *self.source_roots.borrow() != resolution_roots {
            self.source_roots.replace(resolution_roots);
            self.resolved_source_paths.borrow_mut().clear();
        }
        let mut tree_roots = self.source_tree_base_roots.clone();
        prioritize_source_root(&mut tree_roots, session_directory);
        if *self.source_tree_roots.borrow() != tree_roots {
            self.source_tree_roots.replace(tree_roots);
            self.source_tree_cache.borrow_mut().take();
            self.source_tree_indexing.set(false);
            self.source_tree_generation.fetch_add(1, Ordering::Relaxed);
        }
        self.current_session.replace(Some(session));
        self.update_session_display();
        self.update_control_sensitivity();
        if self.source_tree_initialized.get() {
            let refresh = self.source_tree.refresh_handler.borrow().clone();
            if let Some(refresh) = refresh {
                refresh();
            }
        }
    }

    pub fn connect_session_actions(self: &Rc<Self>) {
        let weak_ui = Rc::downgrade(self);
        self.new_session_button.connect_clicked(move |_| {
            let Some(ui) = weak_ui.upgrade() else {
                return;
            };
            ui.session_popover.popdown();
            ui.present_session_manager();
        });

        for (button, action) in [
            (&self.restart_session_button, SessionAction::Restart),
            (&self.kill_session_button, SessionAction::Kill),
            (&self.detach_session_button, SessionAction::Detach),
        ] {
            let weak_ui = Rc::downgrade(self);
            button.connect_clicked(move |_| {
                let Some(ui) = weak_ui.upgrade() else {
                    return;
                };
                ui.session_popover.popdown();
                if matches!(action, SessionAction::Kill | SessionAction::Detach) {
                    ui.confirm_session_action(action);
                } else {
                    ui.emit_session_action(action);
                }
            });
        }
    }

    pub(super) fn update_session_display(&self) {
        let session = self.current_session.borrow();
        let (kind, target) = session.as_ref().map_or_else(
            || ("No session", String::from("Choose a debug session")),
            |session| (session.kind_label(), session.title()),
        );
        self.session_kind_label.set_text(kind);
        self.session_target_label.set_text(&target);
        self.session_target_label.set_tooltip_text(Some(&target));
        self.target_label.set_text(&target);
        self.target_label.set_tooltip_text(Some(&target));
    }

    fn emit_session_action(&self, action: SessionAction) {
        let handler = self.session_action_handler.borrow().clone();
        if let Some(handler) = handler {
            handler(action);
        }
    }

    fn confirm_session_action(&self, action: SessionAction) {
        let (message, detail, accept) = match action {
            SessionAction::Kill => (
                "Kill the inferior?",
                "This terminates the debugged process. GDB and fgdb remain open.",
                "Kill",
            ),
            SessionAction::Detach => (
                "Detach from the inferior?",
                "GDB releases the process and normally resumes it. The process keeps running outside fgdb.",
                "Detach and resume",
            ),
            SessionAction::Restart => {
                self.emit_session_action(action);
                return;
            }
        };
        let dialog = gtk::AlertDialog::builder()
            .message(message)
            .detail(detail)
            .buttons(["Cancel", accept])
            .cancel_button(0)
            .default_button(0)
            .modal(true)
            .build();
        let window = self.window.clone();
        let handler = Rc::clone(&self.session_action_handler);
        glib::spawn_future_local(async move {
            if dialog.choose_future(Some(&window)).await == Ok(1) {
                let handler = handler.borrow().clone();
                if let Some(handler) = handler {
                    handler(action);
                }
            }
        });
    }

    fn present_session_manager(&self) {
        let editor = gtk::Window::builder()
            .title("New debug session")
            .transient_for(&self.window)
            .modal(true)
            .default_width(720)
            .build();
        editor.add_css_class("session-editor");

        let root = gtk::Box::new(gtk::Orientation::Vertical, 8);
        root.set_margin_top(10);
        root.set_margin_bottom(10);
        root.set_margin_start(10);
        root.set_margin_end(10);

        let current_session = self.current_session();
        let status_text = current_session.as_ref().map_or_else(
            || String::from("No debug session is configured"),
            |session| {
                format!(
                    "Current {} session · {}",
                    session.kind_label(),
                    session.title()
                )
            },
        );
        let status = gtk::Label::new(Some(&status_text));
        status.add_css_class("muted");
        status.set_halign(gtk::Align::Start);
        status.set_ellipsize(pango::EllipsizeMode::Middle);
        status.set_tooltip_text(Some(&status_text));
        root.append(&status);

        let notebook = gtk::Notebook::new();
        notebook.set_hexpand(true);
        notebook.add_css_class("session-tabs");

        let launch_executable = gtk::Entry::builder()
            .placeholder_text("/path/to/executable")
            .hexpand(true)
            .build();
        let launch_arguments = gtk::Entry::builder()
            .placeholder_text("--flag 'argument with spaces'")
            .hexpand(true)
            .build();
        let launch_directory = gtk::Entry::builder()
            .placeholder_text("Working directory")
            .hexpand(true)
            .build();
        launch_directory.set_text(
            &self
                .source_roots
                .borrow()
                .first()
                .cloned()
                .unwrap_or_else(|| PathBuf::from("."))
                .to_string_lossy(),
        );
        let launch_environment = gtk::TextView::new();
        launch_environment.set_monospace(true);
        launch_environment.set_wrap_mode(gtk::WrapMode::None);
        launch_environment.set_top_margin(4);
        launch_environment.set_bottom_margin(4);
        launch_environment.set_left_margin(5);
        launch_environment.set_right_margin(5);
        let environment_scroll = gtk::ScrolledWindow::builder()
            .min_content_height(92)
            .hexpand(true)
            .child(&launch_environment)
            .build();
        let launch_page = session_page(None);
        let launch_grid = form_grid();
        add_path_field(
            &launch_grid,
            0,
            "Executable",
            &launch_executable,
            &editor,
            false,
        );
        add_form_field(&launch_grid, 1, "Arguments", &launch_arguments);
        add_path_field(
            &launch_grid,
            2,
            "Working directory",
            &launch_directory,
            &editor,
            true,
        );
        add_form_field(&launch_grid, 3, "Environment", &environment_scroll);
        let environment_hint = gtk::Label::new(Some("One NAME=VALUE entry per line"));
        environment_hint.add_css_class("muted");
        environment_hint.set_halign(gtk::Align::Start);
        launch_grid.attach(&environment_hint, 1, 4, 1, 1);
        launch_page.append(&launch_grid);
        let launch = page_action("Start session");
        append_page_actions(&launch_page, &launch, &editor);
        notebook.append_page(&launch_page, Some(&gtk::Label::new(Some("Launch"))));

        let attach_page = session_page(Some(
            "Attach leaves the process alive when you later choose Detach. Kill is a separate action.",
        ));
        let attach_pid = gtk::SpinButton::with_range(1.0, f64::from(u32::MAX), 1.0);
        attach_pid.set_numeric(true);
        attach_pid.set_width_chars(12);
        let attach_executable = gtk::Entry::builder()
            .placeholder_text("Optional local executable for symbols")
            .hexpand(true)
            .build();
        let attach_grid = form_grid();
        add_form_field(&attach_grid, 0, "PID", &attach_pid);
        add_path_field(
            &attach_grid,
            1,
            "Executable",
            &attach_executable,
            &editor,
            false,
        );
        attach_page.append(&attach_grid);
        let attach = page_action("Attach to process");
        append_page_actions(&attach_page, &attach, &editor);
        notebook.append_page(&attach_page, Some(&gtk::Label::new(Some("Attach"))));

        let core_page = session_page(Some(
            "Open a post-mortem core together with the executable that produced it.",
        ));
        let core_executable = gtk::Entry::builder()
            .placeholder_text("Executable that produced the core")
            .hexpand(true)
            .build();
        let core_dump = gtk::Entry::builder()
            .placeholder_text("Core dump file")
            .hexpand(true)
            .build();
        let core_grid = form_grid();
        add_path_field(
            &core_grid,
            0,
            "Executable",
            &core_executable,
            &editor,
            false,
        );
        add_path_field(&core_grid, 1, "Core dump", &core_dump, &editor, false);
        core_page.append(&core_grid);
        let open_core = page_action("Open core dump");
        append_page_actions(&core_page, &open_core, &editor);
        notebook.append_page(&core_page, Some(&gtk::Label::new(Some("Core dump"))));

        let remote_page = session_page(Some(
            "Connect to gdbserver or another GDB remote target. Extended remote can start a target-side executable.",
        ));
        let remote_endpoint = gtk::Entry::builder()
            .placeholder_text("localhost:1234 or /dev/ttyS0")
            .hexpand(true)
            .build();
        let remote_executable = gtk::Entry::builder()
            .placeholder_text("Optional local executable for symbols")
            .hexpand(true)
            .build();
        let remote_protocol = gtk::DropDown::from_strings(&["Remote", "Extended remote"]);
        let remote_run_path = gtk::Entry::builder()
            .placeholder_text("Optional target-side executable path")
            .hexpand(true)
            .build();
        let remote_grid = form_grid();
        add_form_field(&remote_grid, 0, "Endpoint", &remote_endpoint);
        add_form_field(&remote_grid, 1, "Protocol", &remote_protocol);
        add_path_field(
            &remote_grid,
            2,
            "Local executable",
            &remote_executable,
            &editor,
            false,
        );
        add_form_field(&remote_grid, 3, "Remote executable", &remote_run_path);
        remote_page.append(&remote_grid);
        let connect_remote = page_action("Connect to target");
        append_page_actions(&remote_page, &connect_remote, &editor);
        notebook.append_page(&remote_page, Some(&gtk::Label::new(Some("Remote"))));

        let selected_page = match current_session.as_ref() {
            Some(DebugSession::Launch {
                executable,
                arguments,
                environment,
                working_directory,
            }) => {
                launch_executable.set_text(&executable.to_string_lossy());
                launch_arguments.set_text(&shell_words::join(arguments));
                launch_directory.set_text(&working_directory.to_string_lossy());
                let environment = environment
                    .iter()
                    .map(|(name, value)| format!("{name}={value}"))
                    .collect::<Vec<_>>()
                    .join("\n");
                launch_environment.buffer().set_text(&environment);
                0
            }
            Some(DebugSession::Attach { pid, executable }) => {
                attach_pid.set_value(f64::from(*pid));
                if let Some(executable) = executable {
                    attach_executable.set_text(&executable.to_string_lossy());
                }
                1
            }
            Some(DebugSession::CoreDump {
                executable,
                core_dump: dump,
            }) => {
                core_executable.set_text(&executable.to_string_lossy());
                core_dump.set_text(&dump.to_string_lossy());
                2
            }
            Some(DebugSession::Remote {
                endpoint,
                executable,
                extended,
                remote_executable: run_path,
            }) => {
                remote_endpoint.set_text(endpoint);
                remote_protocol.set_selected(u32::from(*extended));
                if let Some(executable) = executable {
                    remote_executable.set_text(&executable.to_string_lossy());
                }
                if let Some(run_path) = run_path {
                    remote_run_path.set_text(run_path);
                }
                3
            }
            None => 0,
        };
        notebook.set_current_page(Some(selected_page));
        root.append(&notebook);

        let validation = gtk::Label::new(None);
        validation.add_css_class("value-editor-validation");
        validation.set_halign(gtk::Align::Start);
        validation.set_wrap(true);
        validation.set_visible(false);
        root.append(&validation);

        let active_live_target = self.inferior_has_started()
            && !matches!(
                current_session.as_ref(),
                Some(DebugSession::CoreDump { .. })
            );
        for button in [&launch, &attach, &open_core, &connect_remote] {
            button.set_sensitive(!active_live_target);
            if active_live_target {
                button.set_tooltip_text(Some(
                    "Kill or detach the current inferior before configuring another session",
                ));
            }
        }

        let handler = Rc::clone(&self.session_handler);
        let editor_for_launch = editor.clone();
        let validation_for_launch = validation.clone();
        launch.connect_clicked(move |_| {
            submit_session(
                build_launch_session(
                    &launch_executable,
                    &launch_arguments,
                    &launch_directory,
                    &launch_environment,
                ),
                &handler,
                &editor_for_launch,
                &validation_for_launch,
            );
        });

        let handler = Rc::clone(&self.session_handler);
        let editor_for_attach = editor.clone();
        let validation_for_attach = validation.clone();
        attach.connect_clicked(move |_| {
            submit_session(
                build_attach_session(&attach_pid, &attach_executable),
                &handler,
                &editor_for_attach,
                &validation_for_attach,
            );
        });

        let handler = Rc::clone(&self.session_handler);
        let editor_for_core = editor.clone();
        let validation_for_core = validation.clone();
        open_core.connect_clicked(move |_| {
            submit_session(
                build_core_session(&core_executable, &core_dump),
                &handler,
                &editor_for_core,
                &validation_for_core,
            );
        });

        let handler = Rc::clone(&self.session_handler);
        let editor_for_remote = editor.clone();
        let validation_for_remote = validation;
        connect_remote.connect_clicked(move |_| {
            submit_session(
                build_remote_session(
                    &remote_endpoint,
                    &remote_executable,
                    &remote_protocol,
                    &remote_run_path,
                ),
                &handler,
                &editor_for_remote,
                &validation_for_remote,
            );
        });

        editor.set_child(Some(&root));
        editor.present();
    }
}

fn prioritize_source_root(roots: &mut Vec<PathBuf>, priority: Option<&Path>) {
    let Some(priority) = priority else {
        return;
    };
    if let Some(index) = roots.iter().position(|root| root == priority) {
        roots.remove(index);
    }
    roots.insert(0, priority.to_path_buf());
}

fn session_page(hint: Option<&str>) -> gtk::Box {
    let page = gtk::Box::new(gtk::Orientation::Vertical, 8);
    page.set_margin_top(10);
    page.set_margin_bottom(10);
    page.set_margin_start(8);
    page.set_margin_end(8);
    if let Some(hint) = hint {
        let label = gtk::Label::new(Some(hint));
        label.add_css_class("muted");
        label.set_halign(gtk::Align::Start);
        label.set_wrap(true);
        page.append(&label);
    }
    page
}

fn page_action(label: &str) -> gtk::Button {
    let button = gtk::Button::with_label(label);
    button.add_css_class("suggested-action");
    button
}

fn append_page_actions(page: &gtk::Box, primary: &gtk::Button, editor: &gtk::Window) {
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    actions.set_halign(gtk::Align::End);
    let cancel = gtk::Button::with_label("Cancel");
    let editor = editor.clone();
    cancel.connect_clicked(move |_| editor.close());
    actions.append(&cancel);
    actions.append(primary);
    page.append(&actions);
}

fn submit_session(
    session: Result<DebugSession, String>,
    handler: &Rc<RefCell<Option<DebugSessionHandler>>>,
    editor: &gtk::Window,
    validation: &gtk::Label,
) {
    match session {
        Ok(session) => {
            validation.set_visible(false);
            let handler = handler.borrow().clone();
            if let Some(handler) = handler {
                handler(session);
                editor.close();
            } else {
                validation.set_text("GDB is not ready to create a session");
                validation.set_visible(true);
            }
        }
        Err(message) => {
            validation.set_text(&message);
            validation.set_visible(true);
        }
    }
}

fn form_grid() -> gtk::Grid {
    gtk::Grid::builder()
        .row_spacing(7)
        .column_spacing(8)
        .hexpand(true)
        .build()
}

fn add_form_field(grid: &gtk::Grid, row: i32, title: &str, widget: &impl IsA<gtk::Widget>) {
    let label = gtk::Label::new(Some(title));
    label.add_css_class("field-label");
    label.set_halign(gtk::Align::End);
    label.set_valign(gtk::Align::Start);
    label.set_margin_top(5);
    grid.attach(&label, 0, row, 1, 1);
    grid.attach(widget, 1, row, 1, 1);
}

fn add_path_field(
    grid: &gtk::Grid,
    row: i32,
    title: &str,
    entry: &gtk::Entry,
    parent: &gtk::Window,
    select_folder: bool,
) {
    let field = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    field.append(entry);
    let browse = gtk::Button::with_label("Browse…");
    browse.add_css_class("inline-action");
    field.append(&browse);
    add_form_field(grid, row, title, &field);
    let entry = entry.clone();
    let parent = parent.clone();
    let title = title.to_owned();
    browse.connect_clicked(move |_| {
        let chooser = gtk::FileDialog::builder().modal(true).title(&title).build();
        let entry = entry.clone();
        let parent = parent.clone();
        glib::spawn_future_local(async move {
            let result = if select_folder {
                chooser.select_folder_future(Some(&parent)).await
            } else {
                chooser.open_future(Some(&parent)).await
            };
            if let Ok(file) = result
                && let Some(path) = file.path()
            {
                entry.set_text(&path.to_string_lossy());
            }
        });
    });
}

fn build_launch_session(
    executable: &gtk::Entry,
    arguments: &gtk::Entry,
    working_directory: &gtk::Entry,
    environment: &gtk::TextView,
) -> Result<DebugSession, String> {
    let working_directory = required_directory(&working_directory.text(), "Working directory")?;
    let executable =
        required_executable(&executable.text(), "Executable", Some(&working_directory))?;
    let arguments = shell_words::split(arguments.text().trim())
        .map_err(|error| format!("Arguments are not valid shell words: {error}"))?;
    let buffer = environment.buffer();
    let environment =
        parse_environment(&buffer.text(&buffer.start_iter(), &buffer.end_iter(), false))?;
    Ok(DebugSession::Launch {
        executable,
        arguments,
        environment,
        working_directory,
    })
}

fn build_attach_session(
    pid: &gtk::SpinButton,
    executable: &gtk::Entry,
) -> Result<DebugSession, String> {
    let pid = u32::try_from(pid.value_as_int())
        .ok()
        .filter(|pid| *pid > 0)
        .ok_or_else(|| String::from("PID must be a positive process ID"))?;
    Ok(DebugSession::Attach {
        pid,
        executable: optional_executable(&executable.text(), "Executable")?,
    })
}

fn build_core_session(executable: &gtk::Entry, core: &gtk::Entry) -> Result<DebugSession, String> {
    Ok(DebugSession::CoreDump {
        executable: required_executable(&executable.text(), "Executable", None)?,
        core_dump: required_file(&core.text(), "Core dump")?,
    })
}

fn build_remote_session(
    endpoint: &gtk::Entry,
    executable: &gtk::Entry,
    protocol: &gtk::DropDown,
    remote_executable: &gtk::Entry,
) -> Result<DebugSession, String> {
    let endpoint = endpoint.text().trim().to_owned();
    if endpoint.is_empty() {
        return Err(String::from("Remote endpoint is required"));
    }
    if endpoint.chars().any(char::is_whitespace) {
        return Err(String::from(
            "Remote endpoints cannot contain whitespace. Use HOST:PORT or a device path",
        ));
    }
    Ok(DebugSession::Remote {
        endpoint,
        executable: optional_executable(&executable.text(), "Local executable")?,
        extended: protocol.selected() == 1,
        remote_executable: nonempty(remote_executable.text().as_str()),
    })
}

fn parse_environment(text: &str) -> Result<Vec<(String, String)>, String> {
    text.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(index, line)| {
            let (name, value) = line
                .split_once('=')
                .ok_or_else(|| format!("Environment line {} must use NAME=VALUE", index + 1))?;
            let name = name.trim();
            let valid = name.bytes().enumerate().all(|(index, byte)| {
                byte == b'_' || byte.is_ascii_alphabetic() || (index > 0 && byte.is_ascii_digit())
            });
            if name.is_empty() || !valid {
                return Err(format!(
                    "Environment line {} has an invalid name",
                    index + 1
                ));
            }
            Ok((name.to_owned(), value.to_owned()))
        })
        .collect()
}

fn required_directory(value: &str, label: &str) -> Result<PathBuf, String> {
    let path = required_path(value, label)?;
    if !path.is_dir() {
        return Err(format!("{label} is not a directory: {}", path.display()));
    }
    path.canonicalize()
        .map_err(|error| format!("Cannot resolve {label}: {error}"))
}

fn required_executable(
    value: &str,
    label: &str,
    relative_to: Option<&Path>,
) -> Result<PathBuf, String> {
    let mut path = required_path(value, label)?;
    if path.is_relative()
        && let Some(directory) = relative_to
    {
        path = directory.join(path);
    }
    if !path.is_file() {
        return Err(format!("{label} is not a file: {}", path.display()));
    }
    path.canonicalize()
        .map_err(|error| format!("Cannot resolve {label}: {error}"))
}

fn optional_executable(value: &str, label: &str) -> Result<Option<PathBuf>, String> {
    nonempty(value).map_or(Ok(None), |value| {
        required_executable(&value, label, None).map(Some)
    })
}

fn required_file(value: &str, label: &str) -> Result<PathBuf, String> {
    required_executable(value, label, None)
}

fn required_path(value: &str, label: &str) -> Result<PathBuf, String> {
    nonempty(value)
        .map(PathBuf::from)
        .ok_or_else(|| format!("{label} is required"))
}

fn nonempty(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

#[cfg(test)]
mod tests {
    use super::{parse_environment, prioritize_source_root};
    use std::path::PathBuf;

    #[test]
    fn parses_environment_values_without_losing_spaces_or_equals() {
        assert_eq!(
            parse_environment("MODE=debug build\nTOKEN=a=b=c\n\n").unwrap(),
            [
                ("MODE".into(), "debug build".into()),
                ("TOKEN".into(), "a=b=c".into())
            ]
        );
    }

    #[test]
    fn rejects_invalid_environment_names() {
        assert!(parse_environment("9MODE=debug").is_err());
        assert!(parse_environment("NO VALUE").is_err());
    }

    #[test]
    fn session_source_roots_replace_instead_of_accumulating() {
        let base = vec![PathBuf::from("/base")];
        let mut first = base.clone();
        prioritize_source_root(&mut first, Some(PathBuf::from("/project-a").as_path()));
        assert_eq!(first[0], PathBuf::from("/project-a"));
        let mut roots = base;
        prioritize_source_root(&mut roots, Some(PathBuf::from("/project-b").as_path()));
        assert_eq!(roots, [PathBuf::from("/project-b"), PathBuf::from("/base")]);
        assert!(!roots.contains(&PathBuf::from("/project-a")));
        prioritize_source_root(&mut roots, Some(PathBuf::from("/base").as_path()));
        assert_eq!(roots[0], PathBuf::from("/base"));
        assert_eq!(
            roots
                .iter()
                .filter(|root| *root == &PathBuf::from("/base"))
                .count(),
            1
        );
    }
}
