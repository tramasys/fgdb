use crate::debugger::MiEvent;

/// Pure decisions derived from an asynchronous GDB event before any GTK or
/// mutable debugger model is touched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum EventAdmission {
    Apply,
    IgnoreFromQuarantinedBackend,
}

pub(super) fn admit_event(recovery_required: bool, event: &MiEvent) -> EventAdmission {
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
pub(super) struct StopTransition {
    pub(super) terminal_all_stopped: bool,
    pub(super) active_execution_stopped: bool,
}

pub(super) fn reduce_stop_transition(
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::debugger::GdbCapabilities;

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
