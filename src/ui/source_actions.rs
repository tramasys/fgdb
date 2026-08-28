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
        let mut candidates = locations
            .iter()
            .filter_map(|location| {
                let path = self.resolve_source_path(location.source_path())?;
                let score = source_location_score(symbol, location);
                Some((score, path, location))
            })
            .collect::<Vec<_>>();
        candidates.sort_by_key(|candidate| Reverse(candidate.0));
        let Some((_, path, location)) = candidates.first() else {
            self.set_status(
                "Source unavailable",
                &format!(
                    "No source-backed definition for {symbol}. Install matching debuginfo and source files."
                ),
                Some("status-error"),
            );
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
        };
        let Some(document) = open_source_document(path, context) else {
            self.set_status(
                "Source unavailable",
                &format!("Could not read {}", path.display()),
                Some("status-error"),
            );
            return;
        };
        scroll_source_document(&document, location.line);
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
        self.clear_execution_location();
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
            return;
        };
        let path = self.resolve_source_path(reported_path);
        let Some(path) = path else {
            self.status_detail.set_text(&format!(
                "Paused in {} · source unavailable: {reported_path}",
                frame.function
            ));
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
        };
        let Some(document) = open_source_document(&path, context) else {
            self.set_status(
                "Source unavailable",
                &format!("Could not read {}", path.display()),
                Some("status-error"),
            );
            return;
        };
        let source_name = frame
            .file
            .as_deref()
            .unwrap_or(path.as_os_str().to_str().unwrap_or("source"));
        document.tab_label.set_text(&format!(
            "{source_name}:{line} · {}",
            compact_function_name(&frame.function)
        ));
        document.tab.add_css_class("executing-source-tab");
        document.tab_label.set_tooltip_text(Some(&format!(
            "{}\n{}",
            path.to_string_lossy(),
            frame.function
        )));
        let Ok(line) = i32::try_from(line.saturating_sub(1)) else {
            return;
        };
        let Some(iter) = document.buffer.iter_at_line(line) else {
            return;
        };
        let mark = document
            .buffer
            .create_source_mark(None, EXECUTION_CATEGORY, &iter);
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
        for document in self.source_documents.borrow().iter() {
            remove_marks(&document.buffer, EXECUTION_CATEGORY);
            document.breakpoint_renderer.queue_draw();
            document.tab.remove_css_class("executing-source-tab");
            document
                .tab_label
                .set_text(&source_tab_title(&document.path));
            document
                .tab_label
                .set_tooltip_text(Some(&document.path.to_string_lossy()));
        }
    }

    pub fn clear_debugger_state(&self) {
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
        self.show_instructions(&[], "", "", None, false);
        self.show_signal(None, None);
        self.memory_region_store.remove_all();
        self.memory_regions.borrow_mut().clear();
        self.memory_regions_empty.set_visible(true);
        self.clear_kernel_snapshot();
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

    pub(super) fn connect_open_source(&self) {
        let window = self.window.clone();
        let notebook = self.source_notebook.clone();
        let documents = Rc::clone(&self.source_documents);
        let theme = self.source_theme.clone();
        let style_scheme = self.source_style_scheme.clone();
        let breakpoints = Rc::clone(&self.breakpoints);
        let insert_handler = Rc::clone(&self.breakpoint_insert_handler);
        let jump_handler = Rc::clone(&self.source_jump_handler);
        let delete_handler = Rc::clone(&self.breakpoint_delete_handler);
        let enabled_handler = Rc::clone(&self.breakpoint_enabled_handler);
        let symbol_handler = Rc::clone(&self.source_symbol_handler);
        let source_roots = Rc::clone(&self.source_roots);
        let status_label = self.status_label.clone();
        let status_detail = self.status_detail.clone();

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
            if let Some(root) = source_roots.borrow().first() {
                dialog.set_initial_folder(Some(&gio::File::for_path(root)));
            }
            let window = window.clone();
            let notebook = notebook.clone();
            let documents = Rc::clone(&documents);
            let theme = theme.clone();
            let style_scheme = style_scheme.clone();
            let breakpoints = Rc::clone(&breakpoints);
            let insert_handler = Rc::clone(&insert_handler);
            let jump_handler = Rc::clone(&jump_handler);
            let delete_handler = Rc::clone(&delete_handler);
            let enabled_handler = Rc::clone(&enabled_handler);
            let symbol_handler = Rc::clone(&symbol_handler);
            let status_label = status_label.clone();
            let status_detail = status_detail.clone();

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
                    if open_source_document(
                        &path,
                        SourceOpenContext {
                            notebook: &notebook,
                            documents: &documents,
                            theme: &theme,
                            style_scheme: style_scheme.as_ref(),
                            breakpoints: &breakpoints,
                            insert_handler: &insert_handler,
                            jump_handler: &jump_handler,
                            delete_handler: &delete_handler,
                            enabled_handler: &enabled_handler,
                            symbol_handler: &symbol_handler,
                        },
                    )
                    .is_some()
                    {
                        opened += 1;
                    } else {
                        failed.push(path.display().to_string());
                    }
                }
                if failed.is_empty() {
                    set_status_widgets(
                        &status_label,
                        &status_detail,
                        "Source",
                        &format!(
                            "Opened {opened} source file{}",
                            if opened == 1 { "" } else { "s" }
                        ),
                        Some("status-ready"),
                    );
                } else {
                    set_status_widgets(
                        &status_label,
                        &status_detail,
                        "Source open failed",
                        &format!("Could not read {}", failed.join(", ")),
                        Some("status-error"),
                    );
                }
            });
        });
    }
}
