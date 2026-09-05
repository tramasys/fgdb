#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionAction {
    Restart,
    Kill,
    Detach,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ForkFollowMode {
    Parent,
    Child,
}

impl ForkFollowMode {
    pub(crate) const fn gdb_value(self) -> &'static str {
        match self {
            Self::Parent => "parent",
            Self::Child => "child",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum InferiorAction {
    Select(String),
    Resume(String),
    Interrupt(String),
    SetFollowFork(ForkFollowMode),
    SetDetachOnFork(bool),
    Refresh,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InferiorActionPending {
    Selection,
    Execution,
    Setting,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SchedulerLockingMode {
    Off,
    On,
    Step,
    Replay,
}

impl SchedulerLockingMode {
    pub(crate) const fn gdb_value(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::On => "on",
            Self::Step => "step",
            Self::Replay => "replay",
        }
    }

    pub(crate) const fn index(self) -> u32 {
        match self {
            Self::Off => 0,
            Self::On => 1,
            Self::Step => 2,
            Self::Replay => 3,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ThreadAction {
    Refresh,
    SetSchedulerLocking(SchedulerLockingMode),
    SetNonStop(bool),
    RunOnly(String),
    Freeze(String),
    Thaw(String),
    Backtraces {
        generation: u64,
    },
    Compare {
        generation: u64,
        left: String,
        right: String,
    },
    SelectFrame {
        thread: String,
        frame: u32,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ThreadActionPending {
    Setting,
    Execution,
    Analysis,
    Selection,
}
