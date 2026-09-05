use super::*;

impl DebuggerModel {
    pub(crate) fn gdb_recovery_required(&self) -> bool {
        self.execution.gdb_recovery_available.get()
    }

    pub(crate) fn gdb_capabilities(&self) -> GdbCapabilities {
        self.execution.gdb_capabilities.borrow().clone()
    }

    pub(crate) fn set_debugger_pid(&self, pid: Option<u32>) {
        self.execution.debugger_pid.set(pid);
    }

    pub(crate) fn debugger_pid(&self) -> Option<u32> {
        self.execution.debugger_pid.get()
    }

    pub(crate) fn set_inferior_pid(&self, pid: Option<u32>) {
        self.execution.inferior_pid.set(pid);
    }

    pub(crate) fn inferior_pid(&self) -> Option<u32> {
        self.execution.inferior_pid.get()
    }

    pub(crate) fn inferior_is_running(&self) -> bool {
        self.execution.debugger_state.get().inferior_running()
    }

    pub(crate) fn inferior_has_started(&self) -> bool {
        self.execution.debugger_state.get().inferior_started()
    }

    pub(crate) fn target_connection(&self) -> TargetConnection {
        self.execution.debugger_state.get().target_connection()
    }

    pub(crate) fn configured_session_can_start(&self) -> bool {
        configured_target_can_start(
            self.execution.current_session.borrow().as_ref(),
            self.execution.debugger_state.get().target_connection(),
        )
    }

    pub(crate) fn movement_commands_available(&self) -> bool {
        self.execution.debugger_ready.get()
            && self.inferior_has_started()
            && !self.inferior_is_running()
            && !self.execution.command_pending.get()
            && !self.execution.debugger_state.get().transition_pending()
            && !self.execution.session_pending.get()
            && !self.execution.debugger_state.get().resynchronizing()
            && self.execution.inferior_action_pending.get().is_none()
            && matches!(
                self.execution.thread_action_pending.get(),
                None | Some(ThreadActionPending::Analysis)
            )
            && !self.debug_state_is_stale()
            && self
                .execution
                .current_session
                .borrow()
                .as_ref()
                .is_none_or(DebugSession::supports_execution)
    }

    pub(crate) fn stopped_inspection_available(&self) -> bool {
        self.execution.debugger_ready.get()
            && self.inferior_has_started()
            && !self.inferior_is_running()
            && !self.execution.command_pending.get()
            && !self.execution.debugger_state.get().transition_pending()
            && !self.execution.session_pending.get()
            && !self.execution.native_until_active.get()
            && !self.execution.debugger_state.get().resynchronizing()
            && !self.debug_state_is_stale()
    }

    pub(crate) fn debugger_synchronization_available(&self) -> bool {
        self.execution.debugger_ready.get()
            && !self.inferior_is_running()
            && !self.execution.command_pending.get()
            && !self.execution.debugger_state.get().transition_pending()
            && !self.execution.session_pending.get()
            && !self.execution.native_until_active.get()
            && !self.execution.debugger_state.get().resynchronizing()
            && self.execution.inferior_action_pending.get().is_none()
            && self.execution.thread_action_pending.get().is_none()
    }

    pub(crate) fn stop_point_commands_available(&self) -> bool {
        let debugger_state = self.execution.debugger_state.get();

        self.execution.debugger_ready.get()
            && !debugger_state.inferior_running()
            && !self.execution.command_pending.get()
            && !debugger_state.transition_pending()
            && !self.execution.session_pending.get()
            && !self.execution.native_until_active.get()
            && !debugger_state.resynchronizing()
            && !debugger_state.stopped_context_is_stale()
    }

    pub(crate) fn execution_transition_is_pending(&self, generation: u64) -> bool {
        self.execution.execution_transition_generation.get() == generation
            && self.execution.debugger_state.get().transition_pending()
    }

    pub(crate) fn execution_transition_matches_thread(
        &self,
        thread_id: Option<&str>,
        all_stopped: bool,
    ) -> bool {
        if !self.execution.debugger_state.get().transition_pending() {
            return false;
        }

        if all_stopped {
            return true;
        }

        execution_event_matches_thread(
            self.execution.active_thread_execution.borrow().as_deref(),
            thread_id,
            false,
        )
    }

    pub(crate) fn debug_state_is_stale(&self) -> bool {
        self.execution.debugger_state.get().state_stale()
    }

    pub(crate) fn native_until_active(&self) -> bool {
        self.execution.native_until_active.get()
    }

    pub(crate) fn current_session(&self) -> Option<DebugSession> {
        self.execution.current_session.borrow().clone()
    }

    pub(crate) fn thread_execution_transition_matches(
        &self,
        thread_id: Option<&str>,
        all_stopped: bool,
    ) -> bool {
        if self.execution.thread_action_pending.get() != Some(ThreadActionPending::Execution) {
            return false;
        }

        if all_stopped {
            return true;
        }

        let active = self.execution.active_thread_execution.borrow();

        execution_event_matches_thread(active.as_deref(), thread_id, false)
    }

    pub(crate) fn is_thread_analysis_current(&self, generation: u64) -> bool {
        self.execution.thread_analysis_generation.get() == generation
    }

    pub(crate) fn execution(&self) -> ExecutionSnapshot {
        ExecutionSnapshot {
            ready: self.execution.debugger_ready.get(),
            state: self.execution.debugger_state.get(),
            command_pending: self.execution.command_pending.get(),
            session_pending: self.execution.session_pending.get(),
            native_until_active: self.execution.native_until_active.get(),
            inferior_action_pending: self.execution.inferior_action_pending.get(),
            thread_action_pending: self.execution.thread_action_pending.get(),
        }
    }

    pub(crate) fn set_controls_ready(&self, ready: bool) -> bool {
        let changed = self.execution.debugger_ready.replace(ready) != ready;

        if !ready {
            self.invalidate_stop_context();
            self.execution
                .debugger_state
                .set(self.execution.debugger_state.get().reset_backend());
            self.execution.command_pending.set(false);
            self.execution.execution_transition_generation.set(
                self.execution
                    .execution_transition_generation
                    .get()
                    .wrapping_add(1),
            );
            self.execution.session_pending.set(false);
            self.execution.native_until_active.set(false);
            self.execution
                .pending_execution_inferior
                .borrow_mut()
                .take();
            self.execution.active_thread_execution.borrow_mut().take();
            self.execution
                .thread_execution_exit_candidate
                .borrow_mut()
                .take();
            self.execution.inferior_action_pending.set(None);
            self.execution.thread_action_pending.set(None);
        }

        changed
    }

    pub(crate) fn set_controls_running(&self, running: bool) -> bool {
        if running {
            // Returning to stopped must require a fresh context, even when
            // execution was observed outside the normal event-reduction path.
            self.invalidate_stop_context();
        }

        let state = self.execution.debugger_state.get();

        if state.inferior_running() == running {
            return false;
        }

        self.execution
            .debugger_state
            .set(state.with_inferior_running(running));
        true
    }

    pub(crate) fn set_inferior_started(&self, started: bool) -> bool {
        if !started {
            self.execution.inferior_pid.set(None);
        }

        let state = self.execution.debugger_state.get();

        if state.inferior_started() == started {
            return false;
        }

        self.execution
            .debugger_state
            .set(state.with_inferior_started(started));
        true
    }

    pub(crate) fn set_debug_state_stale(&self, stale: bool) -> bool {
        let state = self.execution.debugger_state.get();

        if state.state_stale() == stale {
            return false;
        }

        self.execution
            .debugger_state
            .set(state.with_state_stale(stale));
        true
    }

    pub(crate) fn apply_debugger_state_delta(&self, delta: DebuggerStateDelta) {
        self.invalidate_stop_context();
        self.execution
            .debugger_state
            .set(self.execution.debugger_state.get().applying(delta));

        if delta.clears_inferior() {
            self.set_thread_stop_reason(None);
            self.clear_inferiors();
        }
    }

    pub(crate) fn set_resynchronizing(&self, resynchronizing: bool) -> bool {
        let state = self.execution.debugger_state.get();
        self.execution
            .debugger_state
            .set(state.with_resynchronizing(resynchronizing));

        state.resynchronizing() != resynchronizing
    }

    pub(crate) fn begin_execution_transition(&self) -> u64 {
        let generation = self
            .execution
            .execution_transition_generation
            .get()
            .wrapping_add(1);
        self.execution
            .execution_transition_generation
            .set(generation);
        self.execution.debugger_state.set(
            self.execution
                .debugger_state
                .get()
                .with_transition_pending(true),
        );

        generation
    }

    pub(crate) fn finish_execution_transition(&self) -> bool {
        let state = self.execution.debugger_state.get();

        if !state.transition_pending() {
            return false;
        }

        self.execution
            .debugger_state
            .set(state.with_transition_pending(false));
        self.execution.execution_transition_generation.set(
            self.execution
                .execution_transition_generation
                .get()
                .wrapping_add(1),
        );
        true
    }

    pub(crate) fn set_current_session(&self, session: DebugSession) {
        self.execution.current_session.replace(Some(session));
    }

    pub(crate) fn session(&self) -> std::cell::Ref<'_, Option<DebugSession>> {
        self.execution.current_session.borrow()
    }

    pub(crate) fn set_gdb_capabilities(&self, capabilities: GdbCapabilities) {
        self.execution.gdb_capabilities.replace(capabilities);
    }

    pub(crate) fn begin_thread_analysis(&self) -> u64 {
        let generation = self
            .execution
            .thread_analysis_generation
            .get()
            .wrapping_add(1);
        self.execution.thread_analysis_generation.set(generation);

        generation
    }

    pub(crate) fn finish_inferior_execution_action(&self) -> bool {
        if self.execution.inferior_action_pending.get() != Some(InferiorActionPending::Execution) {
            return false;
        }

        self.clear_inferior_action_pending()
    }

    pub(crate) fn begin_inferior_execution_action(&self, id: String) -> u64 {
        let generation = self
            .execution
            .inferior_execution_generation
            .get()
            .wrapping_add(1);
        self.execution.inferior_execution_generation.set(generation);
        self.set_inferior_action_pending(Some(InferiorActionPending::Execution));
        self.set_pending_execution_inferior(Some(id));

        generation
    }

    pub(crate) fn clear_inferior_action_pending(&self) -> bool {
        if self.execution.inferior_action_pending.get() == Some(InferiorActionPending::Execution) {
            self.execution.inferior_execution_generation.set(
                self.execution
                    .inferior_execution_generation
                    .get()
                    .wrapping_add(1),
            );
        }

        self.set_inferior_action_pending(None)
    }

    pub(crate) fn set_command_pending(&self, value: bool) -> bool {
        self.execution.command_pending.replace(value) != value
    }

    pub(crate) fn set_session_pending(&self, value: bool) -> bool {
        self.execution.session_pending.replace(value) != value
    }

    pub(crate) fn set_native_until_active(&self, value: bool) -> bool {
        self.execution.native_until_active.replace(value) != value
    }

    pub(crate) fn set_gdb_recovery_available(&self, value: bool) -> bool {
        self.execution.gdb_recovery_available.replace(value) != value
    }

    pub(crate) fn set_thread_action_pending(&self, value: Option<ThreadActionPending>) -> bool {
        self.execution.thread_action_pending.replace(value) != value
    }

    pub(crate) fn set_inferior_action_pending(&self, value: Option<InferiorActionPending>) -> bool {
        self.execution.inferior_action_pending.replace(value) != value
    }

    pub(crate) fn values_editable(&self) -> bool {
        let execution = self.execution();
        execution.ready
            && !execution.state.inferior_running()
            && !execution.command_pending
            && !execution.session_pending
    }

    pub(crate) fn can_edit_variable(&self, generation: u64) -> bool {
        self.is_stop_refresh_current(generation) && self.values_editable()
    }
}

pub(crate) fn execution_event_matches_thread(
    active: Option<&str>,
    reported: Option<&str>,
    all_stopped: bool,
) -> bool {
    all_stopped || active.is_none() || matches!(reported, None | Some("all")) || active == reported
}

#[derive(Clone, Copy)]
pub(crate) struct ExecutionSnapshot {
    pub(crate) ready: bool,
    pub(crate) state: DebuggerState,
    pub(crate) command_pending: bool,
    pub(crate) session_pending: bool,
    pub(crate) native_until_active: bool,
    pub(crate) inferior_action_pending: Option<InferiorActionPending>,
    pub(crate) thread_action_pending: Option<ThreadActionPending>,
}

impl DebuggerModel {
    pub(crate) fn gdb_supports(&self, feature: &str) -> bool {
        self.execution.gdb_capabilities.borrow().supports(feature)
    }
}
