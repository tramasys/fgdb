use std::{borrow::Cow, collections::HashMap};

use super::DebuggerModel;

pub(crate) const MAX_CHECKPOINTS: usize = 64;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ExecutionDirection {
    #[default]
    Forward,
    Reverse,
}

impl ExecutionDirection {
    pub(crate) fn from_gdb_output(output: &str) -> Option<Self> {
        output.lines().rev().find_map(|line| match line.trim() {
            "Forward." => Some(Self::Forward),
            "Reverse." => Some(Self::Reverse),
            _ => None,
        })
    }

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Forward => "Forward",
            Self::Reverse => "Reverse",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RecordingMethod {
    Full,
    BranchTrace,
    ProcessorTrace,
    BranchStore,
}

impl RecordingMethod {
    pub(crate) const fn command(self) -> &'static str {
        match self {
            Self::Full => "record full",
            Self::BranchTrace => "record btrace",
            Self::ProcessorTrace => "record btrace pt",
            Self::BranchStore => "record btrace bts",
        }
    }

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Full => "Full recording",
            Self::BranchTrace => "Branch tracing",
            Self::ProcessorTrace => "Intel PT",
            Self::BranchStore => "Intel BTS",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Checkpoint {
    pub id: String,
    pub description: String,
    pub active: bool,
}

#[derive(Default)]
pub(crate) struct ReplayState {
    pub direction: ExecutionDirection,
    pub backend_direction: ExecutionDirection,
    pub reverse_supported: bool,
    pub reverse_owner: Option<String>,
    pub recordings: HashMap<String, RecordingMethod>,
    pub capabilities: ReplayCapabilities,
    pub queried_target: Option<(u64, String)>,
    pub query_generation: u64,
    pub querying: bool,
    pub refresh_requested: bool,
    pub operation_generation: u64,
    pub checkpoints: Vec<Checkpoint>,
    pub checkpoint_owner: Option<String>,
    pub checkpoint_query_complete: bool,
    pub checkpoint_list_capped: bool,
    pub details: String,
}

impl ReplayState {
    pub(crate) fn invalidate_query(&mut self) {
        self.query_generation = self.query_generation.wrapping_add(1);
        self.querying = false;
        self.refresh_requested = false;
        self.queried_target = None;
    }

    pub(crate) fn recording_stopped(&mut self, group: &str, selected: bool) {
        self.recordings.remove(group);

        if selected {
            self.direction = ExecutionDirection::Forward;
        }
    }

    pub(crate) fn set_backend_direction(&mut self, direction: ExecutionDirection) {
        self.backend_direction = direction;
        self.direction = direction;
    }
}

#[derive(Clone, Copy, Default)]
pub(crate) struct ReplayCapabilities {
    pub full_recording: Option<bool>,
    pub branch_tracing: Option<bool>,
    pub checkpoints: Option<bool>,
}

impl ReplayCapabilities {
    pub(crate) fn supports_recording(self, method: RecordingMethod) -> bool {
        let supported = match method {
            RecordingMethod::Full => self.full_recording,
            _ => self.branch_tracing,
        };

        supported == Some(true)
    }
}

#[derive(Clone, Debug)]
pub(crate) enum ReplayAction {
    Refresh,
    Direction(ExecutionDirection),
    Start(RecordingMethod, u32, u32),
    Stop,
    CreateCheckpoint,
    RestoreCheckpoint(String),
    DeleteCheckpoint(String),
    RestartReplay,
}

impl DebuggerModel {
    pub(crate) fn checkpoint_target_available(&self) -> bool {
        if matches!(
            self.session().as_ref(),
            Some(crate::config::DebugSession::RrReplay { .. })
        ) {
            return self.target_connection() == super::TargetConnection::Remote;
        }

        self.target_connection() == super::TargetConnection::Local
            && self.non_stop_mode() == Some(false)
            && self.threads().len() == 1
            && self.recording_method().is_none()
    }

    pub(crate) fn recording_method(&self) -> Option<RecordingMethod> {
        self.selected_inferior_id()
            .and_then(|id| self.replay.borrow().recordings.get(&id).copied())
    }

    pub(crate) fn reverse_available(&self) -> bool {
        let Some(group) = self.selected_inferior_id() else {
            return false;
        };

        let state = self.replay.borrow();

        (state.reverse_supported && state.reverse_owner.as_deref() == Some(&group))
            || state.recordings.contains_key(&group)
    }

    pub(crate) fn execution_direction(&self) -> ExecutionDirection {
        self.replay.borrow().direction
    }

    pub(crate) fn directional_command<'a>(
        &self,
        command: &'a str,
    ) -> Result<Cow<'a, str>, &'static str> {
        if self.execution_direction() == ExecutionDirection::Forward {
            if self.replay.borrow().backend_direction == ExecutionDirection::Reverse {
                return Err("Select Forward to reset GDB's execution direction before continuing");
            }

            return Ok(Cow::Borrowed(command));
        }

        if !self.reverse_available() {
            return Err("The selected target does not currently support reverse execution");
        }

        match command.split_whitespace().next() {
            Some(
                "-exec-continue"
                | "-exec-next"
                | "-exec-step"
                | "-exec-next-instruction"
                | "-exec-step-instruction"
                | "-exec-finish",
            ) => {
                // MI rejects --reverse when a terminal command already put GDB
                // in reverse mode. The UI normally leaves GDB in forward mode.
                if self.replay.borrow().backend_direction == ExecutionDirection::Reverse {
                    Ok(Cow::Borrowed(command))
                } else {
                    Ok(Cow::Owned(format!("{command} --reverse")))
                }
            }
            _ => Err("This action is only available in the forward direction"),
        }
    }

    pub(crate) fn reset_replay(&self) {
        let (generation, operation_generation, backend_direction) = {
            let state = self.replay.borrow();
            (
                state.query_generation.wrapping_add(1),
                state.operation_generation.wrapping_add(1),
                state.backend_direction,
            )
        };

        self.replay.replace(ReplayState {
            query_generation: generation,
            operation_generation,
            backend_direction,
            ..ReplayState::default()
        });
    }

    pub(crate) fn invalidate_replay_target(&self, group: &str) {
        let mut state = self.replay.borrow_mut();
        state.recordings.remove(group);
        state.invalidate_query();

        if self.selected_inferior_id().as_deref() == Some(group) {
            state.direction = ExecutionDirection::Forward;
        }

        if state.reverse_owner.as_deref() == Some(group) {
            state.reverse_supported = false;
            state.reverse_owner = None;
        }

        if state.checkpoint_owner.as_deref() == Some(group) {
            state.checkpoints.clear();
            state.checkpoint_owner = None;
            state.checkpoint_query_complete = false;
            state.checkpoint_list_capped = false;
        }
    }
}

pub(crate) fn checkpoint_id_valid(id: &str) -> bool {
    let mut parts = id.split('.');
    let number = |part: &str| {
        !part.is_empty()
            && part.bytes().all(|byte| byte.is_ascii_digit())
            && part.parse::<u32>().is_ok()
    };

    parts.next().is_some_and(number) && parts.next().is_none_or(number) && parts.next().is_none()
}

pub(crate) fn parse_checkpoints(output: &str) -> (Vec<Checkpoint>, bool) {
    let mut rows = Vec::new();

    for line in output.lines() {
        let line = line.trim();
        let active = line.starts_with('*');
        let line = line.trim_start_matches('*').trim_start();
        let Some((id, description)) = line.split_once(char::is_whitespace) else {
            continue;
        };

        if !checkpoint_id_valid(id) || rows.iter().any(|row: &Checkpoint| row.id == id) {
            continue;
        }

        if rows.len() == MAX_CHECKPOINTS {
            return (rows, true);
        }

        let description = description.trim();
        let active = active || description.split_whitespace().next() == Some("y");
        let description = description
            .split_once(char::is_whitespace)
            .filter(|(first, _)| matches!(*first, "y" | "n"))
            .map_or(description, |(_, rest)| rest.trim());

        rows.push(Checkpoint {
            id: id.to_owned(),
            description: description.trim().to_owned(),
            active,
        });
    }

    (rows, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_invalidation_retires_queries_without_abandoning_restore_operations() {
        let model = DebuggerModel::new(None);
        model
            .processes
            .selected_inferior_id
            .replace(Some(String::from("i1")));

        {
            let mut state = model.replay.borrow_mut();
            state.direction = ExecutionDirection::Reverse;
            state.querying = true;
            state.refresh_requested = true;
            state.queried_target = Some((1, String::from("i1")));
            state.operation_generation = 7;
        }

        model.invalidate_replay_target("i1");
        let state = model.replay.borrow();
        assert_eq!(state.direction, ExecutionDirection::Forward);
        assert_eq!(state.query_generation, 1);
        assert_eq!(state.operation_generation, 7);
        assert!(!state.querying);
        assert!(!state.refresh_requested);
        assert!(state.queried_target.is_none());
        drop(state);
        model.reset_replay();
        assert_eq!(model.replay.borrow().operation_generation, 8);
    }

    #[test]
    fn stopping_recording_resets_only_selected_direction_and_requires_backend_confirmation() {
        assert_eq!(
            ExecutionDirection::from_gdb_output("Reverse.\n"),
            Some(ExecutionDirection::Reverse)
        );
        assert_eq!(
            ExecutionDirection::from_gdb_output("Forward.\n"),
            Some(ExecutionDirection::Forward)
        );
        assert_eq!(
            ExecutionDirection::from_gdb_output("Target does not support this operation."),
            None
        );

        let model = DebuggerModel::new(None);
        let mut state = model.replay.borrow_mut();
        state.set_backend_direction(ExecutionDirection::Reverse);
        state
            .recordings
            .insert(String::from("i1"), RecordingMethod::Full);

        state
            .recordings
            .insert(String::from("i2"), RecordingMethod::Full);

        state.recording_stopped("i2", false);
        assert_eq!(state.direction, ExecutionDirection::Reverse);
        assert!(!state.recordings.contains_key("i2"));
        state.recording_stopped("i1", true);
        assert_eq!(state.direction, ExecutionDirection::Forward);
        assert!(!state.recordings.contains_key("i1"));
        drop(state);
        assert!(model.directional_command("-exec-next").is_err());
        let mut state = model.replay.borrow_mut();
        state.set_backend_direction(ExecutionDirection::Forward);
        state.direction = ExecutionDirection::Reverse;
        state.recording_stopped("i1", true);
        assert_eq!(state.direction, ExecutionDirection::Forward);
        drop(state);
        assert!(matches!(
            model.directional_command("-exec-next"),
            Ok(Cow::Borrowed("-exec-next"))
        ));
    }

    #[test]
    fn checkpoint_identifiers_and_lists_are_bounded() {
        let (rows, capped) = parse_checkpoints(
            "  Id Active Target Id Frame\n* 1.0 y process 42 0x100 main\n  1.2 n process 43 0x120 worker\n",
        );
        assert_eq!(rows.len(), 2);
        assert!(rows[0].active);
        assert!(!rows[1].active);
        assert!(!capped);

        for invalid in ["", "1.2.3", "-1", "1\nquit", "1;quit", "1.", "4294967296"] {
            assert!(!checkpoint_id_valid(invalid));
        }

        let output = (0..100)
            .map(|id| format!("{id} n process 42\n"))
            .collect::<String>();
        let (rows, capped) = parse_checkpoints(&output);
        assert_eq!(rows.len(), MAX_CHECKPOINTS);
        assert!(capped);
    }

    #[test]
    fn reverse_commands_require_capability_and_do_not_affect_forward_only_actions() {
        let model = DebuggerModel::new(None);
        assert!(matches!(
            model.directional_command("-exec-next"),
            Ok(Cow::Borrowed(_))
        ));
        model.replay.borrow_mut().direction = ExecutionDirection::Reverse;
        assert!(model.directional_command("-exec-next").is_err());
        model
            .processes
            .selected_inferior_id
            .replace(Some(String::from("i1")));
        model.replay.borrow_mut().reverse_supported = true;
        model.replay.borrow_mut().reverse_owner = Some(String::from("i1"));
        assert_eq!(
            model.directional_command("-exec-next --thread 2").unwrap(),
            "-exec-next --thread 2 --reverse"
        );
        assert!(model.directional_command("-exec-until").is_err());
        assert!(model.directional_command("-exec-run").is_err());
        model.replay.borrow_mut().backend_direction = ExecutionDirection::Reverse;
        assert_eq!(
            model.directional_command("-exec-next --thread 2").unwrap(),
            "-exec-next --thread 2"
        );

        model.replay.borrow_mut().direction = ExecutionDirection::Forward;
        assert!(model.directional_command("-exec-next").is_err());
        model.replay.borrow_mut().backend_direction = ExecutionDirection::Forward;
        model
            .processes
            .selected_inferior_id
            .replace(Some(String::from("i2")));
        assert!(!model.reverse_available());
        model
            .processes
            .selected_inferior_id
            .replace(Some(String::from("i1")));
        model.invalidate_replay_target("i1");
        assert!(!model.reverse_available());
        model.reset_replay();
        assert!(!model.reverse_available());
        assert_eq!(model.execution_direction(), ExecutionDirection::Forward);
    }
}
