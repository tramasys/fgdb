//! Workspace presentation and file lifecycle, separate from debugger mutation.

use super::*;
use crate::investigation::{
    self as storage, CaptureResult, Investigation, RestoreResult, SavedMemory,
};

type Capture = Rc<dyn Fn(CaptureResult)>;
type Restore = Rc<dyn Fn(Investigation, RestoreResult)>;

#[derive(Default)]
pub(super) struct Workspace {
    ui: RefCell<std::rc::Weak<Ui>>,
    active: RefCell<Option<(PathBuf, String)>>,
    busy: Cell<bool>,
    incomplete: Cell<bool>,
    label: gtk::Label,
    capture: RefCell<Option<Capture>>,
    restore: RefCell<Option<Restore>>,
}

impl Ui {
    pub(crate) fn connect_investigation_actions(
        self: &Rc<Self>,
        capture: impl Fn(CaptureResult) + 'static,
        restore: impl Fn(Investigation, RestoreResult) + 'static,
    ) {
        let state = &self.investigation;
        state.ui.replace(Rc::downgrade(self));
        state.capture.replace(Some(Rc::new(capture)));
        state.restore.replace(Some(Rc::new(restore)));
        let Some(menu) = self.session_popover.child().and_downcast::<gtk::Box>() else {
            return;
        };

        menu.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        state.label.set_text("No workspace file");
        state.label.set_xalign(0.0);
        state.label.set_ellipsize(pango::EllipsizeMode::Middle);
        state.label.set_max_width_chars(36);
        state.label.add_css_class("muted");
        components::inset(&state.label, components::CONTENT_INSET);
        menu.append(&state.label);

        for (label, action) in [
            ("Open workspace…", 0),
            ("Save workspace", 1),
            ("Save workspace as…", 2),
        ] {
            let button = super::build::session_menu_action(label, "");
            let weak = Rc::downgrade(state);

            button.connect_clicked(move |_| {
                if let Some(state) = weak.upgrade()
                    && state.available()
                {
                    if let Some(ui) = state.ui.borrow().upgrade() {
                        ui.session_popover.popdown()
                    }

                    if action == 0 {
                        state.open()
                    } else {
                        state.save(action == 2)
                    }
                }
            });

            menu.append(&button);
        }
    }

    pub(crate) fn configure_workspace_session(&self, session: DebugSession) {
        let handler = self.session_handler.borrow().clone();

        if let Some(handler) = handler {
            handler(session.into())
        }
    }

    pub(crate) fn restore_investigation_views(
        &self,
        workspace: &Investigation,
        mut failures: Vec<String>,
        complete: RestoreResult,
    ) {
        self.expression_watches.replace(workspace.watches.clone());
        // The regular refresh owns varobj deletion and reconstruction.
        let refresh = self.expression_watch_refresh_handler.borrow().clone();

        if let Some(refresh) = refresh {
            refresh()
        }

        memory_view::clear_memory_watches(&self.memory_watch_container, &self.memory_watches);

        for memory in &workspace.memory {
            if !memory_view::restore_memory_watch(
                &self.memory_watch_container,
                &self.memory_watches,
                &self.memory_watch_handler,
                memory.expression.clone(),
                memory.bytes,
                memory.format,
            ) {
                failures.push(format!(
                    "Memory inspector could not be restored: {}",
                    memory.expression
                ));
            }
        }

        self.refresh_memory_watches();

        let restore = Rc::new(SourceRestore {
            ui: self.self_weak.borrow().clone(),
            sources: RefCell::new(workspace.sources.clone().into()),
            failures: RefCell::new(failures),
            complete: RefCell::new(Some(complete)),
            generation: Cell::new(self.source_open_generation.load(Ordering::Relaxed)),
        });

        let weak = Rc::downgrade(&restore);
        glib::timeout_add_local_once(Duration::from_secs(15), move || {
            if let Some(restore) = weak.upgrade() {
                restore.finish(Some("Opening saved source files timed out"));
            }
        });

        restore.schedule(self);
    }
}

struct SourceRestore {
    ui: std::rc::Weak<Ui>,
    sources: RefCell<std::collections::VecDeque<(PathBuf, u32)>>,
    failures: RefCell<Vec<String>>,
    complete: RefCell<Option<RestoreResult>>,
    generation: Cell<u64>,
}

impl SourceRestore {
    fn schedule(self: &Rc<Self>, ui: &Ui) {
        self.generation
            .set(ui.source_open_generation.load(Ordering::Relaxed));

        let restore = Rc::clone(self);
        glib::idle_add_local_once(move || {
            let Some(ui) = restore.ui.upgrade() else {
                return;
            };

            if ui.source_open_generation.load(Ordering::Relaxed) != restore.generation.get() {
                restore.finish(Some("Source restoration was interrupted by navigation"));
                return;
            }

            restore.next(&ui);
        });
    }

    fn next(self: &Rc<Self>, ui: &Ui) {
        if self.complete.borrow().is_none() {
            return;
        }

        let next = self.sources.borrow_mut().pop_front();
        let Some((path, line)) = next else {
            self.finish(None);
            return;
        };

        let restore = Rc::clone(self);
        let description = path.display().to_string();
        let accepted = ui.navigate_to_source_then(&path, line, false, move |ui, opened| {
            if !opened {
                restore
                    .failures
                    .borrow_mut()
                    .push(format!("Source file could not be opened: {description}"));
            }

            restore.schedule(ui);
        });

        self.generation
            .set(ui.source_open_generation.load(Ordering::Relaxed));

        if !accepted {
            self.failures.borrow_mut().push(format!(
                "Source file could not be opened: {}",
                path.display()
            ));
            self.schedule(ui);
        }
    }

    fn finish(&self, error: Option<&str>) {
        let complete = self.complete.borrow_mut().take();
        let Some(complete) = complete else { return };

        if let Some(error) = error {
            self.failures.borrow_mut().push(error.to_owned());

            if let Some(ui) = self.ui.upgrade()
                && ui.source_open_generation.load(Ordering::Relaxed) == self.generation.get()
            {
                ui.source_open_generation.fetch_add(1, Ordering::Relaxed);
            }
        }

        complete(Ok(self.failures.take()));
    }
}

impl Drop for SourceRestore {
    fn drop(&mut self) {
        if let Some(complete) = self.complete.get_mut().take() {
            let mut failures = self.failures.take();
            failures.push(String::from("Source restoration was interrupted"));
            glib::idle_add_local_once(move || complete(Ok(failures)));
        }
    }
}

impl Workspace {
    fn set_active(&self, path: PathBuf, etag: String, incomplete: bool) {
        self.label.set_text(
            &path
                .file_name()
                .unwrap_or(path.as_os_str())
                .to_string_lossy(),
        );

        self.label
            .set_tooltip_text(Some(&path.display().to_string()));

        self.active.replace(Some((path, etag)));
        self.incomplete.set(incomplete);
    }

    fn available(&self) -> bool {
        let Some(ui) = self.ui.borrow().upgrade() else {
            return false;
        };

        let available = !self.busy.get() && ui.model.debugger_synchronization_available();

        if !available {
            ui.set_status(
                "Workspace unavailable",
                "Pause the target and wait for the current debugger operation to finish",
                None,
            );
        }

        available
    }

    fn report(&self, title: &str, detail: &str, error: bool) {
        if let Some(ui) = self.ui.borrow().upgrade() {
            ui.set_status(
                title,
                detail,
                Some(if error {
                    "status-error"
                } else {
                    "status-ready"
                }),
            );
        }
    }

    fn snapshot(&self) -> Result<Investigation, String> {
        let ui = self
            .ui
            .borrow()
            .upgrade()
            .ok_or("The debugger window was closed")?;

        let session = ui
            .model
            .current_session()
            .ok_or("Configure a debug session before saving a workspace")?;

        let memory = ui
            .memory_watches
            .borrow()
            .iter()
            .map(|memory| SavedMemory {
                expression: memory.expression.clone(),
                bytes: memory.byte_count,
                format: memory.format,
            })
            .collect();

        let sources = ui
            .source_documents
            .borrow()
            .iter()
            .map(|document| {
                (
                    document.path.clone(),
                    (document
                        .buffer
                        .iter_at_offset(document.buffer.cursor_position())
                        .line()
                        + 1) as u32,
                )
            })
            .collect();

        Ok(Investigation {
            session,
            breakpoints: Vec::new(),
            watches: ui.expression_watch_expressions(),
            memory,
            sources,
            notes: Vec::new(),
        })
    }

    fn save(self: &Rc<Self>, save_as: bool) {
        if self.incomplete.get() && !save_as {
            self.report(
                "Workspace not overwritten",
                "Some definitions failed to restore. Use Save workspace as \
                 to keep the original definitions intact",
                true,
            );

            return;
        }

        self.busy.set(true);
        let state = Rc::clone(self);

        glib::spawn_future_local(async move {
            let active = state.active.borrow().clone().filter(|_| !save_as);
            let destination = if let Some(active) = active {
                Some((active.0, Some(active.1)))
            } else {
                let Some(ui) = state.ui.borrow().upgrade() else {
                    state.busy.set(false);
                    return;
                };

                let dialog = gtk::FileDialog::builder()
                    .title("Save debugging workspace")
                    .initial_name("debug.fgdb-workspace")
                    .build();

                match dialog.save_future(Some(&ui.action_window())).await {
                    Ok(file) => match file.path() {
                        Some(path) => {
                            match storage::save_revision(&path, !state.incomplete.get()).await {
                                Ok(etag) => Some((path, etag)),
                                Err(error) => {
                                    state.report("Workspace not saved", &error, true);
                                    None
                                }
                            }
                        }
                        None => {
                            state.report("Workspace not saved", "Choose a local file", true);
                            None
                        }
                    },
                    Err(error) => {
                        if !error.matches(gtk::DialogError::Dismissed) {
                            state.report("Workspace not saved", &error.to_string(), true)
                        }

                        None
                    }
                }
            };

            let Some((path, etag)) = destination else {
                state.busy.set(false);
                return;
            };

            let capture = state.capture.borrow().clone();
            let Some(capture) = capture else {
                state.busy.set(false);
                return;
            };

            let callback_state = Rc::clone(&state);

            capture(Box::new(move |result| {
                let state = callback_state;
                let breakpoints = match result {
                    Ok(breakpoints) => breakpoints,
                    Err(error) => {
                        state.busy.set(false);
                        state.report("Workspace not saved", &error, true);
                        return;
                    }
                };

                let mut workspace = match state.snapshot() {
                    Ok(workspace) => workspace,
                    Err(error) => {
                        state.busy.set(false);
                        state.report("Workspace not saved", &error, true);
                        return;
                    }
                };

                workspace.capture_breakpoints(&breakpoints);
                glib::spawn_future_local(async move {
                    let includes_environment = matches!(
                        &workspace.session,
                        DebugSession::Launch { environment, .. } if !environment.is_empty()
                    );

                    if !workspace.notes.is_empty() || includes_environment {
                        let Some(ui) = state.ui.borrow().upgrade() else {
                            state.busy.set(false);
                            return;
                        };

                        let dialog = gtk::AlertDialog::builder()
                            .message("Save reusable workspace definitions?")
                            .detail(format!(
                                "Workspace files include launch settings, environment overrides, \
                                 expressions, and breakpoint commands as plain text. \
                                 New files are private to your user.\n\n{}",
                                workspace.notes.join("\n"),
                            ))
                            .buttons(["Cancel", "Save"])
                            .cancel_button(0)
                            .default_button(0)
                            .build();

                        if dialog.choose_future(Some(&ui.action_window())).await != Ok(1) {
                            state.busy.set(false);
                            return;
                        }
                    }

                    match storage::write(&path, workspace, etag.as_deref()).await {
                        Ok(etag) => {
                            state.set_active(path.clone(), etag, false);
                            state.report("Workspace saved", &path.display().to_string(), false);
                        }

                        Err(error) => state.report("Workspace not saved", &error, true),
                    }

                    state.busy.set(false);
                });
            }));
        });
    }

    fn open(self: &Rc<Self>) {
        self.busy.set(true);
        let state = Rc::clone(self);

        glib::spawn_future_local(async move {
            let Some(ui) = state.ui.borrow().upgrade() else {
                state.busy.set(false);
                return;
            };

            let dialog = gtk::FileDialog::builder()
                .title("Open debugging workspace")
                .build();

            let path = match dialog.open_future(Some(&ui.action_window())).await {
                Ok(file) => file.path(),
                Err(error) => {
                    if !error.matches(gtk::DialogError::Dismissed) {
                        state.report("Workspace not opened", &error.to_string(), true)
                    }

                    None
                }
            };

            let Some(path) = path else {
                state.busy.set(false);
                return;
            };

            match storage::read(&path).await {
                Ok((workspace, etag)) => state.confirm_open(workspace, path, etag),
                Err(error) => {
                    state.busy.set(false);
                    state.report("Workspace not opened", &error, true)
                }
            }
        });
    }

    fn confirm_open(self: &Rc<Self>, workspace: Investigation, path: PathBuf, etag: String) {
        let Some(ui) = self.ui.borrow().upgrade() else {
            self.busy.set(false);
            return;
        };

        let window = gtk::Window::builder()
            .title("Open debugging workspace")
            .transient_for(&ui.action_window())
            .modal(true)
            .default_width(620)
            .build();

        window.add_css_class("session-editor");
        dialogs::connect_escape_to_close(&window);
        let root = gtk::Box::new(gtk::Orientation::Vertical, components::CONTROL_GAP);
        components::inset(&root, components::DIALOG_INSET);

        let summary = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .label(format!(
                "{}\n{}\n{} breakpoints, {} watches, {} memory inspectors\n\n\
                 Opening replaces the session, breakpoints, and watches. \
                 The current launched process may be terminated, or an attached process detached. \
                 Only open workspaces you trust. Expressions and breakpoint commands can execute \
                 target code or GDB commands. Environment overrides are stored in the workspace file.",
                path.display(),
                workspace.session.title(),
                workspace.breakpoints.len(),
                workspace.watches.len(),
                workspace.memory.len(),
            ))
            .build();

        root.append(&summary);
        let pid = gtk::SpinButton::with_range(0.0, f64::from(u32::MAX), 1.0);
        pid.set_numeric(true);

        if matches!(workspace.session, DebugSession::Attach { .. }) {
            root.append(&gtk::Label::new(Some(
                "PID to attach to (not restored from the saved workspace)",
            )));

            root.append(&pid);
        }

        if !workspace.notes.is_empty() {
            let note = gtk::Label::builder()
                .xalign(0.0)
                .wrap(true)
                .label(workspace.notes.join("\n"))
                .build();

            let scroll = gtk::ScrolledWindow::builder()
                .child(&note)
                .max_content_height(150)
                .propagate_natural_height(true)
                .build();

            root.append(&scroll);
        }

        let controls = components::control_row();
        controls.set_halign(gtk::Align::End);
        let cancel = gtk::Button::with_label("Cancel");
        let open = gtk::Button::with_label("Open workspace");
        open.add_css_class("primary-control");

        if matches!(workspace.session, DebugSession::Attach { .. }) {
            open.set_sensitive(false);
            let weak_open = open.downgrade();
            pid.connect_value_changed(move |pid| {
                if let Some(open) = weak_open.upgrade() {
                    open.set_sensitive(pid.value() >= 1.0);
                }
            });
        }

        controls.append(&cancel);
        controls.append(&open);
        root.append(&controls);
        window.set_child(Some(&root));
        let weak_window = window.downgrade();
        cancel.connect_clicked(move |_| {
            if let Some(window) = weak_window.upgrade() {
                window.close()
            }
        });

        let submitted = Rc::new(Cell::new(false));
        let submitted_close = Rc::clone(&submitted);
        let weak = Rc::downgrade(self);

        window.connect_close_request(move |_| {
            if !submitted_close.get()
                && let Some(state) = weak.upgrade()
            {
                state.busy.set(false)
            }

            glib::Propagation::Proceed
        });

        let weak = Rc::downgrade(self);
        let weak_window = window.downgrade();
        open.connect_clicked(move |_| {
            let Some(state) = weak.upgrade() else { return };
            let mut workspace = workspace.clone();

            if let DebugSession::Attach { pid: target, .. } = &mut workspace.session {
                if pid.value() < 1.0 {
                    return;
                }
                *target = pid.value() as u32;
            }

            let restore = state.restore.borrow().clone();
            let Some(restore) = restore else { return };
            submitted.set(true);
            if let Some(window) = weak_window.upgrade() {
                window.close()
            }

            let path = path.clone();
            let etag = etag.clone();
            let callback_state = Rc::clone(&state);

            restore(
                workspace,
                Box::new(move |result| {
                    let state = callback_state;
                    state.busy.set(false);

                    match result {
                        Ok(failures) => {
                            state.set_active(path.clone(), etag, !failures.is_empty());
                            if failures.is_empty() {
                                state.report("Workspace opened", &path.display().to_string(), false)
                            } else {
                                state.report(
                                    "Workspace partially restored",
                                    &failures.join("\n"),
                                    true,
                                )
                            }
                        }

                        Err(error) => {
                            state.incomplete.set(true);
                            state.report("Workspace not opened", &error, true);
                        }
                    }
                }),
            );
        });

        window.present();
    }
}
