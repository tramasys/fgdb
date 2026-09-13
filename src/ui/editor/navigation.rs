use std::{
    sync::mpsc::{self, TryRecvError},
    time::Duration,
};

use super::*;

const MAX_SOURCE_HISTORY: usize = 256;
const MAX_SOURCE_TREE_FILES: usize = 20_000;
const MAX_SOURCE_RESULTS: usize = 200;

impl Ui {
    pub(in crate::ui) fn connect_source_navigation(self: &Rc<Self>) {
        let weak_ui = Rc::downgrade(self);

        self.source.navigation.back.connect_clicked(move |_| {
            if let Some(ui) = weak_ui.upgrade() {
                ui.navigate_source_history(false);
            }
        });

        let weak_ui = Rc::downgrade(self);

        self.source.navigation.forward.connect_clicked(move |_| {
            if let Some(ui) = weak_ui.upgrade() {
                ui.navigate_source_history(true);
            }
        });

        let weak_ui = Rc::downgrade(self);

        self.source.navigation.quick_open.connect_clicked(move |_| {
            if let Some(ui) = weak_ui.upgrade() {
                ui.present_source_palette(SourceSearchMode::Files);
            }
        });

        let weak_ui = Rc::downgrade(self);

        self.source.navigation.find.connect_clicked(move |_| {
            if let Some(ui) = weak_ui.upgrade() {
                ui.present_source_find();
            }
        });

        let weak_ui = Rc::downgrade(self);

        self.source.navigation.go_to_line.connect_clicked(move |_| {
            if let Some(ui) = weak_ui.upgrade() {
                ui.present_go_to_line();
            }
        });

        let weak_ui = Rc::downgrade(self);

        self.source.navigation.symbols.connect_clicked(move |_| {
            if let Some(ui) = weak_ui.upgrade() {
                ui.present_source_palette(SourceSearchMode::Symbols);
            }
        });

        let weak_ui = Rc::downgrade(self);

        self.source
            .navigation
            .loaded_search
            .connect_clicked(move |_| {
                if let Some(ui) = weak_ui.upgrade() {
                    ui.present_source_palette(SourceSearchMode::LoadedText);
                }
            });

        let weak_ui = Rc::downgrade(self);

        self.source
            .navigation
            .tree_search
            .connect_clicked(move |_| {
                if let Some(ui) = weak_ui.upgrade() {
                    ui.present_source_palette(SourceSearchMode::Tree);
                }
            });

        let weak_ui = Rc::downgrade(self);

        self.source
            .navigation
            .reopen_closed
            .connect_clicked(move |_| {
                if let Some(ui) = weak_ui.upgrade() {
                    ui.reopen_last_source_tab();
                }
            });

        let weak_ui = Rc::downgrade(self);

        self.source.navigation.find_entry.connect_changed(move |_| {
            if let Some(ui) = weak_ui.upgrade() {
                ui.source.update_find(false);
            }
        });

        let next = self.source.navigation.find_next.clone();

        self.source
            .navigation
            .find_entry
            .connect_activate(move |_| next.emit_clicked());

        let weak_ui = Rc::downgrade(self);

        self.source.navigation.find_next.connect_clicked(move |_| {
            if let Some(ui) = weak_ui.upgrade() {
                ui.source.move_find(true);
            }
        });

        let weak_ui = Rc::downgrade(self);

        self.source
            .navigation
            .find_previous
            .connect_clicked(move |_| {
                if let Some(ui) = weak_ui.upgrade() {
                    ui.source.move_find(false);
                }
            });

        let weak_ui = Rc::downgrade(self);

        self.source.navigation.find_case.connect_toggled(move |_| {
            if let Some(ui) = weak_ui.upgrade() {
                ui.source.update_find(false);
            }
        });

        let weak_ui = Rc::downgrade(self);

        self.source.navigation.find_close.connect_clicked(move |_| {
            if let Some(ui) = weak_ui.upgrade() {
                ui.source.close_find();
            }
        });

        let weak_ui = Rc::downgrade(self);

        self.source
            .tree
            .open_handler
            .replace(Some(Rc::new(move |path| {
                if let Some(ui) = weak_ui.upgrade() {
                    ui.navigate_to_source(&path, 1, true);
                }
            })));

        let weak_ui = Rc::downgrade(self);

        self.source
            .tree
            .search_handler
            .replace(Some(Rc::new(move |directory| {
                if let Some(ui) = weak_ui.upgrade() {
                    ui.present_source_palette_scoped(SourceSearchMode::Tree, Some(directory));
                }
            })));

        let weak_ui = Rc::downgrade(self);

        self.source
            .tree
            .refresh_handler
            .replace(Some(Rc::new(move || {
                if let Some(ui) = weak_ui.upgrade() {
                    ui.refresh_source_tree();
                }
            })));

        let weak_ui = Rc::downgrade(self);

        self.source.tree.root.connect_map(move |_| {
            let Some(ui) = weak_ui.upgrade() else {
                return;
            };

            if !ui.source.tree_initialized.replace(true) {
                ui.source.tree.status.set_text("Indexing source files");
                ui.request_loaded_source_files();
            }

            ui.start_source_tree_index();
        });

        let weak_ui = Rc::downgrade(self);

        self.source.tree.search.connect_changed(move |_| {
            let Some(ui) = weak_ui.upgrade() else {
                return;
            };

            let generation = ui
                .source
                .tree_render_generation
                .fetch_add(1, Ordering::Relaxed)
                .wrapping_add(1);

            let weak_ui = Rc::downgrade(&ui);

            gtk::glib::timeout_add_local_once(Duration::from_millis(140), move || {
                if let Some(ui) = weak_ui.upgrade()
                    && ui.source.tree_render_generation.load(Ordering::Relaxed) == generation
                {
                    ui.render_source_tree();
                }
            });
        });

        let weak_ui = Rc::downgrade(self);

        self.source.notebook.connect_switch_page(move |_, _, _| {
            let weak_ui = weak_ui.clone();

            gtk::glib::idle_add_local_once(move || {
                if let Some(ui) = weak_ui.upgrade() {
                    ui.source.sync_tree_selection();

                    if ui.source.navigation.find_bar.is_visible() {
                        ui.source.update_find(false);
                    }
                }
            });
        });
    }

    pub(in crate::ui) fn present_source_find(&self) {
        let Some(document) = self.source.current_document() else {
            self.set_status(
                "Find unavailable",
                "Open a source file before searching within it",
                Some("status-error"),
            );

            return;
        };

        self.source.navigation.find_bar.set_visible(true);

        if self.source.navigation.find_entry.text().is_empty()
            && let Some((start, end)) = document.buffer.selection_bounds()
        {
            let selected = document.buffer.text(&start, &end, false);

            if !selected.contains('\n') && selected.chars().count() <= 160 {
                self.source.navigation.find_entry.set_text(&selected);
            }
        }

        self.source.navigation.find_entry.grab_focus();
        self.source.update_find(false);
    }

    fn present_go_to_line(self: &Rc<Self>) {
        let Some(document) = self.source.current_document() else {
            self.set_status(
                "Line navigation unavailable",
                "Open a source file before going to a line",
                Some("status-error"),
            );

            return;
        };

        let dialog = gtk::Window::builder()
            .title("Go to source line")
            .transient_for(&self.panel_window(PanelId::Editor))
            .modal(true)
            .default_width(380)
            .build();

        self.panels.track_dialog(PanelId::Editor, &dialog);
        let content = gtk::Box::new(gtk::Orientation::Vertical, 7);
        components::inset(&content, components::DIALOG_INSET);

        let detail = gtk::Label::new(Some(&format!(
            "{}  lines 1 to {}",
            document.path.display(),
            document.buffer.line_count()
        )));

        detail.add_css_class("muted");
        detail.set_halign(gtk::Align::Start);
        detail.set_ellipsize(pango::EllipsizeMode::Middle);
        content.append(&detail);

        let entry = gtk::Entry::builder()
            .placeholder_text("Line number")
            .activates_default(true)
            .build();

        let current_line = document
            .buffer
            .iter_at_offset(document.buffer.cursor_position())
            .line()
            .saturating_add(1);

        entry.set_text(&current_line.to_string());
        entry.select_region(0, -1);
        content.append(&entry);
        let validation = gtk::Label::new(None);
        validation.add_css_class("configuration-error");
        validation.set_halign(gtk::Align::Start);
        content.append(&validation);
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        actions.set_halign(gtk::Align::End);
        let cancel = gtk::Button::with_label("Cancel");
        let go = gtk::Button::with_label("Go");
        go.add_css_class("inline-action");
        go.set_receives_default(true);
        actions.append(&cancel);
        actions.append(&go);
        content.append(&actions);
        dialog.set_default_widget(Some(&go));
        dialog.set_child(Some(&content));
        let dialog_for_cancel = dialog.clone();
        cancel.connect_clicked(move |_| dialog_for_cancel.close());
        let weak_ui = Rc::downgrade(self);
        let path = document.path;
        let line_count = u32::try_from(document.buffer.line_count()).unwrap_or(u32::MAX);
        let dialog_for_go = dialog.clone();
        let entry_for_go = entry.clone();
        let validation_for_go = validation;

        go.connect_clicked(move |_| {
            let Some(ui) = weak_ui.upgrade() else {
                return;
            };

            let Some(line) = entry_for_go
                .text()
                .trim()
                .parse::<u32>()
                .ok()
                .filter(|line| (1..=line_count).contains(line))
            else {
                validation_for_go.set_text(&format!("Enter a line from 1 to {line_count}"));
                return;
            };

            if ui.navigate_to_source(&path, line, true) {
                dialog_for_go.close();
            }
        });

        dialog.present();
        entry.grab_focus();
    }

    fn reopen_last_source_tab(&self) {
        let Some(closed) = self.source.closed_tabs.borrow().last().cloned() else {
            self.source.navigation.reopen_closed.set_sensitive(false);
            return;
        };
        let expected = closed.clone();
        self.navigate_to_source_then(&closed.path, closed.line, true, move |ui, opened| {
            if opened && ui.source.closed_tabs.borrow().last() == Some(&expected) {
                ui.source.closed_tabs.borrow_mut().pop();
            }
            ui.source
                .navigation
                .reopen_closed
                .set_sensitive(!ui.source.closed_tabs.borrow().is_empty());
        });
    }

    pub(in crate::ui) fn navigate_to_source(&self, path: &Path, line: u32, record: bool) -> bool {
        self.navigate_to_source_then(path, line, record, |_, _| {})
    }

    pub(in crate::ui) fn navigate_to_source_then(
        &self,
        path: &Path,
        line: u32,
        record: bool,
        completed: impl FnOnce(&Ui, bool) + 'static,
    ) -> bool {
        let previous = record.then(|| self.source.current_location()).flatten();

        self.open_source_when_ready(path, move |ui, document| {
            let Some(document) = document else {
                completed(ui, false);
                return;
            };
            let last_line = u32::try_from(document.buffer.line_count())
                .unwrap_or(u32::MAX)
                .max(1);

            let destination = SourceNavigationLocation {
                path: document.path.clone(),
                line: line.clamp(1, last_line),
            };

            ui.source
                .commit_navigation(&document, destination, previous);
            completed(ui, true);
        })
    }

    fn navigate_source_history(&self, forward: bool) {
        let source = if forward {
            &self.source.forward_history
        } else {
            &self.source.back_history
        };
        let Some(location) = source.borrow().last().cloned() else {
            return;
        };
        let current = self.source.current_location();
        let expected = location.clone();

        self.navigate_to_source_then(&location.path, location.line, false, move |ui, opened| {
            ui.source
                .complete_history_navigation(forward, &expected, current, opened);
        });
    }

    fn present_source_palette(self: &Rc<Self>, mode: SourceSearchMode) {
        self.present_source_palette_scoped(mode, None);
    }

    fn present_source_palette_scoped(
        self: &Rc<Self>,
        mode: SourceSearchMode,
        scope: Option<PathBuf>,
    ) {
        let scope = (mode == SourceSearchMode::Tree).then_some(scope).flatten();

        let existing = self.source.palette.borrow().as_ref().map(|palette| {
            (
                palette.mode,
                palette.scope.clone(),
                palette.window.clone(),
                palette.entry.clone(),
            )
        });

        if let Some((existing_mode, existing_scope, window, entry)) = existing {
            if existing_mode == mode && existing_scope == scope {
                window.set_transient_for(Some(&self.panel_window(PanelId::Editor)));
                window.present();
                entry.grab_focus();
                return;
            }

            window.close();
        }

        let (title, placeholder, hint) = match mode {
            SourceSearchMode::Files => (
                "Quick open source file",
                "File name or path, optionally followed by :line",
                String::from("Loaded debugger sources and files from the configured source tree"),
            ),
            SourceSearchMode::Symbols => (
                "Search functions and symbols",
                "Function, method, or variable name",
                String::from("Searches debug symbols currently known to GDB"),
            ),
            SourceSearchMode::LoadedText => (
                "Search loaded source files",
                "Text to find across source files known to GDB",
                String::from(
                    "Searches readable source files reported by the current debugger session",
                ),
            ),
            SourceSearchMode::Tree => (
                "Search source tree",
                "Text to find across project source files",
                scope.as_ref().map_or_else(
                    || String::from("Searches configured source roots in the background"),
                    |scope| format!("Searches within {}", scope.display()),
                ),
            ),
        };

        let window = gtk::Window::builder()
            .title(title)
            .transient_for(&self.panel_window(PanelId::Editor))
            .modal(false)
            .default_width(760)
            .default_height(520)
            .build();

        window.add_css_class("source-palette");
        self.panels.track_dialog(PanelId::Editor, &window);
        let root = gtk::Box::new(gtk::Orientation::Vertical, 7);
        components::inset(&root, components::DIALOG_INSET);
        let heading = gtk::Label::new(Some(title));
        heading.add_css_class("title-2");
        heading.set_halign(gtk::Align::Start);
        root.append(&heading);
        let entry = source_search_entry(placeholder);
        root.append(&entry);
        let hint = gtk::Label::new(Some(&hint));
        hint.add_css_class("muted");
        hint.set_halign(gtk::Align::Start);
        root.append(&hint);

        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .vexpand(true)
            .build();

        let results = gtk::Box::new(gtk::Orientation::Vertical, 1);
        results.add_css_class("source-palette-results");
        scrolled.set_child(Some(&results));
        root.append(&scrolled);
        let status = gtk::Label::new(None);
        status.add_css_class("muted");
        status.set_halign(gtk::Align::Start);
        root.append(&status);
        window.set_child(Some(&root));

        let loaded_files = self
            .source
            .loaded_cache
            .borrow()
            .as_ref()
            .cloned()
            .unwrap_or_default();

        let loaded_files_ready = self.source.loaded_cache.borrow().is_some();
        let loaded_search = self
            .source
            .loaded_search
            .borrow()
            .as_ref()
            .cloned()
            .unwrap_or_default();

        let tree_search = self
            .source
            .tree_search
            .borrow()
            .as_ref()
            .cloned()
            .unwrap_or_default();

        self.source.palette.replace(Some(SourcePalette {
            window: window.clone(),
            mode,
            entry: entry.clone(),
            results,
            status,
            loaded_files,
            loaded_search,
            loaded_files_ready,
            tree_files: self
                .source
                .tree_cache
                .borrow()
                .as_ref()
                .cloned()
                .unwrap_or_default(),
            tree_search,
            scope,
        }));

        let weak_source = Rc::downgrade(&self.source);
        let generation = Arc::clone(&self.source.palette_generation);

        window.connect_close_request(move |_| {
            generation.fetch_add(1, Ordering::Relaxed);

            if let Some(source) = weak_source.upgrade() {
                source.palette.borrow_mut().take();
            }

            glib::Propagation::Proceed
        });

        let weak_ui = Rc::downgrade(self);

        entry.connect_changed(move |_| {
            if let Some(ui) = weak_ui.upgrade() {
                ui.source_palette_query_changed();
            }
        });

        let weak_ui = Rc::downgrade(self);

        entry.connect_activate(move |_| {
            if let Some(ui) = weak_ui.upgrade()
                && let Some(palette) = ui.source.palette.borrow().as_ref()
                && let Some(button) = palette.results.first_child().and_downcast::<gtk::Button>()
            {
                button.emit_clicked();
            }
        });

        window.present();
        entry.grab_focus();

        match mode {
            SourceSearchMode::Files => {
                self.source.set_palette_status("Loading source files");
                self.request_loaded_source_files();
                self.start_source_tree_index();
                self.render_source_file_results();
            }
            SourceSearchMode::Symbols => {
                self.source
                    .set_palette_status("Type at least two characters to search");
            }
            SourceSearchMode::LoadedText => {
                self.source.set_palette_status("Loading source files");
                self.request_loaded_source_files();
            }
            SourceSearchMode::Tree => {
                self.source
                    .set_palette_status("Type at least two characters to search");
                self.start_source_tree_index();
            }
        }
    }

    fn source_palette_query_changed(self: &Rc<Self>) {
        let Some((mode, query, scope)) = self.source.palette.borrow().as_ref().map(|palette| {
            (
                palette.mode,
                palette.entry.text().to_string(),
                palette.scope.clone(),
            )
        }) else {
            return;
        };

        let generation = self
            .source
            .palette_generation
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1);

        match mode {
            SourceSearchMode::Files => {
                self.source.set_palette_status("Filtering source files");
                let weak_ui = Rc::downgrade(self);

                gtk::glib::timeout_add_local_once(Duration::from_millis(100), move || {
                    let Some(ui) = weak_ui.upgrade() else {
                        return;
                    };

                    if ui.source.palette_generation.load(Ordering::Relaxed) == generation {
                        ui.render_source_file_results();
                    }
                });
            }
            SourceSearchMode::Symbols => {
                clear_source_palette_results(self);

                if query.trim().chars().count() < 2 {
                    self.source
                        .set_palette_status("Type at least two characters to search");
                    return;
                }

                self.source.set_palette_status("Searching GDB symbols");
                let weak_ui = Rc::downgrade(self);

                gtk::glib::timeout_add_local_once(Duration::from_millis(180), move || {
                    let Some(ui) = weak_ui.upgrade() else {
                        return;
                    };

                    if ui.source.palette_generation.load(Ordering::Relaxed) != generation {
                        return;
                    }

                    let handler = ui.source_discovery_handler.borrow().clone();

                    if let Some(handler) = handler {
                        handler(SourceDiscoveryRequest::Symbols { query, generation });
                    }
                });
            }
            SourceSearchMode::LoadedText => {
                clear_source_palette_results(self);

                if query.trim().chars().count() < 2 {
                    self.source
                        .set_palette_status("Type at least two characters to search");
                    return;
                }

                let (files, loaded_files_ready) = self
                    .source
                    .palette
                    .borrow()
                    .as_ref()
                    .map(|palette| (palette.loaded_files.clone(), palette.loaded_files_ready))
                    .unwrap_or_default();

                if files.is_empty() {
                    self.source.set_palette_status(if loaded_files_ready {
                        "No readable loaded source files"
                    } else {
                        "Loading source files"
                    });

                    return;
                }

                self.source
                    .set_palette_status("Searching loaded source files");
                self.start_source_content_search(query, generation, files, None);
            }
            SourceSearchMode::Tree => {
                clear_source_palette_results(self);

                if query.trim().chars().count() < 2 {
                    self.source
                        .set_palette_status("Type at least two characters to search");
                    return;
                }

                if self.source.tree_cache.borrow().is_none() {
                    self.source.set_palette_status("Indexing source files");
                    self.start_source_tree_index();
                    return;
                }

                self.source.set_palette_status("Searching source files");
                let weak_ui = Rc::downgrade(self);

                gtk::glib::timeout_add_local_once(Duration::from_millis(180), move || {
                    if let Some(ui) = weak_ui.upgrade()
                        && ui.source.palette_generation.load(Ordering::Relaxed) == generation
                    {
                        let files = ui
                            .source
                            .tree_cache
                            .borrow()
                            .as_ref()
                            .cloned()
                            .unwrap_or_default();

                        ui.start_source_content_search(query, generation, files, scope);
                    }
                });
            }
        }
    }

    fn start_source_tree_index(self: &Rc<Self>) {
        let cached = self
            .source
            .tree_cache
            .borrow()
            .as_ref()
            .cloned()
            .zip(self.source.tree_search.borrow().as_ref().cloned());

        if let Some((files, search)) = cached {
            self.apply_source_tree_index(files, search);
            return;
        }

        if self.source.tree_indexing.replace(true) {
            return;
        }

        let roots = self.source.tree_roots.borrow().clone();
        let generation = self.source.tree_generation.load(Ordering::Relaxed);
        let current_generation = Arc::clone(&self.source.tree_generation);
        let queued_generation = Arc::clone(&current_generation);
        let (sender, receiver) = mpsc::channel();

        if let Err(error) = crate::background::submit_cancellable_with_priority(
            crate::background::Priority::Background,
            move || queued_generation.load(Ordering::Relaxed) == generation,
            move || {
                let discovery =
                    source::discover_source_files_while(&roots, MAX_SOURCE_TREE_FILES, || {
                        current_generation.load(Ordering::Relaxed) == generation
                    });

                if current_generation.load(Ordering::Relaxed) != generation {
                    return;
                }

                let index = Arc::new(source::SourceIndex::new(&discovery.files, &roots));
                let search = Arc::new(source::SourceSearchIndex::new(index.files()));
                let files = Arc::new(index.files().to_vec());
                let _ = sender.send((files, index, search, discovery.truncated));
            },
        ) {
            self.source.tree_indexing.set(false);
            self.source
                .set_palette_status("Source indexing is waiting for background capacity");

            if self.source.tree_initialized.get() {
                self.source
                    .tree
                    .status
                    .set_text("Source indexing could not be queued");
            }

            self.record_performance_notice(crate::performance::PerformanceNotice {
                outcome: crate::performance::BudgetOutcome::Rejected,
                operation: String::from("source indexing"),
                detail: error.to_string(),
            });

            return;
        }

        let weak_ui = Rc::downgrade(self);

        gtk::glib::timeout_add_local(Duration::from_millis(25), move || {
            let Some(ui) = weak_ui.upgrade() else {
                return glib::ControlFlow::Break;
            };

            match receiver.try_recv() {
                Ok((files, index, search, truncated)) => {
                    if !ui.source.publish_tree_index(
                        generation,
                        Arc::clone(&files),
                        index,
                        Arc::clone(&search),
                    ) {
                        return glib::ControlFlow::Break;
                    }

                    ui.refresh_source_breakpoint_index();
                    ui.apply_source_tree_index(files, search);

                    if truncated {
                        ui.record_performance_notice(crate::performance::PerformanceNotice {
                            outcome: crate::performance::BudgetOutcome::Partial,
                            operation: String::from("source indexing"),
                            detail: String::from("The file, directory, or entry budget was reached. Narrow the source roots to index omitted files"),
                        });
                    }

                    glib::ControlFlow::Break
                }
                Err(TryRecvError::Empty) => glib::ControlFlow::Continue,
                Err(TryRecvError::Disconnected) => {
                    if ui.source.tree_generation.load(Ordering::Relaxed) == generation {
                        ui.source.tree_indexing.set(false);
                        ui.source.set_palette_status("Source indexing failed");

                        if ui.source.tree_initialized.get() {
                            ui.source.tree.status.set_text("Source indexing failed");
                        }
                    }

                    glib::ControlFlow::Break
                }
            }
        });
    }

    fn apply_source_tree_index(
        self: &Rc<Self>,
        files: Arc<Vec<PathBuf>>,
        search: Arc<source::SourceSearchIndex>,
    ) {
        if self.source.tree_initialized.get() {
            self.render_source_tree();
        }

        let mode = {
            let mut palette = self.source.palette.borrow_mut();

            let Some(palette) = palette.as_mut() else {
                return;
            };

            palette.tree_files = files;
            palette.tree_search = search;

            palette.mode
        };

        match mode {
            SourceSearchMode::Files => self.render_source_file_results(),
            SourceSearchMode::Tree => self.source_palette_query_changed(),
            SourceSearchMode::Symbols | SourceSearchMode::LoadedText => {}
        }
    }

    fn start_source_content_search(
        self: &Rc<Self>,
        query: String,
        generation: u64,
        files: Arc<Vec<PathBuf>>,
        scope: Option<PathBuf>,
    ) {
        let (sender, receiver) = mpsc::channel();
        let query_for_worker = query.clone();
        let current_generation = Arc::clone(&self.source.palette_generation);
        let queued_generation = Arc::clone(&current_generation);

        if let Err(error) = crate::background::submit_cancellable_with_priority(
            crate::background::Priority::Interactive,
            move || queued_generation.load(Ordering::Relaxed) == generation,
            move || {
                let matches = source::search_source_files(
                    &files,
                    &query_for_worker,
                    MAX_SOURCE_RESULTS,
                    scope.as_deref(),
                    || current_generation.load(Ordering::Relaxed) == generation,
                );

                let _ = sender.send(matches);
            },
        ) {
            self.source
                .set_palette_status(&format!("Source search could not be queued: {error}"));
            return;
        }

        let weak_ui = Rc::downgrade(self);

        gtk::glib::timeout_add_local(Duration::from_millis(25), move || {
            let Some(ui) = weak_ui.upgrade() else {
                return glib::ControlFlow::Break;
            };

            match receiver.try_recv() {
                Ok(matches) => {
                    if ui.source.palette_generation.load(Ordering::Relaxed) == generation {
                        ui.show_source_content_results(&query, matches);
                    }

                    glib::ControlFlow::Break
                }
                Err(TryRecvError::Empty) => glib::ControlFlow::Continue,
                Err(TryRecvError::Disconnected) => glib::ControlFlow::Break,
            }
        });
    }

    pub(crate) fn request_loaded_source_files(&self) {
        let handler = self.source_discovery_handler.borrow().clone();

        let Some(handler) = handler else {
            return;
        };

        if !self.begin_loaded_source_files_request() {
            return;
        }

        let generation = self
            .source
            .loaded_generation
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1);

        handler(SourceDiscoveryRequest::LoadedFiles(generation));
    }

    pub(crate) fn show_loaded_source_files(
        self: &Rc<Self>,
        generation: u64,
        files: Vec<SourceFile>,
    ) {
        if self.source.loaded_generation.load(Ordering::Relaxed) != generation {
            return;
        }

        self.cache_loaded_source_files(&files);

        let reported = files
            .into_iter()
            .take(MAX_SOURCE_TREE_FILES)
            .map(|file| file.source_path().to_owned())
            .collect::<Vec<_>>();

        let roots = self.source.roots.borrow().clone();
        let existing_index = self.source.index.borrow().as_ref().cloned();
        let (sender, receiver) = mpsc::channel();
        let current_generation = Arc::clone(&self.source.loaded_generation);
        let queued_generation = Arc::clone(&current_generation);

        if let Err(error) = crate::background::submit_cancellable_with_priority(
            crate::background::Priority::Background,
            move || queued_generation.load(Ordering::Relaxed) == generation,
            move || {
                let mut resolved = reported
                    .into_iter()
                    .filter_map(|file| {
                        match existing_index.as_ref().map(|index| index.resolve(&file)) {
                            Some(source::SourceResolution::Unique(path)) => Some(path),
                            Some(source::SourceResolution::Ambiguous) => None,
                            Some(source::SourceResolution::Missing) | None => {
                                source::resolve(&file, &roots)
                            }
                        }
                    })
                    .collect::<Vec<_>>();

                if current_generation.load(Ordering::Relaxed) != generation {
                    return;
                }

                resolved.sort_unstable();
                resolved.dedup();

                let index = existing_index
                    .is_none()
                    .then(|| Arc::new(source::SourceIndex::new(&resolved, &roots)));

                let search = Arc::new(source::SourceSearchIndex::new(&resolved));
                let _ = sender.send((resolved, index, search));
            },
        ) {
            if self.source.tree_initialized.get() {
                self.source
                    .tree
                    .status
                    .set_text("Loaded-source resolution is waiting for background capacity");
            }

            self.record_performance_notice(crate::performance::PerformanceNotice {
                outcome: crate::performance::BudgetOutcome::Rejected,
                operation: String::from("loaded-source resolution"),
                detail: error.to_string(),
            });

            return;
        }

        let weak_ui = Rc::downgrade(self);

        gtk::glib::timeout_add_local(Duration::from_millis(25), move || {
            let Some(ui) = weak_ui.upgrade() else {
                return glib::ControlFlow::Break;
            };

            match receiver.try_recv() {
                Ok((resolved, index, search)) => {
                    if ui.source.loaded_generation.load(Ordering::Relaxed) == generation {
                        ui.apply_loaded_source_files(generation, resolved, index, search);
                    }

                    glib::ControlFlow::Break
                }
                Err(TryRecvError::Empty) => glib::ControlFlow::Continue,
                Err(TryRecvError::Disconnected) => glib::ControlFlow::Break,
            }
        });
    }

    fn apply_loaded_source_files(
        self: &Rc<Self>,
        generation: u64,
        resolved: Vec<PathBuf>,
        index: Option<Arc<source::SourceIndex>>,
        search: Arc<source::SourceSearchIndex>,
    ) {
        let Some(index_changed) = self
            .source
            .publish_loaded_files(generation, resolved, index, search)
        else {
            return;
        };

        if index_changed {
            self.refresh_source_breakpoint_index();
        }

        if self.source.tree_initialized.get() {
            self.render_source_tree();
        }

        let Some(mode) = self.source.palette_mode() else {
            return;
        };

        match mode {
            SourceSearchMode::Files => self.render_source_file_results(),
            SourceSearchMode::LoadedText => self.source_palette_query_changed(),
            SourceSearchMode::Symbols | SourceSearchMode::Tree => {}
        }
    }

    fn refresh_source_tree(self: &Rc<Self>) {
        self.source.clear_tree_index();
        self.refresh_source_breakpoint_index();
        self.request_loaded_source_files();
        self.start_source_tree_index();
    }

    fn render_source_tree(self: &Rc<Self>) {
        if !self.source.tree_initialized.get() {
            return;
        }

        let Some(files) = self.source.tree_cache.borrow().as_ref().cloned() else {
            self.source.tree.status.set_text("Indexing source files");
            self.start_source_tree_index();
            return;
        };

        let roots = self.source.tree_roots.borrow().clone();

        let loaded = self
            .source
            .loaded_cache
            .borrow()
            .as_ref()
            .cloned()
            .unwrap_or_default();

        let query = self.source.tree.search.text().trim().to_owned();

        let generation = self
            .source
            .tree_render_generation
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1);

        self.source.tree.status.set_text(if query.is_empty() {
            "Building source tree"
        } else {
            "Filtering source files"
        });

        let (sender, receiver) = mpsc::channel();
        let current_generation = Arc::clone(&self.source.tree_render_generation);
        let queued_generation = Arc::clone(&current_generation);

        if let Err(error) = crate::background::submit_cancellable_with_priority(
            crate::background::Priority::Interactive,
            move || queued_generation.load(Ordering::Relaxed) == generation,
            move || {
                let build =
                    source::build_source_tree_while(&files, &roots, &loaded, &query, || {
                        current_generation.load(Ordering::Relaxed) == generation
                    });

                let _ = sender.send(build);
            },
        ) {
            self.source
                .tree
                .status
                .set_text("Source tree rendering could not be queued");

            self.record_performance_notice(crate::performance::PerformanceNotice {
                outcome: crate::performance::BudgetOutcome::Rejected,
                operation: String::from("source tree rendering"),
                detail: error.to_string(),
            });

            return;
        }

        let weak_ui = Rc::downgrade(self);

        gtk::glib::timeout_add_local(Duration::from_millis(25), move || {
            let Some(ui) = weak_ui.upgrade() else {
                return glib::ControlFlow::Break;
            };

            match receiver.try_recv() {
                Ok(build) => {
                    if ui.source.tree_render_generation.load(Ordering::Relaxed) == generation {
                        ui.source.apply_tree_build(build);
                    }

                    glib::ControlFlow::Break
                }
                Err(TryRecvError::Empty) => glib::ControlFlow::Continue,
                Err(TryRecvError::Disconnected) => {
                    if ui.source.tree_render_generation.load(Ordering::Relaxed) == generation {
                        ui.source
                            .tree
                            .status
                            .set_text("Source tree rendering failed");
                    }

                    glib::ControlFlow::Break
                }
            }
        });
    }

    fn render_source_file_results(self: &Rc<Self>) {
        let Some((query, loaded, tree)) =
            self.source.palette.borrow().as_ref().and_then(|palette| {
                (palette.mode == SourceSearchMode::Files).then(|| {
                    (
                        palette.entry.text().to_string(),
                        Arc::clone(&palette.loaded_search),
                        Arc::clone(&palette.tree_search),
                    )
                })
            })
        else {
            return;
        };

        let (path_query, requested_line) = split_source_file_query(&query);
        let matches = source::search_source_paths(&loaded, &tree, path_query, MAX_SOURCE_RESULTS);
        clear_source_palette_results(self);
        let palette_state = self.source.palette.borrow();

        let Some(palette) = palette_state.as_ref() else {
            return;
        };

        for result in &matches {
            let kind = if result.loaded {
                "GDB source"
            } else {
                "Source tree"
            };

            let button = source_palette_result(
                &source_tab_title(&result.path),
                &result.path.display().to_string(),
                kind,
            );

            let weak_ui = Rc::downgrade(self);
            let path = result.path.clone();

            button.connect_clicked(move |_| {
                if let Some(ui) = weak_ui.upgrade() {
                    ui.navigate_to_source(&path, requested_line.unwrap_or(1), true);
                    ui.source.close_palette();
                }
            });

            palette.results.append(&button);
        }

        palette.status.set_text(&match matches.len() {
            0 if self.source.tree_indexing.get() => String::from("Indexing source files"),
            0 => String::from("No matching source files"),
            1 => String::from("1 matching source file"),
            count => format!("{count} matching source files"),
        });
    }

    pub(crate) fn show_source_symbol_results(
        self: &Rc<Self>,
        generation: u64,
        query: &str,
        locations: Vec<SourceLocation>,
    ) {
        if self.source.palette_generation.load(Ordering::Relaxed) != generation {
            return;
        }

        let current = self.source.palette.borrow().as_ref().and_then(|palette| {
            (palette.mode == SourceSearchMode::Symbols).then(|| palette.entry.text().to_string())
        });

        if current.as_deref() != Some(query) {
            return;
        }

        let results = locations
            .into_iter()
            .filter_map(|location| {
                let path = self.resolve_source_path(location.source_path())?;

                Some((location, path))
            })
            .take(MAX_SOURCE_RESULTS)
            .collect::<Vec<_>>();

        clear_source_palette_results(self);
        let palette_state = self.source.palette.borrow();

        let Some(palette) = palette_state.as_ref() else {
            return;
        };

        for (location, path) in &results {
            let button = source_palette_result(
                &compact_function_name(&location.function),
                &format!("{}:{}", path.display(), location.line),
                "Debug symbol",
            );

            let weak_ui = Rc::downgrade(self);
            let path = path.clone();
            let line = location.line;

            button.connect_clicked(move |_| {
                if let Some(ui) = weak_ui.upgrade() {
                    ui.navigate_to_source(&path, line, true);
                    ui.source.close_palette();
                }
            });

            palette.results.append(&button);
        }

        palette.status.set_text(&match results.len() {
            0 => String::from("No source-backed symbols found"),
            1 => String::from("1 source-backed symbol"),
            count => format!("{count} source-backed symbols"),
        });
    }

    pub(crate) fn source_symbol_request_is_current(&self, generation: u64) -> bool {
        self.source.palette_generation.load(Ordering::Relaxed) == generation
    }

    fn show_source_content_results(
        self: &Rc<Self>,
        query: &str,
        matches: Vec<source::SourceTreeMatch>,
    ) {
        let current = self.source.palette.borrow().as_ref().and_then(|palette| {
            matches!(
                palette.mode,
                SourceSearchMode::LoadedText | SourceSearchMode::Tree
            )
            .then(|| palette.entry.text().to_string())
        });

        if current.as_deref() != Some(query) {
            return;
        }

        clear_source_palette_results(self);
        let palette_state = self.source.palette.borrow();

        let Some(palette) = palette_state.as_ref() else {
            return;
        };

        for result in &matches {
            let button = source_palette_result(
                &format!(
                    "{}:{}:{}",
                    source_tab_title(&result.path),
                    result.line,
                    result.column
                ),
                &result.preview,
                &result.path.display().to_string(),
            );

            let weak_ui = Rc::downgrade(self);
            let path = result.path.clone();
            let line = result.line;

            button.connect_clicked(move |_| {
                if let Some(ui) = weak_ui.upgrade() {
                    ui.navigate_to_source(&path, line, true);
                    ui.source.close_palette();
                }
            });

            palette.results.append(&button);
        }

        palette.status.set_text(&match matches.len() {
            0 => String::from("No source matches"),
            1 => String::from("1 source match"),
            count => format!("{count} source matches"),
        });
    }
}

fn source_match_count(occurrences: i32) -> String {
    match occurrences {
        count if count < 0 => String::from("Searching"),
        1 => String::from("1 match"),
        count => format!("{count} matches"),
    }
}

fn push_source_history(
    history: &RefCell<Vec<SourceNavigationLocation>>,
    location: SourceNavigationLocation,
) {
    let mut history = history.borrow_mut();

    if history.last() == Some(&location) {
        return;
    }

    history.push(location);

    if history.len() > MAX_SOURCE_HISTORY {
        let remove = history.len() - MAX_SOURCE_HISTORY;
        history.drain(..remove);
    }
}

fn clear_source_palette_results(ui: &Ui) {
    if let Some(palette) = ui.source.palette.borrow().as_ref() {
        clear_box(&palette.results);
    }
}

fn source_palette_result(primary: &str, secondary: &str, kind: &str) -> gtk::Button {
    let row = gtk::Box::new(gtk::Orientation::Vertical, 1);
    let heading = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let primary = gtk::Label::new(Some(primary));
    primary.add_css_class("source-palette-primary");
    primary.set_halign(gtk::Align::Start);
    primary.set_xalign(0.0);
    primary.set_ellipsize(pango::EllipsizeMode::Middle);
    primary.set_hexpand(true);
    let kind = gtk::Label::new(Some(kind));
    kind.add_css_class("source-palette-kind");
    kind.set_halign(gtk::Align::End);
    heading.append(&primary);
    heading.append(&kind);
    row.append(&heading);
    let secondary = gtk::Label::new(Some(secondary));
    secondary.add_css_class("source-palette-secondary");
    secondary.set_halign(gtk::Align::Start);
    secondary.set_xalign(0.0);
    secondary.set_ellipsize(pango::EllipsizeMode::Middle);
    secondary.set_tooltip_text(Some(secondary.text().as_str()));
    row.append(&secondary);
    let button = gtk::Button::builder().child(&row).hexpand(true).build();
    button.add_css_class("source-palette-result");

    button
}

fn split_source_file_query(query: &str) -> (&str, Option<u32>) {
    let query = query.trim();

    let Some((path, line)) = query.rsplit_once(':') else {
        return (query, None);
    };

    line.parse::<u32>()
        .ok()
        .filter(|line| *line > 0)
        .map_or((query, None), |line| (path.trim(), Some(line)))
}

impl SourceWorkspace {
    fn clear_tree_index(&self) {
        self.tree_generation.fetch_add(1, Ordering::Relaxed);

        self.tree_render_generation.fetch_add(1, Ordering::Relaxed);

        self.tree_cache.borrow_mut().take();
        self.tree_search.borrow_mut().take();
        self.index.borrow_mut().take();
        self.loaded_cache.borrow_mut().take();
        self.loaded_search.borrow_mut().take();
        self.tree_indexing.set(false);
        self.tree.roots.remove_all();
        self.tree.file_routes.borrow_mut().clear();
        self.tree.status.set_text("Indexing source files");
    }

    fn palette_mode(&self) -> Option<SourceSearchMode> {
        self.palette.borrow().as_ref().map(|palette| palette.mode)
    }

    pub(super) fn publish_tree_index(
        &self,
        generation: u64,
        files: Arc<Vec<PathBuf>>,
        index: Arc<source::SourceIndex>,
        search: Arc<source::SourceSearchIndex>,
    ) -> bool {
        if self.tree_generation.load(Ordering::Relaxed) != generation {
            return false;
        }

        self.tree_indexing.set(false);
        self.tree_cache.replace(Some(files));
        self.tree_search.replace(Some(search));
        self.index.replace(Some(index));
        self.resolved_paths.borrow_mut().clear();
        true
    }

    pub(super) fn publish_loaded_files(
        &self,
        generation: u64,
        resolved: Vec<PathBuf>,
        index: Option<Arc<source::SourceIndex>>,
        search: Arc<source::SourceSearchIndex>,
    ) -> Option<bool> {
        if self.loaded_generation.load(Ordering::Relaxed) != generation {
            return None;
        }

        let mut index_changed = false;

        if self.index.borrow().is_none()
            && let Some(index) = index
        {
            self.index.replace(Some(index));
            index_changed = true;
            self.resolved_paths.borrow_mut().clear();
        }

        let resolved = Arc::new(resolved);

        self.loaded_cache.replace(Some(Arc::clone(&resolved)));

        self.loaded_search.replace(Some(Arc::clone(&search)));

        {
            let mut palette = self.palette.borrow_mut();

            let Some(palette) = palette.as_mut() else {
                return Some(index_changed);
            };

            palette.loaded_files = resolved;
            palette.loaded_search = search;
            palette.loaded_files_ready = true;
        };

        Some(index_changed)
    }

    fn commit_navigation(
        &self,
        document: &SourceDocument,
        destination: SourceNavigationLocation,
        previous: Option<SourceNavigationLocation>,
    ) {
        if let Some(previous) = previous
            && previous != destination
        {
            push_source_history(&self.back_history, previous);
            self.forward_history.borrow_mut().clear();
        }

        scroll_source_document(document, destination.line);
        document.view.grab_focus();
        self.update_history_buttons();
        self.sync_tree_selection();
    }

    pub(super) fn complete_history_navigation(
        &self,
        forward: bool,
        expected: &SourceNavigationLocation,
        current: Option<SourceNavigationLocation>,
        opened: bool,
    ) {
        let (source, destination) = if forward {
            (&self.forward_history, &self.back_history)
        } else {
            (&self.back_history, &self.forward_history)
        };

        if opened && source.borrow().last() == Some(expected) {
            source.borrow_mut().pop();

            if let Some(current) = current {
                push_source_history(destination, current);
            }
        }

        self.update_history_buttons();
    }

    pub(in crate::ui) fn current_document(&self) -> Option<SourceDocument> {
        let page = self.notebook.current_page()?;

        self.documents
            .borrow()
            .iter()
            .find(|document| self.notebook.page_num(&document.page) == Some(page))
            .cloned()
    }

    pub(in crate::ui) fn current_location(&self) -> Option<SourceNavigationLocation> {
        let document = self.current_document()?;

        Some(SourceNavigationLocation {
            path: document.path,
            line: u32::try_from(
                document
                    .buffer
                    .iter_at_offset(document.buffer.cursor_position())
                    .line()
                    .saturating_add(1),
            )
            .unwrap_or(1),
        })
    }

    pub(in crate::ui) fn close_find(&self) {
        self.find_state.borrow_mut().take();
        self.navigation.find_bar.set_visible(false);
        self.navigation.find_count.set_text("");

        if let Some(document) = self.current_document() {
            document.view.grab_focus();
        }
    }

    pub(in crate::ui) fn update_find(&self, select_first: bool) {
        let query = self.navigation.find_entry.text();

        let Some(document) = self.current_document() else {
            self.find_state.borrow_mut().take();
            self.navigation.find_count.set_text("No source");
            return;
        };

        if query.is_empty() {
            self.find_state.borrow_mut().take();
            self.navigation.find_count.set_text("");
            return;
        }

        let settings = sourceview5::SearchSettings::new();
        settings.set_search_text(Some(&query));
        settings.set_case_sensitive(self.navigation.find_case.is_active());
        settings.set_wrap_around(true);
        let context = sourceview5::SearchContext::new(&document.buffer, Some(&settings));
        context.set_highlight(true);
        let count = self.navigation.find_count.clone();

        context.connect_occurrences_count_notify(move |context| {
            let occurrences = context.occurrences_count();
            count.set_text(&source_match_count(occurrences));
        });

        self.find_state.replace(Some(SourceFindState {
            path: document.path,
            context,
        }));

        let occurrences = self
            .find_state
            .borrow()
            .as_ref()
            .map_or(0, |state| state.context.occurrences_count());

        self.navigation
            .find_count
            .set_text(&source_match_count(occurrences));

        if select_first {
            self.move_find(true);
        }
    }

    pub(in crate::ui) fn move_find(&self, forward: bool) {
        let Some(document) = self.current_document() else {
            return;
        };

        let needs_refresh = self
            .find_state
            .borrow()
            .as_ref()
            .is_none_or(|state| state.path != document.path);

        if needs_refresh {
            self.update_find(false);
        }

        let state = self.find_state.borrow();

        let Some(state) = state.as_ref() else {
            return;
        };

        let cursor = document
            .buffer
            .iter_at_offset(document.buffer.cursor_position());

        let start = document
            .buffer
            .selection_bounds()
            .map_or(cursor, |(start, end)| if forward { end } else { start });

        let found = if forward {
            state.context.forward(&start)
        } else {
            state.context.backward(&start)
        };

        let Some((mut match_start, match_end, _)) = found else {
            return;
        };

        document.buffer.select_range(&match_start, &match_end);

        document
            .view
            .scroll_to_iter(&mut match_start, 0.12, true, 0.0, 0.35);

        let position = state.context.occurrence_position(&match_start, &match_end);
        let count = state.context.occurrences_count();

        if position > 0 && count > 0 {
            self.navigation
                .find_count
                .set_text(&format!("{position} of {count}"));
        }
    }

    pub(in crate::ui) fn update_history_buttons(&self) {
        self.navigation
            .back
            .set_sensitive(!self.back_history.borrow().is_empty());

        self.navigation
            .forward
            .set_sensitive(!self.forward_history.borrow().is_empty());
    }

    pub(in crate::ui) fn apply_tree_build(&self, build: source::SourceTreeBuild) {
        let source::SourceTreeBuild {
            roots,
            file_count,
            file_routes,
        } = build;

        let root_count = roots.len();
        self.tree.roots.remove_all();
        self.tree.file_routes.replace(file_routes);

        for root in roots {
            self.tree
                .roots
                .append(&glib::BoxedAnyObject::new(SourceTreeNode {
                    data: Arc::new(root),
                }));
        }

        let query_active = !self.tree.search.text().trim().is_empty();
        let expand_filtered = query_active && file_count <= MAX_SOURCE_RESULTS;
        let mut position = 0;
        let mut expanded_roots = 0;

        while position < self.tree.model.n_items() {
            let Some(row) = self
                .tree
                .model
                .item(position)
                .and_downcast::<gtk::TreeListRow>()
            else {
                position += 1;
                continue;
            };

            if row.depth() == 0 {
                expanded_roots += 1;
            }

            if row.depth() == 0 || expand_filtered {
                row.set_expanded(true);
            }

            position += 1;

            if !expand_filtered && expanded_roots >= root_count {
                break;
            }
        }

        self.tree.status.set_text(&match file_count {
            0 if query_active => String::from("No matching source files"),
            0 => String::from("No source files found"),
            1 => String::from("1 source file"),
            count => format!("{count} source files"),
        });

        self.sync_tree_selection();
    }

    pub(in crate::ui) fn sync_tree_selection(&self) {
        if !self.tree_initialized.get() {
            return;
        }

        let Some(target) = self.current_document().map(|document| document.path) else {
            self.tree.selection.unselect_all();
            return;
        };

        self.tree.selection.unselect_all();

        let target = source::SourceId::from_path(&target);
        let routes = self.tree.file_routes.borrow();
        let Some((root, children)) = routes.get(&target).and_then(|route| route.split_first())
        else {
            return;
        };

        let Some(mut row) = self.tree.model.child_row(*root) else {
            return;
        };

        for &child in children {
            row.set_expanded(true);

            let Some(child_row) = row.child_row(child) else {
                return;
            };

            row = child_row;
        }

        let position = row.position();

        if position != gtk::INVALID_LIST_POSITION {
            self.tree.selection.set_selected(position);

            self.tree
                .view
                .scroll_to(position, gtk::ListScrollFlags::FOCUS, None);
        }
    }

    pub(in crate::ui) fn set_palette_status(&self, text: &str) {
        if let Some(palette) = self.palette.borrow().as_ref() {
            palette.status.set_text(text);
        }
    }

    pub(in crate::ui) fn close_palette(&self) {
        let window = self
            .palette
            .borrow()
            .as_ref()
            .map(|palette| palette.window.clone());

        if let Some(window) = window {
            window.close();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::split_source_file_query;

    #[test]
    fn parses_optional_quick_open_line_numbers() {
        assert_eq!(
            split_source_file_query("src/main.rs:42"),
            ("src/main.rs", Some(42))
        );

        assert_eq!(
            split_source_file_query("src/main.rs"),
            ("src/main.rs", None)
        );

        assert_eq!(
            split_source_file_query("src/main.rs:no"),
            ("src/main.rs:no", None)
        );
    }
}
