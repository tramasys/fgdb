//! Source-file observation is independent of debugger stops and navigation.

use super::*;

pub(in crate::ui) struct SourceFreshness {
    path: PathBuf,
    contents: RefCell<Arc<String>>,
    buffer: sourceview5::Buffer,
    scroll: gtk::ScrolledWindow,
    notice: gtk::Box,
    message: gtk::Label,
    ui: RefCell<std::rc::Weak<Ui>>,
    monitor: RefCell<Option<gio::FileMonitor>>,
    debounce: RefCell<Option<glib::SourceId>>,
    busy: Cell<bool>,
    pending: Cell<bool>,
    force: Cell<bool>,
    generation: Arc<AtomicU64>,
    scroll_restore: RefCell<Option<gtk::TickCallbackId>>,
    changed: Cell<bool>,
}

impl SourceFreshness {
    pub(in crate::ui) fn new(
        path: &Path,
        contents: &Arc<String>,
        buffer: &sourceview5::Buffer,
        scroll: &gtk::ScrolledWindow,
    ) -> Rc<Self> {
        let notice = gtk::Box::new(gtk::Orientation::Horizontal, components::CONTROL_GAP);
        notice.add_css_class("source-change-notice");
        notice.set_visible(false);

        let message = gtk::Label::builder()
            .xalign(0.0)
            .hexpand(true)
            .wrap(true)
            .css_classes(["muted"])
            .build();

        let reload = gtk::Button::with_label("Reload");
        reload.set_valign(gtk::Align::Center);
        let dismiss = gtk::Button::from_icon_name("window-close-symbolic");
        dismiss.set_tooltip_text(Some("Dismiss source notice"));
        dismiss.set_valign(gtk::Align::Center);
        notice.append(&message);
        notice.append(&reload);
        notice.append(&dismiss);

        let state = Rc::new(Self {
            path: path.to_owned(),
            contents: RefCell::new(Arc::clone(contents)),
            buffer: buffer.clone(),
            scroll: scroll.clone(),
            notice,
            message,
            ui: RefCell::new(std::rc::Weak::new()),
            monitor: RefCell::new(None),
            debounce: RefCell::new(None),
            busy: Cell::new(false),
            pending: Cell::new(false),
            force: Cell::new(false),
            generation: Arc::new(AtomicU64::new(0)),
            scroll_restore: RefCell::new(None),
            changed: Cell::new(false),
        });

        let weak = Rc::downgrade(&state);
        reload.connect_clicked(move |_| {
            if let Some(state) = weak.upgrade() {
                state.reload();
            }
        });

        let weak = Rc::downgrade(&state);
        dismiss.connect_clicked(move |_| {
            if let Some(state) = weak.upgrade() {
                state.notice.set_visible(false);
            }
        });

        state
    }

    pub(in crate::ui) fn notice(&self) -> &gtk::Box {
        &self.notice
    }

    pub(in crate::ui) fn bind(self: &Rc<Self>, ui: &Rc<Ui>) {
        if self.ui.borrow().upgrade().is_some() {
            return;
        }

        self.ui.replace(Rc::downgrade(ui));
        let file = gio::File::for_path(&self.path);
        let Some(parent) = file.parent() else { return };

        // Watching the directory also observes editors that replace files atomically.
        match parent.monitor_directory(
            gio::FileMonitorFlags::WATCH_MOVES,
            None::<&gio::Cancellable>,
        ) {
            Ok(monitor) => {
                let weak = Rc::downgrade(self);
                monitor.connect_changed(move |_, changed, other, _| {
                    if (file.equal(changed) || other.is_some_and(|other| file.equal(other)))
                        && let Some(state) = weak.upgrade()
                    {
                        state.schedule();
                    }
                });

                self.monitor.replace(Some(monitor));
            }

            Err(error) => self.show(&format!(
                "Source monitoring unavailable: {error}. Use Reload source to check for edits"
            )),
        }

        self.check();
    }

    fn show(&self, message: &str) {
        self.message.set_text(message);
        self.notice.set_visible(true);
    }

    fn show_unverified(&self) {
        self.show("Source reloaded from disk. Its match to the loaded executable is not verified");
    }

    fn schedule(self: &Rc<Self>) {
        self.generation.fetch_add(1, Ordering::Relaxed);

        if let Some(source) = self.debounce.borrow_mut().take() {
            source.remove();
        }

        let weak = Rc::downgrade(self);
        let source = glib::timeout_add_local_once(Duration::from_millis(200), move || {
            if let Some(state) = weak.upgrade() {
                state.debounce.borrow_mut().take();
                state.check();
            }
        });

        self.debounce.replace(Some(source));
    }

    pub(in crate::ui) fn reload(self: &Rc<Self>) {
        self.generation.fetch_add(1, Ordering::Relaxed);
        self.force.set(true);
        self.check();
    }

    pub(in crate::ui) fn check(self: &Rc<Self>) {
        if self.busy.replace(true) {
            self.pending.set(true);
            return;
        }

        let path = self.path.clone();
        let old = Arc::clone(&self.contents.borrow());
        let current = Arc::clone(&self.generation);
        let generation = current.load(Ordering::Relaxed);
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);

        if let Err(error) = crate::background::submit_cancellable_with_priority(
            crate::background::Priority::Background,
            move || current.load(Ordering::Relaxed) == generation,
            move || {
                let result = super::loading::load_source(&path, &[], None, 16 * 1024 * 1024)
                    .map(|(_, source)| (source.contents != old, source));

                let _ = sender.send(result);
            },
        ) {
            self.busy.set(false);
            self.force.set(false);
            self.show(&format!(
                "Source check deferred: {error}. Use Reload to retry"
            ));

            return;
        }

        let weak = Rc::downgrade(self);
        glib::timeout_add_local(Duration::from_millis(30), move || {
            let Some(state) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };

            let result = match receiver.try_recv() {
                Ok(result) => result,
                Err(std::sync::mpsc::TryRecvError::Empty) => return glib::ControlFlow::Continue,
                Err(_) => Err(String::from("Source check stopped. Use Reload to retry")),
            };

            state.busy.set(false);

            if generation == state.generation.load(Ordering::Relaxed) {
                match result {
                    Ok((changed, snapshot)) => {
                        let force = state.force.replace(false);
                        let auto = state
                            .ui
                            .borrow()
                            .upgrade()
                            .is_some_and(|ui| ui.settings.source_auto_reload());

                        if changed && (force || auto) {
                            state.replace(Arc::clone(&snapshot.contents));

                            if let Some(ui) = state.ui.borrow().upgrade() {
                                ui.reload_source_annotations(&state.path, snapshot);
                            }
                        } else if changed {
                            state.show(
                                "Source changed on disk. Reload to view it. \
                                 The loaded executable has not been rebuilt by fgdb",
                            );
                        } else if state.changed.get() && (force || state.notice.is_visible()) {
                            state.show_unverified();
                        } else if !state.changed.get()
                            && (force || state.monitor.borrow().is_some())
                        {
                            state.notice.set_visible(false);
                        }
                    }

                    Err(error) => {
                        state.force.set(false);
                        state.show(&error);
                    }
                }
            }

            if state.pending.replace(false)
                || generation != state.generation.load(Ordering::Relaxed)
            {
                state.check();
            }

            glib::ControlFlow::Break
        });
    }

    fn replace(self: &Rc<Self>, contents: Arc<String>) {
        let cursor = self.buffer.iter_at_mark(&self.buffer.get_insert());
        let bound = self.buffer.iter_at_mark(&self.buffer.selection_bound());
        let positions = [
            (cursor.line(), cursor.line_offset()),
            (bound.line(), bound.line_offset()),
        ];

        let vertical = self.scroll.vadjustment().value();
        let horizontal = self.scroll.hadjustment().value();
        remove_marks(&self.buffer, EXECUTION_CATEGORY);
        self.buffer.set_text(&contents);
        self.buffer.set_modified(false);
        self.contents.replace(contents);

        let at = |(line, column): (i32, i32)| {
            let mut iter = self
                .buffer
                .iter_at_line(line.min(self.buffer.line_count() - 1))
                .unwrap_or_else(|| self.buffer.end_iter());

            if !iter.ends_line() {
                iter.forward_to_line_end();
            }

            let offset = column.min(iter.line_offset());
            iter.set_line_offset(offset);
            iter
        };

        self.buffer
            .select_range(&at(positions[0]), &at(positions[1]));

        self.changed.set(true);
        self.show_unverified();

        if let Some(ui) = self.ui.borrow().upgrade() {
            // Gutter locations already use source line numbers. No filesystem
            // work is needed to redraw them after replacing the text.
            if ui.execution_source_path.borrow().as_ref() == Some(&self.path)
                && let Some(line) = ui.execution_source_line.get()
                && let Ok(line) = i32::try_from(line.saturating_sub(1))
                && let Some(iter) = self.buffer.iter_at_line(line)
            {
                self.buffer
                    .create_source_mark(None, EXECUTION_CATEGORY, &iter);
            }
        }

        // Restore adjustments after GTK has measured the replacement text.
        if let Some(previous) = self.scroll_restore.borrow_mut().take() {
            previous.remove();
        }

        let weak = Rc::downgrade(self);
        let measured = Cell::new(false);
        let callback = self.scroll.add_tick_callback(move |_, _| {
            if !measured.replace(true) {
                return glib::ControlFlow::Continue;
            }

            if let Some(state) = weak.upgrade() {
                state.scroll_restore.borrow_mut().take();
                state.scroll.vadjustment().set_value(vertical);
                state.scroll.hadjustment().set_value(horizontal);
            }

            glib::ControlFlow::Break
        });

        self.scroll_restore.replace(Some(callback));
    }

    pub(in crate::ui) fn edit(&self) {
        let Some(ui) = self.ui.borrow().upgrade() else {
            return;
        };

        let command = ui.settings.source_editor();
        let cursor = self.buffer.iter_at_offset(self.buffer.cursor_position());

        if command.trim().is_empty() {
            let launcher = gtk::FileLauncher::new(Some(&gio::File::for_path(&self.path)));
            launcher.set_writable(true);
            launcher.launch(Some(&ui.action_window()), None::<&gio::Cancellable>, {
                let weak = Rc::downgrade(&ui);
                move |result| {
                    if let Err(error) = result
                        && let Some(ui) = weak.upgrade()
                    {
                        ui.set_status(
                            "Editor unavailable",
                            &error.to_string(),
                            Some("status-error"),
                        );
                    }
                }
            });

            return;
        }

        let result = editor_arguments(
            &command,
            &self.path,
            cursor.line() + 1,
            cursor.line_offset() + 1,
        )
        .and_then(|arguments| {
            let arguments: Vec<_> = arguments.iter().map(std::ffi::OsStr::new).collect();
            gio::Subprocess::newv(&arguments, gio::SubprocessFlags::NONE)
                .map_err(|error| error.to_string())
        });

        if let Err(error) = result {
            ui.set_status("Editor unavailable", &error, Some("status-error"));
        }
    }
}

fn editor_arguments(
    command: &str,
    path: &Path,
    line: i32,
    column: i32,
) -> Result<Vec<String>, String> {
    let mut arguments = shell_words::split(command).map_err(|error| error.to_string())?;
    let path = path
        .to_str()
        .ok_or("External editor placeholders require a UTF-8 source path")?;

    if arguments.is_empty() {
        return Err(String::from(
            "Configure an external editor command in Settings",
        ));
    }

    let has_file = arguments.iter().any(|argument| argument.contains("{file}"));
    let line = line.to_string();
    let column = column.to_string();

    for argument in &mut arguments {
        *argument = argument
            .replace("{line}", &line)
            .replace("{column}", &column)
            .replace("{file}", path);
    }

    if !has_file {
        arguments.push(path.to_owned());
    }

    Ok(arguments)
}

impl Drop for SourceFreshness {
    fn drop(&mut self) {
        self.generation.fetch_add(1, Ordering::Relaxed);

        if let Some(callback) = self.scroll_restore.get_mut().take() {
            callback.remove();
        }

        if let Some(source) = self.debounce.get_mut().take() {
            source.remove();
        }

        if let Some(monitor) = self.monitor.get_mut().take() {
            monitor.cancel();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_editor_arguments_do_not_interpret_source_paths_as_shell_code() {
        let path = Path::new("/tmp/file with spaces;$(command).rs");
        assert_eq!(
            editor_arguments("code --goto {file}:{line}:{column}", path, 12, 3).unwrap(),
            ["code", "--goto", "/tmp/file with spaces;$(command).rs:12:3"]
        );
        assert_eq!(
            editor_arguments("editor --", path, 1, 1).unwrap(),
            ["editor", "--", path.to_str().unwrap()]
        );
        assert!(editor_arguments("'unterminated", path, 1, 1).is_err());
    }

    #[test]
    #[ignore = "requires a GTK display"]
    fn reload_preserves_selection_and_clamps_shortened_lines() {
        gtk::init().unwrap();
        let buffer = sourceview5::Buffer::new(None::<&gtk::TextTagTable>);
        buffer.set_text("first line\nsecond line\nthird line\n");
        let view = sourceview5::View::with_buffer(&buffer);
        let scroll = gtk::ScrolledWindow::builder().child(&view).build();
        let state = SourceFreshness::new(
            Path::new("/tmp/source.c"),
            &Arc::new("first line\nsecond line\nthird line\n".into()),
            &buffer,
            &scroll,
        );

        let start = buffer.iter_at_line_offset(0, 4).unwrap();
        let end = buffer.iter_at_line_offset(1, 8).unwrap();
        buffer.create_source_mark(None, EXECUTION_CATEGORY, &end);
        buffer.select_range(&end, &start);
        state.replace(Arc::new("first changed\nshort\n".into()));
        let cursor = buffer.iter_at_mark(&buffer.get_insert());
        let bound = buffer.iter_at_mark(&buffer.selection_bound());
        assert_eq!((cursor.line(), cursor.line_offset()), (1, 5));
        assert_eq!((bound.line(), bound.line_offset()), (0, 4));
        assert!(state.notice.is_visible());
        assert!(!buffer.is_modified());
        assert!(
            buffer
                .source_marks_at_line(0, Some(EXECUTION_CATEGORY))
                .is_empty()
        );

        buffer.place_cursor(&buffer.iter_at_line_offset(0, 4).unwrap());
        state.replace(Arc::new("\nnext line\n".into()));
        let cursor = buffer.iter_at_mark(&buffer.get_insert());
        assert_eq!((cursor.line(), cursor.line_offset()), (0, 0));
    }
}
