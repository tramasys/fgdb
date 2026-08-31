use super::*;

impl Ui {
    pub(super) fn resolve_source_path(&self, reported_path: &str) -> Option<PathBuf> {
        const MAX_RESOLVED_SOURCE_PATHS: usize = 4_096;

        if let Some(path) = self
            .resolved_source_paths
            .borrow()
            .get(reported_path)
            .cloned()
        {
            return path;
        }
        let path = source::resolve(reported_path, &self.source_roots.borrow());
        let mut cache = self.resolved_source_paths.borrow_mut();
        if cache.len() >= MAX_RESOLVED_SOURCE_PATHS {
            cache.clear();
        }
        cache.insert(reported_path.to_owned(), path.clone());
        path
    }

    pub fn show_source_locations(&self, symbol: &str, locations: &[SourceLocation]) {
        let candidate = locations
            .iter()
            .filter_map(|location| {
                let path = self.resolve_source_path(location.source_path())?;
                let score = source_location_score(symbol, location);
                Some((score, path, location))
            })
            // Preserve the stable-sort behavior for equal scores by retaining
            // the first candidate rather than Iterator::max_by_key's last one.
            .reduce(|best, candidate| {
                if best.0 >= candidate.0 {
                    best
                } else {
                    candidate
                }
            });
        let Some((_, path, location)) = candidate else {
            self.set_status(
                "Source unavailable",
                &format!(
                    "No source-backed definition for {symbol}. Install matching debuginfo and source files."
                ),
                Some("status-error"),
            );
            return;
        };
        if !self.navigate_to_source(&path, location.line, true) {
            return;
        }
        self.set_status(
            "Source",
            &format!(
                "{} · {}:{}",
                location.function,
                path.display(),
                location.line
            ),
            Some("status-ready"),
        );
    }

    pub fn show_initial_source(&self, source_file: &SourceFile) {
        if !self.source_documents.borrow().is_empty() {
            return;
        }
        let Some(path) = self.resolve_source_path(source_file.source_path()) else {
            return;
        };
        let context = SourceOpenContext {
            notebook: &self.source_notebook,
            documents: &self.source_documents,
            theme: &self.source_theme,
            style_scheme: self.source_style_scheme.as_ref(),
            breakpoints: &self.breakpoints,
            insert_handler: &self.breakpoint_insert_handler,
            jump_handler: &self.source_jump_handler,
            delete_handler: &self.breakpoint_delete_handler,
            enabled_handler: &self.breakpoint_enabled_handler,
            symbol_handler: &self.source_symbol_handler,
            closed_tabs: &self.closed_source_tabs,
            reopen_closed: &self.source_navigation.reopen_closed,
        };
        let Some(document) = open_source_document(&path, context) else {
            return;
        };
        scroll_source_document(&document, source_file.line);
        self.set_status(
            "Ready",
            &format!(
                "Opened {} from the executable's debug information",
                path.display()
            ),
            Some("status-ready"),
        );
    }

    pub fn show_execution_location(&self, frame: &StackFrame) {
        self.selected_frame_level.set(frame.level);
        self.current_source_is_rust.set(
            frame
                .source_path()
                .and_then(|path| Path::new(path).extension())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("rs")),
        );
        if let Some(description) = frame.architecture.as_deref() {
            let architecture = TargetArchitecture::from_gdb_description(description);
            if architecture != TargetArchitecture::Unknown {
                self.set_target_architecture(architecture);
            }
            if let Some(bits) =
                TargetArchitecture::explicit_pointer_bits_from_gdb_description(description)
            {
                self.set_target_pointer_bits(bits);
            }
            if let Some(endian) = TargetEndian::from_architecture_description(description) {
                self.set_target_endian(Some(endian));
            }
        }
        update_selected_frame_buttons(&self.frame_buttons.borrow(), frame.level);
        let (Some(reported_path), Some(line)) = (frame.source_path(), frame.line) else {
            self.clear_execution_mark();
            return;
        };
        let path = self.resolve_source_path(reported_path);
        let Some(path) = path else {
            self.clear_execution_mark();
            self.status_detail.set_text(&format!(
                "Paused in {} · source unavailable: {reported_path}",
                frame.function
            ));
            return;
        };
        let same_location = self.execution_source_line.get() == Some(line)
            && self.execution_source_path.borrow().as_ref() == Some(&path);
        if !same_location {
            self.clear_execution_mark();
        }
        let context = SourceOpenContext {
            notebook: &self.source_notebook,
            documents: &self.source_documents,
            theme: &self.source_theme,
            style_scheme: self.source_style_scheme.as_ref(),
            breakpoints: &self.breakpoints,
            insert_handler: &self.breakpoint_insert_handler,
            jump_handler: &self.source_jump_handler,
            delete_handler: &self.breakpoint_delete_handler,
            enabled_handler: &self.breakpoint_enabled_handler,
            symbol_handler: &self.source_symbol_handler,
            closed_tabs: &self.closed_source_tabs,
            reopen_closed: &self.source_navigation.reopen_closed,
        };
        let Some(document) = open_source_document(&path, context) else {
            self.clear_execution_mark();
            self.set_status(
                "Source unavailable",
                &format!("Could not read {}", path.display()),
                Some("status-error"),
            );
            return;
        };
        document.tab.add_css_class("executing-source-tab");
        document.tab_label.set_tooltip_text(Some(&format!(
            "{}\n{} at line {line}",
            path.to_string_lossy(),
            frame.function
        )));
        let mark_present = i32::try_from(line.saturating_sub(1))
            .ok()
            .is_some_and(|line| {
                !document
                    .buffer
                    .source_marks_at_line(line, Some(EXECUTION_CATEGORY))
                    .is_empty()
            });
        if same_location && mark_present {
            return;
        }
        let Ok(line) = i32::try_from(line.saturating_sub(1)) else {
            return;
        };
        let Some(iter) = document.buffer.iter_at_line(line) else {
            return;
        };
        let mark = document
            .buffer
            .create_source_mark(None, EXECUTION_CATEGORY, &iter);
        self.execution_source_path.replace(Some(path));
        self.execution_source_line.set(frame.line);
        document.breakpoint_renderer.queue_draw();
        document.buffer.place_cursor(&iter);
        let source_view = document.view;
        gtk::glib::idle_add_local_once(move || {
            if mark
                .buffer()
                .is_some_and(|buffer| buffer == source_view.buffer())
            {
                source_view.scroll_to_mark(&mark, 0.15, true, 0.0, 0.35);
            }
        });
    }

    pub fn clear_execution_location(&self) {
        self.selected_frame_level.set(u32::MAX);
        self.current_source_is_rust.set(false);
        update_selected_frame_buttons(&self.frame_buttons.borrow(), u32::MAX);
        self.clear_execution_mark();
    }

    pub fn suspend_execution_location(&self) {
        // Keep the selected frame row stable across a short execution command.
        // The stopped-state refresh updates it when GDB reports the next frame.
        // Removing and immediately restoring this class made the blue row flash
        // on every step even though the sidebar contents stayed visible.
        self.current_source_is_rust.set(false);
        self.execution_source_line.set(None);
        let path = self.execution_source_path.borrow();
        for document in self
            .source_documents
            .borrow()
            .iter()
            .filter(|document| path.as_ref().is_some_and(|path| document.path == *path))
        {
            remove_marks(&document.buffer, EXECUTION_CATEGORY);
            document.breakpoint_renderer.queue_draw();
        }
    }

    fn clear_execution_mark(&self) {
        let path = self.execution_source_path.borrow_mut().take();
        self.execution_source_line.set(None);
        for document in self
            .source_documents
            .borrow()
            .iter()
            .filter(|document| path.as_ref().is_some_and(|path| document.path == *path))
        {
            remove_marks(&document.buffer, EXECUTION_CATEGORY);
            document.breakpoint_renderer.queue_draw();
            document.tab.remove_css_class("executing-source-tab");
            document
                .tab_label
                .set_tooltip_text(Some(&document.path.to_string_lossy()));
        }
    }

    pub fn clear_debugger_state(&self) {
        self.reset_thread_analysis();
        self.clear_thread_action_pending();
        self.defer_displayed_variable_object_deletions();
        if let Some(handler) = self.disassembly_handler.borrow().as_ref() {
            handler(DisassemblyRequest::Clear);
        }
        self.start_stop_refresh();
        self.start_thread_refresh();
        self.clear_execution_location();
        self.show_frames(&[]);
        self.show_threads(&[]);
        self.show_modules(&[]);
        self.show_locals(&[]);
        self.show_expression_watches_unavailable("<inferior exited>");
        self.show_registers(&[]);
        self.show_stack(&[]);
        self.previous_registers.borrow_mut().clear();
        self.disassembly_source_cache.borrow_mut().clear();
        self.show_instructions(Vec::new(), "", "", None, false);
        self.show_signal(None, None);
        self.memory_region_store.remove_all();
        self.memory_regions.borrow_mut().clear();
        self.memory_regions_empty.set_visible(true);
        self.clear_kernel_snapshot();
        self.clear_misc_snapshot();
        for watch in self.memory_watches.borrow().iter() {
            watch.status.remove_css_class("memory-watch-error");
            watch.status.set_text("target is not paused");
            watch.range.set_text("");
            watch.store.remove_all();
            watch.selection.set_selected(gtk::INVALID_LIST_POSITION);
            watch.follow_button.set_sensitive(false);
            watch.previous_begin.set(None);
            watch.previous_bytes.borrow_mut().clear();
        }
    }

    fn defer_displayed_variable_object_deletions(&self) {
        let mut deferred = self.deferred_variable_object_deletions.borrow_mut();
        deferred.extend(
            self.local_variable_objects()
                .into_iter()
                .chain(self.expression_watch_variable_objects())
                .filter_map(|variable| variable.varobj),
        );
    }

    pub(crate) fn take_deferred_variable_object_deletions(&self) -> Vec<String> {
        self.deferred_variable_object_deletions
            .borrow_mut()
            .drain()
            .collect()
    }

    pub(crate) fn defer_variable_object_deletions(
        &self,
        variable_objects: impl IntoIterator<Item = String>,
    ) {
        self.deferred_variable_object_deletions
            .borrow_mut()
            .extend(variable_objects);
    }

    pub(super) fn connect_open_source(self: &Rc<Self>) {
        let weak_ui = Rc::downgrade(self);
        self.open_source_button.connect_clicked(move |_| {
            let dialog = gtk::FileDialog::builder()
                .title("Open source files")
                .modal(true)
                .build();
            let source_filter = gtk::FileFilter::new();
            source_filter.set_name(Some("Source files"));
            for pattern in [
                "*.c", "*.h", "*.cc", "*.cpp", "*.cxx", "*.hpp", "*.hh", "*.rs", "*.s", "*.S",
                "*.asm", "*.inc", "*.inl", "*.m", "*.mm", "*.go", "*.zig",
            ] {
                source_filter.add_pattern(pattern);
            }
            let all_filter = gtk::FileFilter::new();
            all_filter.set_name(Some("All files"));
            all_filter.add_pattern("*");
            let filters = gio::ListStore::new::<gtk::FileFilter>();
            filters.append(&source_filter);
            filters.append(&all_filter);
            dialog.set_filters(Some(&filters));
            dialog.set_default_filter(Some(&source_filter));
            let Some(ui) = weak_ui.upgrade() else {
                return;
            };
            if let Some(root) = ui.source_roots.borrow().first() {
                dialog.set_initial_folder(Some(&gio::File::for_path(root)));
            }
            let window = ui.window.clone();
            let weak_ui = Rc::downgrade(&ui);
            drop(ui);

            gtk::glib::spawn_future_local(async move {
                let Ok(files) = dialog.open_multiple_future(Some(&window)).await else {
                    return;
                };
                let mut opened = 0_u32;
                let mut failed = Vec::new();
                for index in 0..files.n_items() {
                    let Some(file) = files.item(index).and_downcast::<gio::File>() else {
                        continue;
                    };
                    let Some(path) = file.path() else {
                        failed.push(String::from("non-local source"));
                        continue;
                    };
                    let Some(ui) = weak_ui.upgrade() else {
                        return;
                    };
                    if ui.navigate_to_source(&path, 1, true) {
                        opened += 1;
                    } else {
                        failed.push(path.display().to_string());
                    }
                }
                let Some(ui) = weak_ui.upgrade() else {
                    return;
                };
                if failed.is_empty() {
                    ui.set_status(
                        "Source",
                        &format!(
                            "Opened {opened} source file{}",
                            if opened == 1 { "" } else { "s" }
                        ),
                        Some("status-ready"),
                    );
                } else {
                    ui.set_status(
                        "Source open failed",
                        &format!("Could not read {}", failed.join(", ")),
                        Some("status-error"),
                    );
                }
            });
        });
    }
}
