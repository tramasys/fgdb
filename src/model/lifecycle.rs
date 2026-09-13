//! Complete observed transitions establish authority before presentation runs.

use super::*;
use crate::debugger::{MiEvent, ReturnValue};

pub(crate) struct ThreadExitEffect {
    pub(crate) selected_non_stop: bool,
    pub(crate) abort_until: bool,
}

pub(crate) struct SelectionEffect {
    pub(crate) inferior_changed: bool,
    pub(crate) inspectable: bool,
}

impl DebuggerModel {
    pub(crate) fn observe_inferior_started(&self, id: &str, pid: Option<u32>) {
        self.symbols.forget_inferior(id);
        self.reset_target_abi();
        self.record_inferior_started(id, pid);
    }

    pub(crate) fn observe_selection(
        &self,
        thread: Option<&str>,
        group: Option<&str>,
        frame: Option<u32>,
    ) -> SelectionEffect {
        let previous = self.selected_inferior_id();
        self.apply_gdb_selection(thread, group);

        if let Some(frame) = frame {
            self.select_frame(frame);
        }

        let inspectable = self.selected_inferior_context_stopped() && !self.native_until_active();

        if inspectable {
            self.set_controls_running(false);
            self.set_debug_state_stale(false);
        }

        SelectionEffect {
            inferior_changed: previous != self.selected_inferior_id(),
            inspectable,
        }
    }

    pub(crate) fn complete_stop(&self, reason: &str) -> bool {
        self.set_debug_state_stale(false);
        self.set_controls_running(false);
        self.set_thread_stop_reason(Some(stop_reason_label(reason)));
        let exited = reason.starts_with("exited");

        if !exited {
            self.set_inferior_started(true);
        }

        exited
    }

    pub(crate) fn observe_thread_exit(&self, id: &str) -> ThreadExitEffect {
        let selected = self.current_thread_id().as_deref() == Some(id);
        let transition = self.execution_transition_matches_thread(Some(id), false);
        let thread_action = self.thread_execution_transition_matches(Some(id), false);
        let active = self.active_thread_execution().as_deref() == Some(id);
        let non_stop = self.non_stop_mode() == Some(true);
        let abort_until = self.native_until_active() && active && non_stop;
        self.forget_thread_group(id);

        if selected_thread_execution_may_be_orphaned(
            self.active_thread_execution().as_deref(),
            self.current_thread_id().as_deref(),
            id,
            self.inferior_is_running(),
            self.non_stop_mode(),
        ) {
            self.set_thread_execution_exit_candidate(Some(id.to_owned()));
        }

        if active && non_stop {
            if transition {
                self.finish_execution_transition();
                self.set_command_pending(false);
            }

            if thread_action {
                self.finish_thread_execution_action();
            }

            self.set_active_thread_execution(None);
            self.set_thread_execution_exit_candidate(None);
        }

        if selected && non_stop {
            self.set_current_thread_id(None);
            self.set_controls_running(false);
            self.set_debug_state_stale(true);
        }

        ThreadExitEffect {
            selected_non_stop: selected && non_stop,
            abort_until,
        }
    }

    pub(crate) fn observe_backend_ready(&self, capabilities: GdbCapabilities) {
        self.reset_replay();

        self.replay
            .borrow_mut()
            .set_backend_direction(replay::ExecutionDirection::Forward);

        self.printer_scripts.borrow_mut().reset();
        self.finish_execution_transition();
        self.set_command_pending(false);
        self.set_active_thread_execution(None);
        self.set_thread_execution_exit_candidate(None);
        self.set_debug_state_stale(false);
        self.set_gdb_recovery_available(false);
        self.set_gdb_capabilities(capabilities);
        self.reset_target_abi();
        self.set_controls_ready(true);
    }

    pub(crate) fn execution_failed(&self) {
        self.finish_execution_transition();
        self.set_command_pending(false);
        self.set_active_thread_execution(None);
        self.set_thread_execution_exit_candidate(None);
        self.set_pending_execution_inferior(None);
        self.clear_inferior_action_pending();
        self.set_thread_action_pending(None);
    }

    pub(crate) fn enter_recovery(&self) {
        self.execution_failed();
        self.set_debug_state_stale(true);
        self.set_controls_ready(false);
        self.set_gdb_recovery_available(true);
    }

    pub(crate) fn observe_backend_disconnected(&self) {
        self.enter_recovery();
        self.set_resynchronizing(false);
        self.set_gdb_capabilities(GdbCapabilities::default());
        self.set_thread_control_policy(None, None);
        self.clear_inferiors();
        self.set_inferior_started(false);
    }

    pub(crate) fn observe_inferior_exit(&self, id: &str) -> bool {
        self.symbols.forget_inferior(id);
        let selected_exited = self.inferior_exit_owns_selected_context(id);
        let pending_exited = self.pending_execution_inferior().as_deref() == Some(id);
        let active_exited = selected_exited
            || self
                .active_thread_execution()
                .as_deref()
                .and_then(|thread| self.inferior_for_thread(thread))
                .as_deref()
                == Some(id);
        let transition_exited = pending_exited
            || (active_exited && self.execution_transition_matches_thread(None, true));

        if active_exited {
            self.set_active_thread_execution(None);
            self.set_thread_execution_exit_candidate(None);
        }

        self.record_inferior_exited(id);

        if transition_exited {
            self.finish_execution_transition();
            self.set_command_pending(false);
        }

        if pending_exited {
            self.set_pending_execution_inferior(None);
            self.finish_inferior_execution_action();
        }

        if selected_exited {
            self.finish_thread_execution_action();
            self.set_current_thread_id(None);
            self.set_thread_stop_reason(None);
            self.set_controls_running(false);
            self.set_inferior_started(false);
            self.set_debug_state_stale(false);
        }

        selected_exited
    }

    pub(crate) fn observe_running(&self, thread: Option<&str>) -> bool {
        {
            let mut replay = self.replay.borrow_mut();

            if replay.querying {
                replay.invalidate_query();
            }
        }

        let group_transition = self.pending_execution_inferior().is_some();
        let thread_transition = self.execution_transition_matches_thread(thread, false);
        let thread_action = self.thread_execution_transition_matches(thread, false);
        let (selected, inferior_transition) = self.mark_inferior_running(thread);
        self.start_inferior_refresh();

        // Establish the durable interlock before releasing command admission.
        if selected {
            self.set_controls_running(true);
        }

        self.complete_observed_execution(
            group_transition,
            inferior_transition,
            thread_transition,
            thread_action,
        );

        if selected && !self.native_until_active() {
            self.set_debug_state_stale(true);
            self.set_inferior_started(true);
            self.set_thread_stop_reason(None);
        }

        selected
    }

    pub(crate) fn observe_stop(
        &self,
        thread: Option<&str>,
        group: Option<&str>,
        frame: u32,
        fork_pid: Option<u32>,
        all_stopped: bool,
        returned: Option<ReturnValue>,
    ) {
        // Refreshing the same stop changes request generations, not this sequence.
        self.stopped
            .observed_stop_sequence
            .set(self.observed_stop_sequence().wrapping_add(1));

        let transition = reduce_stop_transition(
            self.non_stop_mode(),
            self.thread_execution_exit_candidate().is_some(),
            self.active_thread_execution().as_deref(),
            thread,
            all_stopped,
        );
        let group_transition = self.pending_execution_inferior().is_some();
        let thread_transition =
            self.execution_transition_matches_thread(thread, transition.terminal_all_stopped);
        let thread_action =
            self.thread_execution_transition_matches(thread, transition.terminal_all_stopped);

        if transition.active_execution_stopped || self.native_until_active() {
            self.set_active_thread_execution(None);
            self.set_thread_execution_exit_candidate(None);
        }

        self.record_thread_group(thread, group);
        self.set_current_thread_id(thread);
        self.select_frame(frame);
        self.record_pending_fork(thread, fork_pid);
        let inferior_transition =
            self.mark_inferior_stopped(thread, transition.terminal_all_stopped);
        self.set_controls_running(false);
        self.record_return_value(returned, thread, group);
        self.complete_observed_execution(
            group_transition,
            inferior_transition,
            thread_transition,
            thread_action,
        );
    }

    fn complete_observed_execution(
        &self,
        group_transition: bool,
        inferior_transition: bool,
        thread_transition: bool,
        thread_action: bool,
    ) {
        if inferior_transition {
            self.finish_inferior_execution_action();
        }

        if thread_action {
            self.finish_thread_execution_action();
        }

        if if group_transition {
            inferior_transition
        } else {
            thread_transition
        } {
            self.finish_execution_transition();
            self.set_command_pending(false);
        }
    }

    fn finish_thread_execution_action(&self) {
        if self.execution().thread_action_pending == Some(ThreadActionPending::Execution) {
            self.set_thread_action_pending(None);
        }
    }
}

/// Pure decisions derived from an asynchronous GDB event before any GTK or
/// mutable debugger model is touched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EventAdmission {
    Apply,
    IgnoreFromQuarantinedBackend,
}

pub(crate) fn admit_event(recovery_required: bool, event: &MiEvent) -> EventAdmission {
    if recovery_required
        && !matches!(
            event,
            MiEvent::Ready(_) | MiEvent::DebuggerUnusable(_) | MiEvent::Disconnected
        )
    {
        EventAdmission::IgnoreFromQuarantinedBackend
    } else {
        EventAdmission::Apply
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StopTransition {
    pub(crate) terminal_all_stopped: bool,
    pub(crate) active_execution_stopped: bool,
}

pub(crate) fn reduce_stop_transition(
    non_stop: Option<bool>,
    exit_candidate_present: bool,
    active_thread: Option<&str>,
    reported_thread: Option<&str>,
    all_stopped: bool,
) -> StopTransition {
    // In all-stop mode some GDB versions omit stopped-threads="all" on the
    // replacement stop emitted after the stepping thread exits.
    let replacement_after_thread_exit = exit_candidate_present && non_stop != Some(true);
    let terminal_all_stopped = all_stopped || replacement_after_thread_exit;

    let active_execution_stopped = terminal_all_stopped
        || active_thread.is_some_and(|active| {
            matches!(reported_thread, None | Some("all")) || reported_thread == Some(active)
        });

    StopTransition {
        terminal_all_stopped,
        active_execution_stopped,
    }
}

pub(crate) fn selected_thread_execution_may_be_orphaned(
    active_thread: Option<&str>,
    current_thread: Option<&str>,
    exited_thread: &str,
    running: bool,
    non_stop: Option<bool>,
) -> bool {
    running
        && non_stop != Some(true)
        && active_thread == Some(exited_thread)
        && current_thread == Some(exited_thread)
}

pub(crate) fn stop_reason_label(reason: &str) -> String {
    match reason {
        "breakpoint-hit" => String::from("BREAKPOINT"),
        "end-stepping-range" => String::from("STEP"),
        "function-finished" => String::from("FINISH"),
        "location-reached" => String::from("UNTIL"),
        "signal-received" => String::from("SIGNAL"),
        "watchpoint-trigger" => String::from("WATCHPOINT"),
        other => other.replace('-', " ").to_uppercase(),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn selection_and_thread_exit_complete_without_widgets() {
        let model = crate::model::tests::stopped_model();
        model.set_thread_control_policy(None, Some(true));
        let selected = model.observe_selection(Some("3"), Some("i2"), Some(0));
        assert!(selected.inferior_changed);
        assert!(selected.inspectable);
        assert_eq!(model.current_thread_id().as_deref(), Some("3"));
        model.prepare_execution(
            crate::model::actions::ExecutionTarget::SelectedThread,
            false,
        );
        model.accept_execution();
        model.observe_running(Some("3"));
        let exited = model.observe_thread_exit("3");
        assert!(exited.selected_non_stop);
        assert!(!exited.abort_until);
        assert!(model.current_thread_id().is_none());
        assert!(!model.command_pending());
        assert!(model.active_thread_execution().is_none());
        assert!(!model.inferior_is_running());
        assert!(!model.complete_stop("end-stepping-range"));
        assert_eq!(model.thread_stop_reason().as_deref(), Some("STEP"));
        assert!(model.complete_stop("exited-normally"));
    }

    use super::*;
    use crate::debugger::GdbCapabilities;

    #[test]
    fn execution_intent_rolls_back_and_observations_complete_without_a_view() {
        let model = crate::model::tests::stopped_model();
        model.set_active_thread_execution(Some(String::from("old")));
        model.set_thread_execution_exit_candidate(Some(String::from("old")));
        model.set_pending_execution_inferior(Some(String::from("i2")));
        let origin = model.prepare_execution(ExecutionTarget::Thread("1"), false);
        assert_eq!(model.active_thread_execution().as_deref(), Some("1"));
        assert!(model.pending_execution_inferior().is_none());
        assert!(model.thread_execution_exit_candidate().is_none());
        model.reject_execution(origin);
        assert_eq!(model.active_thread_execution().as_deref(), Some("old"));
        assert_eq!(
            model.thread_execution_exit_candidate().as_deref(),
            Some("old")
        );
        assert_eq!(model.pending_execution_inferior().as_deref(), Some("i2"));
        model.execution_failed();
        model.start_stop_refresh();
        let context = model.bind_stop_context(1).unwrap();
        model.prepare_execution(ExecutionTarget::SelectedThread, false);
        let operation = model.accept_execution();
        assert!(model.execution_transition_is_pending(operation));
        assert!(!model.movement_commands_available());
        assert!(model.observe_running(Some("1")));
        assert!(model.inferior_is_running());
        assert!(!model.is_stop_context_current(&context));
        assert!(!model.execution_transition_is_pending(operation));
        assert!(!model.command_pending());
        model.prepare_execution(ExecutionTarget::All, true);
        assert_eq!(model.active_thread_execution().as_deref(), Some("1"));
        model.accept_execution();
        model.observe_stop(Some("1"), Some("i1"), 2, None, true, None);
        assert!(!model.inferior_is_running());
        assert_eq!(model.selected_frame_level(), 2);
        assert!(model.active_thread_execution().is_none());
        assert!(!model.command_pending());
        assert!(!model.is_stop_context_current(&context));
    }

    #[test]
    fn unrelated_non_stop_execution_does_not_finish_the_active_command() {
        let model = crate::model::tests::stopped_model();
        model.set_thread_control_policy(None, Some(true));
        model.prepare_execution(ExecutionTarget::Thread("1"), false);
        model.set_thread_action_pending(Some(ThreadActionPending::Execution));
        let operation = model.accept_execution();
        assert!(!model.observe_running(Some("3")));
        assert!(model.execution_transition_is_pending(operation));
        assert!(model.command_pending());
        assert!(!model.observe_inferior_exit("i2"));
        assert!(model.execution_transition_is_pending(operation));
        assert!(model.observe_running(Some("1")));
        assert!(!model.execution_transition_is_pending(operation));
        assert!(model.execution().thread_action_pending.is_none());
        assert!(model.observe_inferior_exit("i1"));
        assert!(!model.inferior_has_started());
        assert!(!model.command_pending());
    }

    #[test]
    fn backend_loss_retires_authority_before_any_presentation_cleanup() {
        let model = crate::model::tests::stopped_model();
        model.start_stop_refresh();
        let context = model.bind_stop_context(4).unwrap();
        model.set_native_until_active(true);
        let action = model.begin_inferior_execution_action(String::from("i1"));
        model.prepare_execution(ExecutionTarget::Inferior("i1"), false);
        model.accept_execution();
        model.observe_backend_disconnected();
        assert!(!model.is_stop_context_current(&context));
        assert!(!model.execution().ready);
        assert!(!model.native_until_active());
        assert!(!model.command_pending());
        assert!(model.gdb_recovery_required());
        assert!(model.pending_execution_inferior().is_none());
        assert!(!model.inferior_execution_action_pending_for("i1", action));
        assert!(model.inferiors().is_empty());
        model.observe_backend_ready(GdbCapabilities::default());
        assert!(model.execution().ready);
        assert!(!model.gdb_recovery_required());
        assert!(!model.is_stop_context_current(&context));
    }

    #[test]
    fn quarantined_backends_only_accept_recovery_boundary_events() {
        assert_eq!(
            admit_event(true, &MiEvent::Running { thread_id: None }),
            EventAdmission::IgnoreFromQuarantinedBackend
        );

        assert_eq!(
            admit_event(true, &MiEvent::Ready(GdbCapabilities::default())),
            EventAdmission::Apply
        );

        assert_eq!(
            admit_event(true, &MiEvent::Disconnected),
            EventAdmission::Apply
        );

        assert_eq!(
            admit_event(false, &MiEvent::Running { thread_id: None }),
            EventAdmission::Apply
        );
    }

    #[test]
    fn all_stop_replacement_after_thread_exit_completes_execution() {
        let transition = reduce_stop_transition(Some(false), true, Some("2"), Some("3"), false);
        assert!(transition.terminal_all_stopped);
        assert!(transition.active_execution_stopped);
    }

    #[test]
    fn non_stop_only_completes_the_reported_active_thread() {
        let unrelated = reduce_stop_transition(Some(true), true, Some("2"), Some("3"), false);
        assert!(!unrelated.terminal_all_stopped);
        assert!(!unrelated.active_execution_stopped);
        let active = reduce_stop_transition(Some(true), false, Some("2"), Some("2"), false);
        assert!(!active.terminal_all_stopped);
        assert!(active.active_execution_stopped);
    }
}
