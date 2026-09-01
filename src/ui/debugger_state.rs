#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum TargetConnection {
    #[default]
    None,
    Local,
    Remote,
    Core,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum InferiorExecution {
    #[default]
    None,
    Stopped,
    Running,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum ExecutionTransition {
    #[default]
    Stable,
    Pending,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum DebugStateFreshness {
    Fresh,
    #[default]
    Stale,
    Resynchronizing,
}

/// The debugger state that controls target execution and stopped-state data.
///
/// Target connectivity is deliberately independent from inferior execution:
/// an extended-remote connection can remain live with no inferior. Mutations
/// go through the transition methods below so states such as running without
/// an inferior cannot be represented.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct DebuggerState {
    connection: TargetConnection,
    inferior: InferiorExecution,
    transition: ExecutionTransition,
    freshness: DebugStateFreshness,
}

impl DebuggerState {
    pub(super) fn target_connection(self) -> TargetConnection {
        self.connection
    }

    pub(super) fn with_target_connection(mut self, connection: TargetConnection) -> Self {
        self.connection = connection;
        self
    }

    pub(super) fn inferior_started(self) -> bool {
        self.inferior != InferiorExecution::None
    }

    pub(super) fn inferior_running(self) -> bool {
        self.inferior == InferiorExecution::Running
    }

    pub(super) fn with_inferior_started(mut self, started: bool) -> Self {
        self.inferior = match (started, self.inferior) {
            (false, _) => InferiorExecution::None,
            (true, InferiorExecution::None) => InferiorExecution::Stopped,
            (true, state) => state,
        };
        if !started {
            self.freshness = DebugStateFreshness::Stale;
        }
        self
    }

    pub(super) fn with_inferior_running(mut self, running: bool) -> Self {
        self.inferior = match (running, self.inferior) {
            (true, _) => InferiorExecution::Running,
            (false, InferiorExecution::Running) => InferiorExecution::Stopped,
            (false, state) => state,
        };
        if running {
            self.freshness = DebugStateFreshness::Stale;
        }
        self
    }

    pub(super) fn transition_pending(self) -> bool {
        self.transition == ExecutionTransition::Pending
    }

    pub(super) fn with_transition_pending(mut self, pending: bool) -> Self {
        self.transition = if pending {
            ExecutionTransition::Pending
        } else {
            ExecutionTransition::Stable
        };
        self
    }

    pub(super) fn state_stale(self) -> bool {
        self.freshness != DebugStateFreshness::Fresh
    }

    pub(super) fn with_state_stale(mut self, stale: bool) -> Self {
        if self.freshness != DebugStateFreshness::Resynchronizing {
            self.freshness = if stale {
                DebugStateFreshness::Stale
            } else {
                DebugStateFreshness::Fresh
            };
        }
        self
    }

    pub(super) fn resynchronizing(self) -> bool {
        self.freshness == DebugStateFreshness::Resynchronizing
    }

    pub(super) fn with_resynchronizing(mut self, pending: bool) -> Self {
        self.freshness = if pending {
            DebugStateFreshness::Resynchronizing
        } else if self.freshness == DebugStateFreshness::Resynchronizing {
            DebugStateFreshness::Stale
        } else {
            self.freshness
        };
        self
    }

    pub(super) fn reset_backend(mut self) -> Self {
        self.inferior = InferiorExecution::None;
        self.transition = ExecutionTransition::Stable;
        self.freshness = DebugStateFreshness::Stale;
        self.connection = TargetConnection::None;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn running_always_implies_an_inferior() {
        let running = DebuggerState::default().with_inferior_running(true);
        assert!(running.inferior_started());
        assert!(running.inferior_running());
        assert!(running.state_stale());

        let cleared = running.with_inferior_started(false);
        assert!(!cleared.inferior_started());
        assert!(!cleared.inferior_running());
    }

    #[test]
    fn target_connection_is_independent_from_inferior_execution() {
        let connected = DebuggerState::default()
            .with_target_connection(TargetConnection::Remote)
            .with_inferior_started(false);
        assert_eq!(connected.target_connection(), TargetConnection::Remote);
        assert!(!connected.inferior_started());
    }

    #[test]
    fn transition_and_resynchronization_states_are_explicit() {
        let transitioning = DebuggerState::default()
            .with_inferior_started(true)
            .with_transition_pending(true)
            .with_resynchronizing(true)
            .with_state_stale(false);
        assert!(transitioning.transition_pending());
        assert!(transitioning.resynchronizing());
        assert!(transitioning.state_stale());

        let synchronized = transitioning
            .with_transition_pending(false)
            .with_resynchronizing(false)
            .with_state_stale(false);
        assert!(!synchronized.transition_pending());
        assert!(!synchronized.resynchronizing());
        assert!(!synchronized.state_stale());
    }
}
