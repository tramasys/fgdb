use super::*;

impl DebuggerModel {
    pub(crate) fn inferior_action_is_current(&self, action: &InferiorAction) -> bool {
        if !self.execution.debugger_ready.get()
            || self.execution.command_pending.get()
            || self.execution.debugger_state.get().transition_pending()
            || self.execution.session_pending.get()
            || self.execution.native_until_active.get()
            || self.execution.debugger_state.get().resynchronizing()
            || self.execution.inferior_action_pending.get().is_some()
        {
            return false;
        }

        match action {
            InferiorAction::Select(id) => {
                self.processes.selected_inferior_id.borrow().as_deref() != Some(id)
                    && self
                        .processes
                        .inferiors
                        .borrow()
                        .iter()
                        .any(|inferior| inferior.id == *id)
            }
            InferiorAction::Resume(id) => self
                .processes
                .inferiors
                .borrow()
                .iter()
                .any(|inferior| inferior.id == *id && inferior.state == InferiorState::Stopped),
            InferiorAction::Interrupt(id) => self
                .processes
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
        let generation = self
            .processes
            .inferior_refresh_generation
            .get()
            .wrapping_add(1);
        self.processes.inferior_refresh_generation.set(generation);

        generation
    }

    pub(crate) fn begin_inferior_refresh(&self) -> Option<u64> {
        self.processes
            .inferior_refresh_gate
            .begin()
            .then(|| self.start_inferior_refresh())
    }

    pub(crate) fn finish_inferior_refresh(&self) -> bool {
        self.processes.inferior_refresh_gate.finish()
    }

    pub(crate) fn begin_fork_policy_refresh(&self) -> Option<u64> {
        self.processes
            .fork_policy_refresh_gate
            .begin()
            .then(|| self.start_fork_policy_refresh())
    }

    pub(crate) fn finish_fork_policy_refresh(&self) -> bool {
        self.processes.fork_policy_refresh_gate.finish()
    }

    pub(crate) fn is_inferior_refresh_current(&self, generation: u64) -> bool {
        self.processes.inferior_refresh_generation.get() == generation
    }

    pub(crate) fn start_fork_policy_refresh(&self) -> u64 {
        let generation = self.processes.fork_policy_generation.get().wrapping_add(1);
        self.processes.fork_policy_generation.set(generation);

        generation
    }

    pub(crate) fn is_fork_policy_refresh_current(&self, generation: u64) -> bool {
        self.processes.fork_policy_generation.get() == generation
    }

    pub(crate) fn selected_inferior_id(&self) -> Option<String> {
        self.processes.selected_inferior_id.borrow().clone()
    }

    pub(crate) fn selected_inferior_context_stopped(&self) -> bool {
        let selected = self.processes.selected_inferior_id.borrow();
        let current_thread = self.current_thread_id();

        self.processes
            .inferiors
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
        self.processes
            .inferiors
            .borrow()
            .iter()
            .find(|inferior| inferior.threads.iter().any(|thread| thread.id == thread_id))
            .map(|inferior| inferior.id.clone())
            .or_else(|| {
                self.processes
                    .thread_inferior_ids
                    .borrow()
                    .get(thread_id)
                    .cloned()
            })
    }

    pub(crate) fn record_thread_group(&self, thread_id: Option<&str>, group_id: Option<&str>) {
        if let (Some(thread_id), Some(group_id)) = (thread_id, group_id) {
            self.processes
                .thread_inferior_ids
                .borrow_mut()
                .insert(thread_id.to_owned(), group_id.to_owned());
        }
    }

    pub(crate) fn forget_thread_group(&self, thread_id: &str) {
        self.processes
            .thread_inferior_ids
            .borrow_mut()
            .remove(thread_id);
    }

    pub(crate) fn threads_for_selected_inferior(
        &self,
        mut threads: Vec<ThreadInfo>,
    ) -> Vec<ThreadInfo> {
        let inferiors = self.processes.inferiors.borrow();

        if inferiors.is_empty() {
            return threads;
        }

        let selected = self.processes.selected_inferior_id.borrow();

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

    pub(crate) fn record_pending_fork(&self, thread_id: Option<&str>, child_pid: Option<u32>) {
        let Some(child_pid) = child_pid else {
            return;
        };

        let parent = thread_id
            .and_then(|thread| self.inferior_for_thread(thread))
            .or_else(|| self.selected_inferior_id());

        if let Some(parent) = parent {
            self.processes
                .pending_fork_parents
                .borrow_mut()
                .insert(child_pid, parent);
        }
    }

    pub(crate) fn record_inferior_started(&self, id: &str, pid: Option<u32>) {
        if let Some(parent) = pid.and_then(|pid| {
            self.processes
                .pending_fork_parents
                .borrow_mut()
                .remove(&pid)
        }) {
            self.processes
                .inferior_parents
                .borrow_mut()
                .insert(id.to_owned(), parent);
        }
    }

    pub(crate) fn inferior_exit_owns_selected_context(&self, id: &str) -> bool {
        let selected = self.processes.selected_inferior_id.borrow().clone();

        if selected.as_deref() == Some(id) {
            return true;
        }

        if self.processes.stop_owner_inferior_id.borrow().as_deref() == Some(id) {
            return true;
        }

        let inferiors = self.processes.inferiors.borrow();

        if selected.as_ref().is_some_and(|selected| {
            inferiors
                .iter()
                .any(|inferior| inferior.id == *selected && inferior.state.is_live())
        }) {
            return false;
        }

        let exiting_is_known = inferiors
            .iter()
            .any(|inferior| inferior.id == id && inferior.state != InferiorState::Exited);

        let another_live_inferior = inferiors
            .iter()
            .any(|inferior| inferior.id != id && inferior.state.is_live());

        !another_live_inferior && (exiting_is_known || self.inferior_has_started())
    }

    pub(crate) fn set_pending_execution_inferior(&self, id: Option<String>) {
        self.execution.pending_execution_inferior.replace(id);
    }

    pub(crate) fn set_active_thread_execution(&self, id: Option<String>) {
        self.execution.active_thread_execution.replace(id);
    }

    pub(crate) fn active_thread_execution(&self) -> Option<String> {
        self.execution.active_thread_execution.borrow().clone()
    }

    pub(crate) fn set_thread_execution_exit_candidate(&self, id: Option<String>) {
        self.execution.thread_execution_exit_candidate.replace(id);
    }

    pub(crate) fn thread_execution_exit_candidate(&self) -> Option<String> {
        self.execution
            .thread_execution_exit_candidate
            .borrow()
            .clone()
    }

    pub(crate) fn first_inferior_child(&self, parent: &str) -> Option<String> {
        let relationships = self.processes.inferior_parents.borrow();

        self.processes
            .inferiors
            .borrow()
            .iter()
            .find(|inferior| {
                relationships.get(&inferior.id).map(String::as_str) == Some(parent)
                    && inferior.state != InferiorState::Exited
            })
            .map(|inferior| inferior.id.clone())
    }

    pub(crate) fn inferior_execution_action_pending_for(&self, id: &str, generation: u64) -> bool {
        self.execution.inferior_execution_generation.get() == generation
            && self.execution.inferior_action_pending.get()
                == Some(InferiorActionPending::Execution)
            && self
                .execution
                .pending_execution_inferior
                .borrow()
                .as_deref()
                == Some(id)
    }

    pub(crate) fn pending_execution_inferior(&self) -> Option<String> {
        self.execution.pending_execution_inferior.borrow().clone()
    }

    pub(crate) fn thread_action_dispatch_available(&self) -> bool {
        self.execution.debugger_ready.get()
            && !self.execution.command_pending.get()
            && !self.execution.debugger_state.get().transition_pending()
            && !self.execution.session_pending.get()
            && !self.execution.native_until_active.get()
            && !self.execution.debugger_state.get().resynchronizing()
            && self.execution.thread_action_pending.get().is_none()
    }

    pub(crate) fn thread_selection_can_dispatch(&self, id: &str) -> bool {
        self.execution.debugger_ready.get()
            && !self.execution.command_pending.get()
            && !self.execution.debugger_state.get().transition_pending()
            && !self.execution.session_pending.get()
            && !self.execution.native_until_active.get()
            && !self.execution.debugger_state.get().resynchronizing()
            && self.execution.inferior_action_pending.get().is_none()
            && self.execution.thread_action_pending.get().is_none()
            && self.current_thread_id().as_deref() != Some(id)
            && self
                .processes
                .threads
                .borrow()
                .iter()
                .any(|thread| thread.id == id)
    }

    pub(crate) fn frame_selection_can_dispatch(&self, level: u32) -> bool {
        self.stopped_inspection_available()
            && self.execution.inferior_action_pending.get().is_none()
            && self.execution.thread_action_pending.get().is_none()
            && self.processes.selected_frame_level.get() != level
            && self
                .stopped
                .latest_frames
                .borrow()
                .iter()
                .any(|frame| frame.level == level)
    }

    pub(crate) fn thread_action_can_dispatch(&self, action: &ThreadAction) -> bool {
        self.thread_action_dispatch_available() && self.thread_action_is_current(action)
    }

    pub(crate) fn thread_action_is_current(&self, action: &ThreadAction) -> bool {
        let threads = self.processes.threads.borrow();

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

    pub(crate) fn scheduler_locking_mode(&self) -> Option<SchedulerLockingMode> {
        self.processes.scheduler_locking.get()
    }

    pub(crate) fn start_thread_policy_refresh(&self) -> u64 {
        let generation = self
            .processes
            .thread_policy_generation
            .get()
            .wrapping_add(1);
        self.processes.thread_policy_generation.set(generation);

        generation
    }

    pub(crate) fn is_thread_policy_refresh_current(&self, generation: u64) -> bool {
        self.processes.thread_policy_generation.get() == generation
    }

    pub(crate) fn non_stop_mode(&self) -> Option<bool> {
        self.processes.non_stop_mode.get()
    }

    pub(crate) fn thread_is_stopped(&self, id: &str) -> bool {
        self.processes
            .threads
            .borrow()
            .iter()
            .any(|thread| thread.id == id && thread.state == "stopped")
    }

    pub(crate) fn current_thread_id(&self) -> Option<String> {
        self.processes.selected_thread_id.borrow().clone()
    }

    pub(crate) fn set_current_thread_id(&self, thread_id: Option<&str>) {
        let mut selected = self.processes.selected_thread_id.borrow_mut();

        if selected.as_deref() != thread_id {
            self.invalidate_stop_context();
            *selected = thread_id.map(str::to_owned);
        }
    }

    pub(crate) fn start_thread_refresh(&self) -> u64 {
        let generation = self
            .processes
            .thread_refresh_generation
            .get()
            .wrapping_add(1);
        self.processes.thread_refresh_generation.set(generation);

        generation
    }

    pub(crate) fn is_thread_refresh_current(&self, generation: u64) -> bool {
        self.processes.thread_refresh_generation.get() == generation
    }

    pub(crate) fn show_inferiors(&self, inferiors: Vec<InferiorInfo>) -> bool {
        let unresolved_stop_owner = self.processes.stop_owner_inferior_id.borrow().is_none()
            && self.processes.stop_owner_thread_id.borrow().is_some();

        if unresolved_stop_owner
            && let Some(owner) = self
                .processes
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
            self.processes.stop_owner_inferior_id.replace(Some(owner));
        }

        let existing = self.processes.selected_inferior_id.borrow().clone();

        let stop_owner = unresolved_stop_owner
            .then(|| self.processes.stop_owner_inferior_id.borrow().clone())
            .flatten();

        let current_thread = self.current_thread_id();

        let selected = preferred_inferior_id(
            &inferiors,
            stop_owner.as_deref(),
            existing.as_deref(),
            current_thread.as_deref(),
        );

        let selection_changed =
            self.processes.selected_inferior_id.borrow().as_ref() != selected.as_ref();
        self.processes.inferiors.replace(inferiors);

        if selection_changed {
            self.invalidate_stop_context();
        }

        self.processes.selected_inferior_id.replace(selected);

        self.apply_selected_inferior_state();

        selection_changed
    }

    pub(crate) fn set_selected_inferior(&self, id: &str) -> bool {
        if !self
            .processes
            .inferiors
            .borrow()
            .iter()
            .any(|inferior| inferior.id == id)
        {
            return false;
        }

        if self.processes.selected_inferior_id.borrow().as_deref() == Some(id) {
            return true;
        }

        self.invalidate_stop_context();
        self.processes
            .selected_inferior_id
            .replace(Some(id.to_owned()));
        self.apply_selected_inferior_state();

        true
    }

    pub(crate) fn apply_selected_inferior_state(&self) {
        let current_thread = self.current_thread_id();

        let state = {
            let selected = self.processes.selected_inferior_id.borrow();

            self.processes
                .inferiors
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
            self.publish_threads(&[]);
            return;
        };

        self.set_inferior_pid(pid);
        self.set_inferior_started(started);
        self.set_controls_running(running);
        self.publish_threads(&threads);
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

        let selection_changed = self.current_thread_id().as_deref() != Some(thread_id);
        self.set_current_thread_id(Some(thread_id));
        let mut found = false;
        let mut thread_rows_changed = false;

        for inferior in self.processes.inferiors.borrow_mut().iter_mut() {
            for thread in &mut inferior.threads {
                let current = thread.id == thread_id;
                found |= current;
                thread_rows_changed |= thread.current != current;
                thread.current = current;
            }
        }

        if found && (selection_changed || thread_rows_changed) {
            self.apply_selected_inferior_state();
        }
    }

    pub(crate) fn reconcile_stop_owner_from_threads(&self, threads: &[ThreadInfo]) -> bool {
        if self.processes.stop_owner_inferior_id.borrow().is_some() {
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

        let Some(group) =
            group.filter(|group| {
                self.processes.inferiors.borrow().iter().any(|inferior| {
                    inferior.id == *group && inferior.state == InferiorState::Stopped
                })
            })
        else {
            return false;
        };

        self.processes
            .stop_owner_thread_id
            .replace(Some(thread.id.clone()));
        self.processes
            .stop_owner_inferior_id
            .replace(Some(group.clone()));

        if self.processes.selected_inferior_id.borrow().as_deref() != Some(group.as_str()) {
            self.set_selected_inferior(&group);

            true
        } else {
            false
        }
    }

    pub(crate) fn mark_inferior_running(&self, thread_id: Option<&str>) -> (bool, bool) {
        let pending_group = self.execution.pending_execution_inferior.borrow().clone();
        let exact_thread = thread_id.filter(|thread| *thread != "all");
        let current_thread = self.current_thread_id();

        let group = exact_thread
            .and_then(|thread| self.inferior_for_thread(thread))
            .or_else(|| {
                // In non-stop mode the first running notification can arrive
                // before the initial thread-group refresh. A sole live
                // inferior is still an unambiguous owner for that thread.
                exact_thread.and_then(|_| {
                    let inferiors = self.processes.inferiors.borrow();

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
            self.execution
                .pending_execution_inferior
                .borrow_mut()
                .take();
        }

        // An exact thread ID that is not in our latest snapshot means the
        // snapshot is stale. It does not mean every inferior started running.
        let all = running_event_affects_all(group.as_deref(), exact_thread);
        let mut inferiors = self.processes.inferiors.borrow_mut();

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
            || group.as_deref() == self.processes.selected_inferior_id.borrow().as_deref()
            || self.processes.selected_inferior_id.borrow().is_none();

        let selected_affected = exact_thread.map_or(selected_group_affected, |thread| {
            current_thread
                .as_deref()
                .map_or(selected_group_affected, |current| current == thread)
        });

        let stop_owner_affected = all
            || exact_thread.map_or_else(
                || self.processes.stop_owner_inferior_id.borrow().as_deref() == group.as_deref(),
                |thread| self.processes.stop_owner_thread_id.borrow().as_deref() == Some(thread),
            );

        if stop_owner_affected {
            self.processes.stop_owner_inferior_id.borrow_mut().take();
            self.processes.stop_owner_thread_id.borrow_mut().take();
        }

        let selected_threads = {
            let selected = self.processes.selected_inferior_id.borrow();

            self.processes
                .inferiors
                .borrow()
                .iter()
                .find(|inferior| Some(inferior.id.as_str()) == selected.as_deref())
                .map(|inferior| inferior.threads.clone())
        };

        if let Some(threads) = selected_threads {
            self.stage_threads_for_execution(&threads);
        }

        if selected_affected {
            // Observed execution changes immediately, independently of delayed painting.
            self.set_controls_running(true);
        }

        (selected_affected, pending_affected)
    }

    pub(crate) fn mark_inferior_stopped(&self, thread_id: Option<&str>, all_stopped: bool) -> bool {
        self.invalidate_stop_context();
        let pending_group = self.execution.pending_execution_inferior.borrow().clone();

        let group = thread_id
            .and_then(|thread| self.inferior_for_thread(thread))
            .or_else(|| thread_id.is_none().then(|| pending_group.clone()).flatten());

        let pending_affected =
            pending_group.is_some() && (all_stopped || (group.is_some() && group == pending_group));

        if pending_affected {
            self.execution
                .pending_execution_inferior
                .borrow_mut()
                .take();
        }

        if let Some(thread_id) = thread_id {
            self.processes
                .stop_owner_thread_id
                .replace(Some(thread_id.to_owned()));
        } else {
            self.processes.stop_owner_thread_id.borrow_mut().take();
        }

        if let Some(group) = group.as_ref() {
            self.processes
                .stop_owner_inferior_id
                .replace(Some(group.clone()));
            self.processes
                .selected_inferior_id
                .replace(Some(group.clone()));
        } else {
            self.processes.stop_owner_inferior_id.borrow_mut().take();
        }

        let mut inferiors = self.processes.inferiors.borrow_mut();

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

        if !self.processes.inferiors.borrow().is_empty() {
            self.apply_selected_inferior_state();
        }

        pending_affected
    }

    pub(crate) fn record_inferior_exited(&self, id: &str) {
        self.processes
            .thread_inferior_ids
            .borrow_mut()
            .retain(|_, group| group != id);

        if let Some(inferior) = self
            .processes
            .inferiors
            .borrow_mut()
            .iter_mut()
            .find(|inferior| inferior.id == id)
        {
            inferior.state = InferiorState::Exited;
        }

        if self.processes.stop_owner_inferior_id.borrow().as_deref() == Some(id) {
            self.processes.stop_owner_inferior_id.borrow_mut().take();
            self.processes.stop_owner_thread_id.borrow_mut().take();
        }

        let selected_exited = self.processes.selected_inferior_id.borrow().as_deref() == Some(id);

        if selected_exited {
            self.processes.selected_inferior_id.borrow_mut().take();
            self.apply_selected_inferior_state();
        }
    }

    pub(crate) fn clear_inferiors(&self) {
        self.invalidate_stop_context();
        self.processes.threads.replace(Rc::from([]));
        self.processes.selected_thread_id.borrow_mut().take();
        self.start_inferior_refresh();
        self.processes.inferiors.borrow_mut().clear();
        self.processes.thread_inferior_ids.borrow_mut().clear();
        self.processes.selected_inferior_id.borrow_mut().take();
        self.processes.stop_owner_inferior_id.borrow_mut().take();
        self.processes.stop_owner_thread_id.borrow_mut().take();
        self.processes.inferior_parents.borrow_mut().clear();
        self.processes.pending_fork_parents.borrow_mut().clear();
        self.execution
            .pending_execution_inferior
            .borrow_mut()
            .take();
        self.execution.inferior_action_pending.set(None);
        self.set_inferior_pid(None);
    }

    pub(crate) fn set_fork_follow_mode(&self, mode: Option<ForkFollowMode>) {
        self.processes.fork_follow_mode.set(mode);
    }

    pub(crate) fn set_detach_on_fork(&self, detach: Option<bool>) {
        self.processes.detach_on_fork.set(detach);
    }

    pub(crate) fn set_fork_policy(&self, mode: Option<ForkFollowMode>, detach: Option<bool>) {
        self.processes.fork_follow_mode.set(mode);
        self.processes.detach_on_fork.set(detach);
    }

    pub(crate) fn publish_threads(&self, threads: &[ThreadInfo]) {
        let explicit_current = threads.iter().find(|thread| thread.current);

        let retained_current = if explicit_current.is_none() {
            self.current_thread_id()
                .filter(|selected| threads.iter().any(|thread| thread.id == selected.as_str()))
        } else {
            None
        };

        let normalized_threads = retained_current.as_ref().map(|selected| {
            let mut normalized = threads.to_vec();

            if let Some(thread) = normalized
                .iter_mut()
                .find(|thread| thread.id == selected.as_str())
            {
                thread.current = true;
            }

            normalized
        });

        let threads = normalized_threads.as_deref().unwrap_or(threads);

        self.set_current_thread_id(
            explicit_current
                .map(|thread| thread.id.as_str())
                .or(retained_current.as_deref()),
        );

        let mut latest = self.processes.threads.borrow_mut();

        if latest.as_ref() != threads {
            *latest = Rc::from(threads);
        }
    }

    pub(crate) fn threads(&self) -> Rc<[ThreadInfo]> {
        Rc::clone(&self.processes.threads.borrow())
    }

    pub(crate) fn thread_snapshot(&self) -> Vec<ThreadInfo> {
        self.processes.threads.borrow().to_vec()
    }

    pub(crate) fn stage_threads_for_execution(&self, threads: &[ThreadInfo]) {
        let mut latest = self.processes.threads.borrow_mut();

        if latest.as_ref() != threads {
            *latest = Rc::from(threads);
        }
    }

    pub(crate) fn inferiors(&self) -> std::cell::Ref<'_, Vec<InferiorInfo>> {
        self.processes.inferiors.borrow()
    }

    pub(crate) fn select_thread(&self, id: &str) {
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

        for inferior in self.processes.inferiors.borrow_mut().iter_mut() {
            for thread in &mut inferior.threads {
                thread.current = thread.id == id;
            }
        }

        self.set_current_thread_id(Some(id));
        self.set_controls_running(running);
        self.set_debug_state_stale(running);
        self.publish_threads(&threads);
    }

    pub(crate) fn selected_frame_level(&self) -> u32 {
        self.processes.selected_frame_level.get()
    }

    pub(crate) fn select_frame(&self, level: u32) {
        if self.processes.selected_frame_level.replace(level) != level {
            self.invalidate_stop_context();
        }
    }

    pub(crate) fn set_thread_control_policy(
        &self,
        scheduler: Option<SchedulerLockingMode>,
        non_stop: Option<bool>,
    ) {
        self.processes.scheduler_locking.set(scheduler);
        self.processes.non_stop_mode.set(non_stop);
    }

    pub(crate) fn set_thread_stop_reason(&self, reason: Option<String>) {
        self.processes.thread_stop_reason.replace(reason);
    }

    pub(crate) fn thread_stop_reason(&self) -> Option<String> {
        self.processes.thread_stop_reason.borrow().clone()
    }

    pub(crate) fn stop_owner_inferior_id(&self) -> Option<String> {
        self.processes.stop_owner_inferior_id.borrow().clone()
    }

    pub(crate) fn stop_owner_thread_id(&self) -> Option<String> {
        self.processes.stop_owner_thread_id.borrow().clone()
    }

    pub(crate) fn inferior_parent(&self, id: &str) -> Option<String> {
        self.processes.inferior_parents.borrow().get(id).cloned()
    }

    pub(crate) fn fork_follow_mode(&self) -> Option<ForkFollowMode> {
        self.processes.fork_follow_mode.get()
    }

    pub(crate) fn detach_on_fork(&self) -> Option<bool> {
        self.processes.detach_on_fork.get()
    }

    pub(crate) fn inferior_refresh_generation(&self) -> u64 {
        self.processes.inferior_refresh_generation.get()
    }

    pub(crate) fn prune_inferior_relationships(&self) {
        let inferiors = self.processes.inferiors.borrow();
        self.processes
            .inferior_parents
            .borrow_mut()
            .retain(|child, parent| {
                inferiors.iter().any(|inferior| inferior.id == *child)
                    && inferiors.iter().any(|inferior| inferior.id == *parent)
            });
    }

    pub(crate) fn merge_inferior_relationships(
        &self,
        generation: u64,
        discovered: HashMap<String, String>,
    ) -> bool {
        if !self.is_inferior_refresh_current(generation) {
            return false;
        }

        let live = self.processes.inferiors.borrow();
        let known = live
            .iter()
            .map(|inferior| inferior.id.as_str())
            .collect::<std::collections::HashSet<_>>();
        let mut relationships = self.processes.inferior_parents.borrow_mut();
        relationships.retain(|child, parent| {
            known.contains(child.as_str()) && known.contains(parent.as_str())
        });
        relationships.extend(discovered.into_iter().filter(|(child, parent)| {
            known.contains(child.as_str()) && known.contains(parent.as_str())
        }));

        true
    }
}

pub(crate) fn inferior_context_running(
    inferior: &InferiorInfo,
    current_thread: Option<&str>,
) -> bool {
    inferior_context_thread(inferior, current_thread)
        .map_or(inferior.state == InferiorState::Running, |thread| {
            thread.state == "running"
        })
}

pub(crate) fn inferior_context_thread<'a>(
    inferior: &'a InferiorInfo,
    current_thread: Option<&str>,
) -> Option<&'a ThreadInfo> {
    current_thread
        .and_then(|current| inferior.threads.iter().find(|thread| thread.id == current))
        .or_else(|| inferior.threads.iter().find(|thread| thread.current))
}

pub(crate) fn running_event_affects_all(group: Option<&str>, exact_thread: Option<&str>) -> bool {
    group.is_none() && exact_thread.is_none()
}

pub(crate) fn preferred_inferior_id(
    inferiors: &[InferiorInfo],
    stop_owner: Option<&str>,
    existing: Option<&str>,
    current_thread: Option<&str>,
) -> Option<String> {
    let selectable = |inferior: &&InferiorInfo| inferior.state != InferiorState::Exited;

    stop_owner
        .and_then(|id| {
            inferiors
                .iter()
                .filter(selectable)
                .find(|inferior| inferior.id == id)
        })
        .or_else(|| {
            existing.and_then(|id| {
                inferiors
                    .iter()
                    .filter(selectable)
                    .find(|inferior| inferior.id == id)
            })
        })
        .or_else(|| {
            current_thread.and_then(|thread| {
                inferiors.iter().filter(selectable).find(|inferior| {
                    inferior
                        .threads
                        .iter()
                        .any(|candidate| candidate.id == thread)
                })
            })
        })
        .or_else(|| {
            inferiors
                .iter()
                .find(|inferior| inferior.state == InferiorState::Stopped)
        })
        .or_else(|| inferiors.iter().find(|inferior| inferior.state.is_live()))
        .or_else(|| {
            inferiors
                .iter()
                .find(|inferior| inferior.state == InferiorState::NotStarted)
        })
        .map(|inferior| inferior.id.clone())
}

impl DebuggerModel {
    pub(crate) fn is_selected_inferior(&self, id: &str) -> bool {
        self.processes.selected_inferior_id.borrow().as_deref() == Some(id)
    }

    pub(crate) fn is_stop_owner_inferior(&self, id: &str) -> bool {
        self.processes.stop_owner_inferior_id.borrow().as_deref() == Some(id)
    }
}
