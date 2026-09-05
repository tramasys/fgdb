use super::*;

impl Ui {
    pub(crate) fn source_index_snapshot(&self) -> Option<Arc<source::SourceIndex>> {
        self.source_index.borrow().as_ref().cloned()
    }

    pub(super) fn resolve_source_path(&self, reported_path: &str) -> Option<PathBuf> {
        if let Some(path) = self
            .resolved_source_paths
            .borrow_mut()
            .get_cloned(reported_path)
        {
            return Some(path);
        }

        let indexed = self
            .source_index
            .borrow()
            .as_ref()
            .map(|index| index.resolve_indexed(reported_path));

        let path = match indexed {
            Some(source::SourceResolution::Unique(path)) => path,
            Some(source::SourceResolution::Ambiguous) => {
                self.set_status(
                    "Ambiguous source path",
                    &format!(
                        "More than one indexed file matches {reported_path}. Configure a source directory or substitute path"
                    ),
                    Some("status-error"),
                );

                return None;
            }
            Some(source::SourceResolution::Missing) | None => {
                // Unindexed paths are resolved and validated by the source
                // loader when selected, not while rendering search results.
                return Some(PathBuf::from(reported_path));
            }
        };

        let mut cache = self.resolved_source_paths.borrow_mut();

        // Missing files can appear after a build, debuginfod download, or a
        // substitute-path change. Cache successful work, but never make a
        // transient miss sticky for the rest of the session.
        let evicted = cache.insert(reported_path.to_owned(), path.clone());
        drop(cache);

        if evicted {
            self.record_performance_notice(crate::performance::PerformanceNotice {
                outcome: crate::performance::BudgetOutcome::Evicted,
                operation: String::from("source-path cache"),
                detail: format!(
                    "least-recently used derived path was removed at the {}-entry budget",
                    crate::performance::RESOLVED_SOURCE_PATH_CACHE_BUDGET
                ),
            });
        }

        Some(path)
    }

    pub fn show_source_locations(&self, symbol: &str, locations: &[SourceLocation]) {
        let generation = self
            .source_open_generation
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1);
        let current = Arc::clone(&self.source_open_generation);
        let queued = Arc::clone(&current);
        let roots = self.source_roots.borrow().clone();
        let index = self.source_index_snapshot();
        let locations = locations.to_vec();
        let symbol = symbol.to_owned();
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);

        if let Err(error) = crate::background::submit_cancellable_with_priority(
            crate::background::Priority::Interactive,
            move || queued.load(Ordering::Relaxed) == generation,
            move || {
                let candidate = locations
                    .into_iter()
                    .take_while(|_| current.load(Ordering::Relaxed) == generation)
                    .filter_map(|location| {
                        let path = match index
                            .as_ref()
                            .map(|index| index.resolve(location.source_path()))
                        {
                            Some(source::SourceResolution::Unique(path)) => Some(path),
                            Some(source::SourceResolution::Ambiguous) => None,
                            _ => source::resolve(location.source_path(), &roots),
                        }?;
                        Some((source_location_score(&symbol, &location), path, location))
                    })
                    .reduce(|best, candidate| {
                        if best.0 >= candidate.0 {
                            best
                        } else {
                            candidate
                        }
                    });
                let _ = sender.send(candidate);
            },
        ) {
            self.set_status(
                "Source lookup deferred",
                &error.to_string(),
                Some("status-error"),
            );
            return;
        }

        let weak = self.self_weak.borrow().clone();
        glib::timeout_add_local(Duration::from_millis(10), move || {
            let Some(ui) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if ui.source_open_generation.load(Ordering::Relaxed) != generation {
                return glib::ControlFlow::Break;
            }

            match receiver.try_recv() {
                Ok(Some((_, path, location))) => {
                    let detail = format!("{} · {}:{}", location.function, path.display(), location.line);
                    ui.navigate_to_source_then(&path, location.line, true, move |ui, opened| {
                        if opened { ui.set_status("Source", &detail, Some("status-ready")); }
                    });
                }
                Ok(None) => ui.set_status("Source unavailable", "No readable source-backed definition was found. Install matching debuginfo and source files.", Some("status-error")),
                Err(std::sync::mpsc::TryRecvError::Empty) => return glib::ControlFlow::Continue,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => ui.set_status("Source unavailable", "Source lookup stopped before completing", Some("status-error")),
            }
            glib::ControlFlow::Break
        });
    }

    pub fn show_initial_source(&self, source_file: &SourceFile) {
        if !self.source_documents.borrow().is_empty() {
            return;
        }

        let line = source_file.line;
        self.open_source_when_ready(Path::new(source_file.source_path()), move |ui, document| {
            let Some(document) = document else {
                return;
            };
            scroll_source_document(&document, line);
            ui.set_status(
                "Ready",
                &format!(
                    "Opened {} from the executable's debug information",
                    document.path.display()
                ),
                Some("status-ready"),
            );
        });
    }

    pub fn show_execution_location(&self, frame: &StackFrame) {
        self.model.select_frame(frame.level);

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

        let frame = frame.clone();
        self.open_source_when_ready(Path::new(reported_path), move |ui, document| {
            let Some(document) = document else {
                ui.clear_execution_mark();
                return;
            };
            let path = document.path.clone();
            let same_location = ui.execution_source_line.get() == Some(line)
                && ui.execution_source_path.borrow().as_ref() == Some(&path);

            if !same_location {
                ui.clear_execution_mark();
            }

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

            ui.execution_source_path.replace(Some(path));
            ui.execution_source_line.set(frame.line);
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
        });
    }

    pub fn clear_execution_location(&self) {
        self.source_open_generation.fetch_add(1, Ordering::Relaxed);
        self.model.select_frame(u32::MAX);
        self.current_source_is_rust.set(false);
        update_selected_frame_buttons(&self.frame_buttons.borrow(), u32::MAX);
        self.clear_execution_mark();
    }

    pub fn suspend_execution_location(&self) {
        self.source_open_generation.fetch_add(1, Ordering::Relaxed);
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
        let disassembly_handler = self.disassembly_handler.borrow().clone();

        if let Some(handler) = disassembly_handler {
            handler(DisassemblyRequest::Clear);
        }

        self.start_stop_refresh();
        self.model.start_thread_refresh();
        self.clear_execution_location();
        self.show_frames(&[]);
        self.show_threads(&[]);
        self.show_modules(&[]);
        self.show_locals(&[]);
        self.show_expression_watches_unavailable("<inferior exited>");
        self.show_registers(&[]);
        self.show_stack(&[]);
        self.model.clear_previous_registers();
        self.invalidate_source_io();
        self.show_instructions(Vec::new(), "", "", None, false);
        self.show_signal(None, None);
        self.memory_region_store.remove_all();
        self.model.clear_memory_regions();
        self.memory_regions_empty.set_visible(true);

        self.memory_watch_container
            .refresh_batch
            .borrow_mut()
            .clear();

        update_memory_container_state(&self.memory_watch_container, false);
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

        self.source_navigation.open_file.connect_clicked(move |_| {
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

                let mut paths = std::collections::VecDeque::new();
                let mut failed = Vec::new();
                for index in 0..files.n_items() {
                    if let Some(file) = files.item(index).and_downcast::<gio::File>() {
                        if let Some(path) = file.path() {
                            paths.push_back(path);
                        } else {
                            failed.push(String::from("non-local source"));
                        }
                    }
                }

                if let Some(ui) = weak_ui.upgrade() {
                    ui.open_source_batch(
                        Rc::new(RefCell::new(paths)),
                        Rc::new(RefCell::new((0, failed))),
                    );
                }
            });
        });
    }
}
