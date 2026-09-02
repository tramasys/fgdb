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
enum InferiorStateChange {
    #[default]
    Unchanged,
    Clear,
    Stopped,
    Running,
}

/// An atomic debugger-state consequence of one successful GDB command.
///
/// A configured session is deliberately absent here: it describes what the
/// user wants to debug, while this delta records only what GDB has already
/// made true. In particular, selecting an extended-remote target does not
/// imply that an inferior exists.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct DebuggerStateDelta {
    connection: Option<TargetConnection>,
    inferior: InferiorStateChange,
}

impl DebuggerStateDelta {
    /// Select a target whose inferior state must be discovered separately.
    pub(crate) const fn establish_connection(connection: TargetConnection) -> Self {
        Self {
            connection: Some(connection),
            inferior: InferiorStateChange::Unchanged,
        }
    }

    /// Select a target that is known to expose a stopped inferior.
    pub(crate) const fn establish_stopped_target(connection: TargetConnection) -> Self {
        Self {
            connection: Some(connection),
            inferior: InferiorStateChange::Stopped,
        }
    }

    /// Select a target definition without creating an inferior.
    pub(crate) const fn replace_target_without_inferior(connection: TargetConnection) -> Self {
        Self {
            connection: Some(connection),
            inferior: InferiorStateChange::Clear,
        }
    }

    /// Remove the current target and any inferior owned by it.
    pub(crate) const fn clear_target() -> Self {
        Self {
            connection: Some(TargetConnection::None),
            inferior: InferiorStateChange::Clear,
        }
    }

    /// Remove an inferior while retaining its reusable target definition.
    pub(crate) const fn clear_inferior() -> Self {
        Self {
            connection: None,
            inferior: InferiorStateChange::Clear,
        }
    }

    /// Record an observed stopped inferior without changing its target.
    pub(crate) const fn inferior_stopped() -> Self {
        Self {
            connection: None,
            inferior: InferiorStateChange::Stopped,
        }
    }

    /// Record an observed running inferior without changing its target.
    pub(crate) const fn inferior_running() -> Self {
        Self {
            connection: None,
            inferior: InferiorStateChange::Running,
        }
    }

    pub(crate) const fn clears_inferior(self) -> bool {
        matches!(self.inferior, InferiorStateChange::Clear)
    }

    pub(crate) const fn changes_target(self) -> bool {
        self.connection.is_some()
    }

    #[cfg(test)]
    pub(crate) const fn connection_change(self) -> Option<TargetConnection> {
        self.connection
    }
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
pub(crate) struct DebuggerState {
    connection: TargetConnection,
    inferior: InferiorExecution,
    transition: ExecutionTransition,
    freshness: DebugStateFreshness,
}

impl DebuggerState {
    pub(crate) fn target_connection(self) -> TargetConnection {
        self.connection
    }

    #[cfg(test)]
    pub(super) fn with_target_connection(mut self, connection: TargetConnection) -> Self {
        self.connection = connection;

        self
    }

    pub(crate) fn applying(mut self, delta: DebuggerStateDelta) -> Self {
        if let Some(connection) = delta.connection {
            self.connection = connection;
        }

        self.inferior = match delta.inferior {
            InferiorStateChange::Unchanged => self.inferior,
            InferiorStateChange::Clear => InferiorExecution::None,
            InferiorStateChange::Stopped => InferiorExecution::Stopped,
            InferiorStateChange::Running => InferiorExecution::Running,
        };

        self.transition = ExecutionTransition::Stable;
        self.freshness = DebugStateFreshness::Stale;

        self
    }

    pub(crate) fn inferior_started(self) -> bool {
        self.inferior != InferiorExecution::None
    }

    pub(crate) fn inferior_running(self) -> bool {
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

    pub(crate) fn state_stale(self) -> bool {
        self.freshness != DebugStateFreshness::Fresh
    }

    /// Stale frame, thread, and register data only blocks controls while an
    /// inferior actually exists. A loaded executable without a process has no
    /// stopped context to refresh, so target-level actions remain safe.
    pub(crate) fn stopped_context_is_stale(self) -> bool {
        self.inferior_started() && self.state_stale()
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
    fn disconnect_clears_both_connection_and_inferior_atomically() {
        let disconnected = DebuggerState::default()
            .with_target_connection(TargetConnection::Remote)
            .with_inferior_started(true)
            .applying(DebuggerStateDelta::clear_target());

        assert_eq!(disconnected.target_connection(), TargetConnection::None);
        assert!(!disconnected.inferior_started());
        assert!(!disconnected.inferior_running());
        assert!(disconnected.state_stale());
    }

    #[test]
    fn clearing_an_inferior_preserves_a_reusable_local_target() {
        let killed = DebuggerState::default()
            .with_target_connection(TargetConnection::Local)
            .with_inferior_running(true)
            .applying(DebuggerStateDelta::clear_inferior());

        assert_eq!(killed.target_connection(), TargetConnection::Local);
        assert!(!killed.inferior_started());
        assert!(!killed.inferior_running());
    }

    #[test]
    fn remote_connection_does_not_invent_an_inferior() {
        let connected = DebuggerState::default().applying(
            DebuggerStateDelta::establish_connection(TargetConnection::Remote),
        );

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

    #[test]
    fn target_without_an_inferior_has_no_stale_stopped_context() {
        let loaded_target = DebuggerState::default().applying(
            DebuggerStateDelta::replace_target_without_inferior(TargetConnection::Local),
        );

        assert!(loaded_target.state_stale());
        assert!(!loaded_target.stopped_context_is_stale());
        let stopped = loaded_target.applying(DebuggerStateDelta::inferior_stopped());
        assert!(stopped.stopped_context_is_stale());
    }
}
