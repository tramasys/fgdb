//! Authoritative debugger state, independent of GTK and repaint scheduling.
//!
//! Configured intent, observed execution, process selection and stopped data
//! have separate owners. UI render caches never authorize debugger commands.

use crate::{
    config::DebugSession,
    debugger::{
        GdbCapabilities, InferiorInfo, InferiorState, Register, StackEntry, StackFrame,
        StopContext, ThreadInfo, context::MemoryRegion,
    },
};
use actions::*;
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
};
pub(crate) mod actions;
mod execution;
pub(crate) mod processes;
mod state;
mod stopped;
#[cfg(test)]
pub(crate) use execution::execution_event_matches_thread;
pub(crate) use state::{DebuggerState, DebuggerStateDelta, TargetConnection};

pub(crate) struct DebuggerModel {
    execution: ExecutionState,
    processes: ProcessState,
    stopped: StoppedState,
}

impl DebuggerModel {
    pub(crate) fn new(initial_session: Option<DebugSession>) -> Self {
        Self {
            execution: ExecutionState::new(initial_session),
            processes: ProcessState::new(),
            stopped: StoppedState::new(),
        }
    }
}

struct ExecutionState {
    debugger_pid: Cell<Option<u32>>,
    inferior_pid: Cell<Option<u32>>,
    current_session: RefCell<Option<DebugSession>>,
    gdb_capabilities: RefCell<GdbCapabilities>,
    gdb_recovery_available: Cell<bool>,
    debugger_ready: Cell<bool>,
    debugger_state: Cell<DebuggerState>,
    command_pending: Cell<bool>,
    session_pending: Cell<bool>,
    execution_transition_generation: Cell<u64>,
    native_until_active: Cell<bool>,
    pending_execution_inferior: RefCell<Option<String>>,
    active_thread_execution: RefCell<Option<String>>,
    thread_execution_exit_candidate: RefCell<Option<String>>,
    inferior_action_pending: Cell<Option<InferiorActionPending>>,
    inferior_execution_generation: Cell<u64>,
    thread_action_pending: Cell<Option<ThreadActionPending>>,
    thread_analysis_generation: Cell<u64>,
}

impl ExecutionState {
    fn new(initial_session: Option<DebugSession>) -> Self {
        Self {
            debugger_pid: Cell::new(None),
            inferior_pid: Cell::new(None),
            current_session: RefCell::new(initial_session),
            gdb_capabilities: RefCell::new(GdbCapabilities::default()),
            gdb_recovery_available: Cell::new(false),
            debugger_ready: Cell::new(false),
            debugger_state: Cell::new(DebuggerState::default()),
            command_pending: Cell::new(false),
            session_pending: Cell::new(false),
            execution_transition_generation: Cell::new(0),
            native_until_active: Cell::new(false),
            pending_execution_inferior: RefCell::new(None),
            active_thread_execution: RefCell::new(None),
            thread_execution_exit_candidate: RefCell::new(None),
            inferior_action_pending: Cell::new(None),
            inferior_execution_generation: Cell::new(0),
            thread_action_pending: Cell::new(None),
            thread_analysis_generation: Cell::new(0),
        }
    }
}

struct ProcessState {
    inferiors: RefCell<Vec<InferiorInfo>>,
    thread_inferior_ids: RefCell<HashMap<String, String>>,
    selected_inferior_id: RefCell<Option<String>>,
    selected_thread_id: RefCell<Option<String>>,
    selected_frame_level: Cell<u32>,
    stop_owner_inferior_id: RefCell<Option<String>>,
    stop_owner_thread_id: RefCell<Option<String>>,
    inferior_parents: RefCell<HashMap<String, String>>,
    pending_fork_parents: RefCell<HashMap<u32, String>>,
    fork_follow_mode: Cell<Option<ForkFollowMode>>,
    detach_on_fork: Cell<Option<bool>>,
    scheduler_locking: Cell<Option<SchedulerLockingMode>>,
    non_stop_mode: Cell<Option<bool>>,
    thread_stop_reason: RefCell<Option<String>>,
    inferior_refresh_generation: Cell<u64>,
    inferior_refresh_gate: RefreshGate,
    fork_policy_refresh_gate: RefreshGate,
    fork_policy_generation: Cell<u64>,
    thread_policy_generation: Cell<u64>,
    thread_refresh_generation: Cell<u64>,
    threads: RefCell<Rc<[ThreadInfo]>>,
}

impl ProcessState {
    fn new() -> Self {
        Self {
            inferiors: RefCell::new(Vec::new()),
            thread_inferior_ids: RefCell::new(HashMap::new()),
            selected_inferior_id: RefCell::new(None),
            selected_thread_id: RefCell::new(None),
            selected_frame_level: Cell::new(0),
            stop_owner_inferior_id: RefCell::new(None),
            stop_owner_thread_id: RefCell::new(None),
            inferior_parents: RefCell::new(HashMap::new()),
            pending_fork_parents: RefCell::new(HashMap::new()),
            fork_follow_mode: Cell::new(None),
            detach_on_fork: Cell::new(None),
            scheduler_locking: Cell::new(None),
            non_stop_mode: Cell::new(None),
            thread_stop_reason: RefCell::new(None),
            inferior_refresh_generation: Cell::new(0),
            inferior_refresh_gate: RefreshGate::default(),
            fork_policy_refresh_gate: RefreshGate::default(),
            fork_policy_generation: Cell::new(0),
            thread_policy_generation: Cell::new(0),
            thread_refresh_generation: Cell::new(0),
            threads: RefCell::new(Rc::from([])),
        }
    }
}

struct StoppedState {
    stop_refresh_generation: Cell<u64>,
    active_stop_context: RefCell<Option<crate::debugger::StopContext>>,
    latest_frames: RefCell<Rc<[StackFrame]>>,
    latest_frames_generation: Cell<Option<u64>>,
    latest_registers: RefCell<Vec<Register>>,
    latest_registers_generation: Cell<Option<u64>>,
    register_details_generation: Cell<Option<u64>>,
    latest_stack: RefCell<Vec<StackEntry>>,
    latest_stack_generation: Cell<Option<u64>>,
    stack_memory_refresh_generation: Cell<Option<u64>>,
    stack_details_generation: Cell<Option<u64>>,
    memory_regions: RefCell<Vec<MemoryRegion>>,
    memory_regions_generation: Cell<Option<u64>>,
    memory_watches_refresh_generation: Cell<Option<u64>>,
    tls_runtime_refresh_generation: Cell<Option<u64>>,
    previous_registers: RefCell<HashMap<String, String>>,
    cached_register_names: RefCell<Option<Rc<Vec<String>>>>,
}

impl StoppedState {
    fn new() -> Self {
        Self {
            stop_refresh_generation: Cell::new(0),
            active_stop_context: RefCell::new(None),
            latest_frames: RefCell::new(Rc::from([])),
            latest_frames_generation: Cell::new(None),
            latest_registers: RefCell::new(Vec::new()),
            latest_registers_generation: Cell::new(None),
            register_details_generation: Cell::new(None),
            latest_stack: RefCell::new(Vec::new()),
            latest_stack_generation: Cell::new(None),
            stack_memory_refresh_generation: Cell::new(None),
            stack_details_generation: Cell::new(None),
            memory_regions: RefCell::new(Vec::new()),
            memory_regions_generation: Cell::new(None),
            memory_watches_refresh_generation: Cell::new(None),
            tls_runtime_refresh_generation: Cell::new(None),
            previous_registers: RefCell::new(HashMap::new()),
            cached_register_names: RefCell::new(None),
        }
    }
}

#[derive(Default)]
pub(crate) struct RefreshGate {
    in_flight: Cell<bool>,
    queued: Cell<bool>,
}

impl RefreshGate {
    pub(crate) fn begin(&self) -> bool {
        if self.in_flight.replace(true) {
            self.queued.set(true);

            false
        } else {
            true
        }
    }

    pub(crate) fn finish(&self) -> bool {
        self.in_flight.set(false);

        self.queued.replace(false)
    }

    pub(crate) fn invalidate(&self) {
        if self.in_flight.get() {
            self.queued.set(true);
        }
    }
}

pub(crate) fn configured_target_can_start(
    session: Option<&DebugSession>,
    connection: TargetConnection,
) -> bool {
    match session {
        None => true,
        Some(DebugSession::Launch { .. }) => connection == TargetConnection::Local,
        Some(DebugSession::Remote {
            extended: true,
            remote_executable: Some(_),
            ..
        }) => connection == TargetConnection::Remote,
        Some(
            DebugSession::Attach { .. }
            | DebugSession::CoreDump { .. }
            | DebugSession::Remote { .. },
        ) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn launch_session() -> DebugSession {
        DebugSession::Launch {
            executable: "fixture".into(),
            arguments: vec!["argument".into()],
            environment: vec![("KEY".into(), "value".into())],
            working_directory: ".".into(),
        }
    }

    fn thread(id: &str, group: &str, current: bool) -> ThreadInfo {
        ThreadInfo {
            id: id.into(),
            group_id: Some(group.into()),
            target_id: format!("Thread {id}"),
            name: None,
            state: "stopped".into(),
            core: None,
            frame: None,
            pc_symbol: None,
            current,
        }
    }

    fn inferior(id: &str, pid: u32, threads: Vec<ThreadInfo>) -> InferiorInfo {
        InferiorInfo {
            id: id.into(),
            pid: Some(pid),
            exit_code: None,
            executable: Some("fixture".into()),
            state: InferiorState::Stopped,
            threads,
        }
    }

    fn stopped_model() -> DebuggerModel {
        let model = DebuggerModel::new(Some(launch_session()));
        model.set_controls_ready(true);
        model.apply_debugger_state_delta(DebuggerStateDelta::establish_stopped_target(
            TargetConnection::Local,
        ));
        model.show_inferiors(vec![
            inferior(
                "i1",
                101,
                vec![thread("1", "i1", true), thread("2", "i1", false)],
            ),
            inferior("i2", 202, vec![thread("3", "i2", false)]),
        ]);
        model.set_debug_state_stale(false);

        model
    }

    fn frame(level: u32) -> StackFrame {
        StackFrame {
            level,
            address: format!("0x{level:x}"),
            function: format!("frame_{level}"),
            architecture: None,
            file: None,
            fullname: None,
            line: None,
        }
    }

    #[test]
    fn configured_intent_survives_exit_and_backend_replacement() {
        let model = stopped_model();
        assert!(model.movement_commands_available());
        model.apply_debugger_state_delta(DebuggerStateDelta::clear_inferior());
        model.set_debug_state_stale(false);
        assert!(!model.inferior_has_started());
        assert!(model.configured_session_can_start());
        assert!(model.inferiors().is_empty());
        assert!(model.threads().is_empty());
        assert!(!model.movement_commands_available());
        assert!(model.stop_point_commands_available());

        model.set_controls_ready(false);
        assert!(!model.configured_session_can_start());
        assert_eq!(model.current_session(), Some(launch_session()));
        model.set_controls_ready(true);
        model.apply_debugger_state_delta(DebuggerStateDelta::replace_target_without_inferior(
            TargetConnection::Local,
        ));
        model.set_debug_state_stale(false);
        assert!(model.configured_session_can_start());
        assert!(model.stop_point_commands_available());
    }

    #[test]
    fn execution_interlocks_and_timeouts_are_owned_by_the_model() {
        let model = stopped_model();
        model.set_active_thread_execution(Some("2".into()));
        let first = model.begin_execution_transition();
        assert!(!model.movement_commands_available());
        assert!(!model.stopped_inspection_available());
        assert!(!model.execution_transition_matches_thread(Some("1"), false));
        assert!(model.execution_transition_matches_thread(Some("2"), false));
        assert!(model.finish_execution_transition());
        let second = model.begin_execution_transition();
        assert!(!model.execution_transition_is_pending(first));
        assert!(model.execution_transition_is_pending(second));

        let inferior_request = model.begin_inferior_execution_action("i2".into());
        model.set_thread_action_pending(Some(ThreadActionPending::Execution));
        model.set_thread_execution_exit_candidate(Some("2".into()));
        model.set_controls_ready(false);
        assert!(!model.execution_transition_is_pending(second));
        assert!(!model.inferior_execution_action_pending_for("i2", inferior_request));
        assert!(model.execution().thread_action_pending.is_none());
        assert!(model.active_thread_execution().is_none());
        assert!(model.pending_execution_inferior().is_none());
        assert!(model.thread_execution_exit_candidate().is_none());
    }

    #[test]
    fn delayed_render_snapshots_cannot_authorize_thread_execution() {
        let model = stopped_model();
        let painted = model.threads();
        model.publish_threads(&painted);
        assert!(Rc::ptr_eq(&painted, &model.threads()));
        assert!(model.thread_action_can_dispatch(&ThreadAction::RunOnly("1".into())));

        model.mark_inferior_running(Some("1"));
        assert_eq!(painted[0].state, "stopped");
        assert_eq!(model.threads()[0].state, "running");
        assert!(!model.movement_commands_available());
        assert!(!model.stopped_inspection_available());
        assert!(!model.thread_action_can_dispatch(&ThreadAction::RunOnly("1".into())));
        assert!(!model.thread_action_can_dispatch(&ThreadAction::RunOnly("2".into())));
        model.set_thread_control_policy(Some(SchedulerLockingMode::Off), Some(true));
        assert!(model.thread_action_can_dispatch(&ThreadAction::Freeze("1".into())));

        model.mark_inferior_stopped(Some("1"), true);
        assert!(model.thread_action_can_dispatch(&ThreadAction::RunOnly("1".into())));
        assert!(model.thread_action_can_dispatch(&ThreadAction::Thaw("2".into())));
    }

    #[test]
    fn stop_owner_and_selected_inferior_are_independent() {
        let model = stopped_model();
        model.mark_inferior_stopped(Some("1"), false);
        assert!(model.set_selected_inferior("i2"));
        assert_eq!(model.stop_owner_inferior_id().as_deref(), Some("i1"));
        assert_eq!(model.selected_inferior_id().as_deref(), Some("i2"));
        model.mark_inferior_running(Some("3"));
        assert_eq!(model.stop_owner_thread_id().as_deref(), Some("1"));
        model.record_inferior_exited("i1");
        assert_eq!(model.selected_inferior_id().as_deref(), Some("i2"));
        assert!(model.inferior_has_started());
        assert!(model.stop_owner_thread_id().is_none());
    }

    #[test]
    fn selection_and_backend_changes_revoke_stopped_data_without_a_view() {
        let model = stopped_model();
        let generation = model.start_stop_refresh();
        let context = model.bind_stop_context(7).expect("stopped context");
        assert_eq!(context.generation(), generation);
        assert!(model.publish_frames(Some(generation), &[frame(0), frame(1)]));
        let painted = model.frames();
        assert!(model.claim_register_details(generation));
        assert!(!model.claim_register_details(generation));

        model.select_frame(1);
        model.select_frame(0);
        assert!(!model.is_stop_refresh_current(generation));
        assert!(!model.publish_frames(Some(generation), &[]));
        assert!(model.frames_for_details(generation).is_none());
        assert!(Rc::ptr_eq(&painted, &model.frames()));

        let generation = model.start_stop_refresh();
        model.bind_stop_context(7).expect("new frame context");
        model.select_thread("2");
        model.select_thread("1");
        assert!(!model.is_stop_refresh_current(generation));

        let generation = model.start_stop_refresh();
        model.bind_stop_context(7).expect("new thread context");
        model.set_selected_inferior("i2");
        model.set_selected_inferior("i1");
        assert!(!model.is_stop_refresh_current(generation));

        let generation = model.start_stop_refresh();
        model.bind_stop_context(7).expect("new inferior context");
        model.set_controls_ready(false);
        model.set_controls_ready(true);
        assert!(!model.is_stop_refresh_current(generation));
    }

    #[test]
    fn relationship_results_and_refresh_work_are_generation_scoped() {
        let model = stopped_model();
        let first = model.begin_inferior_refresh().expect("initial refresh");
        assert!(model.begin_inferior_refresh().is_none());
        assert!(model.begin_inferior_refresh().is_none());
        assert!(model.finish_inferior_refresh());
        let second = model.begin_inferior_refresh().expect("one queued refresh");
        assert!(
            !model.merge_inferior_relationships(first, HashMap::from([("i2".into(), "i1".into())]))
        );
        assert!(model.inferior_parent("i2").is_none());
        assert!(model.merge_inferior_relationships(
            second,
            HashMap::from([("i2".into(), "i1".into()), ("gone".into(), "i1".into()),])
        ));
        assert_eq!(model.inferior_parent("i2").as_deref(), Some("i1"));
        assert!(model.inferior_parent("gone").is_none());
        assert!(!model.finish_inferior_refresh());
    }
}
