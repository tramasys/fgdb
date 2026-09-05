mod command;
pub(crate) mod context;
mod mi;
mod model;
mod session;
mod target;

pub(crate) use command::{CliCommandBuilder, MiCommandBuilder, console_command, gdb_cli_string};
pub(crate) use context::StopContext;
pub(crate) use mi::StopRequests;
#[cfg(test)]
pub(crate) use mi::parse_record;
pub use mi::{GdbCapabilities, MiClient, MiEvent, MiRecord, quote};
pub use model::{
    Breakpoint, EnumVariant, InferiorInfo, InferiorState, Instruction, MemoryBlock, MemoryKind,
    Register, SharedLibrary, SourceFile, SourceLocation, StackEntry, StackFrame, ThreadInfo,
    ValueTypeKind, ValueTypeMetadata, Variable, VariableUpdate, breakpoints,
    compact_register_numbers, compare_thread_ids, current_frame, current_source, evaluated_value,
    executable_source_lines, has_exact_command_completion, inferior_pid, inferior_pid_for_group,
    inferiors, inserted_breakpoints, instructions, memory_block, register_names, registers,
    shared_libraries, source_files, source_locations, stack_frames, thread_group_argument,
    thread_id_argument, threads, variable_children, variable_children_have_more, variable_object,
    variable_path_expression, variable_updates, variables,
};
pub use session::{SessionEvent, launch_gdb};
pub use target::{TargetArchitecture, TargetEndian};
