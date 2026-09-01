use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet, VecDeque},
    fmt::Write as _,
    path::{Path, PathBuf},
    rc::{Rc, Weak},
    sync::atomic::{AtomicU64, Ordering},
};

use gtk::prelude::*;

use crate::{
    config::{DebugSession, LaunchConfig},
    debugger::{
        Breakpoint, MemoryKind, MiClient, MiEvent, MiRecord, Register, SessionEvent, StackEntry,
        StackFrame, TargetArchitecture, TargetEndian, ThreadInfo, Variable,
        context::{
            MemoryRegion, annotate_memory_regions, build_stack_entries, is_pointer_register,
            looks_like_string_word, pointer_address, read_memory_regions,
        },
        launch_gdb,
    },
    theme::Theme,
    ui::{
        BreakpointEditRequest, BreakpointSpec, CallAbiTargetRequest, DisassemblyRequest,
        DisassemblySyntax, EventCatchpoint, FilteredCatchpointKind, FilteredCatchpointRequest,
        ForkFollowMode, GefContextControl, HeapInspectionAction, HeapInspectionRequest,
        InferiorAction, InferiorActionPending, SchedulerLockingMode, SessionAction,
        SourceDiscoveryRequest, ThreadAction, ThreadActionPending, ThreadBacktrace,
        ThreadComparison, ThreadComparisonRow, Ui, UntilAction, VariableViewerPlan,
        VariableViewerRequest, VariableViewerRow, VariableViewerSession, WatchpointAccess,
        WatchpointRequest, compact_variable_type,
    },
};

const MAX_POINTER_CHAIN_DEPTH: usize = 3;
const AUTOMATIC_PRINT_ELEMENTS: usize = 128;
const VARIABLE_CHILD_PAGE_SIZE: usize = 128;
const MAX_VARIABLE_CHILDREN: usize = 4096;
const STACK_WORD_COUNT: usize = 32;
const POINTER_STRING_PREVIEW_ELEMENTS: usize = 256;
const POINTER_ENRICHMENT_CONCURRENCY: usize = 4;
const MAX_AUTOMATIC_VARIABLE_OBJECTS: usize = 256;
static NEXT_VARIABLE_OBJECT_ID: AtomicU64 = AtomicU64::new(1);

fn next_variable_object_name() -> String {
    let id = NEXT_VARIABLE_OBJECT_ID.fetch_add(1, Ordering::Relaxed);
    format!("fgdb_var_{}_{id}", std::process::id())
}

struct RegisterRefresh {
    ui: Weak<Ui>,
    generation: u64,
    registers: Vec<Register>,
    pending: VecDeque<usize>,
    active: usize,
    architecture: TargetArchitecture,
    endian: TargetEndian,
    pointer_bits: u32,
}

struct StackRefresh {
    ui: Weak<Ui>,
    generation: u64,
    entries: Vec<StackEntry>,
    stack_register: &'static str,
    pending: VecDeque<usize>,
    active: usize,
    word_size: usize,
    endian: TargetEndian,
}

struct StackInputs {
    ui: Weak<Ui>,
    generation: u64,
    frames: Option<Vec<StackFrame>>,
    registers: Option<Vec<Register>>,
}

struct VariableRefresh {
    ui: Weak<Ui>,
    generation: u64,
    target: VariableRefreshTarget,
    variables: Vec<Variable>,
    fallbacks: Vec<Variable>,
    needs_update: Vec<bool>,
    next_index: usize,
    created: usize,
    created_varobjs: HashSet<String>,
    updates_requested: bool,
    update_index: usize,
    recreate_after_updates: bool,
}

enum VariableRefreshTarget {
    Locals,
    ExpressionWatches(Vec<String>),
}

mod assignments;
mod backend;
mod breakpoints;
mod build;
mod debug_data;
mod disassembly;
use debug_data::handle_debug_data_action;
mod inferiors;
mod kernel;
mod lifecycle;
mod misc;
mod refresh;
mod session;
mod source_control;
mod symbols;
mod threads;
mod type_metadata;
mod until;
mod variable_viewers;
mod watches;

pub use build::build;

#[cfg(test)]
use build::assignment_expression;

use assignments::*;
use backend::*;
use breakpoints::*;
use disassembly::*;
use inferiors::*;
use kernel::*;
use lifecycle::*;
use misc::*;
pub(crate) use refresh::*;
use session::*;
use source_control::*;
use symbols::*;
use threads::*;
use type_metadata::*;
use until::*;
use variable_viewers::*;
use watches::*;

#[cfg(test)]
mod tests {
    use super::{
        assignment_expression, parse_gdb_integer, pointer_expression, register_string_address,
        source_symbol_pattern, stack_pointer_expression, stack_string_address, symbol_annotation,
        vector_assignment_expression,
    };
    use crate::debugger::{MemoryKind, Register, StackEntry, TargetArchitecture, TargetEndian};

    #[test]
    fn builds_pointer_chain_expressions() {
        assert_eq!(pointer_expression("rsp", 0), "(void*)($rsp)");
        assert_eq!(pointer_expression("rsp", 1), "*(void**)($rsp)");
        assert_eq!(pointer_expression("rsp", 2), "*(void**)(*(void**)($rsp))");
        assert_eq!(
            stack_pointer_expression("rsp", 8, 1),
            "*(void**)(*(void**)($rsp+0x8))"
        );
    }

    #[test]
    fn finds_the_pointer_behind_an_inline_stack_string_preview() {
        let mut entry = StackEntry {
            address: 0x7fff_0000,
            offset: 0,
            index: 0,
            pointer_bits: 64,
            endian: TargetEndian::Little,
            value: String::from("0x7fff1000"),
            pointer_chain: vec![
                String::from("0x7fff1000"),
                String::from("0x415242494c5f444c"),
            ],
            address_registers: vec![String::from("rsp")],
            value_registers: Vec::new(),
            return_frame: None,
            memory_kind: MemoryKind::Stack,
            region: Some(String::from("rw-p · [stack]")),
        };
        let word = u64::from_le_bytes(*b"LD_LIBRA");
        assert_eq!(
            stack_string_address(&entry, word, 1, TargetEndian::Little, 8),
            Some(0x7fff_1000)
        );
        entry.memory_kind = MemoryKind::Code;
        assert_eq!(
            stack_string_address(&entry, word, 1, TargetEndian::Little, 8),
            None
        );
    }

    #[test]
    fn finds_the_pointer_behind_an_inline_register_string_preview() {
        let word = u64::from_le_bytes(*b"LD_LIBRA");
        let mut register = Register {
            name: String::from("rsp"),
            value: String::from("0x7fffffffc5f0"),
            pointer_chain: vec![
                String::from("0x7fffffffc5f0"),
                String::from("0x7ffff7feedf6"),
                format!("0x{word:x}"),
            ],
        };
        assert_eq!(
            register_string_address(
                &register,
                word,
                2,
                TargetEndian::Little,
                64,
                TargetArchitecture::X86_64,
            ),
            Some(0x7fff_f7fe_edf6)
        );
        register.name = String::from("rip");
        assert_eq!(
            register_string_address(
                &register,
                word,
                2,
                TargetEndian::Little,
                64,
                TargetArchitecture::X86_64,
            ),
            None
        );
    }

    #[test]
    fn builds_variable_assignment_expressions() {
        assert_eq!(assignment_expression("count", "42"), "count = (42)");
        assert_eq!(
            assignment_expression("message", "\"hello world\""),
            "message = (\"hello world\")"
        );
    }

    #[test]
    fn extracts_addresses_from_symbolic_values() {
        assert_eq!(
            symbol_annotation("0x55555555516f <main+15>"),
            Some("<main+15>")
        );
        assert_eq!(parse_gdb_integer("0x2"), Some(2));
        assert_eq!(parse_gdb_integer("17"), Some(17));
    }

    #[test]
    fn builds_gdb_symbol_search_patterns() {
        assert_eq!(source_symbol_pattern("mmap"), "mmap");
        assert_eq!(source_symbol_pattern("Vec::new"), "Vec.*::new");
        assert_eq!(source_symbol_pattern("foo.bar+1"), r"foo\.bar\+1");
    }

    #[test]
    fn builds_typed_vector_lane_assignments() {
        assert_eq!(
            vector_assignment_expression(
                "ymm0",
                "v8_float",
                &[(0, String::from("1.5")), (7, String::from("-2.0"))],
            )
            .as_deref(),
            Some("($ymm0.v8_float[0] = (1.5), $ymm0.v8_float[7] = (-2.0))")
        );
        assert_eq!(vector_assignment_expression("xmm0", "v2_int64", &[]), None);
    }
}
