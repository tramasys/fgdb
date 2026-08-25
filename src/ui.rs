use std::{
    cell::{Cell, RefCell},
    cmp::Reverse,
    collections::HashMap,
    path::{Path, PathBuf},
    rc::Rc,
};

use gtk::{gio, glib, pango, prelude::*};
use sourceview5::prelude::*;
use vte4::prelude::*;

mod layout;
mod value;

use value::{architecture_pointer_bits, integer_decimal_value};

use crate::{
    breakpoint_gutter::{BreakpointGutterRenderer, LineStyle},
    config::LaunchConfig,
    debugger::{
        Breakpoint, Instruction, MemoryBlock, MemoryKind, MiClient, Register, SharedLibrary,
        SourceFile, SourceLocation, StackEntry, StackFrame, ThreadInfo, Variable,
        context::MemoryRegion,
    },
    source,
    theme::Theme,
};

const EXECUTION_CATEGORY: &str = "execution";
const GENERAL_REGISTERS: &[&str] = &[
    "rax", "rbx", "rcx", "rdx", "rsp", "rbp", "rsi", "rdi", "rip", "r8", "r9", "r10", "r11", "r12",
    "r13", "r14", "r15", "eax", "ebx", "ecx", "edx", "esp", "ebp", "esi", "edi", "eip",
];
const BASE_REGISTERS: &[&str] = &["fs_base", "gs_base"];
const FLAG_REGISTERS: &[&str] = &["eflags", "rflags", "cpsr"];
const SEGMENT_REGISTERS: &[&str] = &["cs", "ss", "ds", "es", "fs", "gs"];
const FLAGS: &[(u8, &str)] = &[
    (21, "ident"),
    (18, "align"),
    (17, "vx86"),
    (16, "resume"),
    (14, "nested"),
    (11, "overflow"),
    (10, "direction"),
    (9, "interrupt"),
    (8, "trap"),
    (7, "sign"),
    (6, "zero"),
    (4, "adjust"),
    (2, "parity"),
    (0, "carry"),
];
const COMMON_SIGNALS: &[(&str, &str)] = &[
    ("SIGSEGV", "Invalid memory reference"),
    ("SIGABRT", "Process abort"),
    ("SIGBUS", "Bus or alignment error"),
    ("SIGILL", "Illegal instruction"),
    ("SIGFPE", "Arithmetic exception"),
    ("SIGTRAP", "Trace or breakpoint trap"),
    ("SIGINT", "Interactive interrupt"),
    ("SIGTERM", "Termination request"),
    ("SIGPIPE", "Write to a closed pipe"),
    ("SIGCHLD", "Child process state changed"),
    ("SIGUSR1", "Application-defined signal 1"),
    ("SIGUSR2", "Application-defined signal 2"),
];
const MORE_SIGNALS: &[(&str, &str)] = &[
    ("SIGHUP", "Terminal hangup"),
    ("SIGQUIT", "Quit request"),
    ("SIGALRM", "Real-time timer expired"),
    ("SIGSYS", "Bad system call"),
    ("SIGXCPU", "CPU time limit exceeded"),
    ("SIGXFSZ", "File size limit exceeded"),
    ("SIGVTALRM", "Virtual timer expired"),
    ("SIGPROF", "Profiling timer expired"),
    ("SIGWINCH", "Terminal window size changed"),
    ("SIGCONT", "Process continued"),
    ("SIGTSTP", "Terminal stop request"),
    ("all", "Every signal GDB can catch"),
];
type FrameSelectionHandler = Rc<dyn Fn(u32)>;
type StringSelectionHandler = Rc<dyn Fn(String)>;
type VariableAssignmentHandler = Rc<dyn Fn(Variable, String)>;
type VariableChildrenHandler = Rc<dyn Fn(Variable, usize)>;
type VectorAssignmentHandler = Rc<dyn Fn(String, String, Vec<(usize, String)>)>;
type BreakpointConditionHandler = Rc<dyn Fn(String, Option<String>)>;
type BreakpointEnabledHandler = Rc<dyn Fn(String, bool)>;
type BreakpointBulkDeleteHandler = Rc<dyn Fn(Vec<String>)>;
type BreakpointInsertHandler = Rc<dyn Fn(PathBuf, u32)>;
type SignalCatchpointHandler = Rc<dyn Fn(String, Option<String>)>;
type EventCatchpointHandler = Rc<dyn Fn(EventCatchpoint, Option<String>)>;
type WatchpointInsertHandler = Rc<dyn Fn(String, WatchpointAccess)>;
type MemoryWatchHandler = Rc<dyn Fn(u64, String, usize)>;
type InstructionMemoryHandler = Rc<dyn Fn(String)>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EventCatchpoint {
    CxxThrow,
    CxxCatch,
    RustPanic,
    Exec,
    Fork,
    Syscall,
}

impl EventCatchpoint {
    const ALL: [(Self, &'static str, &'static str); 6] = [
        (
            Self::CxxThrow,
            "C++ throw",
            "Stop when a C++ exception is thrown",
        ),
        (
            Self::CxxCatch,
            "C++ catch",
            "Stop when a C++ exception is caught",
        ),
        (
            Self::RustPanic,
            "Rust panic",
            "Stop at Rust's panic runtime entry point",
        ),
        (Self::Exec, "exec", "Stop when the inferior calls exec"),
        (Self::Fork, "fork", "Stop when the inferior forks"),
        (
            Self::Syscall,
            "syscall",
            "Stop at every system call; this can trigger very frequently",
        ),
    ];

    pub(crate) const fn command(self) -> &'static str {
        match self {
            Self::CxxThrow => "catch throw",
            Self::CxxCatch => "catch catch",
            Self::RustPanic => "break rust_panic",
            Self::Exec => "catch exec",
            Self::Fork => "catch fork",
            Self::Syscall => "catch syscall",
        }
    }

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::CxxThrow => "C++ throw",
            Self::CxxCatch => "C++ catch",
            Self::RustPanic => "Rust panic",
            Self::Exec => "exec",
            Self::Fork => "fork",
            Self::Syscall => "syscall",
        }
    }

    fn matches(self, breakpoint: &Breakpoint) -> bool {
        match self {
            Self::CxxThrow => breakpoint.catch_type.as_deref() == Some("throw"),
            Self::CxxCatch => breakpoint.catch_type.as_deref() == Some("catch"),
            Self::RustPanic => {
                !breakpoint.is_catchpoint()
                    && breakpoint
                        .original_location
                        .as_deref()
                        .or(breakpoint.function.as_deref())
                        .is_some_and(|name| name == "rust_panic")
            }
            Self::Exec => breakpoint.catch_type.as_deref() == Some("exec"),
            Self::Fork => breakpoint.catch_type.as_deref() == Some("fork"),
            Self::Syscall => breakpoint.catch_type.as_deref() == Some("syscall"),
        }
    }
}

#[derive(Clone)]
struct InstructionRowData {
    instruction: Instruction,
    current: bool,
}

#[derive(Clone)]
struct RegisterRowData {
    register: Register,
    changed: bool,
    ring: Option<u64>,
}

#[derive(Clone)]
struct VariableNode {
    variable: Variable,
    children: gio::ListStore,
    children_loaded: Rc<Cell<bool>>,
    children_loading: Rc<Cell<bool>>,
    expansion_observer_attached: Rc<Cell<bool>>,
    load_more: Option<(Variable, usize)>,
    placeholder: bool,
}

impl VariableNode {
    fn new(variable: Variable) -> Self {
        Self {
            variable,
            children: gio::ListStore::new::<glib::BoxedAnyObject>(),
            children_loaded: Rc::new(Cell::new(false)),
            children_loading: Rc::new(Cell::new(false)),
            expansion_observer_attached: Rc::new(Cell::new(false)),
            load_more: None,
            placeholder: false,
        }
    }

    fn placeholder(name: &str, value: &str) -> Self {
        Self {
            variable: Variable {
                name: name.to_owned(),
                value: value.to_owned(),
                type_name: None,
                varobj: None,
                num_children: 0,
                has_more: false,
            },
            children: gio::ListStore::new::<glib::BoxedAnyObject>(),
            children_loaded: Rc::new(Cell::new(true)),
            children_loading: Rc::new(Cell::new(false)),
            expansion_observer_attached: Rc::new(Cell::new(true)),
            load_more: None,
            placeholder: true,
        }
    }

    fn load_more(parent: Variable, next: usize) -> Self {
        let remaining = parent.num_children.saturating_sub(next);
        let detail = if remaining == 0 {
            String::from("more children are available")
        } else {
            format!(
                "{remaining} child{} remaining",
                if remaining == 1 { "" } else { "ren" }
            )
        };
        Self {
            variable: Variable {
                name: String::from("Load more…"),
                value: detail,
                type_name: None,
                varobj: None,
                num_children: 0,
                has_more: false,
            },
            children: gio::ListStore::new::<glib::BoxedAnyObject>(),
            children_loaded: Rc::new(Cell::new(true)),
            children_loading: Rc::new(Cell::new(false)),
            expansion_observer_attached: Rc::new(Cell::new(true)),
            load_more: Some((parent, next)),
            placeholder: true,
        }
    }
}

#[derive(Clone)]
struct RegisterGroupView {
    kind: RegisterGroupKind,
    store: gio::ListStore,
    view: gtk::ColumnView,
    panel: gtk::Box,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RegisterGroupKind {
    General,
    Bases,
    Flags,
    Segments,
    Vector,
    FloatingPoint,
    Other,
}

#[derive(Clone)]
struct SourceDocument {
    path: PathBuf,
    buffer: sourceview5::Buffer,
    view: sourceview5::View,
    page: gtk::ScrolledWindow,
    tab: gtk::Box,
    tab_label: gtk::Label,
    breakpoint_renderer: BreakpointGutterRenderer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MemoryWatchFormat {
    Bytes,
    Words,
    Pointers,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WatchpointAccess {
    Write,
    Read,
    Access,
}

impl WatchpointAccess {
    pub(crate) fn mi_option(self) -> &'static str {
        match self {
            Self::Write => "",
            Self::Read => "-r",
            Self::Access => "-a",
        }
    }
}

#[derive(Clone)]
struct MemoryWatchView {
    id: u64,
    expression: String,
    byte_count: usize,
    format: MemoryWatchFormat,
    status: gtk::Label,
    output_addresses: gtk::Label,
    output_values: gtk::Label,
    output_decoded: gtk::Label,
}

#[derive(Clone)]
struct StackWordInspector {
    root: gtk::Box,
    address: gtk::Label,
    raw: gtk::Label,
    interpretation: gtk::Label,
    role: gtk::Label,
    region: gtk::Label,
}

#[derive(Debug, PartialEq, Eq)]
struct MemoryWatchText {
    addresses: String,
    values: String,
    decoded: String,
}

impl MemoryWatchFormat {
    const fn label(self) -> &'static str {
        match self {
            Self::Bytes => "BYTE VIEW",
            Self::Words => "32-BIT WORDS",
            Self::Pointers => "POINTERS",
        }
    }
}

#[derive(Clone, Copy)]
struct SourceOpenContext<'a> {
    notebook: &'a gtk::Notebook,
    documents: &'a Rc<RefCell<Vec<SourceDocument>>>,
    theme: &'a Theme,
    style_scheme: Option<&'a sourceview5::StyleScheme>,
    breakpoints: &'a Rc<RefCell<Vec<Breakpoint>>>,
    insert_handler: &'a Rc<RefCell<Option<BreakpointInsertHandler>>>,
    delete_handler: &'a Rc<RefCell<Option<StringSelectionHandler>>>,
    enabled_handler: &'a Rc<RefCell<Option<BreakpointEnabledHandler>>>,
    symbol_handler: &'a Rc<RefCell<Option<StringSelectionHandler>>>,
}

#[derive(Clone, Copy)]
enum RegisterColumn {
    Name,
    Value,
    Details,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LocalColumn {
    Type,
    Value,
    Details,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VectorLaneFormat {
    Int8,
    Int16,
    Int32,
    Int64,
    Float32,
    Float64,
}

impl VectorLaneFormat {
    const ALL: [Self; 6] = [
        Self::Int8,
        Self::Int16,
        Self::Int32,
        Self::Int64,
        Self::Float32,
        Self::Float64,
    ];

    fn from_index(index: u32) -> Self {
        Self::ALL
            .get(index as usize)
            .copied()
            .unwrap_or(Self::Int64)
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Int8 => "8-bit integers",
            Self::Int16 => "16-bit integers",
            Self::Int32 => "32-bit integers",
            Self::Int64 => "64-bit integers",
            Self::Float32 => "32-bit floats",
            Self::Float64 => "64-bit floats",
        }
    }

    const fn lane_bytes(self) -> usize {
        match self {
            Self::Int8 => 1,
            Self::Int16 => 2,
            Self::Int32 | Self::Float32 => 4,
            Self::Int64 | Self::Float64 => 8,
        }
    }

    fn field(self, register_bytes: usize) -> String {
        let lane_count = register_bytes / self.lane_bytes();
        match self {
            Self::Float32 => format!("v{lane_count}_float"),
            Self::Float64 => format!("v{lane_count}_double"),
            Self::Int8 => format!("v{lane_count}_int8"),
            Self::Int16 => format!("v{lane_count}_int16"),
            Self::Int32 => format!("v{lane_count}_int32"),
            Self::Int64 => format!("v{lane_count}_int64"),
        }
    }

    const fn is_float(self) -> bool {
        matches!(self, Self::Float32 | Self::Float64)
    }
}

#[derive(Clone, Copy)]
enum StackColumn {
    Anchor,
    Address,
    Value,
    Offset,
    Index,
    References,
    Region,
}

#[derive(Clone, Copy)]
enum MemoryColumn {
    Start,
    End,
    Size,
    Permissions,
    Path,
}
const INITIAL_SOURCE: &str = r#"// fgdb is connected to a real GDB terminal.
//
// Source opens automatically at the first source-backed stop.
// You can also use “Open source” to keep several files in tabs.
//
// F5        run / continue       F6        pause
// F10       step over            F11       step into
// Ctrl+F10  next instruction     Ctrl+F11  step instruction
// Shift+F11 finish function
//
// Ctrl+hover underlines navigable symbols; Ctrl+click opens definitions.
// Double-click an instruction to toggle an address breakpoint.
"#;

#[derive(Clone)]
pub struct Ui {
    pub window: gtk::ApplicationWindow,
    pub terminal: vte4::Terminal,
    pub open_source_button: gtk::Button,
    pub load_symbols_button: gtk::Button,
    pub run_button: gtk::Button,
    pub pause_button: gtk::Button,
    pub next_button: gtk::Button,
    pub step_button: gtk::Button,
    pub next_instruction_button: gtk::Button,
    pub step_instruction_button: gtk::Button,
    pub finish_button: gtk::Button,
    pub until_button: gtk::ToggleButton,
    until_popover: gtk::Popover,
    pub gef_tools_button: gtk::ToggleButton,
    pub status_label: gtk::Label,
    pub status_detail: gtk::Label,
    debug_state_panels: Vec<gtk::Widget>,
    source_notebook: gtk::Notebook,
    source_documents: Rc<RefCell<Vec<SourceDocument>>>,
    source_theme: Theme,
    source_style_scheme: Option<sourceview5::StyleScheme>,
    resolved_source_paths: Rc<RefCell<HashMap<String, PathBuf>>>,
    call_stack_list: gtk::Box,
    frame_buttons: Rc<RefCell<Vec<(u32, gtk::Button)>>>,
    selected_frame_level: Rc<Cell<u32>>,
    threads_list: gtk::Box,
    modules_list: gtk::Box,
    locals_store: gio::ListStore,
    locals_selection: gtk::SingleSelection,
    locals_view: gtk::ColumnView,
    locals_empty: gtk::Label,
    locals_edit_button: gtk::Button,
    target_pointer_bits: Rc<Cell<u32>>,
    instructions_title: gtk::Label,
    instructions_store: gio::ListStore,
    instructions_selection: gtk::SingleSelection,
    instructions_view: gtk::ColumnView,
    instructions_empty: gtk::Label,
    instruction_flow: gtk::Label,
    instruction_arguments: gtk::Label,
    instruction_memory: gtk::Label,
    current_instruction: Rc<RefCell<Option<Instruction>>>,
    current_instruction_memory_expression: Rc<RefCell<Option<String>>>,
    latest_registers: Rc<RefCell<Vec<Register>>>,
    instruction_memory_handler: Rc<RefCell<Option<InstructionMemoryHandler>>>,
    register_groups: Vec<RegisterGroupView>,
    registers_empty: gtk::Label,
    stack_store: gio::ListStore,
    stack_empty: gtk::Label,
    breakpoints_list: gtk::Box,
    delete_all_breakpoints_button: gtk::Button,
    delete_all_watchpoints_button: gtk::Button,
    delete_all_catchpoints_button: gtk::Button,
    event_catchpoint_buttons: Vec<(gtk::Button, EventCatchpoint)>,
    watchpoint_expression: gtk::Entry,
    watchpoint_access: gtk::DropDown,
    watchpoint_add_button: gtk::Button,
    signal_detail: gtk::Label,
    signal_buttons: Vec<(gtk::Button, &'static str, &'static str)>,
    signal_entry: gtk::Entry,
    signal_add_button: gtk::Button,
    delete_all_signal_catchpoints_button: gtk::Button,
    until_actions: Vec<(gtk::Button, &'static str)>,
    until_condition_entry: gtk::Entry,
    until_condition_button: gtk::Button,
    memory_region_store: gio::ListStore,
    memory_regions_empty: gtk::Label,
    memory_regions: Rc<RefCell<Vec<MemoryRegion>>>,
    memory_watches: Rc<RefCell<Vec<MemoryWatchView>>>,
    memory_watch_list: gtk::Box,
    memory_watches_empty: gtk::Label,
    memory_address_entry: gtk::Entry,
    memory_size: gtk::SpinButton,
    memory_format: gtk::DropDown,
    memory_add_button: gtk::Button,
    memory_watch_handler: Rc<RefCell<Option<MemoryWatchHandler>>>,
    layout: layout::Persistence,
    breakpoints: Rc<RefCell<Vec<Breakpoint>>>,
    previous_registers: Rc<RefCell<HashMap<String, String>>>,
    stop_refresh_generation: Rc<Cell<u64>>,
    thread_refresh_generation: Rc<Cell<u64>>,
    breakpoint_refresh_generation: Rc<Cell<u64>>,
    command_pending: Rc<Cell<bool>>,
    source_roots: Rc<Vec<PathBuf>>,
    frame_selection_handler: Rc<RefCell<Option<FrameSelectionHandler>>>,
    thread_selection_handler: Rc<RefCell<Option<StringSelectionHandler>>>,
    instruction_handler: Rc<RefCell<Option<StringSelectionHandler>>>,
    variable_assignment_handler: Rc<RefCell<Option<VariableAssignmentHandler>>>,
    variable_children_handler: Rc<RefCell<Option<VariableChildrenHandler>>>,
    vector_assignment_handler: Rc<RefCell<Option<VectorAssignmentHandler>>>,
    breakpoint_insert_handler: Rc<RefCell<Option<BreakpointInsertHandler>>>,
    breakpoint_delete_handler: Rc<RefCell<Option<StringSelectionHandler>>>,
    breakpoint_condition_handler: Rc<RefCell<Option<BreakpointConditionHandler>>>,
    breakpoint_enabled_handler: Rc<RefCell<Option<BreakpointEnabledHandler>>>,
    breakpoint_bulk_delete_handler: Rc<RefCell<Option<BreakpointBulkDeleteHandler>>>,
    signal_catchpoint_handler: Rc<RefCell<Option<SignalCatchpointHandler>>>,
    event_catchpoint_handler: Rc<RefCell<Option<EventCatchpointHandler>>>,
    watchpoint_insert_handler: Rc<RefCell<Option<WatchpointInsertHandler>>>,
    source_symbol_handler: Rc<RefCell<Option<StringSelectionHandler>>>,
    thread_stop_reason: Rc<RefCell<Option<String>>>,
    debugger_ready: Rc<Cell<bool>>,
    inferior_running: Rc<Cell<bool>>,
    inferior_started: Rc<Cell<bool>>,
}

struct Topbar {
    root: gtk::HeaderBar,
    open_source_button: gtk::Button,
    load_symbols_button: gtk::Button,
    terminal_toggle_button: gtk::ToggleButton,
    run_button: gtk::Button,
    pause_button: gtk::Button,
    next_button: gtk::Button,
    step_button: gtk::Button,
    next_instruction_button: gtk::Button,
    step_instruction_button: gtk::Button,
    finish_button: gtk::Button,
    until_button: gtk::ToggleButton,
    until_popover: gtk::Popover,
    gef_tools_button: gtk::ToggleButton,
    until_actions: Vec<(gtk::Button, &'static str)>,
    until_condition_entry: gtk::Entry,
    until_condition_button: gtk::Button,
    status_label: gtk::Label,
}

struct Workspace {
    root: gtk::Paned,
    layout_panes: Vec<layout::Pane>,
    terminal_panel: gtk::Box,
    status_detail: gtk::Label,
    debug_state_panels: Vec<gtk::Widget>,
    call_stack_list: gtk::Box,
    threads_list: gtk::Box,
    modules_list: gtk::Box,
    locals_store: gio::ListStore,
    locals_selection: gtk::SingleSelection,
    locals_view: gtk::ColumnView,
    locals_empty: gtk::Label,
    locals_edit_button: gtk::Button,
    instructions_title: gtk::Label,
    instructions_store: gio::ListStore,
    instructions_selection: gtk::SingleSelection,
    instructions_view: gtk::ColumnView,
    instructions_empty: gtk::Label,
    instruction_flow: gtk::Label,
    instruction_arguments: gtk::Label,
    instruction_memory: gtk::Label,
    register_groups: Vec<RegisterGroupView>,
    registers_empty: gtk::Label,
    stack_store: gio::ListStore,
    stack_empty: gtk::Label,
    breakpoints_list: gtk::Box,
    delete_all_breakpoints_button: gtk::Button,
    delete_all_watchpoints_button: gtk::Button,
    delete_all_catchpoints_button: gtk::Button,
    event_catchpoint_buttons: Vec<(gtk::Button, EventCatchpoint)>,
    watchpoint_expression: gtk::Entry,
    watchpoint_access: gtk::DropDown,
    watchpoint_add_button: gtk::Button,
    signal_detail: gtk::Label,
    signal_buttons: Vec<(gtk::Button, &'static str, &'static str)>,
    signal_entry: gtk::Entry,
    signal_add_button: gtk::Button,
    delete_all_signal_catchpoints_button: gtk::Button,
    memory_region_store: gio::ListStore,
    memory_regions_empty: gtk::Label,
    memory_watch_list: gtk::Box,
    memory_watches_empty: gtk::Label,
    memory_address_entry: gtk::Entry,
    memory_size: gtk::SpinButton,
    memory_format: gtk::DropDown,
    memory_add_button: gtk::Button,
}

struct Inspector {
    root: gtk::Notebook,
    context_split: gtk::Paned,
    status_detail: gtk::Label,
    stale_panels: Vec<gtk::Widget>,
    locals_store: gio::ListStore,
    locals_selection: gtk::SingleSelection,
    locals_view: gtk::ColumnView,
    locals_empty: gtk::Label,
    locals_edit_button: gtk::Button,
    instructions_title: gtk::Label,
    instructions_store: gio::ListStore,
    instructions_selection: gtk::SingleSelection,
    instructions_view: gtk::ColumnView,
    instructions_empty: gtk::Label,
    instruction_flow: gtk::Label,
    instruction_arguments: gtk::Label,
    instruction_memory: gtk::Label,
    register_groups: Vec<RegisterGroupView>,
    registers_empty: gtk::Label,
    stack_store: gio::ListStore,
    stack_empty: gtk::Label,
    breakpoints_list: gtk::Box,
    delete_all_breakpoints_button: gtk::Button,
    delete_all_watchpoints_button: gtk::Button,
    delete_all_catchpoints_button: gtk::Button,
    event_catchpoint_buttons: Vec<(gtk::Button, EventCatchpoint)>,
    watchpoint_expression: gtk::Entry,
    watchpoint_access: gtk::DropDown,
    watchpoint_add_button: gtk::Button,
    signal_detail: gtk::Label,
    signal_buttons: Vec<(gtk::Button, &'static str, &'static str)>,
    signal_entry: gtk::Entry,
    signal_add_button: gtk::Button,
    delete_all_signal_catchpoints_button: gtk::Button,
    memory_region_store: gio::ListStore,
    memory_regions_empty: gtk::Label,
    memory_watch_list: gtk::Box,
    memory_watches_empty: gtk::Label,
    memory_address_entry: gtk::Entry,
    memory_size: gtk::SpinButton,
    memory_format: gtk::DropDown,
    memory_add_button: gtk::Button,
}

struct LeftSidebar {
    root: gtk::Box,
    call_stack_list: gtk::Box,
    threads_list: gtk::Box,
    modules_list: gtk::Box,
}

mod build;
mod controls;
mod debug_state;
mod dialogs;
mod formatting;
mod source_actions;
mod source_view;
mod state;
mod views;

use build::*;
use controls::*;
use dialogs::*;
use formatting::*;
use source_view::*;
use views::*;

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{
        EventCatchpoint, MemoryWatchFormat, VectorLaneFormat, architecture_pointer_bits,
        breakpoint_command_number_at_address, breakpoint_command_numbers, compact_function_name,
        event_catchpoint_command_number, event_catchpoint_command_numbers, flags_markup,
        format_memory_watch, format_register_value, full_address,
        instruction_arguments_description, instruction_flow_description,
        instruction_memory_expression, integer_decimal_value, normalized_signal_name,
        register_value_css, set_breakpoint_enabled, signal_catchpoint_command_number,
        signal_catchpoint_command_numbers, source_location_score, source_symbol_at_offset,
        source_tab_title, stop_reason_label, thread_os_id, variable_details, variable_value_parts,
        vector_field_values, without_generic_arguments,
    };
    use crate::debugger::{Breakpoint, Instruction, Register, SourceLocation, Variable};

    #[test]
    fn formats_pointer_words_and_ascii_previews() {
        assert_eq!(
            format_register_value("r12", "0x61732f656d6f682f", true),
            "0x61732f656d6f682f '/home/sa…'"
        );
        assert_eq!(
            format_register_value("rip", "0x40116f <main+15>", false),
            "0x000000000040116f <main+15>"
        );
    }

    #[test]
    fn separates_raw_variable_values_from_gdb_details() {
        assert_eq!(
            variable_value_parts(r#"0x555555559010 "YUU\005""#),
            ("0x555555559010", r#""YUU\005""#)
        );
        assert_eq!(
            variable_value_parts(
                "0x7ffff7ac2010 <error: Cannot access memory at address 0x7ffff7ac2010>"
            ),
            (
                "0x7ffff7ac2010",
                "<error: Cannot access memory at address 0x7ffff7ac2010>"
            )
        );
        assert_eq!(variable_value_parts("65 'A'"), ("65", "'A'"));
        assert_eq!(
            variable_value_parts("{x = 1, y = 2}"),
            ("{x = 1, y = 2}", "")
        );
        assert_eq!(variable_value_parts("0x1"), ("0x1", ""));

        let integer = |type_name: &str, value: &str| Variable {
            name: String::from("value"),
            value: value.to_owned(),
            type_name: Some(type_name.to_owned()),
            varobj: None,
            num_children: 0,
            has_more: false,
        };
        let details = |variable: &Variable, value: &str, annotation: &str| {
            variable_details(variable, value, annotation, 64)
        };
        assert_eq!(details(&integer("int", "0x2a"), "0x2a", ""), "42");
        assert_eq!(
            details(&integer("char", "0x41 'A'"), "0x41", "'A'"),
            "65  ·  'A'"
        );
        assert_eq!(details(&integer("__time_t", "-0x1"), "-0x1", ""), "-1");
        assert_eq!(
            details(&integer("int", "0xffffffff"), "0xffffffff", ""),
            "-1"
        );
        assert_eq!(
            details(&integer("unsigned int", "0xffffffff"), "0xffffffff", ""),
            "4294967295"
        );
        assert_eq!(details(&integer("int", "0xff"), "0xff", ""), "255");
        assert_eq!(details(&integer("i8", "0xff"), "0xff", ""), "-1");
        assert_eq!(details(&integer("void *", "0x2a"), "0x2a", ""), "");
        assert_eq!(details(&integer("double", "0x2a"), "0x2a", ""), "");
    }

    #[test]
    fn decodes_rust_c_and_cpp_integer_types() {
        let decimal = |type_name: &str, value: &str, pointer_bits| {
            let variable = Variable {
                name: String::from("value"),
                value: value.to_owned(),
                type_name: Some(type_name.to_owned()),
                varobj: None,
                num_children: 0,
                has_more: false,
            };
            integer_decimal_value(&variable, value, pointer_bits)
        };

        assert_eq!(
            decimal("i128", "0xffffffffffffffffffffffffffffffff", 64),
            Some("-1".into())
        );
        assert_eq!(
            decimal("u128", "0xffffffffffffffffffffffffffffffff", 64),
            Some("340282366920938463463374607431768211455".into())
        );
        assert_eq!(
            decimal("usize", "0xffffffffffffffff", 64),
            Some("18446744073709551615".into())
        );
        assert_eq!(decimal("isize", "0xffffffff", 32), Some("-1".into()));
        assert_eq!(
            decimal("const signed short int", "0xffff", 64),
            Some("-1".into())
        );
        assert_eq!(
            decimal("long unsigned int", "0xffffffffffffffff", 64),
            Some("18446744073709551615".into())
        );
        assert_eq!(
            decimal("std::uint_least16_t", "0xffff", 64),
            Some("65535".into())
        );
        assert_eq!(
            decimal("int_fast16_t", "0xffffffffffffffff", 64),
            Some("-1".into())
        );
        assert_eq!(
            decimal("unsigned __int64", "0xffffffffffffffff", 64),
            Some("18446744073709551615".into())
        );
        assert_eq!(
            decimal("__int128", "0xffffffffffffffffffffffffffffffff", 64),
            Some("-1".into())
        );
        assert_eq!(
            decimal("unsigned _BitInt(17)", "0x1ffff", 64),
            Some("131071".into())
        );
        assert_eq!(decimal("_BitInt(17)", "0x1ffff", 64), Some("-1".into()));
        assert_eq!(architecture_pointer_bits("i386:x86-64"), Some(64));
        assert_eq!(architecture_pointer_bits("i386"), Some(32));
    }

    #[test]
    fn formats_pretty_printed_vector_registers_as_u64_lanes() {
        let ymm = "{\n  v16_half = {0 <repeats 16 times>},\n  v4_int64 = {\n    [0x0] = 0x1,\n    [0x1] = 0x2,\n    [0x2] = 0x3,\n    [0x3] = 0x4\n  },\n  v2_int128 = {0, 0}\n}";
        assert_eq!(
            format_register_value("ymm0", ymm, false),
            "q0=0x0000000000000001  ·  q1=0x0000000000000002  ·  q2=0x0000000000000003  ·  q3=0x0000000000000004"
        );

        let zero_ymm = "{v4_int64 = {[0x0] = 0x0, [0x1] = 0x0, [0x2] = 0x0, [0x3] = 0x0}}";
        assert_eq!(
            format_register_value("ymm1", zero_ymm, false),
            "q0…q3 = 0x0000000000000000"
        );
        assert_eq!(
            register_value_css(&Register {
                name: String::from("ymm1"),
                value: zero_ymm.to_owned(),
                pointer_chain: Vec::new(),
            }),
            "register-zero"
        );
    }

    #[test]
    fn interprets_vector_union_fields_for_editing() {
        let value = "{ v8_float = {[0x0] = 0x3fc00000, [0x1] = 0xc0000000, [0x2] = 0x0 <repeats 6 times>}, v4_int64 = {[0x0] = 0x1 <repeats 4 times>} }";
        assert_eq!(
            vector_field_values(value, "v8_float", 8, VectorLaneFormat::Float32).unwrap(),
            ["1.5", "-2", "0", "0", "0", "0", "0", "0"]
        );
        assert_eq!(
            vector_field_values(value, "v4_int64", 4, VectorLaneFormat::Int64).unwrap(),
            [
                "0x0000000000000001",
                "0x0000000000000001",
                "0x0000000000000001",
                "0x0000000000000001",
            ]
        );
    }

    #[test]
    fn emphasizes_only_active_flags() {
        let markup = flags_markup("0x206", Some(3));
        assert!(markup.contains("<b>INTERRUPT</b>"));
        assert!(markup.contains("<b>PARITY</b>"));
        assert!(markup.contains(" carry"));
        assert!(markup.ends_with("[Ring=3]"));
    }

    #[test]
    fn keeps_full_addresses_and_colors_register_roles() {
        assert_eq!(full_address("0x55555555516f"), "0x000055555555516f");
        let register = |name: &str, chain: &[&str]| Register {
            name: name.to_owned(),
            value: String::from("0x7fffffffcf40"),
            pointer_chain: chain.iter().map(|value| (*value).to_owned()).collect(),
        };
        assert_eq!(register_value_css(&register("rip", &[])), "memory-code");
        assert_eq!(register_value_css(&register("rsp", &[])), "memory-stack");
        assert_eq!(
            register_value_css(&register("rsi", &["0x1", "0x61732f656d6f682f"])),
            "memory-string"
        );
    }

    #[test]
    fn formats_gef_style_thread_metadata() {
        assert_eq!(
            thread_os_id("Thread 0x7ffff7c00740 (LWP 90140)").as_deref(),
            Some("90140")
        );
        assert_eq!(stop_reason_label("breakpoint-hit"), "BREAKPOINT");
        assert_eq!(stop_reason_label("end-stepping-range"), "STEP");
    }

    #[test]
    fn uses_compact_file_names_for_source_tabs() {
        assert_eq!(
            source_tab_title(Path::new("/project/src/parser.rs")),
            "parser.rs"
        );
    }

    #[test]
    fn derives_instruction_flow_arguments_and_memory() {
        let instruction = Instruction {
            address: String::from("0x401000"),
            function: String::from("main"),
            offset: String::from("12"),
            opcodes: Some(String::from("e8 00 00 00 00")),
            text: String::from("call 0x402000 <mmap@plt>"),
        };
        let registers = ["rdi", "rsi", "rdx", "rcx", "r8", "r9"]
            .iter()
            .enumerate()
            .map(|(index, name)| Register {
                name: (*name).to_owned(),
                value: format!("0x{index:x}"),
                pointer_chain: Vec::new(),
            })
            .collect::<Vec<_>>();
        assert_eq!(
            instruction_flow_description(&instruction, &registers),
            "CALL  →  0x402000 <mmap@plt>"
        );
        let arguments = instruction_arguments_description(&instruction, &registers);
        assert!(arguments.contains("$rdi=0x0000000000000000"));
        assert!(arguments.contains("$r9=0x0000000000000005"));

        let memory_instruction = Instruction {
            text: String::from("mov rax,QWORD PTR [rbp-0x10]"),
            ..instruction.clone()
        };
        let mut with_rbp = registers;
        with_rbp.push(Register {
            name: String::from("rbp"),
            value: String::from("0x7fff0000"),
            pointer_chain: Vec::new(),
        });
        assert_eq!(
            instruction_memory_expression(&memory_instruction, &with_rbp).as_deref(),
            Some("($rbp-0x10)")
        );
    }

    #[test]
    fn predicts_x86_branches_and_decodes_linux_syscalls() {
        let branch = Instruction {
            address: String::from("0x401000"),
            function: String::from("main"),
            offset: String::from("12"),
            opcodes: Some(String::from("75 0a")),
            text: String::from("jne 0x40100c <main+0x1c>"),
        };
        let flags = Register {
            name: String::from("eflags"),
            value: String::from("0x246"),
            pointer_chain: Vec::new(),
        };
        assert_eq!(
            instruction_flow_description(&branch, std::slice::from_ref(&flags)),
            "BRANCH · NOT TAKEN  →  0x40100c <main+0x1c>"
        );

        let syscall = Instruction {
            text: String::from("syscall"),
            ..branch
        };
        let registers = [
            ("rax", "0x1"),
            ("rdi", "0x2"),
            ("rsi", "0x7fff0000"),
            ("rdx", "0x20"),
        ]
        .map(|(name, value)| Register {
            name: name.to_owned(),
            value: value.to_owned(),
            pointer_chain: Vec::new(),
        });
        let arguments = instruction_arguments_description(&syscall, &registers);
        assert!(arguments.starts_with("SYSCALL  #1 write("));
        assert!(arguments.contains("fd=0x0000000000000002"));
        assert!(arguments.contains("count=0x0000000000000020"));
    }

    #[test]
    fn formats_memory_watches() {
        let dump = format_memory_watch(0x1000, &[0x41, 0x42, 0, 0xff], MemoryWatchFormat::Bytes);
        assert_eq!(dump.addresses, "0x0000000000001000");
        assert_eq!(dump.values, "41 42 00 ff");
        assert_eq!(dump.decoded, "AB··");
    }

    #[test]
    fn finds_ctrl_click_source_symbols() {
        let rust = "let values = Vec::new();";
        assert_eq!(
            source_symbol_at_offset(rust, rust.find("new").unwrap() + 1).as_deref(),
            Some("Vec::new")
        );
        assert_eq!(
            source_symbol_at_offset(rust, rust.find("values").unwrap() + 2),
            None
        );
        let c = "void *region = mmap(NULL, size, 0, 0, -1, 0);";
        assert_eq!(
            source_symbol_at_offset(c, c.find("mmap").unwrap() + 2).as_deref(),
            Some("mmap")
        );
        assert_eq!(
            source_symbol_at_offset(c, c.find("region").unwrap() + 2),
            None
        );
        assert_eq!(
            source_symbol_at_offset(c, c.find("size").unwrap() + 1),
            None
        );
        assert_eq!(source_symbol_at_offset(c, c.find('*').unwrap()), None);

        let generic = "let value = factory::build::<Vec<u8>> (input);";
        assert_eq!(
            source_symbol_at_offset(generic, generic.find("build").unwrap() + 2).as_deref(),
            Some("factory::build")
        );
        let control = "if (ready) { worker.run(); }";
        assert_eq!(source_symbol_at_offset(control, 1), None);
        assert_eq!(
            source_symbol_at_offset(control, control.find("run").unwrap() + 1).as_deref(),
            Some("run")
        );
        assert_eq!(
            source_symbol_at_offset(control, control.find("ready").unwrap() + 1),
            None
        );
    }

    #[test]
    fn ranks_generic_rust_method_definitions() {
        let location = |function: &str, file: &str| SourceLocation {
            function: function.to_owned(),
            file: file.to_owned(),
            fullname: None,
            line: 1,
        };
        let vec = location(
            "alloc::vec::Vec<u8, alloc::alloc::Global>::new<u8>",
            "vec/mod.rs",
        );
        let small_vec = location("smallvec::SmallVec<[u8; 16]>::new<[u8; 16]>", "smallvec.rs");
        assert_eq!(
            without_generic_arguments(&vec.function),
            "alloc::vec::Vec::new"
        );
        assert!(
            source_location_score("Vec::new", &vec) > source_location_score("Vec::new", &small_vec)
        );
        let malloc = location("__GI___libc_malloc", "malloc.c");
        let cleanup = location("__malloc_arena_thread_freeres", "arena.c");
        assert!(
            source_location_score("malloc", &malloc) > source_location_score("malloc", &cleanup)
        );

        let verbose =
            "alloc::vec::Vec<alloc::boxed::Box<dyn core::fmt::Debug>, alloc::alloc::Global>::push";
        assert_eq!(compact_function_name(verbose), "alloc::vec::Vec<…>::push");
        assert_eq!(compact_function_name("core::ptr::read"), "core::ptr::read");
    }

    #[test]
    fn separates_bulk_breakpoint_and_watchpoint_numbers() {
        let stop_point = |number: &str, kind: &str| Breakpoint {
            number: number.to_owned(),
            kind: kind.to_owned(),
            enabled: true,
            condition: None,
            catch_type: None,
            address: None,
            function: None,
            file: None,
            fullname: None,
            line: None,
            original_location: None,
        };
        let stop_points = vec![
            stop_point("1.1", "breakpoint"),
            stop_point("1.2", "breakpoint"),
            stop_point("2", "hw watchpoint"),
            Breakpoint {
                original_location: Some(String::from("SIGSEGV")),
                ..stop_point("3", "catchpoint")
            },
            Breakpoint {
                catch_type: Some(String::from("throw")),
                original_location: Some(String::from("exception throw")),
                ..stop_point("4", "catchpoint")
            },
            Breakpoint {
                original_location: Some(String::from("rust_panic")),
                ..stop_point("5", "breakpoint")
            },
        ];
        assert_eq!(breakpoint_command_numbers(&stop_points, false), ["1"]);
        assert_eq!(breakpoint_command_numbers(&stop_points, true), ["2"]);
        assert_eq!(signal_catchpoint_command_numbers(&stop_points), ["3"]);
        assert_eq!(event_catchpoint_command_numbers(&stop_points), ["4", "5"]);
        assert_eq!(
            event_catchpoint_command_number(&stop_points, EventCatchpoint::CxxThrow).as_deref(),
            Some("4")
        );
        assert_eq!(
            event_catchpoint_command_number(&stop_points, EventCatchpoint::RustPanic).as_deref(),
            Some("5")
        );
        assert_eq!(
            signal_catchpoint_command_number(&stop_points, "segv").as_deref(),
            Some("3")
        );
        assert_eq!(normalized_signal_name(" usr1 ").as_deref(), Some("SIGUSR1"));
        assert_eq!(
            normalized_signal_name("SIGRTMIN+1").as_deref(),
            Some("SIGRTMIN+1")
        );
        assert!(normalized_signal_name("SIGSEGV; quit").is_none());

        let mut stop_points = stop_points;
        assert!(set_breakpoint_enabled(&mut stop_points, "1", false));
        assert!(!stop_points[0].enabled);
        assert!(!stop_points[1].enabled);
        assert!(stop_points[2].enabled);
        assert!(!set_breakpoint_enabled(&mut stop_points, "1", false));
        stop_points[0].address = Some(String::from("0x0000000000401000"));
        assert_eq!(
            breakpoint_command_number_at_address(&stop_points, "0x401000").as_deref(),
            Some("1")
        );
        assert_eq!(
            breakpoint_command_number_at_address(&stop_points, "0x402000"),
            None
        );
    }
}
