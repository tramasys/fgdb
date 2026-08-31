use super::*;

impl Ui {
    pub(crate) fn set_inferior_action_handler(&self, handler: impl Fn(InferiorAction) + 'static) {
        self.inferior_controls
            .action_handler
            .replace(Some(Rc::new(handler)));
    }

    pub(crate) fn connect_inferior_controls(self: &Rc<Self>) {
        let weak_ui = Rc::downgrade(self);
        self.inferior_controls
            .selector
            .connect_selected_notify(move |selector| {
                let Some(ui) = weak_ui.upgrade() else {
                    return;
                };
                if ui.inferior_controls.selector_updating.get() {
                    return;
                }
                let Some(id) = ui
                    .inferior_controls
                    .selector_ids
                    .borrow()
                    .get(selector.selected() as usize)
                    .cloned()
                else {
                    return;
                };
                ui.emit_inferior_action(InferiorAction::Select(id));
            });

        let weak_ui = Rc::downgrade(self);
        self.inferior_controls
            .follow_parent
            .connect_toggled(move |button| {
                if !button.is_active() {
                    return;
                }
                let Some(ui) = weak_ui.upgrade() else {
                    return;
                };
                if !ui.inferior_controls.selector_updating.get() {
                    ui.emit_inferior_action(InferiorAction::SetFollowFork(ForkFollowMode::Parent));
                }
            });
        let weak_ui = Rc::downgrade(self);
        self.inferior_controls
            .follow_child
            .connect_toggled(move |button| {
                if !button.is_active() {
                    return;
                }
                let Some(ui) = weak_ui.upgrade() else {
                    return;
                };
                if !ui.inferior_controls.selector_updating.get() {
                    ui.emit_inferior_action(InferiorAction::SetFollowFork(ForkFollowMode::Child));
                }
            });
        let weak_ui = Rc::downgrade(self);
        self.inferior_controls
            .detach_on_fork
            .connect_toggled(move |button| {
                let Some(ui) = weak_ui.upgrade() else {
                    return;
                };
                if !ui.inferior_controls.selector_updating.get() {
                    ui.emit_inferior_action(InferiorAction::SetDetachOnFork(button.is_active()));
                }
            });

        let weak_ui = Rc::downgrade(self);
        self.inferior_controls
            .switch_parent
            .connect_clicked(move |_| {
                let Some(ui) = weak_ui.upgrade() else {
                    return;
                };
                let Some(selected) = ui.selected_inferior_id() else {
                    return;
                };
                if let Some(parent) = ui.inferior_parents.borrow().get(&selected).cloned() {
                    ui.emit_inferior_action(InferiorAction::Select(parent));
                }
            });
        let weak_ui = Rc::downgrade(self);
        self.inferior_controls
            .switch_child
            .connect_clicked(move |_| {
                let Some(ui) = weak_ui.upgrade() else {
                    return;
                };
                let Some(selected) = ui.selected_inferior_id() else {
                    return;
                };
                if let Some(child) = ui.first_inferior_child(&selected) {
                    ui.emit_inferior_action(InferiorAction::Select(child));
                }
            });
        let weak_ui = Rc::downgrade(self);
        self.inferior_controls.refresh.connect_clicked(move |_| {
            if let Some(ui) = weak_ui.upgrade() {
                ui.emit_inferior_action(InferiorAction::Refresh);
            }
        });
    }

    fn emit_inferior_action(&self, action: InferiorAction) {
        if !self.inferior_action_is_current(&action) {
            return;
        }
        if let Some(handler) = self.inferior_controls.action_handler.borrow().clone() {
            handler(action);
        }
    }

    pub(crate) fn inferior_action_is_current(&self, action: &InferiorAction) -> bool {
        if !self.debugger_ready.get()
            || self.command_pending.get()
            || self.execution_transition_pending.get()
            || self.session_pending.get()
            || self.native_until_active.get()
            || self.resynchronization_pending.get()
            || self.inferior_action_pending.get().is_some()
        {
            return false;
        }
        match action {
            InferiorAction::Select(id) => {
                self.selected_inferior_id.borrow().as_deref() != Some(id)
                    && self
                        .inferiors
                        .borrow()
                        .iter()
                        .any(|inferior| inferior.id == *id)
            }
            InferiorAction::Resume(id) => self
                .inferiors
                .borrow()
                .iter()
                .any(|inferior| inferior.id == *id && inferior.state == InferiorState::Stopped),
            InferiorAction::Interrupt(id) => self
                .inferiors
                .borrow()
                .iter()
                .any(|inferior| inferior.id == *id && inferior.state == InferiorState::Running),
            InferiorAction::SetFollowFork(_)
            | InferiorAction::SetDetachOnFork(_)
            | InferiorAction::Refresh => true,
        }
    }

    pub(crate) fn start_inferior_refresh(&self) -> u64 {
        let generation = self.inferior_refresh_generation.get().wrapping_add(1);
        self.inferior_refresh_generation.set(generation);
        generation
    }

    pub(crate) fn is_inferior_refresh_current(&self, generation: u64) -> bool {
        self.inferior_refresh_generation.get() == generation
    }

    pub(crate) fn start_fork_policy_refresh(&self) -> u64 {
        let generation = self.fork_policy_generation.get().wrapping_add(1);
        self.fork_policy_generation.set(generation);
        generation
    }

    pub(crate) fn is_fork_policy_refresh_current(&self, generation: u64) -> bool {
        self.fork_policy_generation.get() == generation
    }

    pub(crate) fn selected_inferior_id(&self) -> Option<String> {
        self.selected_inferior_id.borrow().clone()
    }

    pub(crate) fn selected_inferior_context_stopped(&self) -> bool {
        let selected = self.selected_inferior_id.borrow();
        let current_thread = self.current_thread_id();
        self.inferiors
            .borrow()
            .iter()
            .find(|inferior| Some(inferior.id.as_str()) == selected.as_deref())
            .is_some_and(|inferior| {
                inferior_context_thread(inferior, current_thread.as_deref())
                    .map_or(inferior.state == InferiorState::Stopped, |thread| {
                        thread.state == "stopped"
                    })
            })
    }

    pub(crate) fn inferior_for_thread(&self, thread_id: &str) -> Option<String> {
        self.inferiors
            .borrow()
            .iter()
            .find(|inferior| inferior.threads.iter().any(|thread| thread.id == thread_id))
            .map(|inferior| inferior.id.clone())
            .or_else(|| self.thread_inferior_ids.borrow().get(thread_id).cloned())
    }

    pub(crate) fn record_thread_group(&self, thread_id: Option<&str>, group_id: Option<&str>) {
        if let (Some(thread_id), Some(group_id)) = (thread_id, group_id) {
            self.thread_inferior_ids
                .borrow_mut()
                .insert(thread_id.to_owned(), group_id.to_owned());
        }
    }

    pub(crate) fn forget_thread_group(&self, thread_id: &str) {
        self.thread_inferior_ids.borrow_mut().remove(thread_id);
    }

    pub(crate) fn apply_gdb_selection(&self, thread_id: Option<&str>, group_id: Option<&str>) {
        let selected_group = group_id
            .map(str::to_owned)
            .or_else(|| thread_id.and_then(|thread| self.inferior_for_thread(thread)));
        if let Some(group) = selected_group.as_deref() {
            self.set_selected_inferior(group);
        }
        let Some(thread_id) = thread_id else {
            return;
        };
        self.set_current_thread_id(Some(thread_id));
        let mut found = false;
        for inferior in self.inferiors.borrow_mut().iter_mut() {
            for thread in &mut inferior.threads {
                thread.current = thread.id == thread_id;
                found |= thread.current;
            }
        }
        if found {
            self.latest_threads.borrow_mut().take();
            self.apply_selected_inferior_state();
            self.render_inferior_controls();
        }
    }

    pub(crate) fn threads_for_selected_inferior(
        &self,
        mut threads: Vec<ThreadInfo>,
    ) -> Vec<ThreadInfo> {
        let inferiors = self.inferiors.borrow();
        if inferiors.is_empty() {
            return threads;
        }
        let selected = self.selected_inferior_id.borrow();
        threads.retain_mut(|thread| {
            let group = inferiors.iter().find(|inferior| {
                inferior
                    .threads
                    .iter()
                    .any(|candidate| candidate.id == thread.id)
            });
            if let Some(group) = group {
                thread.group_id = Some(group.id.clone());
            }
            group.is_some_and(|group| Some(group.id.as_str()) == selected.as_deref())
        });
        threads
    }

    pub(crate) fn reconcile_stop_owner_from_threads(&self, threads: &[ThreadInfo]) -> bool {
        if self.stop_owner_inferior_id.borrow().is_some() {
            return false;
        }
        let Some(thread) = threads
            .iter()
            .find(|thread| thread.current && thread.state == "stopped")
        else {
            return false;
        };
        let group = thread
            .group_id
            .clone()
            .or_else(|| self.inferior_for_thread(&thread.id));
        let Some(group) = group.filter(|group| {
            self.inferiors
                .borrow()
                .iter()
                .any(|inferior| inferior.id == *group && inferior.state == InferiorState::Stopped)
        }) else {
            return false;
        };
        self.stop_owner_thread_id.replace(Some(thread.id.clone()));
        self.stop_owner_inferior_id.replace(Some(group.clone()));
        if self.selected_inferior_id.borrow().as_deref() != Some(group.as_str()) {
            self.set_selected_inferior(&group);
            true
        } else {
            self.render_inferior_controls();
            false
        }
    }

    pub(crate) fn show_inferiors(&self, inferiors: Vec<InferiorInfo>) {
        let unresolved_stop_owner = self.stop_owner_inferior_id.borrow().is_none()
            && self.stop_owner_thread_id.borrow().is_some();
        if unresolved_stop_owner
            && let Some(owner) = self
                .stop_owner_thread_id
                .borrow()
                .as_deref()
                .and_then(|thread| {
                    inferiors
                        .iter()
                        .find(|inferior| {
                            inferior
                                .threads
                                .iter()
                                .any(|candidate| candidate.id == thread)
                        })
                        .map(|inferior| inferior.id.clone())
                })
        {
            self.stop_owner_inferior_id.replace(Some(owner));
        }
        self.update_local_inferior_relationships(&inferiors);
        let existing = self.selected_inferior_id.borrow().clone();
        let stop_owner = unresolved_stop_owner
            .then(|| self.stop_owner_inferior_id.borrow().clone())
            .flatten();
        let selected = stop_owner
            .filter(|id| inferiors.iter().any(|inferior| inferior.id == *id))
            .or_else(|| existing.filter(|id| inferiors.iter().any(|inferior| inferior.id == *id)))
            .or_else(|| {
                self.current_thread_id().and_then(|thread| {
                    inferiors
                        .iter()
                        .find(|inferior| {
                            inferior
                                .threads
                                .iter()
                                .any(|candidate| candidate.id == thread)
                        })
                        .map(|inferior| inferior.id.clone())
                })
            })
            .or_else(|| {
                inferiors
                    .iter()
                    .find(|inferior| inferior.state == InferiorState::Stopped)
                    .or_else(|| inferiors.iter().find(|inferior| inferior.state.is_live()))
                    .or_else(|| inferiors.first())
                    .map(|inferior| inferior.id.clone())
            });
        let selection_changed = self.selected_inferior_id.borrow().as_ref() != selected.as_ref();
        self.inferiors.replace(inferiors);
        self.selected_inferior_id.replace(selected);
        if selection_changed {
            self.reset_target_abi();
            self.invalidate_allocator_probe_cache();
            self.latest_modules.borrow_mut().clear();
            clear_box(&self.modules_list);
            self.modules_list
                .append(&empty_label("Modules refresh after selecting an inferior"));
        }
        self.apply_selected_inferior_state();
        self.render_inferior_controls();
    }

    pub(crate) fn set_selected_inferior(&self, id: &str) -> bool {
        if !self
            .inferiors
            .borrow()
            .iter()
            .any(|inferior| inferior.id == id)
        {
            return false;
        }
        if self.selected_inferior_id.borrow().as_deref() == Some(id) {
            return true;
        }
        self.selected_inferior_id.replace(Some(id.to_owned()));
        self.reset_target_abi();
        self.invalidate_allocator_probe_cache();
        self.latest_modules.borrow_mut().clear();
        clear_box(&self.modules_list);
        self.modules_list
            .append(&empty_label("Loading modules for the selected inferior"));
        self.apply_selected_inferior_state();
        self.render_inferior_controls();
        true
    }

    fn apply_selected_inferior_state(&self) {
        let current_thread = self.current_thread_id();
        let state = {
            let selected = self.selected_inferior_id.borrow();
            self.inferiors
                .borrow()
                .iter()
                .find(|inferior| Some(inferior.id.as_str()) == selected.as_deref())
                .map(|inferior| {
                    (
                        inferior.pid,
                        inferior.pid.is_some() && inferior.state.is_live(),
                        inferior_context_running(inferior, current_thread.as_deref()),
                        inferior.threads.clone(),
                    )
                })
        };
        let Some((pid, started, running, threads)) = state else {
            self.set_inferior_pid(None);
            self.set_inferior_started(false);
            self.set_controls_running(false);
            self.show_threads(&[]);
            return;
        };
        self.set_inferior_pid(pid);
        self.set_inferior_started(started);
        self.set_controls_running(running);
        self.show_threads(&threads);
    }

    pub(crate) fn mark_inferior_running(&self, thread_id: Option<&str>) -> (bool, bool) {
        let pending_group = self.pending_execution_inferior.borrow().clone();
        let exact_thread = thread_id.filter(|thread| *thread != "all");
        let current_thread = self.current_thread_id();
        let group = exact_thread
            .and_then(|thread| self.inferior_for_thread(thread))
            .or_else(|| {
                // In non-stop mode the first running notification can arrive
                // before the initial thread-group refresh. A sole live
                // inferior is still an unambiguous owner for that thread.
                exact_thread.and_then(|_| {
                    let inferiors = self.inferiors.borrow();
                    let mut live = inferiors
                        .iter()
                        .filter(|inferior| inferior.state != InferiorState::Exited);
                    let only = live.next()?;
                    live.next().is_none().then(|| only.id.clone())
                })
            })
            .or_else(|| {
                exact_thread
                    .is_none()
                    .then(|| pending_group.clone())
                    .flatten()
            });
        let pending_affected = pending_group.is_some() && group == pending_group;
        if pending_affected {
            self.pending_execution_inferior.borrow_mut().take();
        }
        // An exact thread ID that is not in our latest snapshot means the
        // snapshot is stale. It does not mean every inferior started running.
        let all = running_event_affects_all(group.as_deref(), exact_thread);
        let mut inferiors = self.inferiors.borrow_mut();
        for inferior in inferiors.iter_mut() {
            if (all || group.as_deref() == Some(inferior.id.as_str())) && inferior.state.is_live() {
                inferior.state = InferiorState::Running;
                if let Some(thread_id) = exact_thread {
                    if let Some(thread) = inferior
                        .threads
                        .iter_mut()
                        .find(|thread| thread.id == thread_id)
                    {
                        thread.state = String::from("running");
                    }
                } else {
                    for thread in &mut inferior.threads {
                        thread.state = String::from("running");
                    }
                }
            }
        }
        drop(inferiors);
        let selected_group_affected = all
            || group.as_deref() == self.selected_inferior_id.borrow().as_deref()
            || self.selected_inferior_id.borrow().is_none();
        let selected_affected = exact_thread.map_or(selected_group_affected, |thread| {
            current_thread
                .as_deref()
                .map_or(selected_group_affected, |current| current == thread)
        });
        let stop_owner_affected = all
            || exact_thread.map_or_else(
                || self.stop_owner_inferior_id.borrow().as_deref() == group.as_deref(),
                |thread| self.stop_owner_thread_id.borrow().as_deref() == Some(thread),
            );
        if stop_owner_affected {
            self.stop_owner_inferior_id.borrow_mut().take();
            self.stop_owner_thread_id.borrow_mut().take();
        }
        if selected_affected {
            self.apply_selected_inferior_state();
        }
        self.render_inferior_controls();
        (selected_affected, pending_affected)
    }

    pub(crate) fn mark_inferior_stopped(&self, thread_id: Option<&str>, all_stopped: bool) -> bool {
        let pending_group = self.pending_execution_inferior.borrow().clone();
        let group = thread_id
            .and_then(|thread| self.inferior_for_thread(thread))
            .or_else(|| thread_id.is_none().then(|| pending_group.clone()).flatten());
        let pending_affected =
            pending_group.is_some() && (all_stopped || (group.is_some() && group == pending_group));
        if pending_affected {
            self.pending_execution_inferior.borrow_mut().take();
        }
        if let Some(thread_id) = thread_id {
            self.stop_owner_thread_id
                .replace(Some(thread_id.to_owned()));
        } else {
            self.stop_owner_thread_id.borrow_mut().take();
        }
        if let Some(group) = group.as_ref() {
            self.stop_owner_inferior_id.replace(Some(group.clone()));
            self.selected_inferior_id.replace(Some(group.clone()));
        } else {
            self.stop_owner_inferior_id.borrow_mut().take();
        }
        let mut inferiors = self.inferiors.borrow_mut();
        for inferior in inferiors.iter_mut() {
            if all_stopped && inferior.state.is_live() {
                inferior.state = InferiorState::Stopped;
                for thread in &mut inferior.threads {
                    thread.state = String::from("stopped");
                }
            } else if group.as_deref() == Some(inferior.id.as_str()) && inferior.state.is_live() {
                if let Some(thread_id) = thread_id
                    && let Some(thread) = inferior
                        .threads
                        .iter_mut()
                        .find(|thread| thread.id == thread_id)
                {
                    thread.state = String::from("stopped");
                }
                inferior.state = if inferior
                    .threads
                    .iter()
                    .any(|thread| thread.state == "running")
                {
                    InferiorState::Running
                } else {
                    InferiorState::Stopped
                };
            }
        }
        drop(inferiors);
        if !self.inferiors.borrow().is_empty() {
            self.apply_selected_inferior_state();
        }
        self.render_inferior_controls();
        pending_affected
    }

    pub(crate) fn record_pending_fork(&self, thread_id: Option<&str>, child_pid: Option<u32>) {
        let Some(child_pid) = child_pid else {
            return;
        };
        let parent = thread_id
            .and_then(|thread| self.inferior_for_thread(thread))
            .or_else(|| self.selected_inferior_id());
        if let Some(parent) = parent {
            self.pending_fork_parents
                .borrow_mut()
                .insert(child_pid, parent);
        }
    }

    pub(crate) fn record_inferior_started(&self, id: &str, pid: Option<u32>) {
        if let Some(parent) =
            pid.and_then(|pid| self.pending_fork_parents.borrow_mut().remove(&pid))
        {
            self.inferior_parents
                .borrow_mut()
                .insert(id.to_owned(), parent);
        }
    }

    pub(crate) fn record_inferior_exited(&self, id: &str) {
        self.thread_inferior_ids
            .borrow_mut()
            .retain(|_, group| group != id);
        if let Some(inferior) = self
            .inferiors
            .borrow_mut()
            .iter_mut()
            .find(|inferior| inferior.id == id)
        {
            inferior.state = InferiorState::Exited;
        }
        if self.stop_owner_inferior_id.borrow().as_deref() == Some(id) {
            self.stop_owner_inferior_id.borrow_mut().take();
            self.stop_owner_thread_id.borrow_mut().take();
        }
        if self.selected_inferior_id.borrow().as_deref() == Some(id) {
            self.apply_selected_inferior_state();
        }
        self.render_inferior_controls();
    }

    pub(crate) fn clear_inferiors(&self) {
        self.start_inferior_refresh();
        self.inferiors.borrow_mut().clear();
        self.thread_inferior_ids.borrow_mut().clear();
        self.selected_inferior_id.borrow_mut().take();
        self.stop_owner_inferior_id.borrow_mut().take();
        self.stop_owner_thread_id.borrow_mut().take();
        self.inferior_parents.borrow_mut().clear();
        self.pending_fork_parents.borrow_mut().clear();
        self.pending_execution_inferior.borrow_mut().take();
        self.inferior_action_pending.set(None);
        self.set_inferior_pid(None);
        self.render_inferior_controls();
    }

    pub(crate) fn set_fork_follow_mode(&self, mode: Option<ForkFollowMode>) {
        self.fork_follow_mode.set(mode);
        self.render_inferior_policy();
    }

    pub(crate) fn set_detach_on_fork(&self, detach: Option<bool>) {
        self.detach_on_fork.set(detach);
        self.render_inferior_policy();
    }

    pub(crate) fn set_fork_policy(&self, mode: Option<ForkFollowMode>, detach: Option<bool>) {
        self.fork_follow_mode.set(mode);
        self.detach_on_fork.set(detach);
        self.render_inferior_policy();
    }

    pub(crate) fn set_inferior_action_pending(&self, pending: Option<InferiorActionPending>) {
        if self.inferior_action_pending.replace(pending) != pending {
            self.render_inferior_controls();
            self.update_control_sensitivity();
            self.update_thread_control_sensitivity();
        }
    }

    pub(crate) fn finish_inferior_execution_action(&self) {
        if self.inferior_action_pending.get() == Some(InferiorActionPending::Execution) {
            self.inferior_execution_generation
                .set(self.inferior_execution_generation.get().wrapping_add(1));
            self.set_inferior_action_pending(None);
        }
    }

    pub(crate) fn begin_inferior_execution_action(&self, id: String) -> u64 {
        let generation = self.inferior_execution_generation.get().wrapping_add(1);
        self.inferior_execution_generation.set(generation);
        self.set_inferior_action_pending(Some(InferiorActionPending::Execution));
        self.set_pending_execution_inferior(Some(id));
        generation
    }

    pub(crate) fn inferior_execution_action_pending_for(&self, id: &str, generation: u64) -> bool {
        self.inferior_execution_generation.get() == generation
            && self.inferior_action_pending.get() == Some(InferiorActionPending::Execution)
            && self.pending_execution_inferior.borrow().as_deref() == Some(id)
    }

    pub(crate) fn clear_inferior_action_pending(&self) {
        if self.inferior_action_pending.get() == Some(InferiorActionPending::Execution) {
            self.inferior_execution_generation
                .set(self.inferior_execution_generation.get().wrapping_add(1));
        }
        self.set_inferior_action_pending(None);
    }

    pub(crate) fn set_pending_execution_inferior(&self, id: Option<String>) {
        self.pending_execution_inferior.replace(id);
    }

    pub(crate) fn pending_execution_inferior(&self) -> Option<String> {
        self.pending_execution_inferior.borrow().clone()
    }

    pub(crate) fn set_active_thread_execution(&self, id: Option<String>) {
        self.active_thread_execution.replace(id);
    }

    pub(crate) fn active_thread_execution(&self) -> Option<String> {
        self.active_thread_execution.borrow().clone()
    }

    pub(crate) fn set_thread_execution_exit_candidate(&self, id: Option<String>) {
        self.thread_execution_exit_candidate.replace(id);
    }

    pub(crate) fn thread_execution_exit_candidate(&self) -> Option<String> {
        self.thread_execution_exit_candidate.borrow().clone()
    }

    pub(crate) fn stop_owner_summary(&self) -> Option<String> {
        let group = self.stop_owner_inferior_id.borrow().clone()?;
        let pid = self
            .inferiors
            .borrow()
            .iter()
            .find(|inferior| inferior.id == group)
            .and_then(|inferior| inferior.pid);
        Some(pid.map_or(group.clone(), |pid| format!("{group} PID {pid}")))
    }

    fn first_inferior_child(&self, parent: &str) -> Option<String> {
        let relationships = self.inferior_parents.borrow();
        self.inferiors
            .borrow()
            .iter()
            .find(|inferior| {
                relationships.get(&inferior.id).map(String::as_str) == Some(parent)
                    && inferior.state != InferiorState::Exited
            })
            .map(|inferior| inferior.id.clone())
    }

    fn update_local_inferior_relationships(&self, inferiors: &[InferiorInfo]) {
        let Some(debugger_pid) = self.debugger_pid() else {
            return;
        };
        let by_pid = inferiors
            .iter()
            .filter_map(|inferior| inferior.pid.map(|pid| (pid, inferior.id.clone())))
            .collect::<HashMap<_, _>>();
        let mut relationships = self.inferior_parents.borrow_mut();
        relationships.retain(|child, parent| {
            inferiors.iter().any(|inferior| inferior.id == *child)
                && inferiors.iter().any(|inferior| inferior.id == *parent)
        });
        for inferior in inferiors {
            let Some(pid) = inferior.pid else {
                continue;
            };
            let Some(parent) = crate::kernel::read_local_parent_pid(pid, debugger_pid)
                .and_then(|pid| by_pid.get(&pid))
            else {
                continue;
            };
            relationships.insert(inferior.id.clone(), parent.clone());
        }
    }

    fn render_inferior_policy(&self) {
        let controls = &self.inferior_controls;
        controls.selector_updating.set(true);
        let follow_parent = self.fork_follow_mode.get() == Some(ForkFollowMode::Parent);
        let follow_child = self.fork_follow_mode.get() == Some(ForkFollowMode::Child);
        let detach_on_fork = self.detach_on_fork.get().unwrap_or(true);
        if controls.follow_parent.is_active() != follow_parent {
            controls.follow_parent.set_active(follow_parent);
        }
        if controls.follow_child.is_active() != follow_child {
            controls.follow_child.set_active(follow_child);
        }
        if controls.detach_on_fork.is_active() != detach_on_fork {
            controls.detach_on_fork.set_active(detach_on_fork);
        }
        controls.selector_updating.set(false);
        let available = self.debugger_ready.get()
            && !self.command_pending.get()
            && !self.execution_transition_pending.get()
            && !self.session_pending.get()
            && !self.native_until_active.get()
            && !self.resynchronization_pending.get()
            && !self.debug_state_stale.get()
            && self.inferior_action_pending.get().is_none();
        controls
            .follow_parent
            .set_sensitive(available && self.fork_follow_mode.get().is_some());
        controls
            .follow_child
            .set_sensitive(available && self.fork_follow_mode.get().is_some());
        controls
            .detach_on_fork
            .set_sensitive(available && self.detach_on_fork.get().is_some());
    }

    pub(super) fn render_inferior_controls(&self) {
        let controls = &self.inferior_controls;
        let inferiors = self.inferiors.borrow();
        let selected = self.selected_inferior_id.borrow().clone();
        let labels = inferiors
            .iter()
            .map(inferior_selector_label)
            .collect::<Vec<_>>();
        let selector_changed = controls.selector_model.n_items() as usize != labels.len()
            || labels.iter().enumerate().any(|(index, label)| {
                controls.selector_model.string(index as u32).as_deref() != Some(label.as_str())
            });
        controls.selector_updating.set(true);
        if selector_changed {
            let label_refs = labels.iter().map(String::as_str).collect::<Vec<_>>();
            controls
                .selector_model
                .splice(0, controls.selector_model.n_items(), &label_refs);
            controls.selector_ids.replace(
                inferiors
                    .iter()
                    .map(|inferior| inferior.id.clone())
                    .collect(),
            );
        }
        let selected_index = selected
            .as_ref()
            .and_then(|selected| {
                inferiors
                    .iter()
                    .position(|inferior| inferior.id == *selected)
            })
            .and_then(|index| u32::try_from(index).ok())
            .unwrap_or(gtk::INVALID_LIST_POSITION);
        if controls.selector.selected() != selected_index {
            controls.selector.set_selected(selected_index);
        }
        controls.selector_updating.set(false);
        let process_controls_available = self.debugger_ready.get()
            && !self.command_pending.get()
            && !self.execution_transition_pending.get()
            && !self.session_pending.get()
            && !self.native_until_active.get()
            && !self.resynchronization_pending.get()
            && self.inferior_action_pending.get().is_none();
        controls
            .selector
            .set_sensitive(!inferiors.is_empty() && process_controls_available);
        controls.refresh.set_sensitive(process_controls_available);
        let selected_inferior = inferiors
            .iter()
            .find(|inferior| Some(inferior.id.as_str()) == selected.as_deref());
        set_label_text(
            &controls.selected_state,
            selected_inferior.map_or("idle", |inferior| inferior.state.label()),
        );
        for class in [
            "inferior-running",
            "inferior-stopped",
            "inferior-exited",
            "inferior-unknown",
        ] {
            set_css_class(
                &controls.selected_state,
                class,
                selected_inferior
                    .is_some_and(|inferior| class == inferior_state_css(inferior.state)),
            );
        }
        if let Some(owner) = self.stop_owner_summary() {
            let thread = self.stop_owner_thread_id.borrow();
            let owner = thread.as_ref().map_or_else(
                || format!("STOP OWNER  {owner}"),
                |thread| format!("STOP OWNER  {owner}  thread {thread}"),
            );
            set_label_text(&controls.stop_owner, &owner);
            controls.stop_owner.set_visible(true);
        } else {
            controls.stop_owner.set_visible(false);
            set_label_text(&controls.stop_owner, "");
        }
        let parent = selected
            .as_ref()
            .and_then(|selected| self.inferior_parents.borrow().get(selected).cloned());
        let child = selected
            .as_deref()
            .and_then(|selected| self.first_inferior_child(selected));
        let available = process_controls_available
            && !self.debug_state_stale.get()
            && self.inferior_action_pending.get().is_none();
        controls
            .switch_parent
            .set_sensitive(available && parent.is_some());
        controls
            .switch_child
            .set_sensitive(available && child.is_some());
        if inferiors.is_empty() {
            clear_box(&controls.list);
            controls.cards.borrow_mut().clear();
            controls
                .list
                .append(&empty_label("No GDB inferiors are available"));
        } else {
            let rebuild = {
                let cards = controls.cards.borrow();
                cards.len() != inferiors.len()
                    || cards
                        .iter()
                        .zip(inferiors.iter())
                        .any(|((id, _), inferior)| id != &inferior.id)
            };
            if rebuild {
                clear_box(&controls.list);
                let cards = inferiors
                    .iter()
                    .map(|inferior| {
                        let card = self.create_inferior_card(&inferior.id);
                        controls.list.append(&card.root);
                        (inferior.id.clone(), card)
                    })
                    .collect();
                controls.cards.replace(cards);
            }
            for inferior in inferiors.iter() {
                if let Some((_, card)) = controls
                    .cards
                    .borrow()
                    .iter()
                    .find(|(id, _)| id == &inferior.id)
                {
                    self.update_inferior_card(card, inferior);
                }
            }
        }
        drop(inferiors);
        self.render_inferior_policy();
    }

    fn create_inferior_card(&self, inferior_id: &str) -> InferiorCardControls {
        let card = gtk::Box::new(gtk::Orientation::Vertical, 5);
        card.add_css_class("inferior-card");
        let heading = gtk::Box::new(gtk::Orientation::Horizontal, 5);
        let id = gtk::Label::new(Some(inferior_id));
        id.add_css_class("inferior-id");
        id.set_halign(gtk::Align::Start);
        let name = gtk::Label::new(None);
        name.add_css_class("inferior-name");
        name.set_halign(gtk::Align::Start);
        name.set_hexpand(true);
        name.set_ellipsize(pango::EllipsizeMode::Middle);
        let state = gtk::Label::new(None);
        state.add_css_class("inferior-card-state");
        heading.append(&id);
        heading.append(&name);
        heading.append(&state);
        card.append(&heading);
        let facts = gtk::Label::new(None);
        facts.add_css_class("inferior-facts");
        facts.set_halign(gtk::Align::Start);
        facts.set_ellipsize(pango::EllipsizeMode::End);
        card.append(&facts);
        let relationship = gtk::Label::new(None);
        relationship.add_css_class("inferior-relationship");
        relationship.set_halign(gtk::Align::Start);
        relationship.set_visible(false);
        card.append(&relationship);
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        actions.set_homogeneous(true);
        let select = gtk::Button::with_label("Switch");
        select.add_css_class("inferior-inline-action");
        let id = inferior_id.to_owned();
        let handler = Rc::clone(&self.inferior_controls.action_handler);
        select.connect_clicked(move |_| {
            if let Some(handler) = handler.borrow().clone() {
                handler(InferiorAction::Select(id.clone()));
            }
        });
        actions.append(&select);
        let execution_button = gtk::Button::with_label("Unavailable");
        execution_button.add_css_class("inferior-inline-action");
        let execution_action = Rc::new(RefCell::new(None::<InferiorAction>));
        let handler = Rc::clone(&self.inferior_controls.action_handler);
        let action = Rc::clone(&execution_action);
        execution_button.connect_clicked(move |_| {
            dispatch_inferior_card_action(&action, &handler);
        });
        actions.append(&execution_button);
        card.append(&actions);
        InferiorCardControls {
            root: card,
            name,
            state,
            facts,
            relationship,
            select,
            execution: execution_button,
            execution_action,
        }
    }

    fn update_inferior_card(&self, card: &InferiorCardControls, inferior: &InferiorInfo) {
        let selected = self.selected_inferior_id.borrow().as_deref() == Some(&inferior.id);
        let stop_owner = self.stop_owner_inferior_id.borrow().as_deref() == Some(&inferior.id);
        set_css_class(&card.root, "inferior-card-selected", selected);
        set_css_class(&card.root, "inferior-card-stop-owner", stop_owner);
        set_label_text(&card.name, &inferior_display_name(inferior));
        set_label_text(&card.state, inferior.state.label());
        for class in [
            "inferior-running",
            "inferior-stopped",
            "inferior-exited",
            "inferior-unknown",
        ] {
            set_css_class(
                &card.state,
                class,
                class == inferior_state_css(inferior.state),
            );
        }
        set_label_text(&card.facts, &inferior_facts(inferior, stop_owner));
        let relationship = self
            .inferior_parents
            .borrow()
            .get(&inferior.id)
            .map(|parent| format!("child of {parent}"));
        if let Some(relationship) = relationship {
            set_label_text(&card.relationship, &relationship);
            card.relationship.set_visible(true);
        } else {
            card.relationship.set_visible(false);
        }
        let available = self.debugger_ready.get()
            && !self.command_pending.get()
            && !self.execution_transition_pending.get()
            && !self.session_pending.get()
            && !self.native_until_active.get()
            && !self.resynchronization_pending.get()
            && self.inferior_action_pending.get().is_none();
        set_button_label(&card.select, if selected { "Selected" } else { "Switch" });
        card.select.set_sensitive(!selected && available);
        let execution = match inferior.state {
            InferiorState::Running => {
                Some(("Freeze", InferiorAction::Interrupt(inferior.id.clone())))
            }
            InferiorState::Stopped => Some(("Resume", InferiorAction::Resume(inferior.id.clone()))),
            InferiorState::Exited | InferiorState::NotStarted | InferiorState::Unknown => None,
        };
        set_button_label(
            &card.execution,
            execution
                .as_ref()
                .map_or("Unavailable", |(label, _)| *label),
        );
        card.execution
            .set_sensitive(execution.is_some() && available);
        card.execution_action
            .replace(execution.map(|(_, action)| action));
    }
}

fn inferior_context_running(inferior: &InferiorInfo, current_thread: Option<&str>) -> bool {
    inferior_context_thread(inferior, current_thread)
        .map_or(inferior.state == InferiorState::Running, |thread| {
            thread.state == "running"
        })
}

fn inferior_context_thread<'a>(
    inferior: &'a InferiorInfo,
    current_thread: Option<&str>,
) -> Option<&'a ThreadInfo> {
    current_thread
        .and_then(|current| inferior.threads.iter().find(|thread| thread.id == current))
        .or_else(|| inferior.threads.iter().find(|thread| thread.current))
}

fn running_event_affects_all(group: Option<&str>, exact_thread: Option<&str>) -> bool {
    group.is_none() && exact_thread.is_none()
}

fn inferior_selector_label(inferior: &InferiorInfo) -> String {
    inferior.pid.map_or_else(
        || inferior.id.clone(),
        |pid| format!("{}  PID {pid}", inferior.id),
    )
}

fn set_label_text(label: &gtk::Label, text: &str) {
    if label.text().as_str() != text {
        label.set_text(text);
    }
}

fn set_button_label(button: &gtk::Button, text: &str) {
    if button.label().as_deref() != Some(text) {
        button.set_label(text);
    }
}

fn set_css_class(widget: &impl IsA<gtk::Widget>, class: &str, enabled: bool) {
    if enabled && !widget.has_css_class(class) {
        widget.add_css_class(class);
    } else if !enabled && widget.has_css_class(class) {
        widget.remove_css_class(class);
    }
}

fn dispatch_inferior_card_action(
    action: &RefCell<Option<InferiorAction>>,
    handler: &RefCell<Option<InferiorActionHandler>>,
) {
    let action = action.borrow().clone();
    let handler = handler.borrow().clone();
    if let (Some(action), Some(handler)) = (action, handler) {
        handler(action);
    }
}

fn inferior_display_name(inferior: &InferiorInfo) -> String {
    inferior
        .executable
        .as_deref()
        .and_then(|path| Path::new(path).file_name())
        .and_then(|name| name.to_str())
        .unwrap_or("no executable")
        .to_owned()
}

fn inferior_facts(inferior: &InferiorInfo, stop_owner: bool) -> String {
    let pid = inferior.pid.map_or_else(
        || String::from("PID unavailable"),
        |pid| format!("PID {pid}"),
    );
    let threads = match inferior.threads.len() {
        1 => String::from("1 thread"),
        count => format!("{count} threads"),
    };
    let owner = if stop_owner { "  STOP OWNER" } else { "" };
    format!("{pid}  {threads}{owner}")
}

fn inferior_state_css(state: InferiorState) -> &'static str {
    match state {
        InferiorState::Running => "inferior-running",
        InferiorState::Stopped => "inferior-stopped",
        InferiorState::Exited => "inferior-exited",
        InferiorState::NotStarted | InferiorState::Unknown => "inferior-unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::{
        dispatch_inferior_card_action, inferior_context_running, running_event_affects_all,
    };
    use crate::debugger::{InferiorInfo, InferiorState, ThreadInfo};
    use crate::ui::{InferiorAction, InferiorActionHandler};
    use std::{cell::RefCell, rc::Rc};

    #[test]
    fn card_action_can_be_replaced_during_synchronous_dispatch() {
        let action = Rc::new(RefCell::new(Some(InferiorAction::Resume(String::from(
            "i1",
        )))));
        let action_for_handler = Rc::clone(&action);
        let handler = RefCell::new(Some(Rc::new(move |_| {
            action_for_handler.replace(None);
        }) as InferiorActionHandler));

        dispatch_inferior_card_action(&action, &handler);

        assert!(action.borrow().is_none());
    }

    #[test]
    fn a_stopped_current_thread_remains_inspectable_in_non_stop_mode() {
        let thread = |id: &str, state: &str, current: bool| ThreadInfo {
            id: id.to_owned(),
            group_id: Some(String::from("i1")),
            target_id: id.to_owned(),
            name: None,
            state: state.to_owned(),
            core: None,
            frame: None,
            pc_symbol: None,
            current,
        };
        let inferior = InferiorInfo {
            id: String::from("i1"),
            pid: Some(42),
            executable: None,
            exit_code: None,
            state: InferiorState::Running,
            threads: vec![thread("1", "stopped", true), thread("2", "running", false)],
        };
        assert!(!inferior_context_running(&inferior, Some("1")));
        assert!(inferior_context_running(&inferior, Some("2")));
    }

    #[test]
    fn an_unknown_exact_thread_does_not_mark_every_inferior_running() {
        assert!(!running_event_affects_all(None, Some("99")));
        assert!(!running_event_affects_all(Some("i2"), None));
        assert!(running_event_affects_all(None, None));
    }
}
