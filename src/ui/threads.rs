use super::*;

const THREAD_STATE_FILTERS: [&str; 3] = ["All states", "Stopped", "Running"];
const THREAD_SORTS: [&str; 5] = ["Current first", "Thread ID", "Name", "State", "Core"];
const SCHEDULER_LOCKING_MODES: [&str; 4] = ["Off", "On", "Step", "Replay"];

pub(super) fn build_thread_controls() -> ThreadControls {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 5);
    root.add_css_class("thread-workspace");
    root.set_vexpand(true);

    let summary = gtk::Label::new(Some("Threads appear when the target is paused"));
    summary.add_css_class("thread-workspace-summary");
    summary.set_halign(gtk::Align::Start);
    summary.set_xalign(0.0);
    root.append(&summary);

    let search = gtk::SearchEntry::builder()
        .placeholder_text("Filter ID, name, state, core, or frame")
        .build();
    search.add_css_class("thread-search");
    search.set_hexpand(true);
    root.append(&search);

    let filters = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    let state_filter = gtk::DropDown::from_strings(&THREAD_STATE_FILTERS);
    state_filter.add_css_class("thread-dropdown");
    state_filter.set_hexpand(true);
    state_filter.set_tooltip_text(Some("Filter threads by execution state"));
    let sort = gtk::DropDown::from_strings(&THREAD_SORTS);
    sort.add_css_class("thread-dropdown");
    sort.set_hexpand(true);
    sort.set_tooltip_text(Some("Sort the visible threads"));
    filters.append(&state_filter);
    filters.append(&sort);
    root.append(&filters);

    let advanced = gtk::Box::new(gtk::Orientation::Vertical, 5);
    let policy_title = gtk::Label::new(Some("SCHEDULER LOCKING"));
    policy_title.add_css_class("section-title");
    policy_title.set_halign(gtk::Align::Start);
    advanced.append(&policy_title);
    let policy = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    let scheduler_locking = gtk::DropDown::from_strings(&SCHEDULER_LOCKING_MODES);
    scheduler_locking.add_css_class("thread-dropdown");
    scheduler_locking.set_hexpand(true);
    scheduler_locking.set_tooltip_text(Some(
        "Control which threads GDB permits to run during continue and stepping",
    ));
    let refresh = gtk::Button::with_label("Refresh");
    refresh.set_tooltip_text(Some("Refresh threads and concurrency settings"));
    policy.append(&scheduler_locking);
    policy.append(&refresh);
    advanced.append(&policy);
    let non_stop = gtk::CheckButton::with_label("Use non-stop mode for the next target");
    non_stop.set_tooltip_text(Some(
        "Non-stop mode permits individual thread freeze and thaw and must be chosen before starting or attaching",
    ));
    advanced.append(&non_stop);
    let mode_note = gtk::Label::new(Some("Detecting GDB thread-control mode"));
    mode_note.add_css_class("muted");
    mode_note.set_halign(gtk::Align::Start);
    mode_note.set_xalign(0.0);
    mode_note.set_wrap(true);
    advanced.append(&mode_note);

    let execution = gtk::Grid::builder()
        .column_homogeneous(true)
        .column_spacing(4)
        .row_spacing(4)
        .build();
    let run_only = gtk::Button::with_label("Run only");
    run_only.set_tooltip_text(Some(
        "Resume only the selected thread and enable scheduler locking when needed",
    ));
    let freeze = gtk::Button::with_label("Freeze");
    freeze.set_tooltip_text(Some("Stop the selected running thread in non-stop mode"));
    let thaw = gtk::Button::with_label("Thaw");
    thaw.set_tooltip_text(Some("Resume the selected stopped thread in non-stop mode"));
    let backtraces = gtk::Button::with_label("All backtraces");
    backtraces.set_tooltip_text(Some("Collect bounded backtraces for every stopped thread"));
    execution.attach(&run_only, 0, 0, 1, 1);
    execution.attach(&freeze, 1, 0, 1, 1);
    execution.attach(&thaw, 0, 1, 1, 1);
    execution.attach(&backtraces, 1, 1, 1, 1);
    advanced.append(&execution);

    let compare_title = gtk::Label::new(Some("COMPARE THREADS"));
    compare_title.add_css_class("section-title");
    compare_title.set_halign(gtk::Align::Start);
    advanced.append(&compare_title);
    let compare_left_model = gtk::StringList::new(&[]);
    let compare_right_model = gtk::StringList::new(&[]);
    let compare_left =
        gtk::DropDown::new(Some(compare_left_model.clone()), None::<gtk::Expression>);
    let compare_right =
        gtk::DropDown::new(Some(compare_right_model.clone()), None::<gtk::Expression>);
    compare_left.add_css_class("thread-dropdown");
    compare_right.add_css_class("thread-dropdown");
    compare_left.set_hexpand(true);
    compare_right.set_hexpand(true);
    let compare_selectors = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    compare_selectors.append(&compare_left);
    compare_selectors.append(&compare_right);
    advanced.append(&compare_selectors);
    let compare = gtk::Button::with_label("Compare frames and registers");
    compare.set_hexpand(true);
    advanced.append(&compare);
    root.append(&build_disclosure(
        "CONCURRENCY CONTROLS",
        &advanced,
        false,
        "thread-controls-disclosure",
    ));

    let list = dynamic_list("No threads available");
    let scrolled = gtk::ScrolledWindow::builder()
        .child(&list)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build();
    root.append(&scrolled);

    ThreadControls {
        root,
        list,
        summary,
        search,
        state_filter,
        sort,
        scheduler_locking,
        scheduler_updating: Rc::new(Cell::new(false)),
        non_stop,
        mode_note,
        refresh,
        run_only,
        freeze,
        thaw,
        backtraces,
        compare,
        compare_left,
        compare_right,
        compare_left_model,
        compare_right_model,
        compare_ids: Rc::new(RefCell::new(Vec::new())),
        compare_updating: Rc::new(Cell::new(false)),
        action_handler: Rc::new(RefCell::new(None)),
        action_pending: Rc::new(Cell::new(None)),
        analysis_generation: Rc::new(Cell::new(0)),
        analysis_window: Rc::new(RefCell::new(None)),
        analysis_content: Rc::new(RefCell::new(None)),
    }
}

impl Ui {
    pub(crate) fn set_thread_action_handler(&self, handler: impl Fn(ThreadAction) + 'static) {
        self.thread_controls
            .action_handler
            .replace(Some(Rc::new(handler)));
    }

    pub(crate) fn connect_thread_controls(self: &Rc<Self>) {
        let weak_ui = Rc::downgrade(self);
        self.thread_controls
            .search
            .connect_search_changed(move |_| {
                if let Some(ui) = weak_ui.upgrade() {
                    ui.rerender_threads();
                }
            });
        let weak_ui = Rc::downgrade(self);
        self.thread_controls
            .state_filter
            .connect_selected_notify(move |_| {
                if let Some(ui) = weak_ui.upgrade() {
                    ui.rerender_threads();
                }
            });
        let weak_ui = Rc::downgrade(self);
        self.thread_controls.sort.connect_selected_notify(move |_| {
            if let Some(ui) = weak_ui.upgrade() {
                ui.rerender_threads();
            }
        });
        let weak_ui = Rc::downgrade(self);
        self.thread_controls
            .scheduler_locking
            .connect_selected_notify(move |selector| {
                let Some(ui) = weak_ui.upgrade() else {
                    return;
                };
                if ui.thread_controls.scheduler_updating.get() {
                    return;
                }
                let mode = match selector.selected() {
                    0 => SchedulerLockingMode::Off,
                    1 => SchedulerLockingMode::On,
                    2 => SchedulerLockingMode::Step,
                    3 => SchedulerLockingMode::Replay,
                    _ => return,
                };
                if !ui.emit_thread_action(ThreadAction::SetSchedulerLocking(mode)) {
                    ui.restore_thread_policy_controls();
                }
            });
        let weak_ui = Rc::downgrade(self);
        self.thread_controls
            .non_stop
            .connect_toggled(move |button| {
                let Some(ui) = weak_ui.upgrade() else {
                    return;
                };
                if !ui.thread_controls.scheduler_updating.get()
                    && !ui.emit_thread_action(ThreadAction::SetNonStop(button.is_active()))
                {
                    ui.restore_thread_policy_controls();
                }
            });

        let weak_ui = Rc::downgrade(self);
        self.thread_controls.refresh.connect_clicked(move |_| {
            if let Some(ui) = weak_ui.upgrade() {
                ui.emit_thread_action(ThreadAction::Refresh);
            }
        });
        let weak_ui = Rc::downgrade(self);
        self.thread_controls.run_only.connect_clicked(move |_| {
            if let Some(ui) = weak_ui.upgrade()
                && let Some(id) = ui.current_thread_id()
            {
                ui.emit_thread_action(ThreadAction::RunOnly(id));
            }
        });
        let weak_ui = Rc::downgrade(self);
        self.thread_controls.freeze.connect_clicked(move |_| {
            if let Some(ui) = weak_ui.upgrade()
                && let Some(id) = ui.current_thread_id()
            {
                ui.emit_thread_action(ThreadAction::Freeze(id));
            }
        });
        let weak_ui = Rc::downgrade(self);
        self.thread_controls.thaw.connect_clicked(move |_| {
            if let Some(ui) = weak_ui.upgrade()
                && let Some(id) = ui.current_thread_id()
            {
                ui.emit_thread_action(ThreadAction::Thaw(id));
            }
        });
        let weak_ui = Rc::downgrade(self);
        self.thread_controls.backtraces.connect_clicked(move |_| {
            let Some(ui) = weak_ui.upgrade() else {
                return;
            };
            if !ui.thread_action_dispatch_available() {
                return;
            }
            let generation = ui.begin_thread_analysis(
                "All-thread backtraces",
                "Collecting bounded stacks from stopped threads",
            );
            ui.emit_thread_action(ThreadAction::Backtraces { generation });
        });

        let weak_ui = Rc::downgrade(self);
        self.thread_controls.compare.connect_clicked(move |_| {
            let Some(ui) = weak_ui.upgrade() else {
                return;
            };
            if !ui.thread_action_dispatch_available() {
                return;
            }
            let ids = ui.thread_controls.compare_ids.borrow();
            let left = ids
                .get(ui.thread_controls.compare_left.selected() as usize)
                .cloned();
            let right = ids
                .get(ui.thread_controls.compare_right.selected() as usize)
                .cloned();
            drop(ids);
            let (Some(left), Some(right)) = (left, right) else {
                return;
            };
            let generation = ui.begin_thread_analysis(
                "Compare threads",
                "Reading frames and registers without changing the selected thread",
            );
            ui.emit_thread_action(ThreadAction::Compare {
                generation,
                left,
                right,
            });
        });
        let weak_ui = Rc::downgrade(self);
        self.thread_controls
            .compare_left
            .connect_selected_notify(move |_| {
                if let Some(ui) = weak_ui.upgrade()
                    && !ui.thread_controls.compare_updating.get()
                {
                    ui.update_thread_control_sensitivity();
                }
            });
        let weak_ui = Rc::downgrade(self);
        self.thread_controls
            .compare_right
            .connect_selected_notify(move |_| {
                if let Some(ui) = weak_ui.upgrade()
                    && !ui.thread_controls.compare_updating.get()
                {
                    ui.update_thread_control_sensitivity();
                }
            });
        self.update_thread_control_sensitivity();
    }

    fn emit_thread_action(&self, action: ThreadAction) -> bool {
        if !self.thread_action_can_dispatch(&action) {
            return false;
        }
        let handler = self.thread_controls.action_handler.borrow().clone();
        if let Some(handler) = handler {
            handler(action);
            true
        } else {
            false
        }
    }

    fn thread_action_dispatch_available(&self) -> bool {
        self.debugger_ready.get()
            && !self.command_pending.get()
            && !self.debugger_state.get().transition_pending()
            && !self.session_pending.get()
            && !self.native_until_active.get()
            && !self.debugger_state.get().resynchronizing()
            && self.thread_controls.action_pending.get().is_none()
    }

    pub(crate) fn thread_selection_can_dispatch(&self, id: &str) -> bool {
        self.debugger_ready.get()
            && !self.command_pending.get()
            && !self.debugger_state.get().transition_pending()
            && !self.session_pending.get()
            && !self.native_until_active.get()
            && !self.debugger_state.get().resynchronizing()
            && self.inferior_action_pending.get().is_none()
            && self.thread_controls.action_pending.get().is_none()
            && self.current_thread_id().as_deref() != Some(id)
            && self
                .latest_threads
                .borrow()
                .as_ref()
                .is_some_and(|state| state.source_threads.iter().any(|thread| thread.id == id))
    }

    pub(crate) fn frame_selection_can_dispatch(&self, level: u32) -> bool {
        self.stopped_inspection_available()
            && self.inferior_action_pending.get().is_none()
            && self.thread_controls.action_pending.get().is_none()
            && self.selected_frame_level.get() != level
            && self
                .latest_frames
                .borrow()
                .iter()
                .any(|frame| frame.level == level)
    }

    pub(crate) fn thread_action_can_dispatch(&self, action: &ThreadAction) -> bool {
        self.thread_action_dispatch_available() && self.thread_action_is_current(action)
    }

    fn thread_action_is_current(&self, action: &ThreadAction) -> bool {
        let latest = self.latest_threads.borrow();
        let threads = latest
            .as_ref()
            .map(|state| state.source_threads.as_slice())
            .unwrap_or_default();
        let stopped = |id: &str| {
            threads
                .iter()
                .any(|thread| thread.id == id && thread.state == "stopped")
        };
        let running = |id: &str| {
            threads
                .iter()
                .any(|thread| thread.id == id && thread.state == "running")
        };
        match action {
            ThreadAction::Refresh | ThreadAction::SetSchedulerLocking(_) => true,
            ThreadAction::SetNonStop(_) => !self.inferior_has_started(),
            ThreadAction::RunOnly(id) => {
                stopped(id) && threads.iter().all(|thread| thread.state != "running")
            }
            ThreadAction::Freeze(id) => self.non_stop_mode() == Some(true) && running(id),
            ThreadAction::Thaw(id) => self.non_stop_mode() == Some(true) && stopped(id),
            ThreadAction::Backtraces { generation } => {
                self.is_thread_analysis_current(*generation)
                    && threads.iter().any(|thread| thread.state == "stopped")
            }
            ThreadAction::Compare {
                generation,
                left,
                right,
            } => {
                self.is_thread_analysis_current(*generation)
                    && left != right
                    && stopped(left)
                    && stopped(right)
            }
            ThreadAction::SelectFrame { thread, .. } => stopped(thread),
        }
    }

    pub(crate) fn set_thread_action_pending(&self, pending: Option<ThreadActionPending>) {
        if self.thread_controls.action_pending.replace(pending) != pending {
            self.update_control_sensitivity();
            self.update_thread_control_sensitivity();
            self.render_inferior_controls();
        }
    }

    pub(crate) fn finish_thread_execution_action(&self) {
        if self.thread_controls.action_pending.get() == Some(ThreadActionPending::Execution) {
            self.set_thread_action_pending(None);
        }
    }

    pub(crate) fn thread_execution_transition_matches(
        &self,
        thread_id: Option<&str>,
        all_stopped: bool,
    ) -> bool {
        if self.thread_controls.action_pending.get() != Some(ThreadActionPending::Execution) {
            return false;
        }
        if all_stopped {
            return true;
        }
        let active = self.active_thread_execution.borrow();
        super::controls::execution_event_matches_thread(active.as_deref(), thread_id, false)
    }

    pub(crate) fn clear_thread_action_pending(&self) {
        self.set_thread_action_pending(None);
    }

    pub(crate) fn finish_thread_analysis_action(&self, generation: u64) -> bool {
        if self.thread_controls.analysis_generation.get() != generation {
            return false;
        }
        if self.thread_controls.action_pending.get() == Some(ThreadActionPending::Analysis) {
            self.set_thread_action_pending(None);
        }
        true
    }

    pub(crate) fn is_thread_analysis_current(&self, generation: u64) -> bool {
        self.thread_controls.analysis_generation.get() == generation
    }

    pub(super) fn reset_thread_analysis(&self) {
        self.thread_controls.analysis_generation.set(
            self.thread_controls
                .analysis_generation
                .get()
                .wrapping_add(1),
        );
        if self.thread_controls.action_pending.get() == Some(ThreadActionPending::Analysis) {
            self.set_thread_action_pending(None);
        }
        self.thread_controls.analysis_content.borrow_mut().take();
        let window = { self.thread_controls.analysis_window.borrow_mut().take() };
        if let Some(window) = window {
            window.close();
        }
    }

    pub(crate) fn set_thread_control_policy(
        &self,
        scheduler: Option<SchedulerLockingMode>,
        non_stop: Option<bool>,
    ) {
        self.scheduler_locking.set(scheduler);
        self.non_stop_mode.set(non_stop);
        self.restore_thread_policy_controls();
        let mode_note = match non_stop {
            Some(true) => "Non-stop mode: individual threads can be frozen and thawed",
            Some(false) => {
                "All-stop mode: Run only uses scheduler locking. Freeze and thaw require a new non-stop session"
            }
            None => "GDB did not report its thread-control mode",
        };
        set_label_text(&self.thread_controls.mode_note, mode_note);
        self.update_thread_control_sensitivity();
    }

    fn restore_thread_policy_controls(&self) {
        self.thread_controls.scheduler_updating.set(true);
        let scheduler = self
            .scheduler_locking
            .get()
            .map_or(gtk::INVALID_LIST_POSITION, SchedulerLockingMode::index);
        if self.thread_controls.scheduler_locking.selected() != scheduler {
            self.thread_controls
                .scheduler_locking
                .set_selected(scheduler);
        }
        let non_stop = self.non_stop_mode.get().unwrap_or(false);
        if self.thread_controls.non_stop.is_active() != non_stop {
            self.thread_controls.non_stop.set_active(non_stop);
        }
        self.thread_controls.scheduler_updating.set(false);
    }

    pub(crate) fn scheduler_locking_mode(&self) -> Option<SchedulerLockingMode> {
        self.scheduler_locking.get()
    }

    pub(crate) fn start_thread_policy_refresh(&self) -> u64 {
        let generation = self.thread_policy_generation.get().wrapping_add(1);
        self.thread_policy_generation.set(generation);
        generation
    }

    pub(crate) fn is_thread_policy_refresh_current(&self, generation: u64) -> bool {
        self.thread_policy_generation.get() == generation
    }

    pub(crate) fn non_stop_mode(&self) -> Option<bool> {
        self.non_stop_mode.get()
    }

    pub(crate) fn thread_snapshot(&self) -> Vec<ThreadInfo> {
        self.latest_threads
            .borrow()
            .as_ref()
            .map(|state| state.source_threads.clone())
            .unwrap_or_default()
    }

    /// Update the authoritative thread state without repainting the rows.
    /// A short step can pass through running and stopped faster than GTK can
    /// present either state usefully, but command validation must still see
    /// the running state immediately.
    pub(super) fn stage_threads_for_execution(&self, threads: &[ThreadInfo]) {
        {
            let mut latest = self.latest_threads.borrow_mut();
            let Some(latest) = latest.as_mut() else {
                return;
            };
            latest.source_threads = threads.to_vec();
        }
        self.update_thread_control_sensitivity();
    }

    pub(crate) fn thread_is_stopped(&self, id: &str) -> bool {
        self.latest_threads
            .borrow()
            .as_ref()
            .into_iter()
            .flat_map(|state| &state.source_threads)
            .any(|thread| thread.id == id && thread.state == "stopped")
    }

    pub(crate) fn select_thread_in_view(&self, id: &str) {
        let mut threads = self.thread_snapshot();
        let Some(running) = threads
            .iter()
            .find(|thread| thread.id == id)
            .map(|thread| thread.state == "running")
        else {
            return;
        };
        for thread in &mut threads {
            thread.current = thread.id == id;
        }
        for inferior in self.inferiors.borrow_mut().iter_mut() {
            for thread in &mut inferior.threads {
                thread.current = thread.id == id;
            }
        }
        self.set_current_thread_id(Some(id));
        self.set_controls_running(running);
        self.set_debug_state_stale(running);
        self.latest_threads.borrow_mut().take();
        self.show_threads(&threads);
    }

    pub(super) fn filtered_sorted_thread_page(
        threads: &[ThreadInfo],
        query: &str,
        state_filter: u32,
        sort: u32,
        limit: usize,
    ) -> (Vec<ThreadInfo>, usize) {
        let mut visible = threads
            .iter()
            .filter(|thread| match state_filter {
                1 => thread.state.eq_ignore_ascii_case("stopped"),
                2 => thread.state.eq_ignore_ascii_case("running"),
                _ => true,
            })
            .filter(|thread| thread_matches_query(thread, query))
            .collect::<Vec<_>>();
        let total = visible.len();
        let current = visible.iter().find(|thread| thread.current).copied();
        if visible.len() > limit {
            visible.select_nth_unstable_by(limit, |left, right| thread_order(left, right, sort));
            visible.truncate(limit);
        }
        visible.sort_by(|left, right| thread_order(left, right, sort));
        let mut visible = visible.into_iter().cloned().collect::<Vec<_>>();
        if limit > 0
            && let Some(current) = current
            && !visible.iter().any(|thread| thread.id == current.id)
        {
            visible.pop();
            visible.insert(0, current.clone());
        }
        (visible, total)
    }

    pub(super) fn current_thread_filter_state(&self) -> (String, u32, u32) {
        (
            self.thread_controls
                .search
                .text()
                .trim()
                .to_ascii_lowercase(),
            self.thread_controls.state_filter.selected(),
            self.thread_controls.sort.selected(),
        )
    }

    pub(super) fn sync_thread_controls(&self, threads: &[ThreadInfo], visible: usize) {
        set_label_text(
            &self.thread_controls.summary,
            &format!("{} visible of {} threads", visible, threads.len()),
        );
        let selector_threads =
            bounded_thread_selector_entries(threads, crate::performance::THREAD_SELECTOR_BUDGET);
        let labels = selector_threads
            .iter()
            .map(|thread| {
                format!(
                    "{}  {}",
                    thread.id,
                    thread.name.as_deref().unwrap_or("<unnamed>")
                )
            })
            .collect::<Vec<_>>();
        let ids = selector_threads
            .iter()
            .map(|thread| thread.id.clone())
            .collect::<Vec<_>>();
        if selector_threads.len() < threads.len() {
            self.record_performance_notice(crate::performance::PerformanceNotice::count(
                crate::performance::BudgetOutcome::Partial,
                "thread comparison selectors",
                selector_threads.len(),
                threads.len(),
            ));
        }
        let models_changed = *self.thread_controls.compare_ids.borrow() != ids
            || !string_list_matches(&self.thread_controls.compare_left_model, &labels)
            || !string_list_matches(&self.thread_controls.compare_right_model, &labels);
        if models_changed {
            let old_left = self
                .thread_controls
                .compare_ids
                .borrow()
                .get(self.thread_controls.compare_left.selected() as usize)
                .cloned();
            let old_right = self
                .thread_controls
                .compare_ids
                .borrow()
                .get(self.thread_controls.compare_right.selected() as usize)
                .cloned();
            self.thread_controls.compare_updating.set(true);
            replace_string_list(&self.thread_controls.compare_left_model, &labels);
            replace_string_list(&self.thread_controls.compare_right_model, &labels);
            self.thread_controls.compare_ids.replace(ids.clone());
            let left = (!ids.is_empty()).then(|| {
                old_left
                    .and_then(|id| ids.iter().position(|candidate| candidate == &id))
                    .unwrap_or(0)
            });
            let right = (!ids.is_empty()).then(|| {
                old_right
                    .and_then(|id| ids.iter().position(|candidate| candidate == &id))
                    .unwrap_or_else(|| usize::from(ids.len() > 1))
            });
            self.thread_controls.compare_left.set_selected(
                left.and_then(|index| u32::try_from(index).ok())
                    .unwrap_or(gtk::INVALID_LIST_POSITION),
            );
            self.thread_controls.compare_right.set_selected(
                right
                    .and_then(|index| u32::try_from(index).ok())
                    .unwrap_or(gtk::INVALID_LIST_POSITION),
            );
            self.thread_controls.compare_updating.set(false);
        }
        self.update_thread_control_sensitivity();
    }

    fn rerender_threads(&self) {
        let threads = self.thread_snapshot();
        self.latest_threads.borrow_mut().take();
        self.show_threads(&threads);
    }

    pub(super) fn update_thread_control_sensitivity(&self) {
        let pending = self.thread_controls.action_pending.get().is_some();
        let ready = self.debugger_ready.get();
        let visual_transition = self.execution_visual_transition_pending();
        let selection_available = ready
            && !self.command_pending.get()
            && !self.debugger_state.get().transition_pending()
            && !self.session_pending.get()
            && !self.native_until_active.get()
            && !self.debugger_state.get().resynchronizing()
            && self.inferior_action_pending.get().is_none()
            && !pending;
        let current_thread = self.current_thread_id();
        for (id, button) in self.thread_buttons.borrow().iter() {
            set_transient_execution_sensitive(
                button,
                selection_available && current_thread.as_deref() != Some(id.as_str()),
                visual_transition,
            );
        }
        let debugger_available = ready
            && !self.command_pending.get()
            && !self.debugger_state.get().transition_pending()
            && !self.session_pending.get()
            && !self.native_until_active.get()
            && !self.debugger_state.get().resynchronizing();
        let stopped_inspection_available = debugger_available && !self.debug_state_is_stale();
        for (level, button) in self.frame_buttons.borrow().iter() {
            set_transient_execution_sensitive(
                button,
                stopped_inspection_available
                    && self.inferior_action_pending.get().is_none()
                    && !pending
                    && self.selected_frame_level.get() != *level,
                visual_transition,
            );
        }
        let latest = self.latest_threads.borrow();
        let threads = latest
            .as_ref()
            .map(|state| state.source_threads.as_slice())
            .unwrap_or_default();
        let selected = self
            .current_thread_id()
            .and_then(|id| threads.iter().find(|thread| thread.id == id).cloned());
        let selected_stopped = selected
            .as_ref()
            .is_some_and(|thread| thread.state == "stopped");
        let selected_running = selected
            .as_ref()
            .is_some_and(|thread| thread.state == "running");
        let non_stop = self.non_stop_mode.get() == Some(true);
        let all_threads_stopped = threads.iter().all(|thread| thread.state != "running");
        set_transient_execution_sensitive(
            &self.thread_controls.refresh,
            debugger_available && !pending,
            visual_transition,
        );
        set_transient_execution_sensitive(
            &self.thread_controls.scheduler_locking,
            debugger_available && !pending,
            visual_transition,
        );
        set_transient_execution_sensitive(
            &self.thread_controls.non_stop,
            debugger_available && !pending && !self.inferior_has_started(),
            visual_transition,
        );
        set_transient_execution_sensitive(
            &self.thread_controls.run_only,
            debugger_available && !pending && selected_stopped && all_threads_stopped,
            visual_transition || self.inferior_is_running(),
        );
        set_transient_execution_sensitive(
            &self.thread_controls.freeze,
            debugger_available && !pending && non_stop && selected_running,
            visual_transition,
        );
        set_transient_execution_sensitive(
            &self.thread_controls.thaw,
            debugger_available && !pending && non_stop && selected_stopped,
            visual_transition,
        );
        set_transient_execution_sensitive(
            &self.thread_controls.backtraces,
            debugger_available
                && !pending
                && threads.iter().any(|thread| thread.state == "stopped"),
            visual_transition,
        );
        let ids = self.thread_controls.compare_ids.borrow();
        let left = ids.get(self.thread_controls.compare_left.selected() as usize);
        let right = ids.get(self.thread_controls.compare_right.selected() as usize);
        let comparable = left.zip(right).is_some_and(|(left, right)| {
            left != right
                && threads
                    .iter()
                    .filter(|thread| {
                        thread.state == "stopped" && (thread.id == *left || thread.id == *right)
                    })
                    .count()
                    == 2
        });
        set_transient_execution_sensitive(
            &self.thread_controls.compare,
            debugger_available && !pending && comparable,
            visual_transition,
        );
    }

    fn begin_thread_analysis(self: &Rc<Self>, title: &str, detail: &str) -> u64 {
        let generation = self
            .thread_controls
            .analysis_generation
            .get()
            .wrapping_add(1);
        self.thread_controls.analysis_generation.set(generation);
        self.thread_controls.analysis_content.borrow_mut().take();
        let previous_window = { self.thread_controls.analysis_window.borrow_mut().take() };
        if let Some(window) = previous_window {
            window.close();
        }
        let window = gtk::Window::builder()
            .title(title)
            .transient_for(&self.window)
            .default_width(980)
            .default_height(680)
            .build();
        window.add_css_class("thread-analysis-window");
        let root = gtk::Box::new(gtk::Orientation::Vertical, 8);
        root.set_margin_top(12);
        root.set_margin_bottom(12);
        root.set_margin_start(12);
        root.set_margin_end(12);
        let heading = gtk::Label::new(Some(title));
        heading.add_css_class("dialog-heading");
        heading.set_halign(gtk::Align::Start);
        root.append(&heading);
        let content = gtk::Box::new(gtk::Orientation::Vertical, 6);
        content.set_vexpand(true);
        let loading = gtk::Label::new(Some(detail));
        loading.add_css_class("muted");
        loading.set_halign(gtk::Align::Start);
        content.append(&loading);
        root.append(&content);
        window.set_child(Some(&root));
        connect_escape_to_close(&window);
        self.thread_controls.analysis_content.replace(Some(content));
        self.thread_controls
            .analysis_window
            .replace(Some(window.clone()));
        let weak_ui = Rc::downgrade(self);
        window.connect_close_request(move |_| {
            if let Some(ui) = weak_ui.upgrade()
                && ui.thread_controls.analysis_generation.get() == generation
            {
                ui.thread_controls
                    .analysis_generation
                    .set(generation.wrapping_add(1));
                if ui.thread_controls.action_pending.get() == Some(ThreadActionPending::Analysis) {
                    ui.set_thread_action_pending(None);
                }
                ui.thread_controls.analysis_content.borrow_mut().take();
                ui.thread_controls.analysis_window.borrow_mut().take();
            }
            glib::Propagation::Proceed
        });
        window.present();
        generation
    }

    pub(crate) fn show_thread_analysis_error(&self, generation: u64, message: &str) {
        let Some(content) = self.thread_analysis_content(generation) else {
            return;
        };
        clear_box(&content);
        let error = gtk::Label::new(Some(message));
        error.add_css_class("status-error");
        error.set_halign(gtk::Align::Start);
        error.set_xalign(0.0);
        error.set_wrap(true);
        content.append(&error);
    }

    pub(crate) fn show_thread_backtraces(
        self: &Rc<Self>,
        generation: u64,
        traces: Vec<ThreadBacktrace>,
    ) {
        let Some(content) = self.thread_analysis_content(generation) else {
            return;
        };
        clear_box(&content);
        let stopped = traces.iter().filter(|trace| trace.error.is_none()).count();
        let summary = gtk::Label::new(Some(&format!(
            "{} thread backtraces collected, {} unavailable",
            stopped,
            traces.len().saturating_sub(stopped)
        )));
        summary.add_css_class("muted");
        summary.set_halign(gtk::Align::Start);
        content.append(&summary);
        let results = gtk::Box::new(gtk::Orientation::Vertical, 10);
        for trace in traces {
            let section = gtk::Box::new(gtk::Orientation::Vertical, 3);
            section.add_css_class("thread-backtrace-section");
            let title = gtk::Label::new(Some(&format!(
                "Thread {}  {}  {}",
                trace.thread.id,
                trace.thread.name.as_deref().unwrap_or("<unnamed>"),
                trace.thread.state
            )));
            title.add_css_class("section-title");
            title.set_halign(gtk::Align::Start);
            section.append(&title);
            if let Some(error) = trace.error {
                let error = gtk::Label::new(Some(&error));
                error.add_css_class("status-error");
                error.set_halign(gtk::Align::Start);
                error.set_wrap(true);
                section.append(&error);
            } else if trace.frames.is_empty() {
                section.append(&empty_label("No frames returned"));
            } else {
                for frame in trace.frames {
                    let location = super::debug_state::frame_location_text(&frame);
                    let label = format!(
                        "#{}  {}\n{}",
                        frame.level,
                        compact_function_name(&frame.function),
                        location
                    );
                    let frame_label = gtk::Label::new(Some(&label));
                    frame_label.set_halign(gtk::Align::Fill);
                    frame_label.set_xalign(0.0);
                    frame_label.set_ellipsize(pango::EllipsizeMode::Middle);
                    let button = gtk::Button::builder().child(&frame_label).build();
                    button.add_css_class("thread-backtrace-frame");
                    button.set_halign(gtk::Align::Fill);
                    button.set_tooltip_text(Some(&format!(
                        "Select thread {} and frame {}",
                        trace.thread.id, frame.level
                    )));
                    let action = ThreadAction::SelectFrame {
                        thread: trace.thread.id.clone(),
                        frame: frame.level,
                    };
                    let weak_ui = Rc::downgrade(self);
                    button.connect_clicked(move |_| {
                        if let Some(ui) = weak_ui.upgrade() {
                            ui.emit_thread_action(action.clone());
                        }
                    });
                    section.append(&button);
                }
            }
            results.append(&section);
        }
        let scrolled = gtk::ScrolledWindow::builder()
            .child(&results)
            .vexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .build();
        content.append(&scrolled);
    }

    pub(crate) fn show_thread_comparison(&self, generation: u64, comparison: ThreadComparison) {
        let Some(content) = self.thread_analysis_content(generation) else {
            return;
        };
        clear_box(&content);
        let summary = gtk::Label::new(Some(&format!(
            "Thread {} ({}) compared with thread {} ({})",
            comparison.left.id,
            comparison.left.name.as_deref().unwrap_or("unnamed"),
            comparison.right.id,
            comparison.right.name.as_deref().unwrap_or("unnamed")
        )));
        summary.add_css_class("muted");
        summary.set_halign(gtk::Align::Start);
        content.append(&summary);
        if !comparison.warnings.is_empty() {
            let warning = gtk::Label::new(Some(&comparison.warnings.join("\n")));
            warning.add_css_class("status-warning");
            warning.set_halign(gtk::Align::Start);
            warning.set_xalign(0.0);
            warning.set_wrap(true);
            content.append(&warning);
        }
        let notebook = gtk::Notebook::new();
        notebook.set_vexpand(true);
        notebook.append_page(
            &comparison_table(comparison.frames),
            Some(&gtk::Label::new(Some("Frames"))),
        );
        notebook.append_page(
            &comparison_table(comparison.registers),
            Some(&gtk::Label::new(Some("Registers"))),
        );
        content.append(&notebook);
    }

    fn thread_analysis_content(&self, generation: u64) -> Option<gtk::Box> {
        (self.thread_controls.analysis_generation.get() == generation)
            .then(|| self.thread_controls.analysis_content.borrow().clone())
            .flatten()
    }
}

fn thread_matches_query(thread: &ThreadInfo, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    let frame = thread.frame.as_ref();
    let text = format!(
        "{} {} {} {} {} {} {}",
        thread.id,
        thread.target_id,
        thread.name.as_deref().unwrap_or_default(),
        thread.state,
        thread.core.as_deref().unwrap_or_default(),
        frame
            .map(|frame| frame.function.as_str())
            .unwrap_or_default(),
        thread.pc_symbol.as_deref().unwrap_or_default(),
    )
    .to_ascii_lowercase();
    query.split_whitespace().all(|term| text.contains(term))
}

fn thread_id_order(left: &ThreadInfo, right: &ThreadInfo) -> std::cmp::Ordering {
    crate::debugger::compare_thread_ids(&left.id, &right.id)
}

fn thread_order(left: &ThreadInfo, right: &ThreadInfo, sort: u32) -> std::cmp::Ordering {
    match sort {
        1 => thread_id_order(left, right),
        2 => thread_name(left)
            .cmp(thread_name(right))
            .then_with(|| thread_id_order(left, right)),
        3 => left
            .state
            .cmp(&right.state)
            .then_with(|| thread_id_order(left, right)),
        4 => thread_core_number(left)
            .cmp(&thread_core_number(right))
            .then_with(|| thread_id_order(left, right)),
        _ => right
            .current
            .cmp(&left.current)
            .then_with(|| thread_id_order(left, right)),
    }
}

fn bounded_thread_selector_entries(threads: &[ThreadInfo], limit: usize) -> Vec<&ThreadInfo> {
    let mut entries = threads.iter().take(limit).collect::<Vec<_>>();
    if limit > 0
        && let Some(current) = threads.iter().find(|thread| thread.current)
        && !entries.iter().any(|thread| thread.id == current.id)
    {
        entries.pop();
        entries.insert(0, current);
    }
    entries
}

fn thread_name(thread: &ThreadInfo) -> &str {
    thread.name.as_deref().unwrap_or("")
}

fn thread_core_number(thread: &ThreadInfo) -> u64 {
    thread
        .core
        .as_deref()
        .and_then(|core| core.parse().ok())
        .unwrap_or(u64::MAX)
}

fn replace_string_list(model: &gtk::StringList, values: &[String]) {
    let values = values.iter().map(String::as_str).collect::<Vec<_>>();
    model.splice(0, model.n_items(), &values);
}

fn string_list_matches(model: &gtk::StringList, values: &[String]) -> bool {
    model.n_items() as usize == values.len()
        && values
            .iter()
            .enumerate()
            .all(|(index, value)| model.string(index as u32).as_deref() == Some(value.as_str()))
}

fn comparison_table(rows: Vec<ThreadComparisonRow>) -> gtk::ScrolledWindow {
    let store = gio::ListStore::new::<glib::BoxedAnyObject>();
    replace_boxed_store(&store, rows);
    let selection = gtk::NoSelection::new(Some(store));
    let view = gtk::ColumnView::new(Some(selection));
    view.add_css_class("debug-table");
    view.set_vexpand(true);
    for (title, width, expand, field) in [
        ("ITEM", 190, false, 0_u8),
        ("LEFT THREAD", 330, true, 1),
        ("RIGHT THREAD", 330, true, 2),
    ] {
        let factory = gtk::SignalListItemFactory::new();
        factory.connect_setup(move |_, object| {
            let Some(item) = object.downcast_ref::<gtk::ListItem>() else {
                return;
            };
            let label = gtk::Label::new(None);
            label.add_css_class("debug-table-cell");
            label.set_halign(gtk::Align::Start);
            label.set_ellipsize(pango::EllipsizeMode::Middle);
            enable_stable_text_selection(&label);
            item.set_child(Some(&label));
        });
        factory.connect_bind(move |_, object| {
            let Some(item) = object.downcast_ref::<gtk::ListItem>() else {
                return;
            };
            let Some(label) = item.child().and_downcast::<gtk::Label>() else {
                return;
            };
            let Some(data) = item.item().and_downcast::<glib::BoxedAnyObject>() else {
                return;
            };
            let row = data.borrow::<ThreadComparisonRow>();
            label.set_text(match field {
                0 => &row.item,
                1 => &row.left,
                _ => &row.right,
            });
            if row.different {
                label.add_css_class("thread-comparison-changed");
            } else {
                label.remove_css_class("thread-comparison-changed");
            }
        });
        let column = gtk::ColumnViewColumn::new(Some(title), Some(factory));
        column.set_fixed_width(width);
        column.set_expand(expand);
        column.set_resizable(true);
        view.append_column(&column);
    }
    gtk::ScrolledWindow::builder()
        .child(&view)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thread(id: &str, name: &str, state: &str, core: &str) -> ThreadInfo {
        ThreadInfo {
            id: id.to_owned(),
            group_id: Some(String::from("i1")),
            target_id: format!("Thread {id}"),
            name: Some(name.to_owned()),
            state: state.to_owned(),
            core: Some(core.to_owned()),
            frame: None,
            pc_symbol: None,
            current: false,
        }
    }

    #[test]
    fn filters_across_thread_identity_state_and_frame_metadata() {
        let worker = thread("1.7", "http-worker", "stopped", "3");
        assert!(thread_matches_query(&worker, "worker stopped"));
        assert!(!thread_matches_query(&worker, "1.7 core"));
        assert!(!thread_matches_query(&worker, "running"));
    }

    #[test]
    fn numeric_thread_ids_sort_before_lexically_smaller_double_digits() {
        let mut threads = [
            thread("10", "ten", "stopped", "2"),
            thread("2", "two", "stopped", "1"),
            thread("1.3", "qualified", "stopped", "0"),
        ];
        threads.sort_by(thread_id_order);
        assert_eq!(
            threads.map(|thread| thread.id),
            [String::from("1.3"), String::from("2"), String::from("10")]
        );
    }

    #[test]
    fn bounded_thread_page_keeps_counts_and_the_current_thread() {
        let mut threads = (1..=1_000)
            .map(|id| thread(&id.to_string(), "worker", "stopped", "0"))
            .collect::<Vec<_>>();
        threads.last_mut().unwrap().current = true;

        let (page, total) = Ui::filtered_sorted_thread_page(&threads, "", 0, 1, 10);

        assert_eq!(total, 1_000);
        assert_eq!(page.len(), 10);
        assert_eq!(page[0].id, "1000");
        assert!(page[0].current);
    }

    #[test]
    fn bounded_thread_selectors_keep_the_current_thread() {
        let mut threads = (1..=20)
            .map(|id| thread(&id.to_string(), "worker", "stopped", "0"))
            .collect::<Vec<_>>();
        threads.last_mut().unwrap().current = true;

        let selectors = bounded_thread_selector_entries(&threads, 5);

        assert_eq!(selectors.len(), 5);
        assert_eq!(selectors[0].id, "20");
    }
}
