use super::*;
use std::sync::mpsc::{self, TryRecvError};

fn load_source(
    reported: &Path,
    roots: &[PathBuf],
    index: Option<&source::SourceIndex>,
    maximum_bytes: usize,
) -> Result<(PathBuf, source::CachedSource), String> {
    let reported = reported.to_string_lossy();
    let path = match index.map(|index| index.resolve(&reported)) {
        Some(source::SourceResolution::Unique(path)) => Some(path),
        Some(source::SourceResolution::Ambiguous) => return Err(format!("More than one source file matches {reported}. Configure a source directory or substitute path")),
        _ => source::resolve(&reported, roots),
    }.ok_or_else(|| format!("Could not locate {reported}"))?;

    let metadata =
        std::fs::metadata(&path).map_err(|error| format!("Could not read {reported}: {error}"))?;
    if metadata.len() > maximum_bytes as u64 {
        return Err(format!(
            "{reported} exceeds the {} MiB source-file limit",
            maximum_bytes / (1024 * 1024)
        ));
    }
    let snapshot = source::read_source_snapshot(&path).ok_or_else(|| format!("Could not read {reported}. Check file permissions or retry after the file has finished changing"))?;

    if snapshot.contents.len() > maximum_bytes {
        return Err(format!(
            "{reported} grew past the source-file limit while being read"
        ));
    }

    if snapshot.exceeds_lines(250_000) {
        return Err(format!(
            "{reported} exceeds the 250000-line source-file limit"
        ));
    }

    Ok((path, snapshot))
}

impl Ui {
    pub(super) fn open_source_batch(
        &self,
        paths: Rc<RefCell<std::collections::VecDeque<PathBuf>>>,
        outcome: Rc<RefCell<(usize, Vec<String>)>>,
    ) {
        let path = paths.borrow_mut().pop_front();
        let Some(path) = path else {
            let outcome = outcome.borrow();
            if outcome.1.is_empty() {
                self.set_status(
                    "Source",
                    &format!("Opened {} source files", outcome.0),
                    Some("status-ready"),
                );
            } else {
                self.set_status(
                    "Source open failed",
                    &format!(
                        "Opened {} source files. Could not read {}",
                        outcome.0,
                        outcome.1.join(", ")
                    ),
                    Some("status-error"),
                );
            }
            return;
        };

        let path_for_result = path.clone();
        let complete = Rc::new(move |ui: &Ui, opened| {
            if opened {
                outcome.borrow_mut().0 += 1;
            } else {
                outcome
                    .borrow_mut()
                    .1
                    .push(path_for_result.display().to_string());
            }
            let weak = ui.self_weak.borrow().clone();
            let generation = ui.source_open_generation.load(Ordering::Relaxed);
            let paths = Rc::clone(&paths);
            let outcome = Rc::clone(&outcome);
            glib::idle_add_local_once(move || {
                if let Some(ui) = weak.upgrade()
                    && ui.source_open_generation.load(Ordering::Relaxed) == generation
                {
                    ui.open_source_batch(paths, outcome);
                }
            });
        });
        let callback = Rc::clone(&complete);
        if !self.navigate_to_source_then(&path, 1, true, move |ui, opened| callback(ui, opened)) {
            complete(self, false);
        }
    }

    pub(crate) fn connect_source_loading(self: &Rc<Self>) {
        self.self_weak.replace(Rc::downgrade(self));
        let generation = Arc::clone(&self.source_open_generation);
        self.source_notebook.connect_switch_page(move |_, _, _| {
            generation.fetch_add(1, Ordering::Relaxed);
        });
    }

    pub(super) fn invalidate_source_io(&self) {
        self.source_io_epoch.fetch_add(1, Ordering::Relaxed);
        self.source_open_generation.fetch_add(1, Ordering::Relaxed);
        self.disassembly_source_cache.borrow_mut().clear();
        self.disassembly_source_pending.borrow_mut().clear();
    }

    pub(super) fn open_source_when_ready(
        &self,
        reported: &Path,
        ready: impl FnOnce(&Ui, Option<SourceDocument>) + 'static,
    ) -> bool {
        let generation = self
            .source_open_generation
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1);
        let cached_path = self
            .resolved_source_paths
            .borrow_mut()
            .get_cloned(&reported.to_string_lossy().into_owned())
            .flatten();
        let document = self
            .source_documents
            .borrow()
            .iter()
            .find(|doc| doc.path == reported || cached_path.as_ref() == Some(&doc.path))
            .cloned();

        if let Some(document) = document {
            if let Some(page) = self.source_notebook.page_num(&document.page) {
                self.source_notebook.set_current_page(Some(page));
            }
            document.view.grab_focus();
            ready(self, Some(document));
            return true;
        }

        let path = reported.to_path_buf();
        let roots = self.source_roots.borrow().clone();
        let index = self.source_index_snapshot();
        let current = Arc::clone(&self.source_open_generation);
        let epoch = self.source_io_epoch.load(Ordering::Relaxed);
        let (sender, receiver) = mpsc::sync_channel(1);

        if let Err(error) = crate::background::submit_cancellable_with_priority(
            crate::background::Priority::Interactive,
            move || current.load(Ordering::Relaxed) == generation,
            move || {
                let _ = sender.send(load_source(
                    &path,
                    &roots,
                    index.as_deref(),
                    16 * 1024 * 1024,
                ));
            },
        ) {
            self.set_status(
                "Source loading deferred",
                &error.to_string(),
                Some("status-error"),
            );
            return false;
        }

        self.set_status("Loading source", &reported.to_string_lossy(), None);
        let weak = self.self_weak.borrow().clone();
        let reported = reported.to_path_buf();
        let mut ready = Some(ready);

        glib::timeout_add_local(Duration::from_millis(10), move || {
            let Some(ui) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if ui.source_open_generation.load(Ordering::Relaxed) != generation
                || ui.source_io_epoch.load(Ordering::Relaxed) != epoch
            {
                return glib::ControlFlow::Break;
            }

            match receiver.try_recv() {
                Ok(Ok((path, snapshot))) => {
                    ui.resolved_source_paths
                        .borrow_mut()
                        .insert(reported.to_string_lossy().into_owned(), Some(path.clone()));
                    let context = SourceOpenContext {
                        notebook: &ui.source_notebook,
                        documents: &ui.source_documents,
                        theme: &ui.source_theme,
                        style_scheme: ui.source_style_scheme.as_ref(),
                        breakpoints: &ui.breakpoints,
                        source_index: &ui.source_index,
                        insert_handler: &ui.breakpoint_insert_handler,
                        jump_handler: &ui.source_jump_handler,
                        delete_handler: &ui.breakpoint_delete_handler,
                        enabled_handler: &ui.breakpoint_enabled_handler,
                        symbol_handler: &ui.source_symbol_handler,
                        closed_tabs: &ui.closed_source_tabs,
                        reopen_closed: &ui.source_navigation.reopen_closed,
                    };

                    let document = open_source_document(&path, &snapshot.contents, context);
                    if let Some(ready) = ready.take() {
                        ready(&ui, Some(document));
                    }
                    glib::ControlFlow::Break
                }
                Ok(Err(error)) => {
                    ui.set_status("Source unavailable", &error, Some("status-error"));
                    if let Some(ready) = ready.take() {
                        ready(&ui, None);
                    }
                    glib::ControlFlow::Break
                }
                Err(TryRecvError::Empty) => glib::ControlFlow::Continue,
                Err(TryRecvError::Disconnected) => {
                    ui.set_status(
                        "Source unavailable",
                        "Source loading stopped before completing",
                        Some("status-error"),
                    );
                    if let Some(ready) = ready.take() {
                        ready(&ui, None);
                    }
                    glib::ControlFlow::Break
                }
            }
        });
        true
    }

    pub(super) fn disassembly_source_text(
        &self,
        instruction: &Instruction,
    ) -> Option<source::SourceLine> {
        if !self.disassembly_controls.source_column.is_visible() {
            return None;
        }

        let location = instruction.source.as_ref()?;
        let path = PathBuf::from(location.source_path());
        let index = usize::try_from(location.line).ok()?.checked_sub(1)?;
        let stop_generation = self.current_stop_refresh_generation();
        if let Some((failed_at, snapshot)) =
            self.disassembly_source_cache.borrow_mut().get_cloned(&path)
        {
            if let Some(snapshot) = snapshot {
                return snapshot.line(index);
            }
            if failed_at == stop_generation {
                return None;
            }
        }

        if !self
            .disassembly_source_pending
            .borrow_mut()
            .insert(path.clone())
        {
            return None;
        }

        let roots = self.source_roots.borrow().clone();
        let source_index = self.source_index_snapshot();
        let current = Arc::clone(&self.source_io_epoch);
        let epoch = current.load(Ordering::Relaxed);
        let (sender, receiver) = mpsc::sync_channel(1);
        let load_path = path.clone();

        if crate::background::submit_cancellable_with_priority(
            crate::background::Priority::Background,
            move || current.load(Ordering::Relaxed) == epoch,
            move || {
                let snapshot =
                    load_source(&load_path, &roots, source_index.as_deref(), 2 * 1024 * 1024)
                        .ok()
                        .map(|(_, snapshot)| snapshot);
                let _ = sender.send(snapshot);
            },
        )
        .is_err()
        {
            self.disassembly_source_pending.borrow_mut().remove(&path);
            self.cache_disassembly_source(path, stop_generation, None);
            return None;
        }

        let weak = self.self_weak.borrow().clone();
        glib::timeout_add_local(Duration::from_millis(25), move || {
            let Some(ui) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if ui.source_io_epoch.load(Ordering::Relaxed) != epoch {
                return glib::ControlFlow::Break;
            }

            match receiver.try_recv() {
                Ok(snapshot) => {
                    ui.disassembly_source_pending.borrow_mut().remove(&path);
                    ui.cache_disassembly_source(path.clone(), stop_generation, snapshot.clone());

                    // Rebind only matching rows, preserving selection and the
                    // scroll anchor while source annotations arrive.
                    if let Some(snapshot) = snapshot {
                        for position in 0..ui.instructions_store.n_items() {
                            let Some(object) = ui
                                .instructions_store
                                .item(position)
                                .and_downcast::<glib::BoxedAnyObject>()
                            else {
                                continue;
                            };
                            let mut row = object.borrow::<InstructionRowData>().clone();
                            if let Some(location) = row.instruction.source.as_ref()
                                && Path::new(location.source_path()) == path
                            {
                                row.source_text = location
                                    .line
                                    .checked_sub(1)
                                    .and_then(|line| snapshot.line(line as usize));
                                ui.instructions_store.splice(
                                    position,
                                    1,
                                    &[glib::BoxedAnyObject::new(row)],
                                );
                            }
                        }
                    }
                    glib::ControlFlow::Break
                }
                Err(TryRecvError::Empty) => glib::ControlFlow::Continue,
                Err(TryRecvError::Disconnected) => {
                    ui.disassembly_source_pending.borrow_mut().remove(&path);
                    glib::ControlFlow::Break
                }
            }
        });
        None
    }

    fn cache_disassembly_source(
        &self,
        path: PathBuf,
        generation: u64,
        snapshot: Option<source::CachedSource>,
    ) {
        let evicted = self
            .disassembly_source_cache
            .borrow_mut()
            .insert(path, (generation, snapshot));
        if evicted {
            self.record_performance_notice(crate::performance::PerformanceNotice {
                outcome: crate::performance::BudgetOutcome::Evicted,
                operation: String::from("disassembly source cache"),
                detail: String::from(
                    "The least-recently used source snapshot was evicted at the file budget",
                ),
            });
        }
    }
}
