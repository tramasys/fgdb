use crate::debugger::{Breakpoint, StackFrame, ThreadInfo};

pub(crate) use crate::model::actions::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ThreadBacktrace {
    pub(crate) thread: ThreadInfo,
    pub(crate) frames: Vec<StackFrame>,
    pub(crate) error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ThreadComparisonRow {
    pub(crate) item: String,
    pub(crate) left: String,
    pub(crate) right: String,
    pub(crate) different: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ThreadComparison {
    pub(crate) left: ThreadInfo,
    pub(crate) right: ThreadInfo,
    pub(crate) frames: Vec<ThreadComparisonRow>,
    pub(crate) registers: Vec<ThreadComparisonRow>,
    pub(crate) warnings: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum UntilAction {
    CurrentLine,
    FunctionReturns,
    NextCall,
    NextReturn,
    NextSyscall,
    NextIndirectBranch,
    NextControlFlow,
    MemoryAccess,
    UserCode,
    LibcCode,
    RegionChange,
    Expression(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HeapInspectionAction {
    Arenas,
    Arena,
    Top,
    Chunks,
    Parsed,
    CompactBins,
    AllBins,
    TcacheBins,
    FastBins,
    UnsortedBin,
    SmallBins,
    LargeBins,
    Chunk,
    Backend,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HeapInspectionRequest {
    pub action: HeapInspectionAction,
    pub expression: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum GefContextControl {
    #[default]
    None,
    ContextCommand,
    OriginalGef,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DisassemblySyntax {
    Intel,
    Att,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DisassemblyRequest {
    Stopped {
        pc: String,
        architecture: Option<String>,
    },
    Clear,
    Navigate(String),
    Back,
    Forward,
    PreviousFunction,
    NextFunction,
    Mixed(bool),
    Syntax(DisassemblySyntax),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BreakpointSpec {
    pub location: String,
    pub regex: bool,
    pub hardware: bool,
    pub enabled: bool,
    pub temporary: bool,
    pub allow_pending: bool,
    pub condition: Option<String>,
    pub stop_after: u64,
    pub thread: Option<String>,
    pub inferior: Option<String>,
    pub commands: Vec<String>,
    pub logpoint: bool,
}

impl BreakpointSpec {
    pub(super) fn from_breakpoint(breakpoint: &Breakpoint) -> Self {
        let logpoint = breakpoint.is_logpoint();
        let mut commands = breakpoint.commands.clone();

        if logpoint {
            if commands.first().is_some_and(|command| command == "silent") {
                commands.remove(0);
            }

            if commands
                .last()
                .is_some_and(|command| matches!(command.trim(), "continue" | "cont" | "c"))
            {
                commands.pop();
            }
        }

        Self {
            location: breakpoint
                .original_location
                .clone()
                .or_else(|| breakpoint.address.clone())
                .unwrap_or_default(),
            regex: false,
            hardware: breakpoint.is_hardware_breakpoint(),
            enabled: breakpoint.enabled,
            temporary: breakpoint.disposition.as_deref() == Some("del"),
            allow_pending: breakpoint.pending.is_some(),
            condition: breakpoint.condition.clone(),
            stop_after: breakpoint.ignore_count.saturating_add(1),
            thread: breakpoint.thread.clone(),
            inferior: breakpoint.inferior.clone(),
            commands,
            logpoint,
        }
    }

    pub(crate) fn effective_commands(&self) -> Vec<String> {
        if !self.logpoint {
            return self.commands.clone();
        }

        let mut commands = Vec::with_capacity(self.commands.len() + 2);

        if !self
            .commands
            .first()
            .is_some_and(|command| command == "silent")
        {
            commands.push(String::from("silent"));
        }

        commands.extend(self.commands.iter().cloned());

        if !commands
            .last()
            .is_some_and(|command| matches!(command.trim(), "continue" | "cont" | "c"))
        {
            commands.push(String::from("continue"));
        }

        commands
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BreakpointEditRequest {
    pub original: Option<Breakpoint>,
    pub spec: BreakpointSpec,
}
