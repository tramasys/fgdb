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

impl Ui {
    pub fn build(application: &gtk::Application, config: &LaunchConfig, theme: &Theme) -> Self {
        let window = gtk::ApplicationWindow::builder()
            .application(application)
            .title("fgdb")
            .default_width(1380)
            .default_height(820)
            .build();
        window.add_css_class("fgdb-window");

        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("debugger-root");
        let terminal = build_terminal(theme);
        let topbar = build_topbar(config, &window, &terminal);
        window.set_titlebar(Some(&topbar.root));

        let source_style_scheme = theme.source_style_scheme();
        let source_notebook = build_source_notebook(source_style_scheme.as_ref());
        let source_documents = Rc::new(RefCell::new(Vec::new()));
        let breakpoints = Rc::new(RefCell::new(Vec::new()));
        let variable_children_handler = Rc::new(RefCell::new(None));
        let target_pointer_bits = Rc::new(Cell::new(usize::BITS));

        let workspace = build_workspace(
            config,
            theme,
            &source_notebook,
            &terminal,
            &variable_children_handler,
            &target_pointer_bits,
        );
        root.append(&workspace.root);
        root.append(&workspace.status_detail);
        let terminal_panel = workspace.terminal_panel.clone();
        topbar
            .terminal_toggle_button
            .connect_toggled(move |button| terminal_panel.set_visible(button.is_active()));
        window.set_child(Some(&root));
        let layout = layout::Persistence::install(&window, workspace.layout_panes.clone());

        let ui = Self {
            window,
            terminal,
            open_source_button: topbar.open_source_button,
            load_symbols_button: topbar.load_symbols_button,
            run_button: topbar.run_button,
            pause_button: topbar.pause_button,
            next_button: topbar.next_button,
            step_button: topbar.step_button,
            next_instruction_button: topbar.next_instruction_button,
            step_instruction_button: topbar.step_instruction_button,
            finish_button: topbar.finish_button,
            until_button: topbar.until_button,
            until_popover: topbar.until_popover,
            gef_tools_button: topbar.gef_tools_button,
            status_label: topbar.status_label,
            status_detail: workspace.status_detail,
            debug_state_panels: workspace.debug_state_panels,
            source_notebook,
            source_documents,
            source_theme: theme.clone(),
            source_style_scheme,
            resolved_source_paths: Rc::new(RefCell::new(HashMap::new())),
            call_stack_list: workspace.call_stack_list,
            frame_buttons: Rc::new(RefCell::new(Vec::new())),
            selected_frame_level: Rc::new(Cell::new(0)),
            threads_list: workspace.threads_list,
            modules_list: workspace.modules_list,
            locals_store: workspace.locals_store,
            locals_selection: workspace.locals_selection,
            locals_view: workspace.locals_view,
            locals_empty: workspace.locals_empty,
            locals_edit_button: workspace.locals_edit_button,
            target_pointer_bits,
            instructions_title: workspace.instructions_title,
            instructions_store: workspace.instructions_store,
            instructions_selection: workspace.instructions_selection,
            instructions_view: workspace.instructions_view,
            instructions_empty: workspace.instructions_empty,
            instruction_flow: workspace.instruction_flow,
            instruction_arguments: workspace.instruction_arguments,
            instruction_memory: workspace.instruction_memory,
            current_instruction: Rc::new(RefCell::new(None)),
            current_instruction_memory_expression: Rc::new(RefCell::new(None)),
            latest_registers: Rc::new(RefCell::new(Vec::new())),
            instruction_memory_handler: Rc::new(RefCell::new(None)),
            register_groups: workspace.register_groups,
            registers_empty: workspace.registers_empty,
            stack_store: workspace.stack_store,
            stack_empty: workspace.stack_empty,
            breakpoints_list: workspace.breakpoints_list,
            delete_all_breakpoints_button: workspace.delete_all_breakpoints_button,
            delete_all_watchpoints_button: workspace.delete_all_watchpoints_button,
            delete_all_catchpoints_button: workspace.delete_all_catchpoints_button,
            event_catchpoint_buttons: workspace.event_catchpoint_buttons,
            watchpoint_expression: workspace.watchpoint_expression,
            watchpoint_access: workspace.watchpoint_access,
            watchpoint_add_button: workspace.watchpoint_add_button,
            signal_detail: workspace.signal_detail,
            signal_buttons: workspace.signal_buttons,
            signal_entry: workspace.signal_entry,
            signal_add_button: workspace.signal_add_button,
            delete_all_signal_catchpoints_button: workspace.delete_all_signal_catchpoints_button,
            until_actions: topbar.until_actions,
            until_condition_entry: topbar.until_condition_entry,
            until_condition_button: topbar.until_condition_button,
            memory_region_store: workspace.memory_region_store,
            memory_regions_empty: workspace.memory_regions_empty,
            memory_regions: Rc::new(RefCell::new(Vec::new())),
            memory_watches: Rc::new(RefCell::new(Vec::new())),
            memory_watch_list: workspace.memory_watch_list,
            memory_watches_empty: workspace.memory_watches_empty,
            memory_address_entry: workspace.memory_address_entry,
            memory_size: workspace.memory_size,
            memory_format: workspace.memory_format,
            memory_add_button: workspace.memory_add_button,
            memory_watch_handler: Rc::new(RefCell::new(None)),
            layout,
            breakpoints,
            previous_registers: Rc::new(RefCell::new(HashMap::new())),
            stop_refresh_generation: Rc::new(Cell::new(0)),
            thread_refresh_generation: Rc::new(Cell::new(0)),
            breakpoint_refresh_generation: Rc::new(Cell::new(0)),
            command_pending: Rc::new(Cell::new(false)),
            source_roots: Rc::new(source::roots(config)),
            frame_selection_handler: Rc::new(RefCell::new(None)),
            thread_selection_handler: Rc::new(RefCell::new(None)),
            instruction_handler: Rc::new(RefCell::new(None)),
            variable_assignment_handler: Rc::new(RefCell::new(None)),
            variable_children_handler,
            vector_assignment_handler: Rc::new(RefCell::new(None)),
            breakpoint_insert_handler: Rc::new(RefCell::new(None)),
            breakpoint_delete_handler: Rc::new(RefCell::new(None)),
            breakpoint_condition_handler: Rc::new(RefCell::new(None)),
            breakpoint_enabled_handler: Rc::new(RefCell::new(None)),
            breakpoint_bulk_delete_handler: Rc::new(RefCell::new(None)),
            signal_catchpoint_handler: Rc::new(RefCell::new(None)),
            event_catchpoint_handler: Rc::new(RefCell::new(None)),
            watchpoint_insert_handler: Rc::new(RefCell::new(None)),
            source_symbol_handler: Rc::new(RefCell::new(None)),
            thread_stop_reason: Rc::new(RefCell::new(None)),
            debugger_ready: Rc::new(Cell::new(false)),
            inferior_running: Rc::new(Cell::new(false)),
            inferior_started: Rc::new(Cell::new(false)),
        };
        ui.connect_instruction_activation();
        ui.connect_local_activation();
        ui.connect_register_activation();
        ui.connect_memory_controls();
        ui.connect_watchpoint_controls();
        ui.connect_breakpoint_bulk_controls();
        ui.connect_event_catchpoint_controls();
        ui.connect_keyboard_shortcuts();
        ui
    }

    pub fn save_layout(&self) {
        self.layout.save();
    }

    pub fn connect_debug_controls(self: &Rc<Self>, client: &Rc<MiClient>) {
        let client_for_run = Rc::clone(client);
        let weak_ui = Rc::downgrade(self);
        self.run_button.connect_clicked(move |_| {
            let Some(ui) = weak_ui.upgrade() else {
                return;
            };
            let (command, detail) = if ui.inferior_started.get() {
                ("-exec-continue", "Continuing the inferior…")
            } else {
                ("-exec-run", "Starting the inferior…")
            };
            issue_execution_command(&ui, &client_for_run, command, detail);
        });
        connect_execution_button(
            &self.pause_button,
            self,
            client,
            "-exec-interrupt",
            "Interrupting the inferior…",
        );
        connect_execution_button(
            &self.next_button,
            self,
            client,
            "-exec-next",
            "Stepping over the current source line…",
        );
        connect_execution_button(
            &self.step_button,
            self,
            client,
            "-exec-step",
            "Stepping into the current source line…",
        );
        connect_execution_button(
            &self.next_instruction_button,
            self,
            client,
            "-exec-next-instruction",
            "Stepping over one machine instruction…",
        );
        connect_execution_button(
            &self.step_instruction_button,
            self,
            client,
            "-exec-step-instruction",
            "Stepping into one machine instruction…",
        );
        connect_execution_button(
            &self.finish_button,
            self,
            client,
            "-exec-finish",
            "Running until the current function returns…",
        );
        for (button, command) in &self.until_actions {
            let client = Rc::clone(client);
            let command = *command;
            let until_popover = self.until_popover.clone();
            let weak_ui = Rc::downgrade(self);
            button.connect_clicked(move |_| {
                until_popover.popdown();
                let Some(ui) = weak_ui.upgrade() else {
                    return;
                };
                let command = if command.starts_with('-') {
                    command.to_owned()
                } else {
                    format!(
                        "-interpreter-exec console {}",
                        crate::debugger::quote(command)
                    )
                };
                issue_execution_command(&ui, &client, &command, "Running to the selected event…");
            });
        }
        let condition_client = Rc::clone(client);
        let condition_entry = self.until_condition_entry.clone();
        let until_popover = self.until_popover.clone();
        let weak_ui = Rc::downgrade(self);
        self.until_condition_button.connect_clicked(move |_| {
            let condition = condition_entry.text().trim().to_owned();
            if condition.is_empty() {
                return;
            }
            until_popover.popdown();
            let command = format!("exec-until cond {condition}");
            let command = format!(
                "-interpreter-exec console {}",
                crate::debugger::quote(&command)
            );
            if let Some(ui) = weak_ui.upgrade() {
                issue_execution_command(
                    &ui,
                    &condition_client,
                    &command,
                    "Running until the expression becomes true…",
                );
            }
        });
        let symbol_client = Rc::clone(client);
        let weak_ui = Rc::downgrade(self);
        self.load_symbols_button.connect_clicked(move |_| {
            let command = format!(
                "-interpreter-exec console {}",
                crate::debugger::quote("sharedlibrary")
            );
            let weak_ui_for_response = weak_ui.clone();
            if let Some(ui) = weak_ui.upgrade() {
                ui.set_status("Loading symbols", "Loading shared-library symbols…", None);
            }
            if symbol_client
                .request(&command, move |_, record| {
                    if let Some(ui) = weak_ui_for_response.upgrade() {
                        if record.is_done() {
                            ui.set_status(
                                "Paused",
                                "Shared-library symbols are loaded",
                                Some("status-ready"),
                            );
                        } else {
                            ui.set_status(
                                "Symbol load failed",
                                record
                                    .error_message()
                                    .unwrap_or("GDB rejected sharedlibrary"),
                                Some("status-error"),
                            );
                        }
                    }
                })
                .is_err()
                && let Some(ui) = weak_ui.upgrade()
            {
                ui.set_status(
                    "Symbol load failed",
                    "The MI channel is unavailable",
                    Some("status-error"),
                );
            }
        });
        for (button, signal, _) in &self.signal_buttons {
            let signal = (*signal).to_owned();
            let weak_ui = Rc::downgrade(self);
            button.connect_clicked(move |_| {
                if let Some(ui) = weak_ui.upgrade() {
                    request_signal_catchpoint_toggle(&ui, &signal);
                }
            });
        }
        let signal_button = self.signal_add_button.clone();
        self.signal_entry.connect_activate(move |_| {
            if signal_button.is_sensitive() {
                signal_button.emit_clicked();
            }
        });
        let signal_button = self.signal_add_button.clone();
        let ready = Rc::clone(&self.debugger_ready);
        let running = Rc::clone(&self.inferior_running);
        let pending = Rc::clone(&self.command_pending);
        self.signal_entry.connect_changed(move |entry| {
            signal_button.set_sensitive(
                ready.get()
                    && !running.get()
                    && !pending.get()
                    && normalized_signal_name(&entry.text()).is_some(),
            );
        });
        let signal_entry = self.signal_entry.clone();
        let weak_ui = Rc::downgrade(self);
        self.signal_add_button.connect_clicked(move |_| {
            if let Some(ui) = weak_ui.upgrade() {
                request_signal_catchpoint_toggle(&ui, &signal_entry.text());
            }
        });
    }

    pub fn connect_source_actions(&self) {
        self.connect_open_source();
    }

    fn connect_keyboard_shortcuts(&self) {
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let run = self.run_button.clone();
        let pause = self.pause_button.clone();
        let next = self.next_button.clone();
        let step = self.step_button.clone();
        let next_instruction = self.next_instruction_button.clone();
        let step_instruction = self.step_instruction_button.clone();
        let finish = self.finish_button.clone();
        keys.connect_key_pressed(move |_, key, _, state| {
            let control = state.contains(gtk::gdk::ModifierType::CONTROL_MASK);
            let shift = state.contains(gtk::gdk::ModifierType::SHIFT_MASK);
            let blocked = state
                .intersects(gtk::gdk::ModifierType::ALT_MASK | gtk::gdk::ModifierType::SUPER_MASK);
            if blocked {
                return gtk::glib::Propagation::Proceed;
            }
            let button = match (key, control, shift) {
                (gtk::gdk::Key::F5, false, false) => Some(&run),
                (gtk::gdk::Key::F6, false, false) => Some(&pause),
                (gtk::gdk::Key::F10, false, false) => Some(&next),
                (gtk::gdk::Key::F10, true, false) => Some(&next_instruction),
                (gtk::gdk::Key::F11, false, false) => Some(&step),
                (gtk::gdk::Key::F11, true, false) => Some(&step_instruction),
                (gtk::gdk::Key::F11, false, true) => Some(&finish),
                _ => None,
            };
            let Some(button) = button.filter(|button| button.is_sensitive()) else {
                return gtk::glib::Propagation::Proceed;
            };
            button.emit_clicked();
            gtk::glib::Propagation::Stop
        });
        self.window.add_controller(keys);
    }

    pub fn set_status(&self, text: &str, detail: &str, class: Option<&str>) {
        set_status_widgets(&self.status_label, &self.status_detail, text, detail, class);
    }

    pub fn set_controls_ready(&self, ready: bool) {
        self.debugger_ready.set(ready);
        if !ready {
            self.inferior_running.set(false);
            self.command_pending.set(false);
        }
        self.update_control_sensitivity();
    }

    pub fn set_controls_running(&self, running: bool) {
        self.inferior_running.set(running);
        self.update_control_sensitivity();
    }

    pub fn set_command_pending(&self, pending: bool) {
        self.command_pending.set(pending);
        self.update_control_sensitivity();
    }

    pub fn set_debug_state_stale(&self, stale: bool) {
        for panel in &self.debug_state_panels {
            if stale {
                panel.add_css_class("debug-state-stale");
            } else {
                panel.remove_css_class("debug-state-stale");
            }
        }
    }

    pub fn set_inferior_started(&self, started: bool) {
        self.inferior_started.set(started);
        self.update_control_sensitivity();
        self.run_button
            .set_label(if started { "Continue" } else { "Run" });
    }

    fn update_control_sensitivity(&self) {
        let ready = self.debugger_ready.get();
        let started = self.inferior_started.get();
        let running = self.inferior_running.get();
        let pending = self.command_pending.get();
        let can_move = ready && started && !running && !pending;

        self.run_button.set_sensitive(ready && !running && !pending);
        self.pause_button
            .set_sensitive(ready && started && running && !pending);
        self.next_button.set_sensitive(can_move);
        self.step_button.set_sensitive(can_move);
        self.next_instruction_button.set_sensitive(can_move);
        self.step_instruction_button.set_sensitive(can_move);
        self.finish_button.set_sensitive(can_move);
        self.until_button.set_sensitive(can_move);
        self.gef_tools_button
            .set_sensitive(ready && !running && !pending);
        self.locals_view.set_sensitive(can_move);
        self.locals_edit_button.set_sensitive(
            can_move
                && variable_at(&self.locals_selection, self.locals_selection.selected()).is_some(),
        );
        for group in &self.register_groups {
            group.view.set_sensitive(can_move);
        }
        self.memory_add_button
            .set_sensitive(can_move && !self.memory_address_entry.text().trim().is_empty());
        self.watchpoint_add_button.set_sensitive(can_move);
        self.load_symbols_button.set_sensitive(can_move);
        let breakpoints = self.breakpoints.borrow();
        let can_edit_stop_points = ready && !running && !pending;
        for (button, _, _) in &self.signal_buttons {
            button.set_sensitive(can_edit_stop_points);
        }
        for (button, _) in &self.event_catchpoint_buttons {
            button.set_sensitive(can_edit_stop_points);
        }
        self.signal_entry.set_sensitive(can_edit_stop_points);
        self.signal_add_button.set_sensitive(
            can_edit_stop_points && normalized_signal_name(&self.signal_entry.text()).is_some(),
        );
        self.delete_all_signal_catchpoints_button.set_sensitive(
            can_edit_stop_points && breakpoints.iter().any(Breakpoint::is_signal_catchpoint),
        );
        self.delete_all_catchpoints_button.set_sensitive(
            can_edit_stop_points
                && breakpoints.iter().any(|breakpoint| {
                    EventCatchpoint::ALL
                        .iter()
                        .any(|(event, _, _)| event.matches(breakpoint))
                }),
        );
        self.delete_all_breakpoints_button.set_sensitive(
            can_edit_stop_points
                && breakpoints
                    .iter()
                    .any(|breakpoint| !breakpoint.is_watchpoint() && !breakpoint.is_catchpoint()),
        );
        self.delete_all_watchpoints_button.set_sensitive(
            can_edit_stop_points && breakpoints.iter().any(Breakpoint::is_watchpoint),
        );
    }

    pub fn set_frame_selection_handler(&self, handler: impl Fn(u32) + 'static) {
        self.frame_selection_handler.replace(Some(Rc::new(handler)));
    }

    pub fn set_thread_selection_handler(&self, handler: impl Fn(String) + 'static) {
        self.thread_selection_handler
            .replace(Some(Rc::new(handler)));
    }

    pub fn set_instruction_handler(&self, handler: impl Fn(String) + 'static) {
        self.instruction_handler.replace(Some(Rc::new(handler)));
    }

    pub fn set_instruction_memory_handler(&self, handler: impl Fn(String) + 'static) {
        self.instruction_memory_handler
            .replace(Some(Rc::new(handler)));
    }

    pub fn set_memory_watch_handler(&self, handler: impl Fn(u64, String, usize) + 'static) {
        self.memory_watch_handler.replace(Some(Rc::new(handler)));
    }

    pub fn set_variable_object_assignment_handler(
        &self,
        handler: impl Fn(Variable, String) + 'static,
    ) {
        self.variable_assignment_handler
            .replace(Some(Rc::new(handler)));
    }

    pub fn set_variable_children_handler(&self, handler: impl Fn(Variable, usize) + 'static) {
        self.variable_children_handler
            .replace(Some(Rc::new(handler)));
    }

    pub fn set_vector_assignment_handler(
        &self,
        handler: impl Fn(String, String, Vec<(usize, String)>) + 'static,
    ) {
        self.vector_assignment_handler
            .replace(Some(Rc::new(handler)));
    }

    pub fn set_breakpoint_insert_handler(&self, handler: impl Fn(PathBuf, u32) + 'static) {
        self.breakpoint_insert_handler
            .replace(Some(Rc::new(handler)));
    }

    pub fn set_breakpoint_delete_handler(&self, handler: impl Fn(String) + 'static) {
        self.breakpoint_delete_handler
            .replace(Some(Rc::new(handler)));
    }

    pub fn set_breakpoint_condition_handler(
        &self,
        handler: impl Fn(String, Option<String>) + 'static,
    ) {
        self.breakpoint_condition_handler
            .replace(Some(Rc::new(handler)));
    }

    pub fn set_breakpoint_enabled_handler(&self, handler: impl Fn(String, bool) + 'static) {
        self.breakpoint_enabled_handler
            .replace(Some(Rc::new(handler)));
    }

    pub fn set_breakpoint_bulk_delete_handler(&self, handler: impl Fn(Vec<String>) + 'static) {
        self.breakpoint_bulk_delete_handler
            .replace(Some(Rc::new(handler)));
    }

    pub fn set_event_catchpoint_handler(
        &self,
        handler: impl Fn(EventCatchpoint, Option<String>) + 'static,
    ) {
        self.event_catchpoint_handler
            .replace(Some(Rc::new(handler)));
    }

    pub fn set_signal_catchpoint_handler(
        &self,
        handler: impl Fn(String, Option<String>) + 'static,
    ) {
        self.signal_catchpoint_handler
            .replace(Some(Rc::new(handler)));
    }

    pub fn set_watchpoint_insert_handler(
        &self,
        handler: impl Fn(String, WatchpointAccess) + 'static,
    ) {
        self.watchpoint_insert_handler
            .replace(Some(Rc::new(handler)));
    }

    pub fn set_source_symbol_handler(&self, handler: impl Fn(String) + 'static) {
        self.source_symbol_handler.replace(Some(Rc::new(handler)));
    }

    pub fn set_thread_stop_reason(&self, reason: Option<&str>) {
        self.thread_stop_reason
            .replace(reason.map(stop_reason_label));
    }

    pub fn show_frames(&self, frames: &[StackFrame]) {
        clear_box(&self.call_stack_list);
        self.frame_buttons.borrow_mut().clear();
        if frames.is_empty() {
            self.call_stack_list
                .append(&empty_label("No stack frames available"));
            return;
        }

        for frame in frames {
            let location_text = frame.line.map_or_else(
                || frame.address.clone(),
                |line| {
                    format!(
                        "{}:{line}",
                        frame.source_path().unwrap_or(frame.address.as_str())
                    )
                },
            );
            let row = gtk::Box::new(gtk::Orientation::Vertical, 0);
            let displayed_function = compact_function_name(&frame.function);
            let function =
                gtk::Label::new(Some(&format!("#{}  {displayed_function}", frame.level)));
            function.set_halign(gtk::Align::Start);
            function.set_ellipsize(pango::EllipsizeMode::End);
            function.set_tooltip_text(Some(&frame.function));
            let location = gtk::Label::new(Some(&location_text));
            location.add_css_class("muted");
            location.set_halign(gtk::Align::Start);
            location.set_ellipsize(pango::EllipsizeMode::Middle);
            location.set_tooltip_text(Some(&location_text));
            row.append(&function);
            row.append(&location);
            let button = gtk::Button::builder().child(&row).build();
            button.add_css_class("stack-frame");
            if frame.level == self.selected_frame_level.get() {
                button.add_css_class("current-debug-item");
            }
            let level = frame.level;
            let handler = Rc::clone(&self.frame_selection_handler);
            let frame_buttons = Rc::clone(&self.frame_buttons);
            let selected_frame_level = Rc::clone(&self.selected_frame_level);
            button.connect_clicked(move |_| {
                selected_frame_level.set(level);
                update_selected_frame_buttons(&frame_buttons.borrow(), level);
                if let Some(handler) = handler.borrow().as_ref() {
                    handler(level);
                }
            });
            self.frame_buttons
                .borrow_mut()
                .push((level, button.clone()));
            self.call_stack_list.append(&button);
        }
    }

    pub fn show_locals(&self, variables: &[Variable]) {
        replace_boxed_store(
            &self.locals_store,
            variables.iter().cloned().map(VariableNode::new),
        );
        self.locals_selection
            .set_selected(gtk::INVALID_LIST_POSITION);
        if variables.is_empty() {
            self.locals_empty.set_visible(true);
            self.locals_edit_button.set_sensitive(false);
        } else {
            self.locals_empty.set_visible(false);
            self.locals_selection.set_selected(0);
            self.locals_edit_button.set_sensitive(true);
        }
    }

    pub fn show_locals_for_refresh(&self, generation: u64, variables: &[Variable]) {
        if self.is_stop_refresh_current(generation) {
            self.show_locals(variables);
        }
    }

    pub fn show_variable_children_page(
        &self,
        parent: &Variable,
        from: usize,
        variables: &[Variable],
        has_more: bool,
    ) -> bool {
        let Some(parent_name) = parent.varobj.as_deref() else {
            return false;
        };
        let Some(node) = find_variable_node(&self.locals_store, parent_name) else {
            return false;
        };
        if from != 0 {
            remove_load_more_rows(&node.children);
        }
        let mut additions = variables
            .iter()
            .cloned()
            .map(VariableNode::new)
            .map(glib::BoxedAnyObject::new)
            .collect::<Vec<_>>();
        if has_more {
            additions.push(glib::BoxedAnyObject::new(VariableNode::load_more(
                parent.clone(),
                from.saturating_add(variables.len()),
            )));
        }
        if from == 0 {
            node.children.splice(0, node.children.n_items(), &additions);
        } else {
            node.children.extend_from_slice(&additions);
        }
        node.children_loading.set(false);
        node.children_loaded.set(true);
        true
    }

    pub fn show_variable_children(&self, parent: &str, variables: &[Variable]) -> bool {
        let Some(node) = find_variable_node(&self.locals_store, parent) else {
            return false;
        };
        let parent = node.variable.clone();
        self.show_variable_children_page(&parent, 0, variables, false)
    }

    pub fn has_variable_object(&self, varobj: &str) -> bool {
        find_variable_node(&self.locals_store, varobj).is_some()
    }

    pub fn show_variable_children_error(&self, parent: &str, error: &str) {
        let Some(node) = find_variable_node(&self.locals_store, parent) else {
            return;
        };
        node.children.splice(
            0,
            node.children.n_items(),
            &[glib::BoxedAnyObject::new(VariableNode::placeholder(
                "unavailable",
                error,
            ))],
        );
        node.children_loading.set(false);
        node.children_loaded.set(true);
    }

    pub fn variable_object_names(&self) -> Vec<String> {
        let mut names = Vec::new();
        collect_variable_object_roots(&self.locals_store, None, &mut names);
        names
    }

    fn connect_local_activation(&self) {
        let window = self.window.clone();
        let selection = self.locals_selection.clone();
        let handler = Rc::clone(&self.variable_assignment_handler);
        let children_handler = Rc::clone(&self.variable_children_handler);
        self.locals_view.connect_activate(move |_, position| {
            let Some((row, node)) = variable_node_at(&selection, position) else {
                return;
            };
            if node.load_more.is_some() {
                request_next_variable_page_if_needed(&node, &children_handler);
            } else if !node.placeholder {
                if row.is_expandable() {
                    row.set_expanded(!row.is_expanded());
                } else {
                    open_variable_editor(&window, node.variable, Rc::clone(&handler));
                }
            }
        });

        let window = self.window.clone();
        let selection = self.locals_selection.clone();
        let handler = Rc::clone(&self.variable_assignment_handler);
        self.locals_edit_button.connect_clicked(move |_| {
            if let Some(variable) = variable_at(&selection, selection.selected()) {
                open_variable_editor(&window, variable, Rc::clone(&handler));
            }
        });

        let edit_button = self.locals_edit_button.clone();
        let ready = Rc::clone(&self.debugger_ready);
        let started = Rc::clone(&self.inferior_started);
        let running = Rc::clone(&self.inferior_running);
        let pending = Rc::clone(&self.command_pending);
        self.locals_selection
            .connect_selected_notify(move |selection| {
                edit_button.set_sensitive(
                    ready.get()
                        && started.get()
                        && !running.get()
                        && !pending.get()
                        && variable_at(selection, selection.selected()).is_some(),
                );
            });
    }

    fn connect_register_activation(&self) {
        for group in &self.register_groups {
            let parent = self.window.clone();
            let store = group.store.clone();
            let handler = Rc::clone(&self.variable_assignment_handler);
            let vector_handler = Rc::clone(&self.vector_assignment_handler);
            group.view.connect_activate(move |_, position| {
                let Some(item) = store
                    .item(position)
                    .and_then(|item| item.downcast::<glib::BoxedAnyObject>().ok())
                else {
                    return;
                };
                let register = item.borrow::<RegisterRowData>().register.clone();
                if matches!(register.name.as_str(), "eflags" | "rflags") {
                    open_flag_editor(&parent, register, Rc::clone(&handler));
                } else if vector_register_bytes(&register.name).is_some() {
                    open_vector_editor(&parent, register, Rc::clone(&vector_handler));
                } else {
                    open_variable_editor(
                        &parent,
                        Variable {
                            name: format!("${}", register.name),
                            value: register.value,
                            type_name: None,
                            varobj: None,
                            num_children: 0,
                            has_more: false,
                        },
                        Rc::clone(&handler),
                    );
                }
            });
        }
    }

    pub fn show_threads(&self, threads: &[ThreadInfo]) {
        clear_box(&self.threads_list);
        if threads.is_empty() {
            self.threads_list
                .append(&empty_label("No threads available"));
            return;
        }
        let stop_reason = self.thread_stop_reason.borrow().clone();
        for thread in threads {
            let marker = if thread.current { "*" } else { " " };
            let tid = thread_os_id(&thread.target_id).unwrap_or_else(|| String::from("?"));
            let name = thread.name.as_deref().unwrap_or("<unnamed>");
            let reason = thread
                .current
                .then(|| stop_reason.as_deref().unwrap_or("STOPPED"));
            let detail = thread_detail(thread, reason);
            let row = gtk::Box::new(gtk::Orientation::Vertical, 0);
            let heading = gtk::Label::new(Some(&format!(
                "[{marker}Thread Id:{}, tid:{tid}]",
                thread.id
            )));
            heading.add_css_class("thread-heading");
            heading.set_halign(gtk::Align::Start);
            heading.set_ellipsize(pango::EllipsizeMode::End);
            let name = gtk::Label::new(Some(&format!("Name: \"{name}\"")));
            name.add_css_class("thread-name");
            name.set_halign(gtk::Align::Start);
            name.set_ellipsize(pango::EllipsizeMode::End);
            let detail_widget = thread_detail_widget(thread, reason);
            let full_symbol = thread.frame.as_ref().map(|frame| frame.function.as_str());
            detail_widget.set_tooltip_text(Some(&match full_symbol {
                Some(symbol) => format!(
                    "{detail}\nFull symbol: {symbol}\nGDB target: {}",
                    thread.target_id
                ),
                None => format!("{detail}\nGDB target: {}", thread.target_id),
            }));
            row.append(&heading);
            row.append(&name);
            row.append(&detail_widget);
            let button = gtk::Button::builder().child(&row).build();
            button.add_css_class("stack-frame");
            if thread.current {
                button.add_css_class("current-debug-item");
            }
            let id = thread.id.clone();
            let handler = Rc::clone(&self.thread_selection_handler);
            button.connect_clicked(move |_| {
                if let Some(handler) = handler.borrow().as_ref() {
                    handler(id.clone());
                }
            });
            self.threads_list.append(&button);
        }
    }

    pub fn show_modules(&self, modules: &[SharedLibrary]) {
        clear_box(&self.modules_list);
        if modules.is_empty() {
            self.modules_list
                .append(&empty_label("No shared libraries loaded"));
            return;
        }

        for module in modules {
            let row = gtk::Box::new(gtk::Orientation::Vertical, 0);
            row.add_css_class("module-row");
            let heading = gtk::Box::new(gtk::Orientation::Horizontal, 4);
            let name = Path::new(&module.target_name)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(&module.target_name);
            let name = gtk::Label::new(Some(name));
            name.add_css_class("module-name");
            name.set_halign(gtk::Align::Start);
            name.set_hexpand(true);
            name.set_ellipsize(pango::EllipsizeMode::End);
            let symbol_state = gtk::Label::new(Some(if module.symbols_loaded {
                "SYMBOLS"
            } else {
                "NO SYMBOLS"
            }));
            symbol_state.add_css_class("module-symbol-state");
            symbol_state.add_css_class(if module.symbols_loaded {
                "module-symbols-loaded"
            } else {
                "module-symbols-missing"
            });
            heading.append(&name);
            heading.append(&symbol_state);

            let range = match (&module.from, &module.to) {
                (Some(from), Some(to)) => format!("{from}–{to}"),
                _ => String::from("address range unavailable"),
            };
            let range = gtk::Label::new(Some(&range));
            range.add_css_class("module-range");
            range.set_halign(gtk::Align::Start);
            range.set_selectable(true);
            let path = module.host_name.as_deref().unwrap_or(&module.target_name);
            let path_label = gtk::Label::new(Some(path));
            path_label.add_css_class("module-path");
            path_label.set_halign(gtk::Align::Start);
            path_label.set_ellipsize(pango::EllipsizeMode::Middle);
            path_label.set_selectable(true);
            path_label.set_tooltip_text(Some(&format!(
                "Target: {}\nHost: {}",
                module.target_name, path
            )));
            row.append(&heading);
            row.append(&range);
            row.append(&path_label);
            self.modules_list.append(&row);
        }
    }

    pub fn start_thread_refresh(&self) -> u64 {
        let generation = self.thread_refresh_generation.get().wrapping_add(1);
        self.thread_refresh_generation.set(generation);
        generation
    }

    pub fn show_threads_for_refresh(&self, generation: u64, threads: &[ThreadInfo]) {
        if self.is_thread_refresh_current(generation) {
            self.show_threads(threads);
        }
    }

    pub fn is_thread_refresh_current(&self, generation: u64) -> bool {
        self.thread_refresh_generation.get() == generation
    }

    pub fn show_instructions(
        &self,
        instructions: &[Instruction],
        pc: &str,
        architecture: Option<&str>,
    ) {
        self.instructions_selection
            .set_selected(gtk::INVALID_LIST_POSITION);
        let title = architecture.map_or_else(
            || String::from("INSTRUCTIONS"),
            |architecture| format!("INSTRUCTIONS · {architecture} · GDB NATIVE"),
        );
        self.instructions_title.set_text(&title);
        self.instructions_title.set_tooltip_text(Some(&title));
        if instructions.is_empty() {
            self.instructions_empty.set_visible(true);
            self.current_instruction.replace(None);
            self.current_instruction_memory_expression.replace(None);
            self.instruction_flow
                .set_text("Flow information appears at a branch or call");
            self.instruction_flow.set_visible(true);
            self.instruction_arguments.set_visible(false);
            self.instruction_memory.set_visible(false);
            self.update_control_sensitivity();
            return;
        }
        self.instructions_empty.set_visible(false);
        let current = instructions
            .iter()
            .position(|instruction| addresses_equal(&instruction.address, pc))
            .unwrap_or(0);
        self.current_instruction
            .replace(instructions.get(current).cloned());
        let start = current.saturating_sub(3);
        let rows = instructions
            .iter()
            .skip(start)
            .take(9)
            .map(|instruction| InstructionRowData {
                instruction: instruction.clone(),
                current: addresses_equal(&instruction.address, pc),
            })
            .collect::<Vec<_>>();
        let selected = rows
            .iter()
            .position(|row| row.current)
            .map(|index| index as u32);
        replace_boxed_store(&self.instructions_store, rows);
        if let Some(selected) = selected {
            self.instructions_selection.set_selected(selected);
        }
        self.update_instruction_insight();
        self.update_control_sensitivity();
    }

    fn update_instruction_insight(&self) {
        let Some(instruction) = self.current_instruction.borrow().clone() else {
            return;
        };
        let registers = self.latest_registers.borrow();
        let branch_taken = conditional_branch_taken(&instruction, &registers);
        let flow = instruction_flow_description(&instruction, &registers);
        self.instruction_flow.set_text(&flow);
        self.instruction_flow.set_tooltip_text(Some(&flow));
        self.instruction_flow.set_visible(true);
        self.instruction_flow.remove_css_class("branch-taken");
        self.instruction_flow.remove_css_class("branch-not-taken");
        if let Some(taken) = branch_taken {
            self.instruction_flow.add_css_class(if taken {
                "branch-taken"
            } else {
                "branch-not-taken"
            });
        }

        let arguments = instruction_arguments_description(&instruction, &registers);
        self.instruction_arguments
            .set_visible(!arguments.is_empty());
        self.instruction_arguments.set_text(&arguments);
        self.instruction_arguments
            .set_tooltip_text((!arguments.is_empty()).then_some(arguments.as_str()));

        let expression = instruction_memory_expression(&instruction, &registers);
        drop(registers);
        let mut current = self.current_instruction_memory_expression.borrow_mut();
        if current.as_ref() == expression.as_ref() {
            return;
        }
        current.clone_from(&expression);
        drop(current);
        let Some(expression) = expression else {
            self.instruction_memory.set_visible(false);
            return;
        };
        self.instruction_memory
            .set_text(&format!("MEM  {expression} · reading…"));
        self.instruction_memory.set_visible(true);
        if let Some(handler) = self.instruction_memory_handler.borrow().as_ref() {
            handler(expression);
        }
    }

    pub fn show_instruction_memory(&self, expression: &str, result: Result<&MemoryBlock, &str>) {
        if self
            .current_instruction_memory_expression
            .borrow()
            .as_deref()
            != Some(expression)
        {
            return;
        }
        let text = match result {
            Ok(memory) => format!(
                "MEM  {expression} = 0x{:016x}  {}",
                memory.begin,
                compact_memory_preview(&memory.bytes)
            ),
            Err(error) => format!("MEM  {expression} · {error}"),
        };
        self.instruction_memory.set_text(&text);
        self.instruction_memory.set_tooltip_text(Some(&text));
        self.instruction_memory.set_visible(true);
    }

    fn connect_instruction_activation(&self) {
        let store = self.instructions_store.clone();
        let handler = Rc::clone(&self.instruction_handler);
        self.instructions_view.connect_activate(move |_, position| {
            let Some(item) = store
                .item(position)
                .and_then(|item| item.downcast::<glib::BoxedAnyObject>().ok())
            else {
                return;
            };
            let address = item
                .borrow::<InstructionRowData>()
                .instruction
                .address
                .clone();
            if let Some(handler) = handler.borrow().as_ref() {
                handler(address);
            }
        });
    }

    pub fn show_signal(&self, name: Option<&str>, meaning: Option<&str>) {
        let text = match (name, meaning) {
            (Some(name), Some(meaning)) => format!("{name} · {meaning}"),
            (Some(name), None) => name.to_owned(),
            (None, _) => String::from("No signal at the current stop"),
        };
        self.signal_detail.set_text(&text);
        if name.is_some() {
            self.signal_detail.add_css_class("signal-active");
        } else {
            self.signal_detail.remove_css_class("signal-active");
        }
    }

    pub fn show_registers(&self, registers: &[Register]) {
        for group in &self.register_groups {
            group.panel.set_visible(false);
            if registers.is_empty() {
                group.store.remove_all();
            }
        }
        if registers.is_empty() {
            self.registers_empty.set_visible(true);
        } else {
            self.registers_empty.set_visible(false);
            let previous = self.previous_registers.borrow();
            let by_name = registers
                .iter()
                .map(|register| (register.name.as_str(), register))
                .collect::<HashMap<_, _>>();
            let ring = by_name
                .get("cs")
                .and_then(|register| hex_value(&register.value))
                .map(|value| value & 0x3);
            for group in self.register_groups.iter() {
                let grouped = registers.iter().filter(|register| {
                    register_in_group(group.kind, &register.name)
                        && (group.kind != RegisterGroupKind::Other
                            || !self.register_groups.iter().any(|candidate| {
                                candidate.kind != RegisterGroupKind::Other
                                    && register_in_group(candidate.kind, &register.name)
                            }))
                });
                populate_register_group(group, grouped, &previous, ring);
            }
        }
        let values_changed = {
            let latest = self.latest_registers.borrow();
            !same_register_values(&latest, registers)
        };
        if values_changed {
            self.latest_registers.replace(registers.to_vec());
        }
        self.update_instruction_insight();
    }

    pub fn start_stop_refresh(&self) -> u64 {
        let latest = self.latest_registers.borrow();
        let mut previous = self.previous_registers.borrow_mut();
        previous.clear();
        previous.reserve(latest.len());
        previous.extend(
            latest
                .iter()
                .map(|register| (register.name.clone(), register.value.clone())),
        );
        drop(previous);
        drop(latest);
        let generation = self.stop_refresh_generation.get().wrapping_add(1);
        self.stop_refresh_generation.set(generation);
        generation
    }

    pub fn is_stop_refresh_current(&self, generation: u64) -> bool {
        self.stop_refresh_generation.get() == generation
    }

    pub fn show_registers_for_refresh(&self, generation: u64, registers: &[Register]) {
        if self.is_stop_refresh_current(generation) {
            self.show_registers(registers);
        }
    }

    pub fn show_stack(&self, entries: &[StackEntry]) {
        replace_boxed_store(&self.stack_store, entries.iter().cloned());
        if entries.is_empty() {
            self.stack_empty.set_visible(true);
            return;
        }
        self.stack_empty.set_visible(false);
    }

    pub fn show_stack_for_refresh(&self, generation: u64, entries: &[StackEntry]) {
        if self.is_stop_refresh_current(generation) {
            self.show_stack(entries);
        }
    }

    pub fn show_memory_regions_for_refresh(&self, generation: u64, regions: &[MemoryRegion]) {
        if !self.is_stop_refresh_current(generation) {
            return;
        }
        replace_boxed_store(&self.memory_region_store, regions.iter().cloned());
        self.memory_regions.replace(regions.to_vec());
        self.memory_regions_empty.set_visible(regions.is_empty());
    }

    fn connect_memory_controls(&self) {
        let list = self.memory_watch_list.clone();
        let empty = self.memory_watches_empty.clone();
        let watches = Rc::clone(&self.memory_watches);
        let handler = Rc::clone(&self.memory_watch_handler);
        let expression = self.memory_address_entry.clone();
        let size = self.memory_size.clone();
        let format = self.memory_format.clone();
        self.memory_add_button.connect_clicked(move |_| {
            let expression_text = expression.text().trim().to_owned();
            if expression_text.is_empty() {
                return;
            }
            let byte_count = usize::try_from(size.value_as_int()).unwrap_or(128);
            let format = match format.selected() {
                1 => MemoryWatchFormat::Words,
                2 => MemoryWatchFormat::Pointers,
                _ => MemoryWatchFormat::Bytes,
            };
            add_memory_watch(
                &list,
                &empty,
                &watches,
                &handler,
                expression_text,
                byte_count,
                format,
            );
            expression.set_text("");
            expression.grab_focus();
        });
        let button = self.memory_add_button.clone();
        self.memory_address_entry.connect_activate(move |_| {
            if button.is_sensitive() {
                button.emit_clicked();
            }
        });

        let button = self.memory_add_button.clone();
        let ready = Rc::clone(&self.debugger_ready);
        let started = Rc::clone(&self.inferior_started);
        let running = Rc::clone(&self.inferior_running);
        let pending = Rc::clone(&self.command_pending);
        self.memory_address_entry.connect_changed(move |entry| {
            button.set_sensitive(
                ready.get()
                    && started.get()
                    && !running.get()
                    && !pending.get()
                    && !entry.text().trim().is_empty(),
            );
        });
    }

    fn connect_watchpoint_controls(&self) {
        let expression = self.watchpoint_expression.clone();
        let access = self.watchpoint_access.clone();
        let handler = Rc::clone(&self.watchpoint_insert_handler);
        self.watchpoint_add_button.connect_clicked(move |_| {
            let expression = expression.text().trim().to_owned();
            if expression.is_empty() {
                return;
            }
            let access = match access.selected() {
                1 => WatchpointAccess::Read,
                2 => WatchpointAccess::Access,
                _ => WatchpointAccess::Write,
            };
            if let Some(handler) = handler.borrow().as_ref() {
                handler(expression, access);
            }
        });
        let button = self.watchpoint_add_button.clone();
        self.watchpoint_expression
            .connect_activate(move |_| button.emit_clicked());
    }

    fn connect_breakpoint_bulk_controls(&self) {
        let breakpoints = Rc::clone(&self.breakpoints);
        let handler = Rc::clone(&self.breakpoint_bulk_delete_handler);
        self.delete_all_breakpoints_button
            .connect_clicked(move |_| {
                let numbers = breakpoint_command_numbers(&breakpoints.borrow(), false);
                if !numbers.is_empty()
                    && let Some(handler) = handler.borrow().as_ref()
                {
                    handler(numbers);
                }
            });
        let breakpoints = Rc::clone(&self.breakpoints);
        let handler = Rc::clone(&self.breakpoint_bulk_delete_handler);
        self.delete_all_watchpoints_button
            .connect_clicked(move |_| {
                let numbers = breakpoint_command_numbers(&breakpoints.borrow(), true);
                if !numbers.is_empty()
                    && let Some(handler) = handler.borrow().as_ref()
                {
                    handler(numbers);
                }
            });
        let breakpoints = Rc::clone(&self.breakpoints);
        let handler = Rc::clone(&self.breakpoint_bulk_delete_handler);
        self.delete_all_catchpoints_button
            .connect_clicked(move |_| {
                let numbers = event_catchpoint_command_numbers(&breakpoints.borrow());
                if !numbers.is_empty()
                    && let Some(handler) = handler.borrow().as_ref()
                {
                    handler(numbers);
                }
            });
        let breakpoints = Rc::clone(&self.breakpoints);
        let handler = Rc::clone(&self.breakpoint_bulk_delete_handler);
        self.delete_all_signal_catchpoints_button
            .connect_clicked(move |_| {
                let numbers = signal_catchpoint_command_numbers(&breakpoints.borrow());
                if !numbers.is_empty()
                    && let Some(handler) = handler.borrow().as_ref()
                {
                    handler(numbers);
                }
            });
    }

    fn connect_event_catchpoint_controls(&self) {
        for (button, event) in &self.event_catchpoint_buttons {
            let event = *event;
            let breakpoints = Rc::clone(&self.breakpoints);
            let handler = Rc::clone(&self.event_catchpoint_handler);
            button.connect_clicked(move |_| {
                let existing = event_catchpoint_command_number(&breakpoints.borrow(), event);
                if let Some(handler) = handler.borrow().as_ref() {
                    handler(event, existing);
                }
            });
        }
    }

    pub fn refresh_memory_watches(&self) {
        let Some(handler) = self.memory_watch_handler.borrow().clone() else {
            return;
        };
        for watch in self.memory_watches.borrow().iter() {
            watch.status.remove_css_class("memory-watch-error");
            watch.status.set_text("reading…");
            handler(watch.id, watch.expression.clone(), watch.byte_count);
        }
    }

    pub fn show_memory_watch(&self, id: u64, result: Result<&MemoryBlock, &str>) {
        let watches = self.memory_watches.borrow();
        let Some(watch) = watches.iter().find(|watch| watch.id == id) else {
            return;
        };
        match result {
            Ok(memory) => {
                watch.status.remove_css_class("memory-watch-error");
                let region = self
                    .memory_regions
                    .borrow()
                    .iter()
                    .find(|region| region.contains(memory.begin))
                    .map(MemoryRegion::description)
                    .unwrap_or_else(|| String::from("unmapped"));
                watch
                    .status
                    .set_text(&format!("0x{:016x} · {region}", memory.begin));
                let output = format_memory_watch(memory.begin, &memory.bytes, watch.format);
                watch.output_addresses.set_text(&output.addresses);
                watch.output_values.set_text(&output.values);
                watch.output_decoded.set_text(&output.decoded);
            }
            Err(error) => {
                watch.status.add_css_class("memory-watch-error");
                watch.status.set_text(error);
                watch.output_addresses.set_text("");
                watch.output_values.set_text("");
                watch.output_decoded.set_text("");
            }
        }
    }

    pub fn show_breakpoints(&self, breakpoints: Vec<Breakpoint>) {
        self.breakpoints.replace(breakpoints);
        clear_box(&self.breakpoints_list);
        let breakpoints = self.breakpoints.borrow();
        if breakpoints.is_empty() {
            self.breakpoints_list.append(&empty_label(
                "No breakpoints, catchpoints, or watchpoints set",
            ));
        } else {
            for breakpoint in breakpoints.iter() {
                let name = if breakpoint.is_watchpoint() {
                    breakpoint
                        .original_location
                        .as_deref()
                        .or(breakpoint.function.as_deref())
                        .or(breakpoint.address.as_deref())
                        .unwrap_or("unresolved expression")
                } else if breakpoint.is_catchpoint() {
                    breakpoint
                        .original_location
                        .as_deref()
                        .or(breakpoint.catch_type.as_deref())
                        .unwrap_or("event")
                } else {
                    breakpoint
                        .function
                        .as_deref()
                        .or(breakpoint.original_location.as_deref())
                        .or(breakpoint.address.as_deref())
                        .unwrap_or("unresolved")
                };
                let location = match (breakpoint.source_path(), breakpoint.line) {
                    (Some(file), Some(line)) => format!("{file}:{line}"),
                    _ if breakpoint.is_watchpoint() => breakpoint.kind.clone(),
                    _ if breakpoint.is_catchpoint() => {
                        breakpoint.catch_type.as_deref().map_or_else(
                            || String::from("event catchpoint"),
                            |kind| format!("{kind} catchpoint"),
                        )
                    }
                    _ => breakpoint
                        .address
                        .clone()
                        .unwrap_or_else(|| String::from("pending")),
                };
                let row = gtk::Box::new(gtk::Orientation::Vertical, 0);
                row.add_css_class("stack-row");
                row.add_css_class("breakpoint-row");
                if breakpoint.is_watchpoint() {
                    row.add_css_class("watchpoint-row");
                }
                if !breakpoint.enabled {
                    row.add_css_class("breakpoint-row-disabled");
                }
                let heading_row = gtk::Box::new(gtk::Orientation::Horizontal, 3);
                let kind = if breakpoint.is_watchpoint() || breakpoint.is_catchpoint() {
                    breakpoint.kind.to_ascii_uppercase()
                } else {
                    String::from("BREAKPOINT")
                };
                let badge = gtk::Button::with_label(&format!("#{}", breakpoint.number));
                badge.add_css_class("breakpoint-badge");
                badge.set_focus_on_click(false);
                badge.add_css_class(if breakpoint.enabled {
                    "breakpoint-badge-enabled"
                } else {
                    "breakpoint-badge-disabled"
                });
                badge.set_tooltip_text(Some(if breakpoint.enabled {
                    "Disable this stop point"
                } else {
                    "Enable this stop point"
                }));
                let heading_text = format!("{kind}  {}", compact_function_name(name));
                let heading = gtk::Label::new(Some(&heading_text));
                heading.set_halign(gtk::Align::Start);
                heading.set_ellipsize(pango::EllipsizeMode::End);
                heading.set_hexpand(true);
                heading.set_tooltip_text(Some(&format!("{kind}  {name}")));
                let condition_button = gtk::Button::with_label(if breakpoint.condition.is_some() {
                    "Edit condition"
                } else {
                    "Condition"
                });
                condition_button.add_css_class("inline-action");
                condition_button.set_tooltip_text(Some("Add, edit, or clear a GDB condition"));
                let delete_button = gtk::Button::with_label("Delete");
                delete_button.add_css_class("inline-action");
                delete_button.add_css_class("danger-action");
                delete_button.set_tooltip_text(Some("Delete this breakpoint"));
                heading_row.append(&badge);
                heading_row.append(&heading);
                heading_row.append(&condition_button);
                heading_row.append(&delete_button);
                let location_text = location;
                let location = gtk::Label::new(Some(&location_text));
                location.add_css_class("muted");
                location.set_halign(gtk::Align::Start);
                location.set_ellipsize(pango::EllipsizeMode::Middle);
                location.set_selectable(true);
                location.set_tooltip_text(Some(&location_text));
                row.append(&heading_row);
                row.append(&location);
                if let Some(condition) = breakpoint.condition.as_deref() {
                    let condition = gtk::Label::new(Some(&format!("WHEN  {condition}")));
                    condition.add_css_class("breakpoint-condition");
                    condition.set_halign(gtk::Align::Start);
                    condition.set_ellipsize(pango::EllipsizeMode::End);
                    condition.set_tooltip_text(Some(condition.text().as_str()));
                    row.append(&condition);
                }

                let parent = self.window.clone();
                let breakpoint_for_condition = breakpoint.clone();
                let condition_handler = Rc::clone(&self.breakpoint_condition_handler);
                condition_button.connect_clicked(move |_| {
                    open_breakpoint_condition_editor(
                        &parent,
                        breakpoint_for_condition.clone(),
                        Rc::clone(&condition_handler),
                    );
                });
                let number = breakpoint.command_number().to_owned();
                let enable = !breakpoint.enabled;
                let enabled_handler = Rc::clone(&self.breakpoint_enabled_handler);
                badge.connect_clicked(move |_| {
                    if let Some(handler) = enabled_handler.borrow().as_ref() {
                        handler(number.clone(), enable);
                    }
                });
                let number = breakpoint.command_number().to_owned();
                let delete_handler = Rc::clone(&self.breakpoint_delete_handler);
                delete_button.connect_clicked(move |_| {
                    if let Some(handler) = delete_handler.borrow().as_ref() {
                        handler(number.clone());
                    }
                });
                self.breakpoints_list.append(&row);
            }
        }
        for (button, signal, description) in &self.signal_buttons {
            if let Some(number) = signal_catchpoint_command_number(&breakpoints, signal) {
                button.add_css_class("signal-caught");
                button.set_tooltip_text(Some(&format!(
                    "{description}\nCatchpoint #{number} is active; click to remove it"
                )));
            } else {
                button.remove_css_class("signal-caught");
                button.set_tooltip_text(Some(&format!(
                    "{description}\nClick to add a GDB signal catchpoint"
                )));
            }
        }
        for (button, event) in &self.event_catchpoint_buttons {
            if let Some(number) = event_catchpoint_command_number(&breakpoints, *event) {
                button.add_css_class("signal-caught");
                button.set_tooltip_text(Some(&format!(
                    "{} catchpoint #{number} is active; click to remove it",
                    event.label()
                )));
            } else {
                button.remove_css_class("signal-caught");
                let description = EventCatchpoint::ALL
                    .iter()
                    .find(|(candidate, _, _)| candidate == event)
                    .map(|(_, _, description)| *description)
                    .unwrap_or("Click to add this catchpoint");
                button.set_tooltip_text(Some(description));
            }
        }
        for document in self.source_documents.borrow().iter() {
            document.breakpoint_renderer.queue_draw();
        }
        drop(breakpoints);
        self.update_control_sensitivity();
    }

    pub fn start_breakpoint_refresh(&self) -> u64 {
        let generation = self.breakpoint_refresh_generation.get().wrapping_add(1);
        self.breakpoint_refresh_generation.set(generation);
        generation
    }

    pub fn show_breakpoints_for_refresh(&self, generation: u64, breakpoints: Vec<Breakpoint>) {
        if self.breakpoint_refresh_generation.get() == generation {
            self.show_breakpoints(breakpoints);
        }
    }

    pub fn set_breakpoint_enabled_pending(&self, number: &str, enabled: bool) -> bool {
        let mut breakpoints = self.breakpoints.borrow().clone();
        let changed = set_breakpoint_enabled(&mut breakpoints, number, enabled);
        if changed {
            self.start_breakpoint_refresh();
            self.show_breakpoints(breakpoints);
        }
        changed
    }

    pub fn breakpoint_number_at_address(&self, address: &str) -> Option<String> {
        breakpoint_command_number_at_address(&self.breakpoints.borrow(), address)
    }

    fn resolve_source_path(&self, reported_path: &str) -> Option<PathBuf> {
        if let Some(path) = self
            .resolved_source_paths
            .borrow()
            .get(reported_path)
            .cloned()
        {
            return Some(path);
        }
        let path = source::resolve(reported_path, &self.source_roots)?;
        self.resolved_source_paths
            .borrow_mut()
            .insert(reported_path.to_owned(), path.clone());
        Some(path)
    }

    pub fn show_source_locations(&self, symbol: &str, locations: &[SourceLocation]) {
        let mut candidates = locations
            .iter()
            .filter_map(|location| {
                let path = self.resolve_source_path(location.source_path())?;
                let score = source_location_score(symbol, location);
                Some((score, path, location))
            })
            .collect::<Vec<_>>();
        candidates.sort_by_key(|candidate| Reverse(candidate.0));
        let Some((_, path, location)) = candidates.first() else {
            self.set_status(
                "Source unavailable",
                &format!(
                    "No source-backed definition for {symbol}. Install matching debuginfo and source files."
                ),
                Some("status-error"),
            );
            return;
        };
        let context = SourceOpenContext {
            notebook: &self.source_notebook,
            documents: &self.source_documents,
            theme: &self.source_theme,
            style_scheme: self.source_style_scheme.as_ref(),
            breakpoints: &self.breakpoints,
            insert_handler: &self.breakpoint_insert_handler,
            delete_handler: &self.breakpoint_delete_handler,
            enabled_handler: &self.breakpoint_enabled_handler,
            symbol_handler: &self.source_symbol_handler,
        };
        let Some(document) = open_source_document(path, context) else {
            self.set_status(
                "Source unavailable",
                &format!("Could not read {}", path.display()),
                Some("status-error"),
            );
            return;
        };
        scroll_source_document(&document, location.line);
        self.set_status(
            "Source",
            &format!(
                "{} · {}:{}",
                location.function,
                path.display(),
                location.line
            ),
            Some("status-ready"),
        );
    }

    pub fn show_initial_source(&self, source_file: &SourceFile) {
        if !self.source_documents.borrow().is_empty() {
            return;
        }
        let Some(path) = self.resolve_source_path(source_file.source_path()) else {
            return;
        };
        let context = SourceOpenContext {
            notebook: &self.source_notebook,
            documents: &self.source_documents,
            theme: &self.source_theme,
            style_scheme: self.source_style_scheme.as_ref(),
            breakpoints: &self.breakpoints,
            insert_handler: &self.breakpoint_insert_handler,
            delete_handler: &self.breakpoint_delete_handler,
            enabled_handler: &self.breakpoint_enabled_handler,
            symbol_handler: &self.source_symbol_handler,
        };
        let Some(document) = open_source_document(&path, context) else {
            return;
        };
        scroll_source_document(&document, source_file.line);
        self.set_status(
            "Ready",
            &format!(
                "Opened {} from the executable's debug information",
                path.display()
            ),
            Some("status-ready"),
        );
    }

    pub fn show_execution_location(&self, frame: &StackFrame) {
        self.clear_execution_location();
        self.selected_frame_level.set(frame.level);
        if let Some(bits) = frame
            .architecture
            .as_deref()
            .and_then(architecture_pointer_bits)
        {
            self.target_pointer_bits.set(bits);
        }
        update_selected_frame_buttons(&self.frame_buttons.borrow(), frame.level);
        let (Some(reported_path), Some(line)) = (frame.source_path(), frame.line) else {
            return;
        };
        let path = self.resolve_source_path(reported_path);
        let Some(path) = path else {
            self.status_detail.set_text(&format!(
                "Paused in {} · source unavailable: {reported_path}",
                frame.function
            ));
            return;
        };
        let context = SourceOpenContext {
            notebook: &self.source_notebook,
            documents: &self.source_documents,
            theme: &self.source_theme,
            style_scheme: self.source_style_scheme.as_ref(),
            breakpoints: &self.breakpoints,
            insert_handler: &self.breakpoint_insert_handler,
            delete_handler: &self.breakpoint_delete_handler,
            enabled_handler: &self.breakpoint_enabled_handler,
            symbol_handler: &self.source_symbol_handler,
        };
        let Some(document) = open_source_document(&path, context) else {
            self.set_status(
                "Source unavailable",
                &format!("Could not read {}", path.display()),
                Some("status-error"),
            );
            return;
        };
        let source_name = frame
            .file
            .as_deref()
            .unwrap_or(path.as_os_str().to_str().unwrap_or("source"));
        document.tab_label.set_text(&format!(
            "{source_name}:{line} · {}",
            compact_function_name(&frame.function)
        ));
        document.tab.add_css_class("executing-source-tab");
        document.tab_label.set_tooltip_text(Some(&format!(
            "{}\n{}",
            path.to_string_lossy(),
            frame.function
        )));
        let Ok(line) = i32::try_from(line.saturating_sub(1)) else {
            return;
        };
        let Some(iter) = document.buffer.iter_at_line(line) else {
            return;
        };
        let mark = document
            .buffer
            .create_source_mark(None, EXECUTION_CATEGORY, &iter);
        document.breakpoint_renderer.queue_draw();
        document.buffer.place_cursor(&iter);
        let source_view = document.view;
        gtk::glib::idle_add_local_once(move || {
            if mark
                .buffer()
                .is_some_and(|buffer| buffer == source_view.buffer())
            {
                source_view.scroll_to_mark(&mark, 0.15, true, 0.0, 0.35);
            }
        });
    }

    pub fn clear_execution_location(&self) {
        self.selected_frame_level.set(u32::MAX);
        update_selected_frame_buttons(&self.frame_buttons.borrow(), u32::MAX);
        for document in self.source_documents.borrow().iter() {
            remove_marks(&document.buffer, EXECUTION_CATEGORY);
            document.breakpoint_renderer.queue_draw();
            document.tab.remove_css_class("executing-source-tab");
            document
                .tab_label
                .set_text(&source_tab_title(&document.path));
            document
                .tab_label
                .set_tooltip_text(Some(&document.path.to_string_lossy()));
        }
    }

    pub fn clear_debugger_state(&self) {
        self.start_stop_refresh();
        self.start_thread_refresh();
        self.clear_execution_location();
        self.show_frames(&[]);
        self.show_threads(&[]);
        self.show_modules(&[]);
        self.show_locals(&[]);
        self.show_registers(&[]);
        self.show_stack(&[]);
        self.previous_registers.borrow_mut().clear();
        self.show_instructions(&[], "", None);
        self.show_signal(None, None);
        self.memory_region_store.remove_all();
        self.memory_regions.borrow_mut().clear();
        self.memory_regions_empty.set_visible(true);
        for watch in self.memory_watches.borrow().iter() {
            watch.status.remove_css_class("memory-watch-error");
            watch.status.set_text("target is not paused");
            watch.output_addresses.set_text("");
            watch.output_values.set_text("");
            watch.output_decoded.set_text("");
        }
    }

    fn connect_open_source(&self) {
        let window = self.window.clone();
        let notebook = self.source_notebook.clone();
        let documents = Rc::clone(&self.source_documents);
        let theme = self.source_theme.clone();
        let style_scheme = self.source_style_scheme.clone();
        let breakpoints = Rc::clone(&self.breakpoints);
        let insert_handler = Rc::clone(&self.breakpoint_insert_handler);
        let delete_handler = Rc::clone(&self.breakpoint_delete_handler);
        let enabled_handler = Rc::clone(&self.breakpoint_enabled_handler);
        let symbol_handler = Rc::clone(&self.source_symbol_handler);
        let source_roots = Rc::clone(&self.source_roots);
        let status_label = self.status_label.clone();
        let status_detail = self.status_detail.clone();

        self.open_source_button.connect_clicked(move |_| {
            let dialog = gtk::FileDialog::builder()
                .title("Open source files")
                .modal(true)
                .build();
            let source_filter = gtk::FileFilter::new();
            source_filter.set_name(Some("Source files"));
            for pattern in [
                "*.c", "*.h", "*.cc", "*.cpp", "*.cxx", "*.hpp", "*.hh", "*.rs", "*.s", "*.S",
                "*.asm", "*.inc", "*.inl", "*.m", "*.mm", "*.go", "*.zig",
            ] {
                source_filter.add_pattern(pattern);
            }
            let all_filter = gtk::FileFilter::new();
            all_filter.set_name(Some("All files"));
            all_filter.add_pattern("*");
            let filters = gio::ListStore::new::<gtk::FileFilter>();
            filters.append(&source_filter);
            filters.append(&all_filter);
            dialog.set_filters(Some(&filters));
            dialog.set_default_filter(Some(&source_filter));
            if let Some(root) = source_roots.first() {
                dialog.set_initial_folder(Some(&gio::File::for_path(root)));
            }
            let window = window.clone();
            let notebook = notebook.clone();
            let documents = Rc::clone(&documents);
            let theme = theme.clone();
            let style_scheme = style_scheme.clone();
            let breakpoints = Rc::clone(&breakpoints);
            let insert_handler = Rc::clone(&insert_handler);
            let delete_handler = Rc::clone(&delete_handler);
            let enabled_handler = Rc::clone(&enabled_handler);
            let symbol_handler = Rc::clone(&symbol_handler);
            let status_label = status_label.clone();
            let status_detail = status_detail.clone();

            gtk::glib::spawn_future_local(async move {
                let Ok(files) = dialog.open_multiple_future(Some(&window)).await else {
                    return;
                };
                let mut opened = 0_u32;
                let mut failed = Vec::new();
                for index in 0..files.n_items() {
                    let Some(file) = files.item(index).and_downcast::<gio::File>() else {
                        continue;
                    };
                    let Some(path) = file.path() else {
                        failed.push(String::from("non-local source"));
                        continue;
                    };
                    if open_source_document(
                        &path,
                        SourceOpenContext {
                            notebook: &notebook,
                            documents: &documents,
                            theme: &theme,
                            style_scheme: style_scheme.as_ref(),
                            breakpoints: &breakpoints,
                            insert_handler: &insert_handler,
                            delete_handler: &delete_handler,
                            enabled_handler: &enabled_handler,
                            symbol_handler: &symbol_handler,
                        },
                    )
                    .is_some()
                    {
                        opened += 1;
                    } else {
                        failed.push(path.display().to_string());
                    }
                }
                if failed.is_empty() {
                    set_status_widgets(
                        &status_label,
                        &status_detail,
                        "Source",
                        &format!(
                            "Opened {opened} source file{}",
                            if opened == 1 { "" } else { "s" }
                        ),
                        Some("status-ready"),
                    );
                } else {
                    set_status_widgets(
                        &status_label,
                        &status_detail,
                        "Source open failed",
                        &format!("Could not read {}", failed.join(", ")),
                        Some("status-error"),
                    );
                }
            });
        });
    }
}

fn build_topbar(
    config: &LaunchConfig,
    window: &gtk::ApplicationWindow,
    terminal: &vte4::Terminal,
) -> Topbar {
    let topbar = gtk::HeaderBar::new();
    topbar.add_css_class("topbar");
    topbar.set_show_title_buttons(false);

    let title_group = gtk::Box::new(gtk::Orientation::Horizontal, 5);
    title_group.add_css_class("titlebar-identity");
    let title = gtk::Label::new(Some("fgdb"));
    title.add_css_class("app-title");
    title_group.append(&title);
    let title_separator = gtk::Label::new(Some("·"));
    title_separator.add_css_class("muted");
    title_group.append(&title_separator);
    let target = gtk::Label::new(Some(config.target_name()));
    target.add_css_class("target-label");
    target.set_ellipsize(pango::EllipsizeMode::Middle);
    target.set_max_width_chars(32);
    target.set_tooltip_text(Some(config.target_name()));
    title_group.append(&target);
    topbar.set_title_widget(Some(&title_group));

    let leading = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    leading.add_css_class("titlebar-actions");
    let open_source = gtk::Button::with_label("Open source");
    open_source.add_css_class("toolbar-action");
    open_source.set_tooltip_text(Some("Open one or more source files in editor tabs"));
    leading.append(&open_source);
    let load_symbols = gtk::Button::with_label("Load libs");
    load_symbols.add_css_class("toolbar-action");
    load_symbols.set_tooltip_text(Some(
        "Load symbols for shared libraries (useful when auto-solib-add is off)",
    ));
    load_symbols.set_sensitive(false);
    leading.append(&load_symbols);
    let terminal_toggle = gtk::ToggleButton::with_label("Terminal");
    terminal_toggle.add_css_class("toolbar-action");
    terminal_toggle.add_css_class("toolbar-toggle");
    terminal_toggle.set_active(true);
    terminal_toggle.set_tooltip_text(Some("Show or hide the interactive GDB terminal"));
    leading.append(&terminal_toggle);
    let gef_tools = build_gef_tools_menu(terminal, &terminal_toggle);
    leading.append(&gef_tools);
    topbar.pack_start(&leading);

    let controls = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    let run = control_button("Run", "Start or continue the inferior · F5", true);
    let pause = control_button("Pause", "Interrupt the inferior · F6", false);
    let next = control_button("Next", "Step over the current source line · F10", false);
    let step = control_button("Step", "Step into the current source line · F11", false);
    let next_instruction = control_button(
        "Nexti",
        "Execute one machine instruction, stepping over calls · Ctrl+F10",
        false,
    );
    let step_instruction = control_button(
        "Stepi",
        "Execute one machine instruction, stepping into calls · Ctrl+F11",
        false,
    );
    let finish = control_button(
        "Finish",
        "Run until the current function returns · Shift+F11",
        false,
    );
    let until_popover = gtk::Popover::new();
    let until_menu = gtk::Box::new(gtk::Orientation::Vertical, 1);
    until_menu.add_css_class("until-menu");
    until_menu.append(&section_title("RUN UNTIL"));
    let until_actions = [
        ("Current line", "-exec-until"),
        ("Function returns", "-exec-finish"),
        ("Next call", "exec-until call"),
        ("Next return", "exec-until ret"),
        ("Next syscall", "exec-until syscall"),
        ("Next indirect branch", "exec-until indirect-branch"),
        ("Next call / jump / return", "exec-until all-branch"),
        ("Memory access", "exec-until memaccess"),
        ("User code", "exec-until user-code"),
        ("libc code", "exec-until libc-code"),
        ("Region change", "exec-until region-change"),
    ]
    .into_iter()
    .map(|(label, command)| {
        let button = gtk::Button::new();
        let label = gtk::Label::new(Some(label));
        label.set_halign(gtk::Align::Start);
        label.set_hexpand(true);
        button.set_child(Some(&label));
        button.set_halign(gtk::Align::Fill);
        until_menu.append(&button);
        (button, command)
    })
    .collect::<Vec<_>>();
    until_menu.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    let until_condition_entry = gtk::Entry::builder()
        .placeholder_text("$rax == 0")
        .hexpand(true)
        .build();
    until_condition_entry.set_tooltip_text(Some("GDB expression used by GEF exec-until cond"));
    until_menu.append(&until_condition_entry);
    let until_condition_button = gtk::Button::with_label("Expression");
    until_condition_button.add_css_class("inline-action");
    until_menu.append(&until_condition_button);
    until_popover.set_child(Some(&until_menu));
    let until = header_popup_button("Until", &until_popover);
    until.add_css_class("debug-control");
    until.set_tooltip_text(Some("Run until a selected control-flow or memory event"));
    until.set_sensitive(false);
    controls.append(&run);
    controls.append(&pause);
    controls.append(&next);
    controls.append(&step);
    controls.append(&next_instruction);
    controls.append(&step_instruction);
    controls.append(&finish);
    controls.append(&until);
    let status = gtk::Label::new(Some("Starting GDB"));
    status.add_css_class("status-readout");
    let trailing = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    trailing.add_css_class("titlebar-actions");
    trailing.append(&controls);
    trailing.append(&status);
    let window_controls = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    window_controls.add_css_class("window-controls");

    let minimize = window_control_button("−", "Minimize", "minimize");
    let maximize = window_control_button("□", "Maximize or restore", "maximize");
    let close = window_control_button("×", "Close", "close");

    let controlled_window = window.clone();
    minimize.connect_clicked(move |_| controlled_window.minimize());
    let controlled_window = window.clone();
    maximize.connect_clicked(move |_| {
        if controlled_window.is_maximized() {
            controlled_window.unmaximize();
        } else {
            controlled_window.maximize();
        }
    });
    let controlled_window = window.clone();
    close.connect_clicked(move |_| controlled_window.close());

    window_controls.append(&minimize);
    window_controls.append(&maximize);
    window_controls.append(&close);
    trailing.append(&window_controls);
    topbar.pack_end(&trailing);

    Topbar {
        root: topbar,
        open_source_button: open_source,
        load_symbols_button: load_symbols,
        terminal_toggle_button: terminal_toggle,
        run_button: run,
        pause_button: pause,
        next_button: next,
        step_button: step,
        next_instruction_button: next_instruction,
        step_instruction_button: step_instruction,
        finish_button: finish,
        until_button: until,
        until_popover,
        gef_tools_button: gef_tools,
        until_actions,
        until_condition_entry,
        until_condition_button,
        status_label: status,
    }
}

fn build_gef_tools_menu(
    terminal: &vte4::Terminal,
    terminal_toggle: &gtk::ToggleButton,
) -> gtk::ToggleButton {
    let popover = gtk::Popover::new();
    let menu = gtk::Box::new(gtk::Orientation::Vertical, 0);
    menu.add_css_class("gef-tools-menu");
    menu.append(&section_title("GEF / LOW-LEVEL TOOLS"));
    let tools = gtk::Notebook::new();
    tools.add_css_class("gef-tools-tabs");
    for (title, commands) in [
        (
            "Context",
            &[
                ("Current instruction", "xinfo $pc", "xinfo $pc"),
                ("Function arguments", "dumpargs", "dumpargs"),
                ("Current syscall", "syscall-args", "syscall-args"),
                ("Future calls", "future-calls", "future-calls"),
                ("Entire stack frame", "stack-frame", "stack-frame"),
            ][..],
        ),
        (
            "Process",
            &[
                ("Virtual memory map", "vmmap", "vmmap"),
                ("Open file descriptors", "fds", "fds"),
                ("ELF auxiliary vector", "auxv", "auxv"),
                ("Current errno", "errno", "errno"),
                ("Thread-local storage", "tls", "tls"),
                ("Fork following", "follow", "follow"),
            ][..],
        ),
        (
            "Binary",
            &[
                ("Binary protections", "checksec", "checksec"),
                ("GOT / PLT", "got", "got"),
                ("Stack canary", "canary", "canary"),
                (
                    "Exception unwind data",
                    "dwarf-exception-handler",
                    "dwarf-exception-handler",
                ),
                ("Dynamic section", "dynamic", "dynamic"),
                ("Runtime link map", "link-map", "link-map"),
            ][..],
        ),
        (
            "Heap",
            &[
                ("Compact bins", "heap bins-simple", "heap bins-simple"),
                ("Heap arenas", "heap arenas", "heap arenas"),
                ("Heap chunks", "heap chunks", "heap chunks"),
                ("Top chunk", "heap top", "heap top"),
                ("Parsed heap", "heap parse", "heap parse"),
            ][..],
        ),
    ] {
        let page = build_gef_tool_page(commands, terminal, terminal_toggle, &popover);
        tools.append_page(&page, Some(&gtk::Label::new(Some(title))));
    }
    menu.append(&tools);

    menu.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    let expression_row = gtk::Box::new(gtk::Orientation::Horizontal, 1);
    let expression = gtk::Entry::builder()
        .placeholder_text("address or expression")
        .hexpand(true)
        .build();
    expression.set_tooltip_text(Some(
        "Address, expression, or type for xinfo, telescope, and dt",
    ));
    let inspect = gtk::Button::with_label("xinfo");
    let telescope = gtk::Button::with_label("telescope");
    let data_type = gtk::Button::with_label("dt");
    for button in [&inspect, &telescope, &data_type] {
        button.add_css_class("inline-action");
    }
    expression_row.append(&expression);
    expression_row.append(&inspect);
    expression_row.append(&telescope);
    expression_row.append(&data_type);
    menu.append(&expression_row);

    let submit = |prefix: &'static str| {
        let terminal = terminal.clone();
        let terminal_toggle = terminal_toggle.clone();
        let popover = popover.clone();
        let expression = expression.clone();
        Rc::new(move || {
            let expression = expression.text().replace(['\r', '\n'], " ");
            let expression = expression.trim();
            if expression.is_empty() {
                return;
            }
            run_terminal_command(
                &terminal,
                &terminal_toggle,
                &popover,
                &format!("{prefix} {expression}"),
            );
        })
    };
    let inspect_submit = submit("xinfo");
    let submit_for_button = Rc::clone(&inspect_submit);
    inspect.connect_clicked(move |_| submit_for_button());
    let submit_for_button = submit("telescope");
    telescope.connect_clicked(move |_| submit_for_button());
    let submit_for_button = submit("dt");
    data_type.connect_clicked(move |_| submit_for_button());
    expression.connect_activate(move |_| inspect_submit());

    popover.set_child(Some(&menu));
    let button = header_popup_button("GEF tools", &popover);
    button.add_css_class("debug-control");
    button.set_tooltip_text(Some(
        "Run useful bata24/GEF investigations in this debugger's terminal",
    ));
    button.set_sensitive(false);
    button
}

fn build_gef_tool_page(
    commands: &[(&'static str, &'static str, &'static str)],
    terminal: &vte4::Terminal,
    terminal_toggle: &gtk::ToggleButton,
    popover: &gtk::Popover,
) -> gtk::Box {
    let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
    for (label, detail, command) in commands {
        let button = gef_tool_button(label, detail);
        connect_gef_tool(&button, terminal, terminal_toggle, popover, command);
        page.append(&button);
    }
    page
}

fn header_popup_button(label: &str, popover: &gtk::Popover) -> gtk::ToggleButton {
    let button = gtk::ToggleButton::with_label(label);
    button.set_focus_on_click(false);
    popover.set_parent(&button);
    popover.set_position(gtk::PositionType::Bottom);
    let popover_for_toggle = popover.clone();
    button.connect_toggled(move |button| {
        if button.is_active() {
            popover_for_toggle.popup();
        } else {
            popover_for_toggle.popdown();
        }
    });
    let weak_button = button.downgrade();
    popover.connect_closed(move |_| {
        if let Some(button) = weak_button.upgrade()
            && button.is_active()
        {
            button.set_active(false);
        }
    });
    button
}

fn gef_tool_button(label: &str, detail: &str) -> gtk::Button {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let label = gtk::Label::new(Some(label));
    label.set_halign(gtk::Align::Start);
    label.set_hexpand(true);
    let detail = gtk::Label::new(Some(detail));
    detail.add_css_class("gef-command");
    detail.set_halign(gtk::Align::End);
    row.append(&label);
    row.append(&detail);
    gtk::Button::builder().child(&row).build()
}

fn connect_gef_tool(
    button: &gtk::Button,
    terminal: &vte4::Terminal,
    terminal_toggle: &gtk::ToggleButton,
    popover: &gtk::Popover,
    command: &'static str,
) {
    let terminal = terminal.clone();
    let terminal_toggle = terminal_toggle.clone();
    let popover = popover.clone();
    button.connect_clicked(move |_| {
        run_terminal_command(&terminal, &terminal_toggle, &popover, command);
    });
}

fn run_terminal_command(
    terminal: &vte4::Terminal,
    terminal_toggle: &gtk::ToggleButton,
    popover: &gtk::Popover,
    command: &str,
) {
    terminal_toggle.set_active(true);
    popover.popdown();
    terminal.feed_child(format!("\u{15}{command}\n").as_bytes());
    terminal.grab_focus();
}

fn window_control_button(label: &str, tooltip: &str, class: &str) -> gtk::Button {
    let button = gtk::Button::with_label(label);
    button.add_css_class("window-control");
    button.add_css_class(class);
    button.set_focus_on_click(false);
    button.set_tooltip_text(Some(tooltip));
    button
}

fn build_workspace(
    config: &LaunchConfig,
    theme: &Theme,
    source_notebook: &gtk::Notebook,
    terminal: &vte4::Terminal,
    variable_children_handler: &Rc<RefCell<Option<VariableChildrenHandler>>>,
    target_pointer_bits: &Rc<Cell<u32>>,
) -> Workspace {
    let workspace = gtk::Paned::new(gtk::Orientation::Horizontal);
    workspace.add_css_class("workspace-columns");
    workspace.set_vexpand(true);
    workspace.set_position(980);
    workspace.set_shrink_start_child(false);
    workspace.set_resize_start_child(true);
    let inspector = build_inspector(variable_children_handler, target_pointer_bits);
    workspace.set_end_child(Some(&inspector.root));

    let navigation_and_editor = gtk::Paned::new(gtk::Orientation::Horizontal);
    navigation_and_editor.add_css_class("workspace-columns");
    navigation_and_editor.set_position(260);
    navigation_and_editor.set_shrink_start_child(false);
    navigation_and_editor.set_resize_start_child(false);
    let left_sidebar = build_left_sidebar(config, theme);
    navigation_and_editor.set_start_child(Some(&left_sidebar.root));
    navigation_and_editor.set_end_child(Some(&build_editor_panel(source_notebook)));

    let main_and_terminal = gtk::Paned::new(gtk::Orientation::Vertical);
    main_and_terminal.set_position(515);
    main_and_terminal.set_shrink_start_child(false);
    main_and_terminal.set_resize_start_child(true);
    main_and_terminal.set_start_child(Some(&navigation_and_editor));
    let terminal_panel = build_terminal_panel(terminal);
    main_and_terminal.set_end_child(Some(&terminal_panel));
    workspace.set_start_child(Some(&main_and_terminal));
    let layout_panes = vec![
        layout::Pane::new("workspace_inspector", &workspace),
        layout::Pane::new("navigation_source", &navigation_and_editor),
        layout::Pane::new("workspace_terminal", &main_and_terminal),
        layout::Pane::new("locals_instructions", &inspector.context_split),
    ];
    let mut debug_state_panels = inspector.stale_panels.clone();
    debug_state_panels.push(left_sidebar.root.clone().upcast());
    Workspace {
        root: workspace,
        layout_panes,
        terminal_panel,
        status_detail: inspector.status_detail,
        debug_state_panels,
        call_stack_list: left_sidebar.call_stack_list,
        threads_list: left_sidebar.threads_list,
        modules_list: left_sidebar.modules_list,
        locals_store: inspector.locals_store,
        locals_selection: inspector.locals_selection,
        locals_view: inspector.locals_view,
        locals_empty: inspector.locals_empty,
        locals_edit_button: inspector.locals_edit_button,
        instructions_title: inspector.instructions_title,
        instructions_store: inspector.instructions_store,
        instructions_selection: inspector.instructions_selection,
        instructions_view: inspector.instructions_view,
        instructions_empty: inspector.instructions_empty,
        instruction_flow: inspector.instruction_flow,
        instruction_arguments: inspector.instruction_arguments,
        instruction_memory: inspector.instruction_memory,
        register_groups: inspector.register_groups,
        registers_empty: inspector.registers_empty,
        stack_store: inspector.stack_store,
        stack_empty: inspector.stack_empty,
        breakpoints_list: inspector.breakpoints_list,
        delete_all_breakpoints_button: inspector.delete_all_breakpoints_button,
        delete_all_watchpoints_button: inspector.delete_all_watchpoints_button,
        delete_all_catchpoints_button: inspector.delete_all_catchpoints_button,
        event_catchpoint_buttons: inspector.event_catchpoint_buttons,
        watchpoint_expression: inspector.watchpoint_expression,
        watchpoint_access: inspector.watchpoint_access,
        watchpoint_add_button: inspector.watchpoint_add_button,
        signal_detail: inspector.signal_detail,
        signal_buttons: inspector.signal_buttons,
        signal_entry: inspector.signal_entry,
        signal_add_button: inspector.signal_add_button,
        delete_all_signal_catchpoints_button: inspector.delete_all_signal_catchpoints_button,
        memory_region_store: inspector.memory_region_store,
        memory_regions_empty: inspector.memory_regions_empty,
        memory_watch_list: inspector.memory_watch_list,
        memory_watches_empty: inspector.memory_watches_empty,
        memory_address_entry: inspector.memory_address_entry,
        memory_size: inspector.memory_size,
        memory_format: inspector.memory_format,
        memory_add_button: inspector.memory_add_button,
    }
}

fn build_left_sidebar(config: &LaunchConfig, theme: &Theme) -> LeftSidebar {
    let sidebar = gtk::Box::new(gtk::Orientation::Vertical, 4);
    sidebar.add_css_class("sidebar");
    sidebar.set_size_request(190, -1);

    let session_rows = gtk::Box::new(gtk::Orientation::Vertical, 1);
    session_rows.append(&sidebar_row("Target", config.target_name()));
    session_rows.append(&sidebar_row("Debugger", &config.gdb_executable));
    session_rows.append(&sidebar_row("Interface", "GDB/MI 2"));
    session_rows.append(&sidebar_row("Theme", theme.name));
    let session = build_disclosure("SESSION", &session_rows, false, "session-disclosure");
    sidebar.append(&session);
    let call_stack_list = dynamic_list("Frames appear when the target is paused");
    let stack_scrolled = gtk::ScrolledWindow::builder()
        .child(&call_stack_list)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build();
    let threads_list = dynamic_list("Threads appear when the target is paused");
    let threads_scrolled = gtk::ScrolledWindow::builder()
        .child(&threads_list)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build();
    let modules_list = dynamic_list("Modules appear after the inferior starts");
    let modules_scrolled = gtk::ScrolledWindow::builder()
        .child(&modules_list)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build();
    let navigation = gtk::Notebook::new();
    navigation.add_css_class("sidebar-tabs");
    navigation.set_vexpand(true);
    navigation.append_page(&stack_scrolled, Some(&gtk::Label::new(Some("Call Stack"))));
    navigation.append_page(&threads_scrolled, Some(&gtk::Label::new(Some("Threads"))));
    navigation.append_page(&modules_scrolled, Some(&gtk::Label::new(Some("Modules"))));
    sidebar.append(&navigation);
    LeftSidebar {
        root: sidebar,
        call_stack_list,
        threads_list,
        modules_list,
    }
}

fn build_inspector(
    variable_children_handler: &Rc<RefCell<Option<VariableChildrenHandler>>>,
    target_pointer_bits: &Rc<Cell<u32>>,
) -> Inspector {
    let notebook = gtk::Notebook::new();
    notebook.set_size_request(260, -1);
    notebook.set_scrollable(true);
    notebook.add_css_class("panel");

    let state = gtk::Box::new(gtk::Orientation::Vertical, 5);
    state.add_css_class("sidebar");
    let detail = gtk::Label::new(Some("Waiting for the MI channel"));
    detail.add_css_class("status-detail");
    detail.set_halign(gtk::Align::Start);
    detail.set_ellipsize(pango::EllipsizeMode::Middle);
    detail.set_single_line_mode(true);
    let (locals_view, locals_store, locals_selection) =
        build_locals_view(variable_children_handler, target_pointer_bits);
    let locals_empty = empty_label("Values appear when the target is paused");
    let locals_scrolled = gtk::ScrolledWindow::builder()
        .child(&locals_view)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .build();
    let (instructions_view, instructions_store, instructions_selection) = build_instruction_view();
    let instructions_empty = empty_label("Paused target required");
    let instructions_scrolled = gtk::ScrolledWindow::builder()
        .child(&instructions_view)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .build();
    let context = gtk::Paned::new(gtk::Orientation::Vertical);
    context.add_css_class("context-split");
    context.set_vexpand(true);
    context.set_position(310);
    context.set_wide_handle(false);
    context.set_resize_start_child(true);
    context.set_shrink_start_child(false);
    let locals_panel = gtk::Box::new(gtk::Orientation::Vertical, 0);
    locals_panel.set_vexpand(true);
    let locals_header = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    locals_header.add_css_class("subpanel-header");
    let locals_title = section_title("LOCALS / ARGUMENTS");
    locals_title.set_hexpand(true);
    locals_header.append(&locals_title);
    let locals_hint = gtk::Label::new(Some("Click name to expand"));
    locals_hint.add_css_class("muted");
    locals_hint.set_tooltip_text(Some(
        "Click an expandable name or its chevron to open it. Double-click a scalar to edit; the Edit button works for every selected value.",
    ));
    locals_header.append(&locals_hint);
    let locals_edit_button = gtk::Button::with_label("Edit");
    locals_edit_button.add_css_class("inline-action");
    locals_edit_button.set_tooltip_text(Some("Edit the selected value"));
    locals_edit_button.set_sensitive(false);
    locals_header.append(&locals_edit_button);
    locals_panel.append(&locals_header);
    locals_panel.append(&locals_empty);
    locals_panel.append(&locals_scrolled);
    let instructions_panel = gtk::Box::new(gtk::Orientation::Vertical, 0);
    instructions_panel.set_vexpand(true);
    let instructions_title = section_title("INSTRUCTIONS");
    instructions_title.set_ellipsize(pango::EllipsizeMode::End);
    instructions_title.set_hexpand(true);
    instructions_title.set_tooltip_text(Some("INSTRUCTIONS"));
    let instructions_header = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    instructions_header.add_css_class("subpanel-header");
    instructions_header.append(&instructions_title);
    instructions_panel.append(&instructions_header);
    let instruction_insight = gtk::Box::new(gtk::Orientation::Vertical, 0);
    instruction_insight.add_css_class("instruction-insight");
    let instruction_flow = insight_label("Flow information appears at a branch or call");
    let instruction_arguments = insight_label("");
    let instruction_memory = insight_label("");
    instruction_insight.append(&instruction_flow);
    instruction_insight.append(&instruction_arguments);
    instruction_insight.append(&instruction_memory);
    instructions_panel.append(&instruction_insight);
    instructions_panel.append(&instructions_empty);
    instructions_panel.append(&instructions_scrolled);
    context.set_start_child(Some(&locals_panel));
    context.set_end_child(Some(&instructions_panel));
    state.append(&context);

    let registers_page = gtk::Box::new(gtk::Orientation::Vertical, 2);
    registers_page.add_css_class("sidebar");
    registers_page.append(&section_title("REGISTERS"));
    let (registers_view, register_groups) = build_register_view();
    let registers_empty = empty_label("Values appear when the target is paused");
    let registers_scrolled = gtk::ScrolledWindow::builder()
        .child(&registers_view)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .build();
    registers_page.append(&registers_empty);
    registers_page.append(&registers_scrolled);

    let stack_page = gtk::Box::new(gtk::Orientation::Vertical, 2);
    stack_page.add_css_class("sidebar");
    stack_page.append(&build_context_legend());
    stack_page.append(&section_title("STACK"));
    let (stack_view, stack_store, stack_word_inspector) = build_stack_view();
    let stack_empty = empty_label("Stack values appear when the target is paused");
    let stack_scrolled = gtk::ScrolledWindow::builder()
        .child(&stack_view)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .build();
    stack_page.append(&stack_empty);
    stack_page.append(&stack_scrolled);
    stack_page.append(&stack_word_inspector.root);

    let memory_page = gtk::Box::new(gtk::Orientation::Vertical, 3);
    memory_page.add_css_class("sidebar");
    memory_page.append(&section_title("ADD MEMORY WATCH"));
    let memory_controls = gtk::Box::new(gtk::Orientation::Vertical, 3);
    memory_controls.add_css_class("memory-watch-command");
    let expression_row = gtk::Box::new(gtk::Orientation::Horizontal, 3);
    let memory_address_entry = gtk::Entry::builder()
        .placeholder_text("$rsp, ptr + 0x20, or 0x404000")
        .hexpand(true)
        .build();
    memory_address_entry
        .set_tooltip_text(Some("Any GDB expression that resolves to a memory address"));
    let memory_add_button = gtk::Button::with_label("Add watch");
    memory_add_button.add_css_class("inline-action");
    memory_add_button.set_sensitive(false);
    expression_row.append(&memory_address_entry);
    expression_row.append(&memory_add_button);

    let memory_options = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    memory_options.add_css_class("memory-watch-options");
    memory_options.append(&section_title("LENGTH"));
    let memory_size = gtk::SpinButton::with_range(8.0, 4096.0, 8.0);
    memory_size.set_value(128.0);
    memory_size.set_width_chars(4);
    memory_size.set_tooltip_text(Some("Bytes to read"));
    let memory_size_unit = gtk::Label::new(Some("bytes"));
    memory_size_unit.add_css_class("muted");
    let memory_options_spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    memory_options_spacer.set_hexpand(true);
    memory_options.append(&memory_size);
    memory_options.append(&memory_size_unit);
    memory_options.append(&memory_options_spacer);
    memory_options.append(&section_title("DISPLAY"));
    let memory_format = gtk::DropDown::from_strings(&["Bytes", "Words", "Pointers"]);
    memory_format.set_selected(0);
    memory_format.set_tooltip_text(Some("How to group and render the memory values"));
    memory_options.append(&memory_format);
    memory_controls.append(&expression_row);
    memory_controls.append(&memory_options);
    memory_page.append(&memory_controls);
    memory_page.append(&section_title("WATCHES"));
    let memory_watches_empty = empty_label("No memory watches. Add an expression above.");
    memory_page.append(&memory_watches_empty);
    let memory_watch_list = gtk::Box::new(gtk::Orientation::Vertical, 3);
    let memory_watches_scrolled = gtk::ScrolledWindow::builder()
        .child(&memory_watch_list)
        .min_content_height(170)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .build();
    memory_page.append(&memory_watches_scrolled);
    memory_page.append(&section_title("VIRTUAL MEMORY MAP"));
    let (memory_regions_view, memory_region_store) = build_memory_region_view();
    let memory_regions_empty = empty_label("Mappings appear when the target is paused");
    let memory_regions_scrolled = gtk::ScrolledWindow::builder()
        .child(&memory_regions_view)
        .min_content_height(190)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .build();
    memory_page.append(&memory_regions_empty);
    memory_page.append(&memory_regions_scrolled);

    let breakpoints_page = gtk::Box::new(gtk::Orientation::Vertical, 3);
    breakpoints_page.add_css_class("sidebar");
    breakpoints_page.append(&section_title("BREAKPOINTS / WATCHPOINTS"));
    let hint = gtk::Label::new(Some(
        "Click the source gutter to add a breakpoint. Conditions are shown on each row.",
    ));
    hint.add_css_class("muted");
    hint.set_halign(gtk::Align::Start);
    hint.set_wrap(true);
    breakpoints_page.append(&hint);
    let breakpoints_list = dynamic_list("No breakpoints or watchpoints set");
    let breakpoints_scrolled = gtk::ScrolledWindow::builder()
        .child(&breakpoints_list)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build();
    let breakpoint_bulk_actions = gtk::Box::new(gtk::Orientation::Horizontal, 2);
    let delete_all_breakpoints_button = gtk::Button::with_label("Delete all BPs");
    delete_all_breakpoints_button.add_css_class("inline-action");
    delete_all_breakpoints_button.add_css_class("danger-action");
    delete_all_breakpoints_button
        .set_tooltip_text(Some("Delete all breakpoints, preserving watchpoints"));
    delete_all_breakpoints_button.set_sensitive(false);
    let delete_all_watchpoints_button = gtk::Button::with_label("Delete all WPs");
    delete_all_watchpoints_button.add_css_class("inline-action");
    delete_all_watchpoints_button.add_css_class("danger-action");
    delete_all_watchpoints_button
        .set_tooltip_text(Some("Delete all watchpoints, preserving breakpoints"));
    delete_all_watchpoints_button.set_sensitive(false);
    let delete_all_catchpoints_button = gtk::Button::with_label("Delete all CPs");
    delete_all_catchpoints_button.add_css_class("inline-action");
    delete_all_catchpoints_button.add_css_class("danger-action");
    delete_all_catchpoints_button.set_tooltip_text(Some(
        "Delete event catchpoints, preserving signal catchpoints",
    ));
    delete_all_catchpoints_button.set_sensitive(false);
    breakpoint_bulk_actions.append(&delete_all_breakpoints_button);
    breakpoint_bulk_actions.append(&delete_all_watchpoints_button);
    breakpoint_bulk_actions.append(&delete_all_catchpoints_button);
    breakpoints_page.append(&breakpoint_bulk_actions);
    breakpoints_page.append(&breakpoints_scrolled);
    breakpoints_page.append(&section_title("QUICK CATCHPOINTS"));
    let event_catchpoint_grid = gtk::Grid::builder()
        .column_spacing(2)
        .row_spacing(2)
        .column_homogeneous(true)
        .build();
    let event_catchpoint_buttons = EventCatchpoint::ALL
        .into_iter()
        .enumerate()
        .map(|(index, (event, label, tooltip))| {
            let button = gtk::Button::with_label(label);
            button.add_css_class("signal-action");
            button.set_tooltip_text(Some(tooltip));
            button.set_sensitive(false);
            event_catchpoint_grid.attach(&button, (index % 3) as i32, (index / 3) as i32, 1, 1);
            (button, event)
        })
        .collect::<Vec<_>>();
    breakpoints_page.append(&event_catchpoint_grid);
    breakpoints_page.append(&section_title("ADD WATCHPOINT"));
    let watchpoint_controls = gtk::Box::new(gtk::Orientation::Horizontal, 2);
    let watchpoint_expression = gtk::Entry::builder()
        .placeholder_text("variable or address expression")
        .hexpand(true)
        .build();
    watchpoint_expression.set_tooltip_text(Some("Examples: counter, *pointer, *(int*)0x404040"));
    let watchpoint_access = gtk::DropDown::from_strings(&["Write", "Read", "Access"]);
    watchpoint_access.set_selected(0);
    watchpoint_access.set_tooltip_text(Some(
        "Stop on writes, reads, or either kind of memory access",
    ));
    let watchpoint_add_button = gtk::Button::with_label("Add");
    watchpoint_add_button.add_css_class("inline-action");
    watchpoint_add_button.set_sensitive(false);
    watchpoint_controls.append(&watchpoint_expression);
    watchpoint_controls.append(&watchpoint_access);
    watchpoint_controls.append(&watchpoint_add_button);
    breakpoints_page.append(&watchpoint_controls);

    let signals_content = gtk::Box::new(gtk::Orientation::Vertical, 4);
    signals_content.add_css_class("sidebar");
    signals_content.append(&section_title("CURRENT STOP"));
    let signal_detail = gtk::Label::new(Some("No signal at the current stop"));
    signal_detail.add_css_class("signal-detail");
    signal_detail.set_halign(gtk::Align::Start);
    signal_detail.set_wrap(true);
    signal_detail.set_xalign(0.0);
    signals_content.append(&signal_detail);
    let signal_actions_header = gtk::Box::new(gtk::Orientation::Horizontal, 3);
    let signal_actions_title = section_title("COMMON CATCHPOINTS");
    signal_actions_title.set_hexpand(true);
    let delete_all_signal_catchpoints_button = gtk::Button::with_label("Clear catches");
    delete_all_signal_catchpoints_button.add_css_class("inline-action");
    delete_all_signal_catchpoints_button.add_css_class("danger-action");
    delete_all_signal_catchpoints_button.set_tooltip_text(Some(
        "Delete every signal catchpoint without affecting breakpoints or watchpoints",
    ));
    delete_all_signal_catchpoints_button.set_sensitive(false);
    signal_actions_header.append(&signal_actions_title);
    signal_actions_header.append(&delete_all_signal_catchpoints_button);
    signals_content.append(&signal_actions_header);
    let signal_hint = gtk::Label::new(Some(
        "Click a signal to add its catchpoint, active signals are green and click again removes them.",
    ));
    signal_hint.add_css_class("muted");
    signal_hint.set_halign(gtk::Align::Start);
    signal_hint.set_wrap(true);
    signals_content.append(&signal_hint);
    let (common_signal_grid, mut signal_buttons) = build_signal_grid(COMMON_SIGNALS);
    signals_content.append(&common_signal_grid);
    let (more_signal_grid, mut more_signal_buttons) = build_signal_grid(MORE_SIGNALS);
    signal_buttons.append(&mut more_signal_buttons);
    signals_content.append(&build_disclosure(
        "MORE POSIX SIGNALS",
        &more_signal_grid,
        false,
        "signal-disclosure",
    ));
    signals_content.append(&section_title("CUSTOM SIGNAL"));
    let custom_signal_row = gtk::Box::new(gtk::Orientation::Horizontal, 2);
    let signal_entry = gtk::Entry::builder()
        .placeholder_text("SIGRTMIN+1 or 35")
        .hexpand(true)
        .build();
    signal_entry.set_tooltip_text(Some(
        "Signal name or number; names without the SIG prefix are normalized",
    ));
    let signal_add_button = gtk::Button::with_label("Toggle catch");
    signal_add_button.add_css_class("inline-action");
    signal_add_button.set_sensitive(false);
    custom_signal_row.append(&signal_entry);
    custom_signal_row.append(&signal_add_button);
    signals_content.append(&custom_signal_row);
    let signals_page = gtk::ScrolledWindow::builder()
        .child(&signals_content)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build();

    notebook.append_page(&state, Some(&gtk::Label::new(Some("Context"))));
    notebook.append_page(&registers_page, Some(&gtk::Label::new(Some("Registers"))));
    notebook.append_page(&stack_page, Some(&gtk::Label::new(Some("Stack"))));
    notebook.append_page(&memory_page, Some(&gtk::Label::new(Some("Memory"))));
    notebook.append_page(
        &breakpoints_page,
        Some(&gtk::Label::new(Some("Breakpoints"))),
    );
    notebook.append_page(&signals_page, Some(&gtk::Label::new(Some("Signals"))));
    let stale_panels = vec![
        state.clone().upcast(),
        registers_page.clone().upcast(),
        stack_page.clone().upcast(),
        memory_page.clone().upcast(),
        signals_page.clone().upcast(),
    ];
    Inspector {
        root: notebook,
        context_split: context,
        status_detail: detail,
        stale_panels,
        locals_store,
        locals_selection,
        locals_view,
        locals_empty,
        locals_edit_button,
        instructions_title,
        instructions_store,
        instructions_selection,
        instructions_view,
        instructions_empty,
        instruction_flow,
        instruction_arguments,
        instruction_memory,
        register_groups,
        registers_empty,
        stack_store,
        stack_empty,
        breakpoints_list,
        delete_all_breakpoints_button,
        delete_all_watchpoints_button,
        delete_all_catchpoints_button,
        event_catchpoint_buttons,
        watchpoint_expression,
        watchpoint_access,
        watchpoint_add_button,
        signal_detail,
        signal_buttons,
        signal_entry,
        signal_add_button,
        delete_all_signal_catchpoints_button,
        memory_region_store,
        memory_regions_empty,
        memory_watch_list,
        memory_watches_empty,
        memory_address_entry,
        memory_size,
        memory_format,
        memory_add_button,
    }
}

fn build_locals_view(
    children_handler: &Rc<RefCell<Option<VariableChildrenHandler>>>,
    target_pointer_bits: &Rc<Cell<u32>>,
) -> (gtk::ColumnView, gio::ListStore, gtk::SingleSelection) {
    let store = gio::ListStore::new::<glib::BoxedAnyObject>();
    let tree = gtk::TreeListModel::new(store.clone(), false, false, |item| {
        let item = item.downcast_ref::<glib::BoxedAnyObject>()?;
        let node = item.borrow::<VariableNode>();
        node.variable
            .can_expand()
            .then(|| node.children.clone().upcast())
    });
    let selection = gtk::SingleSelection::new(Some(tree));
    selection.set_autoselect(true);
    selection.set_can_unselect(false);
    let view = gtk::ColumnView::new(Some(selection.clone()));
    view.add_css_class("debug-table");
    view.add_css_class("locals-table");
    view.set_vexpand(true);
    view.set_single_click_activate(false);
    view.set_reorderable(true);

    view.append_column(&local_name_column(&selection, children_handler));
    view.append_column(&local_text_column(
        "TYPE",
        155,
        false,
        LocalColumn::Type,
        Rc::clone(target_pointer_bits),
    ));
    view.append_column(&local_text_column(
        "VALUE",
        190,
        false,
        LocalColumn::Value,
        Rc::clone(target_pointer_bits),
    ));
    view.append_column(&local_text_column(
        "DETAILS",
        300,
        true,
        LocalColumn::Details,
        Rc::clone(target_pointer_bits),
    ));
    (view, store, selection)
}

fn insight_label(placeholder: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(placeholder));
    label.add_css_class("instruction-insight-line");
    label.set_halign(gtk::Align::Start);
    label.set_xalign(0.0);
    label.set_ellipsize(pango::EllipsizeMode::End);
    label.set_selectable(true);
    label.set_visible(!placeholder.is_empty());
    label
}

fn build_memory_region_view() -> (gtk::ColumnView, gio::ListStore) {
    let store = gio::ListStore::new::<glib::BoxedAnyObject>();
    let selection = gtk::NoSelection::new(Some(store.clone()));
    let view = gtk::ColumnView::new(Some(selection));
    view.add_css_class("debug-table");
    view.add_css_class("memory-map-table");
    view.set_vexpand(true);
    view.set_reorderable(true);
    for (title, width, expand, column) in [
        ("START", 175, false, MemoryColumn::Start),
        ("END", 175, false, MemoryColumn::End),
        ("SIZE", 90, false, MemoryColumn::Size),
        ("PERM", 65, false, MemoryColumn::Permissions),
        ("PATH", 280, true, MemoryColumn::Path),
    ] {
        view.append_column(&memory_region_column(title, width, expand, column));
    }
    (view, store)
}

fn memory_region_column(
    title: &str,
    width: i32,
    expand: bool,
    column: MemoryColumn,
) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(move |_, object| {
        let Some(item) = object.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let label = gtk::Label::new(None);
        label.add_css_class("debug-table-cell");
        label.set_halign(gtk::Align::Start);
        label.set_ellipsize(pango::EllipsizeMode::Middle);
        label.set_selectable(true);
        item.set_child(Some(&label));
    });
    factory.connect_bind(move |_, object| {
        let Some(item) = object.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let (Some(label), Some(data)) = (
            item.child().and_downcast::<gtk::Label>(),
            item.item().and_downcast::<glib::BoxedAnyObject>(),
        ) else {
            return;
        };
        let region = data.borrow::<MemoryRegion>();
        reset_semantic_css(&label);
        label.add_css_class(memory_kind_css(region.kind));
        let text = match column {
            MemoryColumn::Start => format!("0x{:016x}", region.start),
            MemoryColumn::End => format!("0x{:016x}", region.end),
            MemoryColumn::Size => format_memory_size(region.end.saturating_sub(region.start)),
            MemoryColumn::Permissions => region.permissions.clone(),
            MemoryColumn::Path => region
                .path
                .clone()
                .unwrap_or_else(|| String::from("anonymous")),
        };
        label.set_text(&text);
        label.set_tooltip_text(Some(&format!(
            "0x{:016x}–0x{:016x} · {}",
            region.start,
            region.end,
            region.description()
        )));
    });
    let column = gtk::ColumnViewColumn::new(Some(title), Some(factory));
    column.set_fixed_width(width);
    column.set_resizable(true);
    column.set_expand(expand);
    column
}

fn format_memory_size(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

fn add_memory_watch(
    list: &gtk::Box,
    empty: &gtk::Label,
    watches: &Rc<RefCell<Vec<MemoryWatchView>>>,
    handler: &Rc<RefCell<Option<MemoryWatchHandler>>>,
    expression: String,
    byte_count: usize,
    format: MemoryWatchFormat,
) {
    let existing = {
        let watches = watches.borrow();
        watches
            .iter()
            .find(|watch| {
                watch.expression == expression
                    && watch.byte_count == byte_count
                    && watch.format == format
            })
            .cloned()
    };
    if let Some(watch) = existing {
        watch.status.remove_css_class("memory-watch-error");
        watch.status.set_text("reading…");
        if let Some(handler) = handler.borrow().as_ref() {
            handler(watch.id, expression, byte_count);
        }
        return;
    }

    let id = watches
        .borrow()
        .iter()
        .map(|watch| watch.id)
        .max()
        .unwrap_or(0)
        .wrapping_add(1);
    let row = gtk::Box::new(gtk::Orientation::Vertical, 1);
    row.add_css_class("memory-watch");
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 3);
    let title = gtk::Label::new(Some(&expression));
    title.add_css_class("local-name");
    title.set_halign(gtk::Align::Start);
    title.set_ellipsize(pango::EllipsizeMode::Middle);
    title.set_hexpand(true);
    title.set_tooltip_text(Some(&expression));
    let metadata = gtk::Label::new(Some(&format!(
        "{} · {}",
        format_memory_size(byte_count as u64),
        format.label()
    )));
    metadata.add_css_class("memory-watch-format");
    metadata.set_tooltip_text(Some("Requested length and display format"));
    let refresh = gtk::Button::with_label("Refresh");
    refresh.add_css_class("inline-action");
    refresh.set_tooltip_text(Some("Read this memory expression again"));
    let remove = gtk::Button::with_label("Remove");
    remove.add_css_class("inline-action");
    remove.add_css_class("danger-action");
    remove.set_tooltip_text(Some("Remove this memory watch"));
    header.append(&title);
    header.append(&metadata);
    header.append(&refresh);
    header.append(&remove);
    let status = gtk::Label::new(Some("reading…"));
    status.add_css_class("muted");
    status.set_halign(gtk::Align::Start);
    status.set_ellipsize(pango::EllipsizeMode::Middle);
    let output = gtk::Grid::builder().column_spacing(10).build();
    output.add_css_class("memory-watch-output");
    let output_addresses = memory_watch_column(
        "memory-watch-addresses",
        "Addresses · select or copy this column independently",
    );
    let output_values = memory_watch_column(
        "memory-watch-values",
        "Raw hexadecimal values · select or copy this column independently",
    );
    let output_decoded = memory_watch_column(
        "memory-watch-decoded",
        "Decoded bytes · select or copy this column independently",
    );
    output.attach(&output_addresses, 0, 0, 1, 1);
    output.attach(&output_values, 1, 0, 1, 1);
    output.attach(&output_decoded, 2, 0, 1, 1);
    row.append(&header);
    row.append(&status);
    row.append(&output);
    list.append(&row);
    empty.set_visible(false);

    let weak_watches = Rc::downgrade(watches);
    let weak_row = row.downgrade();
    let list_for_remove = list.clone();
    let empty_for_remove = empty.clone();
    remove.connect_clicked(move |_| {
        if let Some(row) = weak_row.upgrade() {
            list_for_remove.remove(&row);
        }
        if let Some(watches) = weak_watches.upgrade() {
            let is_empty = {
                let mut watches = watches.borrow_mut();
                watches.retain(|watch| watch.id != id);
                watches.is_empty()
            };
            empty_for_remove.set_visible(is_empty);
        }
    });
    let handler_for_refresh = Rc::clone(handler);
    let expression_for_refresh = expression.clone();
    let status_for_refresh = status.clone();
    refresh.connect_clicked(move |_| {
        status_for_refresh.remove_css_class("memory-watch-error");
        status_for_refresh.set_text("reading…");
        if let Some(handler) = handler_for_refresh.borrow().as_ref() {
            handler(id, expression_for_refresh.clone(), byte_count);
        }
    });
    watches.borrow_mut().push(MemoryWatchView {
        id,
        expression: expression.clone(),
        byte_count,
        format,
        status,
        output_addresses,
        output_values,
        output_decoded,
    });
    if let Some(handler) = handler.borrow().as_ref() {
        handler(id, expression, byte_count);
    }
}

fn memory_watch_column(css_class: &str, tooltip: &str) -> gtk::Label {
    let label = gtk::Label::new(None);
    label.add_css_class(css_class);
    label.set_halign(gtk::Align::Start);
    label.set_valign(gtk::Align::Start);
    label.set_xalign(0.0);
    label.set_yalign(0.0);
    label.set_selectable(true);
    label.set_tooltip_text(Some(tooltip));
    label
}

fn format_memory_watch(begin: u64, bytes: &[u8], format: MemoryWatchFormat) -> MemoryWatchText {
    use std::fmt::Write as _;

    let chunk_size = match format {
        MemoryWatchFormat::Words => 4,
        MemoryWatchFormat::Bytes | MemoryWatchFormat::Pointers => 8,
    };
    let line_count = bytes.len().div_ceil(chunk_size);
    let mut addresses = String::with_capacity(line_count * 19);
    let mut values = String::with_capacity(bytes.len() * 3 + line_count);
    let mut decoded = String::with_capacity(bytes.len() + line_count);
    for (index, chunk) in bytes.chunks(chunk_size).enumerate() {
        if index != 0 {
            addresses.push('\n');
            values.push('\n');
            decoded.push('\n');
        }
        let _ = write!(addresses, "0x{:016x}", begin + (index * chunk_size) as u64);
        match format {
            MemoryWatchFormat::Bytes => push_hex_bytes(&mut values, chunk),
            MemoryWatchFormat::Words => match <[u8; 4]>::try_from(chunk) {
                Ok(chunk) => {
                    let _ = write!(values, "0x{:08x}", u32::from_le_bytes(chunk));
                }
                Err(_) => push_hex_bytes(&mut values, chunk),
            },
            MemoryWatchFormat::Pointers => match <[u8; 8]>::try_from(chunk) {
                Ok(chunk) => {
                    let _ = write!(values, "0x{:016x}", u64::from_le_bytes(chunk));
                }
                Err(_) => push_hex_bytes(&mut values, chunk),
            },
        }
        decoded.extend(chunk.iter().map(|byte| {
            if byte.is_ascii_graphic() || *byte == b' ' {
                char::from(*byte)
            } else {
                '·'
            }
        }));
    }
    MemoryWatchText {
        addresses,
        values,
        decoded,
    }
}

fn push_hex_bytes(output: &mut String, bytes: &[u8]) {
    use std::fmt::Write as _;

    for (index, byte) in bytes.iter().enumerate() {
        if index != 0 {
            output.push(' ');
        }
        let _ = write!(output, "{byte:02x}");
    }
}

fn local_name_column(
    selection: &gtk::SingleSelection,
    children_handler: &Rc<RefCell<Option<VariableChildrenHandler>>>,
) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    let selection = selection.clone();
    let children_handler_for_setup = Rc::clone(children_handler);
    factory.connect_setup(move |_, object| {
        let Some(item) = object.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let label = gtk::Label::new(None);
        label.add_css_class("debug-table-cell");
        label.add_css_class("local-name");
        label.set_halign(gtk::Align::Start);
        label.set_ellipsize(pango::EllipsizeMode::End);
        label.set_hexpand(true);
        let expander = gtk::TreeExpander::new();
        expander.set_hexpand(true);
        expander.set_child(Some(&label));

        let click = gtk::GestureClick::new();
        click.set_button(gtk::gdk::BUTTON_PRIMARY);
        click.set_propagation_phase(gtk::PropagationPhase::Capture);
        let expander_for_click = expander.clone();
        let selection = selection.clone();
        let children_handler = Rc::clone(&children_handler_for_setup);
        click.connect_pressed(move |gesture, presses, _, _| {
            if presses != 1 {
                return;
            }
            let Some(row) = expander_for_click.list_row() else {
                return;
            };
            let node = row
                .item()
                .and_then(|item| item.downcast::<glib::BoxedAnyObject>().ok())
                .map(|item| item.borrow::<VariableNode>().clone());
            let Some(node) = node else {
                return;
            };
            if !row.is_expandable() && node.load_more.is_none() {
                return;
            }
            selection.set_selected(row.position());
            if row.is_expandable() {
                row.set_expanded(!row.is_expanded());
            } else {
                request_next_variable_page_if_needed(&node, &children_handler);
            }
            gesture.set_state(gtk::EventSequenceState::Claimed);
        });
        label.add_controller(click);
        item.set_child(Some(&expander));
    });
    let children_handler = Rc::clone(children_handler);
    factory.connect_bind(move |_, object| {
        let Some(item) = object.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let (Some(expander), Some(row)) = (
            item.child().and_downcast::<gtk::TreeExpander>(),
            item.item().and_downcast::<gtk::TreeListRow>(),
        ) else {
            return;
        };
        let Some(data) = row.item().and_downcast::<glib::BoxedAnyObject>() else {
            return;
        };
        let node = data.borrow::<VariableNode>();
        let Some(label) = expander.child().and_downcast::<gtk::Label>() else {
            return;
        };
        expander.set_list_row(Some(&row));
        label.set_text(&node.variable.name);
        let expandable = node.variable.can_expand();
        if expandable && !node.expansion_observer_attached.replace(true) {
            let node = node.clone();
            let children_handler = Rc::clone(&children_handler);
            row.connect_expanded_notify(move |row| {
                if row.is_expanded() {
                    request_variable_children_if_needed(&node, &children_handler);
                }
            });
        }
        let load_more = node.load_more.is_some();
        if expandable || load_more {
            label.add_css_class("local-expandable");
            label.set_cursor_from_name(Some("pointer"));
        } else {
            label.remove_css_class("local-expandable");
            label.set_cursor(None);
        }
        let tooltip = if node.placeholder {
            format!("{}\n{}", node.variable.name, node.variable.value)
        } else {
            variable_tooltip(&node.variable)
        };
        label.set_tooltip_text(Some(&tooltip));
        label.remove_css_class("local-load-more");
        if load_more {
            label.remove_css_class("muted");
            label.remove_css_class("local-name");
            label.add_css_class("local-load-more");
        } else if node.placeholder {
            label.remove_css_class("local-name");
            label.add_css_class("muted");
        } else {
            label.remove_css_class("muted");
            label.add_css_class("local-name");
        }
    });
    let column = gtk::ColumnViewColumn::new(Some("NAME / EXPRESSION"), Some(factory));
    column.set_fixed_width(175);
    column.set_resizable(true);
    column
}

fn request_variable_children_if_needed(
    node: &VariableNode,
    children_handler: &Rc<RefCell<Option<VariableChildrenHandler>>>,
) {
    if node.children_loaded.get() || node.children_loading.replace(true) {
        return;
    }
    node.children
        .append(&glib::BoxedAnyObject::new(VariableNode::placeholder(
            "loading…",
            "waiting for GDB",
        )));
    if let Some(handler) = children_handler.borrow().as_ref() {
        handler(node.variable.clone(), 0);
    } else {
        node.children.remove_all();
        node.children_loading.set(false);
    }
}

fn request_next_variable_page_if_needed(
    node: &VariableNode,
    children_handler: &Rc<RefCell<Option<VariableChildrenHandler>>>,
) {
    let Some((parent, from)) = node.load_more.as_ref() else {
        return;
    };
    if node.children_loading.replace(true) {
        return;
    }
    if let Some(handler) = children_handler.borrow().as_ref() {
        handler(parent.clone(), *from);
    } else {
        node.children_loading.set(false);
    }
}

fn local_text_column(
    title: &str,
    width: i32,
    expand: bool,
    column: LocalColumn,
    target_pointer_bits: Rc<Cell<u32>>,
) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(move |_, object| {
        let Some(item) = object.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let label = gtk::Label::new(None);
        label.add_css_class("debug-table-cell");
        label.add_css_class(match column {
            LocalColumn::Type => "local-type",
            LocalColumn::Value => "local-value",
            LocalColumn::Details => "local-details",
        });
        label.set_halign(gtk::Align::Start);
        label.set_ellipsize(pango::EllipsizeMode::End);
        label.set_selectable(true);
        item.set_child(Some(&label));
    });
    factory.connect_bind(move |_, object| {
        let Some(item) = object.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let (Some(label), Some(row)) = (
            item.child().and_downcast::<gtk::Label>(),
            item.item().and_downcast::<gtk::TreeListRow>(),
        ) else {
            return;
        };
        let Some(data) = row.item().and_downcast::<glib::BoxedAnyObject>() else {
            return;
        };
        let node = data.borrow::<VariableNode>();
        let variable = &node.variable;
        let (value, details) = variable_value_parts(&variable.value);
        label.remove_css_class("local-details-error");
        match column {
            LocalColumn::Type => {
                label.set_text(variable.type_name.as_deref().unwrap_or("<unknown>"));
            }
            LocalColumn::Value => label.set_text(value),
            LocalColumn::Details => {
                let decoded = variable_details(variable, value, details, target_pointer_bits.get());
                label.set_text(&decoded);
                if decoded.contains("<error:") {
                    label.add_css_class("local-details-error");
                }
            }
        }
        if node.placeholder {
            label.add_css_class("muted");
        } else {
            label.remove_css_class("muted");
        }
        let tooltip = if node.placeholder {
            format!("{}\n{}", variable.name, variable.value)
        } else {
            variable_tooltip(variable)
        };
        label.set_tooltip_text(Some(&tooltip));
    });
    let column_view = gtk::ColumnViewColumn::new(Some(title), Some(factory));
    column_view.set_fixed_width(width);
    column_view.set_resizable(true);
    column_view.set_expand(expand);
    column_view
}

fn variable_value_parts(value: &str) -> (&str, &str) {
    let value = value.trim();
    let Some(separator) = value.find(char::is_whitespace) else {
        return (value, "");
    };
    let (raw, remainder) = value.split_at(separator);
    let details = remainder.trim_start();
    let raw_is_address = raw.strip_prefix("0x").is_some_and(|digits| {
        !digits.is_empty() && digits.chars().all(|digit| digit.is_ascii_hexdigit())
    });
    let raw_is_number = raw
        .strip_prefix('-')
        .unwrap_or(raw)
        .chars()
        .all(|digit| digit.is_ascii_digit());
    let details_describe_value = details.starts_with(['"', '\'', '<']) || details.starts_with("->");
    if (raw_is_address || raw_is_number) && details_describe_value {
        (raw, details)
    } else {
        (value, "")
    }
}

fn variable_details(
    variable: &Variable,
    value: &str,
    details: &str,
    target_pointer_bits: u32,
) -> String {
    let Some(decimal) = integer_decimal_value(variable, value, target_pointer_bits) else {
        return details.to_owned();
    };
    if details.is_empty() {
        decimal
    } else {
        format!("{decimal}  ·  {details}")
    }
}

fn variable_tooltip(variable: &Variable) -> String {
    let interaction = if variable.can_expand() {
        "Click the name or press Enter to expand; use Edit to change the value"
    } else {
        "Double-click or press Enter to edit"
    };
    format!(
        "{}  {}\n{}\n{} child{}\n{interaction}",
        variable.type_name.as_deref().unwrap_or("<unknown type>"),
        variable.name,
        variable.value,
        variable.num_children,
        if variable.num_children == 1 {
            ""
        } else {
            "ren"
        }
    )
}

fn build_instruction_view() -> (gtk::ColumnView, gio::ListStore, gtk::SingleSelection) {
    let store = gio::ListStore::new::<glib::BoxedAnyObject>();
    let selection = gtk::SingleSelection::new(Some(store.clone()));
    selection.set_autoselect(false);
    selection.set_can_unselect(true);
    let view = gtk::ColumnView::new(Some(selection.clone()));
    view.add_css_class("instruction-table");
    view.add_css_class("debug-table");
    view.set_vexpand(true);
    view.set_single_click_activate(false);
    view.set_reorderable(true);
    view.set_tooltip_text(Some(
        "Double-click an instruction to toggle an address breakpoint",
    ));

    for column in [
        instruction_column(
            "ADDRESS",
            170,
            false,
            "instruction-address",
            &selection,
            |row| {
                let marker = if row.current { "›" } else { " " };
                format!("{marker} {}", full_address(&row.instruction.address))
            },
        ),
        instruction_column(
            "OPCODE",
            72,
            false,
            "instruction-mnemonic",
            &selection,
            |row| split_instruction(&row.instruction.text).0.to_owned(),
        ),
        instruction_column(
            "OPERANDS",
            180,
            true,
            "instruction-operands",
            &selection,
            |row| split_instruction(&row.instruction.text).1.to_owned(),
        ),
        instruction_column(
            "BYTES",
            130,
            false,
            "instruction-opcodes",
            &selection,
            |row| {
                row.instruction
                    .opcodes
                    .clone()
                    .unwrap_or_else(|| String::from("unavailable"))
            },
        ),
        instruction_column(
            "SYMBOL",
            140,
            false,
            "instruction-symbol",
            &selection,
            |row| instruction_symbol(&row.instruction),
        ),
    ] {
        view.append_column(&column);
    }
    (view, store, selection)
}

fn build_register_view() -> (gtk::Box, Vec<RegisterGroupView>) {
    let content = gtk::Box::new(gtk::Orientation::Vertical, 4);
    content.add_css_class("register-groups");
    content.set_hexpand(true);
    let mut groups = Vec::new();
    for (title, kind) in [
        ("GENERAL PURPOSE", RegisterGroupKind::General),
        ("THREAD BASES", RegisterGroupKind::Bases),
        ("FLAGS", RegisterGroupKind::Flags),
        ("SEGMENTS", RegisterGroupKind::Segments),
        ("SIMD / VECTOR", RegisterGroupKind::Vector),
        ("FLOATING POINT", RegisterGroupKind::FloatingPoint),
        ("OTHER", RegisterGroupKind::Other),
    ] {
        let (view, store) = build_register_group_table();
        let panel = gtk::Box::new(gtk::Orientation::Vertical, 0);
        panel.add_css_class("register-group-panel");
        panel.set_visible(false);
        let title_label = section_title(title);
        title_label.add_css_class("register-section");
        title_label.set_hexpand(true);
        title_label.set_xalign(0.0);
        panel.append(&title_label);
        panel.append(&view);
        content.append(&panel);
        groups.push(RegisterGroupView {
            kind,
            store,
            view,
            panel,
        });
    }
    (content, groups)
}

fn build_register_group_table() -> (gtk::ColumnView, gio::ListStore) {
    let store = gio::ListStore::new::<glib::BoxedAnyObject>();
    let selection = gtk::SingleSelection::new(Some(store.clone()));
    selection.set_autoselect(false);
    selection.set_can_unselect(true);
    let view = gtk::ColumnView::new(Some(selection.clone()));
    view.add_css_class("debug-table");
    view.add_css_class("register-table");
    view.set_hexpand(true);
    view.set_reorderable(true);
    view.set_single_click_activate(false);

    for (title, width, expand, column) in [
        ("REGISTER", 90, false, RegisterColumn::Name),
        ("VALUE", 185, false, RegisterColumn::Value),
        ("POINTER CHAIN / FLAGS", 330, true, RegisterColumn::Details),
    ] {
        view.append_column(&register_column(title, width, expand, column));
    }
    (view, store)
}

fn register_column(
    title: &str,
    width: i32,
    expand: bool,
    column: RegisterColumn,
) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(move |_, object| {
        let Some(item) = object.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let label = gtk::Label::new(None);
        label.add_css_class("debug-table-cell");
        label.add_css_class(register_column_css(column));
        label.set_halign(gtk::Align::Start);
        label.set_ellipsize(pango::EllipsizeMode::End);
        label.set_selectable(!matches!(column, RegisterColumn::Name));
        item.set_child(Some(&label));
    });
    factory.connect_bind(move |_, object| {
        let Some(item) = object.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let (Some(label), Some(data)) = (
            item.child().and_downcast::<gtk::Label>(),
            item.item().and_downcast::<glib::BoxedAnyObject>(),
        ) else {
            return;
        };
        let data = data.borrow::<RegisterRowData>();
        reset_semantic_css(&label);
        if data.changed && matches!(column, RegisterColumn::Name) {
            label.add_css_class("modified-register");
        }
        let semantic_class = register_value_css(&data.register);
        match column {
            RegisterColumn::Name => {
                label.set_text(&format!("${}:", data.register.name));
                label.set_tooltip_text(Some(&format!(
                    "{}\nDouble-click or press Enter to edit",
                    data.register.name
                )));
            }
            RegisterColumn::Value => {
                label.add_css_class(semantic_class);
                let text = register_primary_value(&data.register);
                label.set_text(&text);
                label.set_tooltip_text(Some(&format!(
                    "{}\nDouble-click or press Enter to edit",
                    register_text(&data.register)
                )));
            }
            RegisterColumn::Details => {
                label.add_css_class(semantic_class);
                if is_flags_register(&data.register.name) {
                    label.set_markup(&flags_details_markup(&data.register.value, data.ring));
                } else {
                    label.set_text(&register_details(&data.register));
                }
                label.set_tooltip_text(Some(&format!(
                    "{}\nDouble-click or press Enter to edit",
                    register_text(&data.register)
                )));
            }
        }
    });
    let column_view = gtk::ColumnViewColumn::new(Some(title), Some(factory));
    column_view.set_fixed_width(width);
    column_view.set_resizable(true);
    column_view.set_expand(expand);
    column_view
}

fn build_stack_view() -> (gtk::ColumnView, gio::ListStore, StackWordInspector) {
    let store = gio::ListStore::new::<glib::BoxedAnyObject>();
    let selection = gtk::SingleSelection::new(Some(store.clone()));
    selection.set_autoselect(true);
    selection.set_can_unselect(false);
    let view = gtk::ColumnView::new(Some(selection.clone()));
    view.add_css_class("debug-table");
    view.add_css_class("stack-table");
    view.set_vexpand(true);
    view.set_reorderable(true);

    for (title, width, expand, column) in [
        ("ANCHOR", 80, false, StackColumn::Anchor),
        ("ADDRESS", 175, false, StackColumn::Address),
        ("VALUE / POINTER CHAIN", 285, true, StackColumn::Value),
        ("OFFSET", 82, false, StackColumn::Offset),
        ("INDEX", 62, false, StackColumn::Index),
        ("REFERENCES", 155, false, StackColumn::References),
        ("REGION", 210, false, StackColumn::Region),
    ] {
        view.append_column(&stack_column(title, width, expand, column, &selection));
    }
    let inspector = build_stack_word_inspector();
    let inspector_for_selection = inspector.clone();
    selection.connect_selected_item_notify(move |selection| {
        let Some(data) = selection
            .selected_item()
            .and_downcast::<glib::BoxedAnyObject>()
        else {
            inspector_for_selection.clear();
            return;
        };
        inspector_for_selection.show(&data.borrow::<StackEntry>());
    });
    (view, store, inspector)
}

fn build_stack_word_inspector() -> StackWordInspector {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 2);
    root.add_css_class("stack-word-inspector");
    root.append(&section_title("SELECTED WORD"));
    let grid = gtk::Grid::builder()
        .column_spacing(8)
        .row_spacing(1)
        .build();
    let address = stack_inspector_row(&grid, 0, "ADDRESS");
    let raw = stack_inspector_row(&grid, 1, "RAW");
    let interpretation = stack_inspector_row(&grid, 2, "INTERPRETATION");
    let role = stack_inspector_row(&grid, 3, "ROLE");
    let region = stack_inspector_row(&grid, 4, "REGION");
    for value in [&interpretation, &role, &region] {
        value.set_ellipsize(pango::EllipsizeMode::None);
        value.set_wrap(true);
        value.set_wrap_mode(pango::WrapMode::Char);
    }
    root.append(&grid);
    let inspector = StackWordInspector {
        root,
        address,
        raw,
        interpretation,
        role,
        region,
    };
    inspector.clear();
    inspector
}

fn stack_inspector_row(grid: &gtk::Grid, row: i32, title: &str) -> gtk::Label {
    let title = gtk::Label::new(Some(title));
    title.add_css_class("stack-inspector-key");
    title.set_halign(gtk::Align::Start);
    grid.attach(&title, 0, row, 1, 1);
    let value = gtk::Label::new(None);
    value.add_css_class("stack-inspector-value");
    value.set_halign(gtk::Align::Start);
    value.set_hexpand(true);
    value.set_selectable(true);
    value.set_ellipsize(pango::EllipsizeMode::Middle);
    grid.attach(&value, 1, row, 1, 1);
    value
}

impl StackWordInspector {
    fn clear(&self) {
        self.address.set_text("Select a stack word");
        self.raw.set_text("");
        self.interpretation.set_text("");
        self.role.set_text("");
        self.region.set_text("");
        reset_semantic_css(&self.interpretation);
    }

    fn show(&self, entry: &StackEntry) {
        self.address.set_text(&format!(
            "0x{:016x}  ·  SP+0x{:x}  ·  word {}",
            entry.address, entry.offset, entry.index
        ));
        self.address.set_tooltip_text(Some(&self.address.text()));
        self.raw.set_text(&entry.value);
        self.raw.set_tooltip_text(Some(&entry.value));
        let interpretation = stack_entry_text(entry);
        self.interpretation.set_text(&interpretation);
        self.interpretation.set_tooltip_text(Some(&interpretation));
        reset_semantic_css(&self.interpretation);
        self.interpretation
            .add_css_class(memory_kind_css(entry.memory_kind));
        let role = stack_word_role(entry);
        self.role.set_text(&role);
        self.role.set_tooltip_text(Some(&role));
        let region = entry.region.as_deref().unwrap_or("unmapped / scalar");
        self.region.set_text(region);
        self.region.set_tooltip_text(Some(region));
    }
}

fn stack_column(
    title: &str,
    width: i32,
    expand: bool,
    column: StackColumn,
    selection: &gtk::SingleSelection,
) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    let selection = selection.clone();
    factory.connect_setup(move |_, object| {
        let Some(item) = object.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let label = gtk::Label::new(None);
        label.add_css_class("debug-table-cell");
        label.add_css_class(stack_column_css(column));
        label.set_halign(gtk::Align::Start);
        label.set_ellipsize(pango::EllipsizeMode::End);
        label.set_selectable(true);
        let click = gtk::GestureClick::new();
        let item_for_click = item.clone();
        let selection = selection.clone();
        click.connect_pressed(move |_, _, _, _| {
            selection.set_selected(item_for_click.position());
        });
        label.add_controller(click);
        item.set_child(Some(&label));
    });
    factory.connect_bind(move |_, object| {
        let Some(item) = object.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let (Some(label), Some(data)) = (
            item.child().and_downcast::<gtk::Label>(),
            item.item().and_downcast::<glib::BoxedAnyObject>(),
        ) else {
            return;
        };
        let entry = data.borrow::<StackEntry>();
        reset_semantic_css(&label);
        let text = match column {
            StackColumn::Anchor => entry
                .address_registers
                .iter()
                .map(|name| format!("${name}"))
                .collect::<Vec<_>>()
                .join(","),
            StackColumn::Address => {
                label.add_css_class("memory-stack");
                format!("0x{:016x}", entry.address)
            }
            StackColumn::Value => {
                label.add_css_class(memory_kind_css(entry.memory_kind));
                stack_entry_text(&entry)
            }
            StackColumn::Offset => format!("+0x{:04x}", entry.offset),
            StackColumn::Index => format!("+{:03}", entry.index),
            StackColumn::References => stack_references(&entry),
            StackColumn::Region => entry.region.clone().unwrap_or_default(),
        };
        label.set_text(&text);
        label.set_tooltip_text(Some(&stack_tooltip(&entry)));
    });
    let column_view = gtk::ColumnViewColumn::new(Some(title), Some(factory));
    column_view.set_fixed_width(width);
    column_view.set_resizable(true);
    column_view.set_expand(expand);
    column_view
}

fn register_column_css(column: RegisterColumn) -> &'static str {
    match column {
        RegisterColumn::Name => "register-name",
        RegisterColumn::Value => "register-value",
        RegisterColumn::Details => "register-details",
    }
}

fn stack_column_css(column: StackColumn) -> &'static str {
    match column {
        StackColumn::Anchor => "stack-register-marker",
        StackColumn::Address => "stack-address",
        StackColumn::Value => "stack-value",
        StackColumn::Offset | StackColumn::Index => "stack-position",
        StackColumn::References => "stack-references",
        StackColumn::Region => "stack-region",
    }
}

fn reset_semantic_css(label: &gtk::Label) {
    for class in [
        "memory-code",
        "memory-heap",
        "memory-stack",
        "memory-writable",
        "memory-readonly",
        "memory-rwx",
        "memory-string",
        "memory-none",
        "register-zero",
        "modified-register",
    ] {
        label.remove_css_class(class);
    }
}

fn instruction_column(
    title: &str,
    width: i32,
    expand: bool,
    class: &'static str,
    selection: &gtk::SingleSelection,
    text: fn(&InstructionRowData) -> String,
) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    let selection = selection.clone();
    factory.connect_setup(move |_, object| {
        let Some(item) = object.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let label = gtk::Label::new(None);
        label.add_css_class("instruction-cell");
        label.add_css_class(class);
        label.set_halign(gtk::Align::Start);
        label.set_ellipsize(pango::EllipsizeMode::End);
        label.set_selectable(true);
        label.set_cursor_from_name(Some("text"));
        let click = gtk::GestureClick::new();
        click.set_button(gtk::gdk::BUTTON_PRIMARY);
        let item_for_click = item.clone();
        let selection = selection.clone();
        click.connect_pressed(move |_, _, _, _| {
            selection.set_selected(item_for_click.position());
        });
        label.add_controller(click);
        item.set_child(Some(&label));
    });
    factory.connect_bind(move |_, object| {
        let Some(item) = object.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let (Some(label), Some(data)) = (
            item.child().and_downcast::<gtk::Label>(),
            item.item().and_downcast::<glib::BoxedAnyObject>(),
        ) else {
            return;
        };
        let data = data.borrow::<InstructionRowData>();
        if data.current {
            label.add_css_class("current-instruction-cell");
        } else {
            label.remove_css_class("current-instruction-cell");
        }
        label.set_text(&text(&data));
        label.set_tooltip_text(Some(&format!(
            "{} · {}\n{}\nSelect text to copy; press Enter or double-click outside a text selection to toggle an instruction breakpoint",
            data.instruction.address,
            data.instruction.text,
            instruction_symbol_full(&data.instruction),
        )));
    });
    let column = gtk::ColumnViewColumn::new(Some(title), Some(factory));
    column.set_fixed_width(width);
    column.set_resizable(true);
    column.set_expand(expand);
    column
}

fn build_editor_panel(notebook: &gtk::Notebook) -> gtk::Box {
    let panel = gtk::Box::new(gtk::Orientation::Vertical, 0);
    panel.add_css_class("panel");
    panel.append(notebook);
    panel
}

fn build_terminal_panel(terminal: &vte4::Terminal) -> gtk::Box {
    let panel = gtk::Box::new(gtk::Orientation::Vertical, 0);
    panel.add_css_class("panel");

    let header = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    header.add_css_class("panel-header");
    header.append(&section_title("TERMINAL"));
    panel.append(&header);

    let scrolled = gtk::ScrolledWindow::builder()
        .child(terminal)
        .hexpand(true)
        .vexpand(true)
        .build();
    panel.append(&scrolled);
    panel
}

fn build_source_notebook(style_scheme: Option<&sourceview5::StyleScheme>) -> gtk::Notebook {
    let notebook = gtk::Notebook::new();
    notebook.add_css_class("source-notebook");
    notebook.set_scrollable(true);
    notebook.set_show_border(false);
    notebook.set_hexpand(true);
    notebook.set_vexpand(true);
    append_welcome_source(&notebook, style_scheme);
    notebook
}

fn append_welcome_source(
    notebook: &gtk::Notebook,
    style_scheme: Option<&sourceview5::StyleScheme>,
) {
    let buffer = build_source_buffer(INITIAL_SOURCE, None, style_scheme);
    let view = build_source_view(&buffer);
    let page = gtk::ScrolledWindow::builder()
        .child(&view)
        .hexpand(true)
        .vexpand(true)
        .build();
    let tab = gtk::Label::new(Some("welcome.c"));
    tab.add_css_class("source-tab");
    notebook.append_page(&page, Some(&tab));
}

fn build_source_buffer(
    contents: &str,
    path: Option<&Path>,
    style_scheme: Option<&sourceview5::StyleScheme>,
) -> sourceview5::Buffer {
    let manager = sourceview5::LanguageManager::default();
    let language = path.map_or_else(
        || manager.language("c"),
        |path| manager.guess_language(Some(path), None),
    );
    let buffer = sourceview5::Buffer::builder()
        .highlight_matching_brackets(true)
        .highlight_syntax(true)
        .text(contents)
        .build();
    buffer.set_language(language.as_ref());
    buffer.set_style_scheme(style_scheme);
    buffer
}

fn build_source_view(buffer: &sourceview5::Buffer) -> sourceview5::View {
    sourceview5::View::builder()
        .buffer(buffer)
        .editable(false)
        .highlight_current_line(true)
        .show_line_marks(false)
        .show_line_numbers(false)
        .tab_width(4)
        .top_margin(5)
        .bottom_margin(5)
        .left_margin(4)
        .right_margin(6)
        .monospace(true)
        .build()
}

fn build_terminal(theme: &Theme) -> vte4::Terminal {
    let terminal = vte4::Terminal::new();
    terminal.set_hexpand(true);
    terminal.set_vexpand(true);
    terminal.set_scrollback_lines(20_000);
    terminal.set_scroll_on_output(false);
    terminal.set_scroll_on_keystroke(true);
    terminal.set_audible_bell(false);
    terminal.set_cursor_blink_mode(vte4::CursorBlinkMode::On);
    terminal.set_font(Some(&pango::FontDescription::from_string("Monospace 9.5")));
    theme.style_terminal(&terminal);
    terminal
}

fn control_button(label: &str, tooltip: &str, suggested: bool) -> gtk::Button {
    let button = gtk::Button::with_label(label);
    button.add_css_class("debug-control");
    button.set_tooltip_text(Some(tooltip));
    button.set_sensitive(false);
    if suggested {
        button.add_css_class("primary-control");
    }
    button
}

fn section_title(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.add_css_class("section-title");
    label.set_halign(gtk::Align::Start);
    label
}

fn sidebar_row(key: &str, value: &str) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    row.add_css_class("sidebar-row");
    let key = gtk::Label::new(Some(key));
    key.add_css_class("muted");
    key.set_halign(gtk::Align::Start);
    key.set_width_chars(8);
    let value = gtk::Label::new(Some(value));
    value.set_halign(gtk::Align::Start);
    value.set_ellipsize(pango::EllipsizeMode::Middle);
    value.set_hexpand(true);
    row.append(&key);
    row.append(&value);
    row
}

fn populate_register_group<'a>(
    group: &RegisterGroupView,
    registers: impl IntoIterator<Item = &'a Register>,
    previous: &HashMap<String, String>,
    ring: Option<u64>,
) {
    let rows = registers
        .into_iter()
        .map(|register| RegisterRowData {
            register: register.clone(),
            changed: register_changed(register, previous),
            ring,
        })
        .collect::<Vec<_>>();
    let count = rows.len() as i32;
    replace_boxed_store(&group.store, rows);
    if count == 0 {
        return;
    }
    group.panel.set_visible(true);
    group.view.set_size_request(-1, 24 + count * 26);
}

fn register_in_group(group: RegisterGroupKind, name: &str) -> bool {
    match group {
        RegisterGroupKind::General => GENERAL_REGISTERS.contains(&name),
        RegisterGroupKind::Bases => BASE_REGISTERS.contains(&name),
        RegisterGroupKind::Flags => FLAG_REGISTERS.contains(&name),
        RegisterGroupKind::Segments => SEGMENT_REGISTERS.contains(&name),
        RegisterGroupKind::Vector => {
            ["xmm", "ymm", "zmm", "mm"].iter().any(|prefix| {
                name.strip_prefix(prefix).is_some_and(|index| {
                    !index.is_empty() && index.chars().all(|character| character.is_ascii_digit())
                })
            }) || name == "mxcsr"
        }
        RegisterGroupKind::FloatingPoint => {
            matches!(
                name,
                "fctrl" | "fstat" | "ftag" | "fiseg" | "fioff" | "foseg" | "fooff" | "fop"
            ) || name.strip_prefix("st").is_some_and(|index| {
                !index.is_empty() && index.chars().all(|character| character.is_ascii_digit())
            })
        }
        RegisterGroupKind::Other => true,
    }
}

fn register_changed(register: &Register, previous: &HashMap<String, String>) -> bool {
    previous
        .get(&register.name)
        .is_some_and(|value| value != &register.value)
}

fn same_register_values(left: &[Register], right: &[Register]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| left.name == right.name && left.value == right.value)
}

fn register_value_css(register: &Register) -> &'static str {
    if matches!(register.name.as_str(), "rip" | "eip") {
        "memory-code"
    } else if matches!(register.name.as_str(), "rsp" | "rbp" | "esp" | "ebp") {
        "memory-stack"
    } else if register.pointer_chain.iter().skip(1).any(|value| {
        value.contains('"')
            || hex_value(value).is_some_and(|value| {
                ascii_annotation(value).is_some_and(|annotation| !annotation.starts_with('('))
            })
    }) {
        "memory-string"
    } else if matches!(register.name.as_str(), "fs_base" | "gs_base")
        || register
            .pointer_chain
            .first()
            .is_some_and(|value| value.contains('<'))
    {
        "memory-writable"
    } else if hex_value(&register.value) == Some(0)
        || vector_lane_values(&register.name, &register.value)
            .is_some_and(|lanes| lanes.iter().all(|lane| lane == "0x0000000000000000"))
    {
        "register-zero"
    } else {
        "memory-none"
    }
}

fn register_text(register: &Register) -> String {
    let values = if register.pointer_chain.is_empty() {
        std::slice::from_ref(&register.value)
    } else {
        register.pointer_chain.as_slice()
    };
    values
        .iter()
        .enumerate()
        .map(|(index, value)| format_register_value(&register.name, value, index > 0))
        .collect::<Vec<_>>()
        .join("  →  ")
}

fn register_primary_value(register: &Register) -> String {
    let value = register.pointer_chain.first().unwrap_or(&register.value);
    format_register_value(&register.name, value, false)
}

fn register_details(register: &Register) -> String {
    register
        .pointer_chain
        .iter()
        .skip(1)
        .map(|value| format_register_value(&register.name, value, true))
        .collect::<Vec<_>>()
        .join("  →  ")
}

fn is_flags_register(name: &str) -> bool {
    matches!(name, "eflags" | "rflags" | "cpsr")
}

fn format_register_value(register: &str, value: &str, show_ascii: bool) -> String {
    if let Some(vector) = format_vector_register_value(register, value) {
        return vector;
    }
    if value.starts_with('[') {
        return value.to_owned();
    }
    let Some(number) = hex_value(value) else {
        return value.lines().next().unwrap_or(value).to_owned();
    };
    let width = register_hex_width(register);
    let mut formatted = format!("0x{number:0width$x}");
    if let Some((_, annotation)) = value.trim().split_once(char::is_whitespace) {
        formatted.push(' ');
        formatted.push_str(annotation.trim());
    } else if show_ascii && let Some(annotation) = ascii_annotation(number) {
        formatted.push(' ');
        formatted.push_str(&annotation);
    }
    formatted
}

fn format_vector_register_value(register: &str, value: &str) -> Option<String> {
    let lanes = vector_lane_values(register, value)?;
    if lanes.len() > 1 && lanes.iter().all(|lane| lane == &lanes[0]) {
        return Some(format!("q0…q{} = {}", lanes.len() - 1, lanes[0]));
    }
    Some(
        lanes
            .iter()
            .enumerate()
            .map(|(index, lane)| format!("q{index}={lane}"))
            .collect::<Vec<_>>()
            .join("  ·  "),
    )
}

fn vector_lane_values(register: &str, value: &str) -> Option<Vec<String>> {
    let register_bytes = vector_register_bytes(register)?;
    let format = VectorLaneFormat::Int64;
    let lane_count = register_bytes / format.lane_bytes();
    vector_field_values(value, &format.field(register_bytes), lane_count, format)
}

fn vector_register_bytes(register: &str) -> Option<usize> {
    [("xmm", 16), ("ymm", 32), ("zmm", 64)]
        .into_iter()
        .find_map(|(prefix, bytes)| {
            register.strip_prefix(prefix).and_then(|index| {
                (!index.is_empty() && index.chars().all(|character| character.is_ascii_digit()))
                    .then_some(bytes)
            })
        })
}

fn vector_field_values(
    value: &str,
    field: &str,
    lane_count: usize,
    format: VectorLaneFormat,
) -> Option<Vec<String>> {
    let field = value
        .find(field)
        .map(|index| &value[index + field.len()..])?;
    let start = field.find('{')? + 1;
    let end = field[start..].find('}')? + start;
    let mut lanes = Vec::with_capacity(lane_count);
    for part in field[start..end]
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
    {
        let (lane, repeats) = if let Some((lane, repeats)) = part.split_once("<repeats") {
            let repeats = repeats
                .split_whitespace()
                .next()
                .and_then(|count| count.parse::<usize>().ok())
                .unwrap_or(1);
            (lane.trim(), repeats)
        } else {
            (part, 1)
        };
        let lane = format_vector_lane(lane, format);
        lanes.extend(std::iter::repeat_n(lane, repeats));
    }
    lanes.truncate(lane_count);
    (lanes.len() == lane_count).then_some(lanes)
}

fn format_vector_lane(lane: &str, format: VectorLaneFormat) -> String {
    let lane = lane
        .rsplit_once('=')
        .map_or(lane, |(_, value)| value)
        .trim();
    if format.is_float() {
        if let Some(hex) = lane.strip_prefix("0x")
            && let Ok(bits) = u64::from_str_radix(hex, 16)
        {
            return if format == VectorLaneFormat::Float32 {
                format_float(f32::from_bits(bits as u32) as f64)
            } else {
                format_float(f64::from_bits(bits))
            };
        }
        return lane.split_whitespace().collect::<Vec<_>>().join(" ");
    }
    let width = format.lane_bytes() * 2;
    if let Some(hex) = lane.strip_prefix("0x")
        && let Ok(value) = u64::from_str_radix(hex, 16)
    {
        return format!("0x{value:0width$x}");
    }
    if let Ok(value) = lane.parse::<u64>() {
        return format!("0x{value:0width$x}");
    }
    if let Ok(value) = lane.parse::<i64>() {
        let bits = u32::try_from(format.lane_bytes() * 8).unwrap_or(64);
        let mask = if bits == 64 {
            u64::MAX
        } else {
            (1_u64 << bits) - 1
        };
        return format!("0x{:0width$x}", (value as u64) & mask);
    }
    lane.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn format_float(value: f64) -> String {
    if value.is_nan() {
        String::from("NaN")
    } else if value == f64::INFINITY {
        String::from("+Inf")
    } else if value == f64::NEG_INFINITY {
        String::from("-Inf")
    } else {
        format!("{value:.12}")
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_owned()
    }
}

fn register_hex_width(register: &str) -> usize {
    if matches!(register, "cs" | "ss" | "ds" | "es" | "fs" | "gs") {
        4
    } else if matches!(
        register,
        "eax" | "ebx" | "ecx" | "edx" | "esp" | "ebp" | "esi" | "edi" | "eip" | "cpsr"
    ) {
        8
    } else {
        16
    }
}

fn hex_value(value: &str) -> Option<u64> {
    let hex = value
        .split_whitespace()
        .next()?
        .trim_end_matches(',')
        .strip_prefix("0x")?;
    u64::from_str_radix(hex, 16).ok()
}

fn ascii_annotation(value: u64) -> Option<String> {
    let bytes = value.to_le_bytes();
    let printable = bytes
        .iter()
        .take_while(|byte| byte.is_ascii_graphic() || **byte == b' ')
        .copied()
        .collect::<Vec<_>>();
    if printable.len() < 2 {
        return None;
    }
    let text = printable
        .iter()
        .flat_map(|byte| std::ascii::escape_default(*byte))
        .map(char::from)
        .collect::<String>();
    if printable.len() >= 4 {
        let continuation = if printable.len() == bytes.len() {
            "…"
        } else {
            ""
        };
        Some(format!("'{text}{continuation}'"))
    } else {
        Some(format!("('{text}'?)"))
    }
}

#[cfg(test)]
fn flags_markup(value: &str, ring: Option<u64>) -> String {
    let details = flags_details_markup(value, ring);
    let Some(value) = hex_value(value) else {
        return details;
    };
    format!("0x{value:x}  {details}")
}

fn flags_details_markup(value: &str, ring: Option<u64>) -> String {
    let Some(value) = hex_value(value) else {
        return gtk::glib::markup_escape_text(value).to_string();
    };
    let flags = FLAGS
        .iter()
        .map(|(bit, name)| {
            if value & (1_u64 << bit) != 0 {
                format!("<b>{}</b>", name.to_uppercase())
            } else {
                (*name).to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    let ring = ring.map_or_else(String::new, |ring| format!("  [Ring={ring}]"));
    format!("[{flags}]{ring}")
}

fn build_context_legend() -> gtk::Box {
    let grid = gtk::Grid::builder()
        .column_spacing(8)
        .row_spacing(2)
        .build();
    let items = [
        ("Modified", "legend-modified"),
        ("Code", "memory-code"),
        ("Heap", "memory-heap"),
        ("Stack", "memory-stack"),
        ("Writable", "memory-writable"),
        ("Read-only", "memory-readonly"),
        ("None", "memory-none"),
        ("RWX", "memory-rwx"),
        ("String", "memory-string"),
    ];
    for (index, (text, class)) in items.into_iter().enumerate() {
        let item = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        let swatch = gtk::Label::new(Some("■"));
        swatch.add_css_class("legend-swatch");
        swatch.add_css_class(class);
        let label = gtk::Label::new(Some(text));
        label.set_halign(gtk::Align::Start);
        item.append(&swatch);
        item.append(&label);
        grid.attach(&item, (index % 2) as i32, (index / 2) as i32, 1, 1);
    }
    build_disclosure("LEGEND", &grid, false, "context-legend")
}

fn build_disclosure(
    title: &str,
    child: &impl IsA<gtk::Widget>,
    expanded: bool,
    class: &str,
) -> gtk::Box {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.add_css_class("disclosure");
    root.add_css_class(class);
    let heading = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    let arrow = gtk::Label::new(Some(if expanded { "⌄" } else { "›" }));
    arrow.add_css_class("disclosure-arrow");
    let title = gtk::Label::new(Some(title));
    title.add_css_class("section-title");
    title.set_halign(gtk::Align::Start);
    heading.append(&arrow);
    heading.append(&title);
    let button = gtk::Button::builder().child(&heading).build();
    button.add_css_class("disclosure-header");
    button.set_halign(gtk::Align::Fill);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.append(child);
    content.set_visible(expanded);
    let content_for_click = content.clone();
    button.connect_clicked(move |_| {
        let reveal = !content_for_click.is_visible();
        content_for_click.set_visible(reveal);
        arrow.set_text(if reveal { "⌄" } else { "›" });
    });
    root.append(&button);
    root.append(&content);
    root
}

fn stack_references(entry: &StackEntry) -> String {
    let mut references = Vec::new();
    if !entry.value_registers.is_empty() {
        references.push(
            entry
                .value_registers
                .iter()
                .map(|name| format!("${name}"))
                .collect::<Vec<_>>()
                .join(", "),
        );
    }
    if let Some(frame) = entry.return_frame {
        references.push(format!("retaddr[{frame}]"));
    }
    references.join(" · ")
}

fn stack_word_role(entry: &StackEntry) -> String {
    let mut roles = vec![memory_kind_label(entry.memory_kind).to_owned()];
    if !entry.address_registers.is_empty() {
        roles.push(format!(
            "addressed by {}",
            entry
                .address_registers
                .iter()
                .map(|name| format!("${name}"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !entry.value_registers.is_empty() {
        roles.push(format!(
            "value held by {}",
            entry
                .value_registers
                .iter()
                .map(|name| format!("${name}"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if let Some(frame) = entry.return_frame {
        roles.push(format!("return address for frame #{frame}"));
    }
    roles.join("  ·  ")
}

const fn memory_kind_label(kind: MemoryKind) -> &'static str {
    match kind {
        MemoryKind::Code => "CODE POINTER",
        MemoryKind::Heap => "HEAP POINTER",
        MemoryKind::Stack => "STACK POINTER",
        MemoryKind::Writable => "WRITABLE POINTER",
        MemoryKind::ReadOnly => "READ-ONLY POINTER",
        MemoryKind::Rwx => "RWX POINTER",
        MemoryKind::String => "ASCII / STRING",
        MemoryKind::None => "SCALAR / UNKNOWN",
    }
}

fn stack_tooltip(entry: &StackEntry) -> String {
    let anchors = entry
        .address_registers
        .iter()
        .map(|name| format!("${name}"))
        .collect::<Vec<_>>()
        .join(", ");
    let references = stack_references(entry);
    let region = entry.region.as_deref().unwrap_or("unmapped");
    format!(
        "0x{:016x}  +0x{:04x} / +{:03}\n{}\nanchors: {} · references: {}\n{}",
        entry.address,
        entry.offset,
        entry.index,
        stack_entry_text(entry),
        if anchors.is_empty() { "none" } else { &anchors },
        if references.is_empty() {
            "none"
        } else {
            &references
        },
        region,
    )
}

fn stack_entry_text(entry: &StackEntry) -> String {
    let values = if entry.pointer_chain.is_empty() {
        std::slice::from_ref(&entry.value)
    } else {
        entry.pointer_chain.as_slice()
    };
    values
        .iter()
        .enumerate()
        .map(|(index, value)| format_register_value("rsp", value, index > 0))
        .collect::<Vec<_>>()
        .join("  →  ")
}

fn memory_kind_css(kind: MemoryKind) -> &'static str {
    match kind {
        MemoryKind::Code => "memory-code",
        MemoryKind::Heap => "memory-heap",
        MemoryKind::Stack => "memory-stack",
        MemoryKind::Writable => "memory-writable",
        MemoryKind::ReadOnly => "memory-readonly",
        MemoryKind::Rwx => "memory-rwx",
        MemoryKind::String => "memory-string",
        MemoryKind::None => "memory-none",
    }
}

fn thread_os_id(target_id: &str) -> Option<String> {
    if let Some(lwp) = target_id
        .split_once("(LWP ")
        .and_then(|(_, suffix)| suffix.split_once(')'))
        .map(|(lwp, _)| lwp)
    {
        return Some(lwp.to_owned());
    }
    if let Some(tid) = target_id
        .split_once("tid:")
        .map(|(_, suffix)| suffix)
        .and_then(|suffix| suffix.split_whitespace().next())
    {
        return Some(tid.trim_end_matches([',', ']']).to_owned());
    }
    target_id
        .strip_prefix("process ")
        .and_then(|pid| pid.split_whitespace().next())
        .map(str::to_owned)
}

fn stop_reason_label(reason: &str) -> String {
    match reason {
        "breakpoint-hit" => String::from("BREAKPOINT"),
        "end-stepping-range" => String::from("STEP"),
        "function-finished" => String::from("FINISH"),
        "location-reached" => String::from("UNTIL"),
        "signal-received" => String::from("SIGNAL"),
        "watchpoint-trigger" => String::from("WATCHPOINT"),
        other => other.replace('-', " ").to_uppercase(),
    }
}

fn thread_detail(thread: &ThreadInfo, stop_reason: Option<&str>) -> String {
    let mut detail = thread.frame.as_ref().map_or_else(
        || thread.state.clone(),
        |frame| format!("{} at {}", thread.state, frame.address),
    );
    let metadata = thread_metadata(thread, stop_reason);
    if !metadata.is_empty() {
        if thread.frame.is_some() {
            detail.push(' ');
        } else {
            detail.push_str(", ");
        }
        detail.push_str(&metadata);
    }
    detail
}

fn thread_detail_widget(thread: &ThreadInfo, stop_reason: Option<&str>) -> gtk::Box {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    if let Some(frame) = thread.frame.as_ref() {
        let location = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let state = gtk::Label::new(Some(&format!("{} at ", thread.state)));
        state.add_css_class("thread-detail");
        let address = gtk::Label::new(Some(&frame.address));
        address.add_css_class("thread-detail");
        address.add_css_class("memory-code");
        location.append(&state);
        location.append(&address);
        root.append(&location);
    } else {
        let state = gtk::Label::new(Some(&thread.state));
        state.add_css_class("thread-detail");
        state.set_halign(gtk::Align::Start);
        root.append(&state);
    }

    let metadata = thread_metadata(thread, stop_reason);
    if !metadata.is_empty() {
        let metadata = gtk::Label::new(Some(&metadata));
        metadata.add_css_class("thread-detail");
        metadata.set_halign(gtk::Align::Start);
        metadata.set_wrap(true);
        metadata.set_wrap_mode(pango::WrapMode::WordChar);
        root.append(&metadata);
    }
    root
}

fn thread_metadata(thread: &ThreadInfo, stop_reason: Option<&str>) -> String {
    let mut metadata = Vec::new();
    if let Some(frame) = thread.frame.as_ref()
        && let Some(symbol) = thread
            .pc_symbol
            .clone()
            .or_else(|| (frame.function != "??").then(|| format!("<{}>", frame.function)))
    {
        metadata.push(compact_function_name(&symbol));
    }
    if let Some(core) = thread.core.as_deref() {
        metadata.push(format!("core:{core}"));
    }
    if let Some(reason) = stop_reason {
        metadata.push(format!("reason: {reason}"));
    }
    metadata.join(", ")
}

fn full_address(address: &str) -> String {
    hex_value(address).map_or_else(|| address.to_owned(), |address| format!("0x{address:016x}"))
}

fn split_instruction(instruction: &str) -> (&str, &str) {
    let instruction = instruction.trim();
    match instruction.find(char::is_whitespace) {
        Some(index) => (&instruction[..index], instruction[index..].trim()),
        None => (instruction, ""),
    }
}

fn instruction_flow_description(instruction: &Instruction, registers: &[Register]) -> String {
    let (mnemonic, operands) = split_instruction(&instruction.text);
    let mnemonic = mnemonic.to_ascii_lowercase();
    let (kind, detail) = if mnemonic.starts_with("call") {
        ("CALL", operands)
    } else if mnemonic == "ret" || mnemonic.starts_with("ret ") {
        ("RETURN", "pop target from stack")
    } else if mnemonic == "syscall" || mnemonic == "sysenter" {
        ("SYSCALL", "kernel transition")
    } else if mnemonic == "jmp" || mnemonic.starts_with("jmp") {
        ("JUMP", operands)
    } else if mnemonic.starts_with('j') || mnemonic.starts_with("loop") {
        let decision = conditional_branch_taken(instruction, registers).map(|taken| {
            if taken {
                "BRANCH · TAKEN"
            } else {
                "BRANCH · NOT TAKEN"
            }
        });
        (decision.unwrap_or("BRANCH"), operands)
    } else {
        ("FLOW", "sequential")
    };
    if detail.is_empty() {
        kind.to_owned()
    } else {
        format!("{kind}  →  {detail}")
    }
}

fn instruction_arguments_description(instruction: &Instruction, registers: &[Register]) -> String {
    let mnemonic = split_instruction(&instruction.text).0.to_ascii_lowercase();
    if matches!(mnemonic.as_str(), "syscall" | "sysenter") {
        return syscall_arguments_description(registers);
    }
    if !mnemonic.starts_with("call") {
        return String::new();
    }
    let arguments = ["rdi", "rsi", "rdx", "rcx", "r8", "r9"]
        .iter()
        .filter_map(|name| {
            registers
                .iter()
                .find(|register| register.name == *name)
                .map(|register| format!("${name}={}", register_primary_value(register)))
        })
        .collect::<Vec<_>>();
    if arguments.is_empty() {
        String::new()
    } else {
        format!("ARGS  {}", arguments.join("  "))
    }
}

fn conditional_branch_taken(instruction: &Instruction, registers: &[Register]) -> Option<bool> {
    let mnemonic = split_instruction(&instruction.text).0.to_ascii_lowercase();
    let flags =
        register_number(registers, "rflags").or_else(|| register_number(registers, "eflags"));
    let flag = |bit: u8| flags.map(|flags| flags & (1_u64 << bit) != 0);
    let carry = || flag(0);
    let parity = || flag(2);
    let zero = || flag(6);
    let sign = || flag(7);
    let overflow = || flag(11);
    match mnemonic.as_str() {
        "jo" => overflow(),
        "jno" => overflow().map(|value| !value),
        "jb" | "jc" | "jnae" => carry(),
        "jae" | "jnb" | "jnc" => carry().map(|value| !value),
        "je" | "jz" => zero(),
        "jne" | "jnz" => zero().map(|value| !value),
        "jbe" | "jna" => Some(carry()? || zero()?),
        "ja" | "jnbe" => Some(!carry()? && !zero()?),
        "js" => sign(),
        "jns" => sign().map(|value| !value),
        "jp" | "jpe" => parity(),
        "jnp" | "jpo" => parity().map(|value| !value),
        "jl" | "jnge" => Some(sign()? != overflow()?),
        "jge" | "jnl" => Some(sign()? == overflow()?),
        "jle" | "jng" => Some(zero()? || sign()? != overflow()?),
        "jg" | "jnle" => Some(!zero()? && sign()? == overflow()?),
        "jcxz" => register_number(registers, "cx")
            .or_else(|| register_number(registers, "ecx"))
            .or_else(|| register_number(registers, "rcx"))
            .map(|value| value & 0xffff == 0),
        "jecxz" => register_number(registers, "ecx")
            .or_else(|| register_number(registers, "rcx"))
            .map(|value| value & 0xffff_ffff == 0),
        "jrcxz" => register_number(registers, "rcx").map(|value| value == 0),
        "loop" | "loope" | "loopz" | "loopne" | "loopnz" => {
            let counter =
                register_number(registers, "rcx").or_else(|| register_number(registers, "ecx"))?;
            let repeats = counter.wrapping_sub(1) != 0;
            match mnemonic.as_str() {
                "loope" | "loopz" => Some(repeats && zero()?),
                "loopne" | "loopnz" => Some(repeats && !zero()?),
                _ => Some(repeats),
            }
        }
        _ => None,
    }
}

fn register_number(registers: &[Register], name: &str) -> Option<u64> {
    registers
        .iter()
        .find(|register| register.name == name)
        .and_then(|register| hex_value(&register.value))
}

fn syscall_arguments_description(registers: &[Register]) -> String {
    let Some(number) = register_number(registers, "rax") else {
        return String::from("SYSCALL  number unavailable");
    };
    let (name, argument_names) = syscall_signature(number);
    let values = ["rdi", "rsi", "rdx", "r10", "r8", "r9"]
        .iter()
        .zip(argument_names.iter())
        .filter_map(|(register_name, argument_name)| {
            registers
                .iter()
                .find(|register| register.name == *register_name)
                .map(|register| format!("{argument_name}={}", register_primary_value(register)))
        })
        .collect::<Vec<_>>();
    if values.is_empty() {
        format!("SYSCALL  #{number} {name}")
    } else {
        format!("SYSCALL  #{number} {name}({})", values.join(", "))
    }
}

fn syscall_signature(number: u64) -> (&'static str, &'static [&'static str]) {
    match number {
        0 => ("read", &["fd", "buf", "count"]),
        1 => ("write", &["fd", "buf", "count"]),
        2 => ("open", &["path", "flags", "mode"]),
        3 => ("close", &["fd"]),
        8 => ("lseek", &["fd", "offset", "whence"]),
        9 => ("mmap", &["addr", "length", "prot", "flags", "fd", "offset"]),
        10 => ("mprotect", &["addr", "length", "prot"]),
        11 => ("munmap", &["addr", "length"]),
        12 => ("brk", &["addr"]),
        13 => (
            "rt_sigaction",
            &["signal", "action", "old_action", "sigset_size"],
        ),
        14 => ("rt_sigprocmask", &["how", "set", "old_set", "sigset_size"]),
        16 => ("ioctl", &["fd", "request", "argument"]),
        17 => ("pread64", &["fd", "buf", "count", "offset"]),
        18 => ("pwrite64", &["fd", "buf", "count", "offset"]),
        19 => ("readv", &["fd", "iov", "iov_count"]),
        20 => ("writev", &["fd", "iov", "iov_count"]),
        21 => ("access", &["path", "mode"]),
        32 => ("dup", &["old_fd"]),
        33 => ("dup2", &["old_fd", "new_fd"]),
        39 => ("getpid", &[]),
        41 => ("socket", &["domain", "type", "protocol"]),
        42 => ("connect", &["fd", "address", "length"]),
        43 => ("accept", &["fd", "address", "length"]),
        56 => (
            "clone",
            &["flags", "stack", "parent_tid", "child_tid", "tls"],
        ),
        57 => ("fork", &[]),
        58 => ("vfork", &[]),
        59 => ("execve", &["path", "argv", "envp"]),
        60 => ("exit", &["status"]),
        61 => ("wait4", &["pid", "status", "options", "usage"]),
        62 => ("kill", &["pid", "signal"]),
        72 => ("fcntl", &["fd", "command", "argument"]),
        80 => ("chdir", &["path"]),
        87 => ("unlink", &["path"]),
        158 => ("arch_prctl", &["code", "address"]),
        186 => ("gettid", &[]),
        202 => (
            "futex",
            &[
                "address",
                "operation",
                "value",
                "timeout",
                "address2",
                "value3",
            ],
        ),
        231 => ("exit_group", &["status"]),
        257 => ("openat", &["dir_fd", "path", "flags", "mode"]),
        262 => ("newfstatat", &["dir_fd", "path", "stat", "flags"]),
        263 => ("unlinkat", &["dir_fd", "path", "flags"]),
        273 => ("set_robust_list", &["head", "length"]),
        318 => ("getrandom", &["buf", "count", "flags"]),
        332 => ("statx", &["dir_fd", "path", "flags", "mask", "statx"]),
        435 => ("clone3", &["arguments", "size"]),
        436 => ("close_range", &["first", "last", "flags"]),
        437 => ("openat2", &["dir_fd", "path", "how", "size"]),
        _ => ("unknown", &["arg0", "arg1", "arg2", "arg3", "arg4", "arg5"]),
    }
}

fn instruction_memory_expression(
    instruction: &Instruction,
    registers: &[Register],
) -> Option<String> {
    if let Some(comment) = instruction.text.split_once('#').map(|(_, comment)| comment)
        && let Some(address) = comment
            .split_whitespace()
            .find(|part| part.starts_with("0x"))
    {
        return Some(address.trim_end_matches([',', ';']).to_owned());
    }
    if registers.is_empty() {
        return None;
    }
    let start = instruction.text.find('[')? + 1;
    let end = instruction.text[start..].find(']')? + start;
    let operand = &instruction.text[start..end];
    let register_names = registers
        .iter()
        .map(|register| register.name.as_str())
        .collect::<Vec<_>>();
    let mut expression = String::with_capacity(operand.len() + 8);
    let mut token = String::new();
    let flush_token = |token: &mut String, expression: &mut String| {
        if token.is_empty() {
            return;
        }
        if register_names.contains(&token.as_str()) {
            expression.push('$');
        }
        expression.push_str(token);
        token.clear();
    };
    for character in operand.chars() {
        if character.is_ascii_alphanumeric() || character == '_' {
            token.push(character);
        } else {
            flush_token(&mut token, &mut expression);
            expression.push(character);
        }
    }
    flush_token(&mut token, &mut expression);
    let expression = expression.trim();
    (!expression.is_empty()).then(|| format!("({expression})"))
}

fn compact_memory_preview(bytes: &[u8]) -> String {
    let preview = bytes.iter().take(16).copied().collect::<Vec<_>>();
    let hex = preview
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join(" ");
    let ascii = preview
        .iter()
        .map(|byte| {
            if byte.is_ascii_graphic() || *byte == b' ' {
                char::from(*byte)
            } else {
                '·'
            }
        })
        .collect::<String>();
    format!("{hex}  |{ascii}|")
}

fn instruction_symbol_full(instruction: &Instruction) -> String {
    let offset = instruction.offset.parse::<u64>().unwrap_or(0);
    if offset == 0 {
        format!("<{}>", instruction.function)
    } else {
        format!("<{}+0x{offset:x}>", instruction.function)
    }
}

fn instruction_symbol(instruction: &Instruction) -> String {
    compact_function_name(&instruction_symbol_full(instruction))
}

fn variable_at(selection: &gtk::SingleSelection, position: u32) -> Option<Variable> {
    variable_row_at(selection, position).map(|(_, variable)| variable)
}

fn variable_row_at(
    selection: &gtk::SingleSelection,
    position: u32,
) -> Option<(gtk::TreeListRow, Variable)> {
    variable_node_at(selection, position)
        .and_then(|(row, node)| (!node.placeholder).then_some((row, node.variable)))
}

fn variable_node_at(
    selection: &gtk::SingleSelection,
    position: u32,
) -> Option<(gtk::TreeListRow, VariableNode)> {
    selection
        .item(position)
        .and_then(|item| item.downcast::<gtk::TreeListRow>().ok())
        .and_then(|row| {
            let item = row
                .item()
                .and_then(|item| item.downcast::<glib::BoxedAnyObject>().ok())?;
            let node = item.borrow::<VariableNode>();
            Some((row, node.clone()))
        })
}

fn find_variable_node(store: &gio::ListStore, varobj: &str) -> Option<VariableNode> {
    for position in 0..store.n_items() {
        let item = store
            .item(position)?
            .downcast::<glib::BoxedAnyObject>()
            .ok()?;
        let node = item.borrow::<VariableNode>().clone();
        if node.variable.varobj.as_deref() == Some(varobj) {
            return Some(node);
        }
        if let Some(node) = find_variable_node(&node.children, varobj) {
            return Some(node);
        }
    }
    None
}

fn collect_variable_object_roots(
    store: &gio::ListStore,
    owner: Option<&str>,
    names: &mut Vec<String>,
) {
    for position in 0..store.n_items() {
        let Some(item) = store
            .item(position)
            .and_then(|item| item.downcast::<glib::BoxedAnyObject>().ok())
        else {
            continue;
        };
        let node = item.borrow::<VariableNode>();
        let mut child_owner = owner.map(str::to_owned);
        if let Some(name) = &node.variable.varobj {
            let belongs_to_owner = owner.is_some_and(|owner| {
                name == owner
                    || name
                        .strip_prefix(owner)
                        .is_some_and(|suffix| suffix.starts_with('.'))
            });
            if !belongs_to_owner {
                names.push(name.clone());
                child_owner = Some(name.clone());
            }
        }
        collect_variable_object_roots(&node.children, child_owner.as_deref(), names);
    }
}

fn remove_load_more_rows(store: &gio::ListStore) {
    for position in (0..store.n_items()).rev() {
        let is_load_more = store
            .item(position)
            .and_then(|item| item.downcast::<glib::BoxedAnyObject>().ok())
            .is_some_and(|item| item.borrow::<VariableNode>().load_more.is_some());
        if is_load_more {
            store.remove(position);
        }
    }
}

fn open_variable_editor(
    parent: &gtk::ApplicationWindow,
    variable: Variable,
    handler: Rc<RefCell<Option<VariableAssignmentHandler>>>,
) {
    let editor = gtk::Window::builder()
        .title(format!("Edit {}", variable.name))
        .transient_for(parent)
        .modal(true)
        .default_width(620)
        .build();
    editor.add_css_class("value-editor");
    let content = gtk::Box::new(gtk::Orientation::Vertical, 6);
    content.set_spacing(6);
    content.set_margin_top(10);
    content.set_margin_bottom(10);
    content.set_margin_start(10);
    content.set_margin_end(10);
    let expression = gtk::Label::new(Some(&variable.name));
    expression.add_css_class("local-name");
    expression.set_halign(gtk::Align::Start);
    content.append(&expression);
    let type_name = gtk::Label::new(Some(
        variable.type_name.as_deref().unwrap_or("<unknown type>"),
    ));
    type_name.add_css_class("local-type");
    type_name.set_halign(gtk::Align::Start);
    content.append(&type_name);
    let entry = gtk::Entry::new();
    let (editable_value, _) = variable_value_parts(&variable.value);
    entry.set_text(editable_value);
    entry.set_activates_default(true);
    entry.set_hexpand(true);
    entry.set_tooltip_text(Some(
        "Enter a GDB expression for the new value, then press Enter",
    ));
    content.append(&entry);

    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    actions.set_halign(gtk::Align::End);
    let cancel = gtk::Button::with_label("Cancel");
    let apply = gtk::Button::with_label("Set value");
    apply.add_css_class("primary-control");
    actions.append(&cancel);
    actions.append(&apply);
    content.append(&actions);
    editor.set_child(Some(&content));
    connect_escape_to_close(&editor);

    let original_value = editable_value.to_owned();
    let variable_for_submit = variable;
    let entry_for_submit = entry.clone();
    let editor_for_submit = editor.clone();
    let submit = Rc::new(move || {
        let value = entry_for_submit.text().trim().to_owned();
        if !value.is_empty()
            && value != original_value
            && let Some(handler) = handler.borrow().as_ref()
        {
            handler(variable_for_submit.clone(), value);
        }
        editor_for_submit.close();
    });
    let submit_for_button = Rc::clone(&submit);
    apply.connect_clicked(move |_| submit_for_button());
    entry.connect_activate(move |_| submit());
    let editor_for_cancel = editor.clone();
    cancel.connect_clicked(move |_| editor_for_cancel.close());

    editor.present();
    entry.grab_focus();
    entry.select_region(0, -1);
}

fn open_vector_editor(
    parent: &gtk::ApplicationWindow,
    register: Register,
    handler: Rc<RefCell<Option<VectorAssignmentHandler>>>,
) {
    let Some(register_bytes) = vector_register_bytes(&register.name) else {
        return;
    };
    let editor = gtk::Window::builder()
        .title(format!("Edit ${}", register.name))
        .transient_for(parent)
        .modal(true)
        .default_width(700)
        .default_height(470)
        .build();
    editor.add_css_class("value-editor");
    editor.add_css_class("vector-editor");

    let content = gtk::Box::new(gtk::Orientation::Vertical, 7);
    content.set_margin_top(10);
    content.set_margin_bottom(10);
    content.set_margin_start(10);
    content.set_margin_end(10);
    let heading = gtk::Label::new(Some(&format!(
        "${} · {} bits · edit interpreted lanes",
        register.name,
        register_bytes * 8
    )));
    heading.add_css_class("local-name");
    heading.set_halign(gtk::Align::Start);
    content.append(&heading);

    let interpretation_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let interpretation_label = gtk::Label::new(Some("Interpret as"));
    interpretation_label.add_css_class("muted");
    let interpretations = gtk::StringList::new(
        VectorLaneFormat::ALL
            .map(VectorLaneFormat::label)
            .as_slice(),
    );
    let interpretation = gtk::DropDown::new(Some(interpretations), gtk::Expression::NONE);
    interpretation.set_selected(3);
    interpretation.set_hexpand(true);
    interpretation_row.append(&interpretation_label);
    interpretation_row.append(&interpretation);
    content.append(&interpretation_row);

    let hint = gtk::Label::new(Some(
        "Each view addresses the same register bits. Apply edits before changing the interpretation; switching views resets unapplied lane edits.",
    ));
    hint.add_css_class("muted");
    hint.set_halign(gtk::Align::Start);
    hint.set_wrap(true);
    content.append(&hint);

    let grid = gtk::Grid::builder()
        .column_spacing(8)
        .row_spacing(4)
        .hexpand(true)
        .build();
    let entries = Rc::new(RefCell::new(Vec::<gtk::Entry>::new()));
    let original_values = Rc::new(RefCell::new(Vec::<String>::new()));
    populate_vector_lane_grid(
        &grid,
        &entries,
        &original_values,
        &register.value,
        register_bytes,
        VectorLaneFormat::Int64,
    );
    let scroll = gtk::ScrolledWindow::builder()
        .child(&grid)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build();
    content.append(&scroll);

    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    actions.set_halign(gtk::Align::End);
    let cancel = gtk::Button::with_label("Cancel");
    let apply = gtk::Button::with_label("Apply lanes");
    apply.add_css_class("primary-control");
    actions.append(&cancel);
    actions.append(&apply);
    content.append(&actions);
    editor.set_child(Some(&content));
    connect_escape_to_close(&editor);

    let grid_for_format = grid.clone();
    let entries_for_format = Rc::clone(&entries);
    let originals_for_format = Rc::clone(&original_values);
    let register_value = register.value.clone();
    interpretation.connect_selected_notify(move |dropdown| {
        populate_vector_lane_grid(
            &grid_for_format,
            &entries_for_format,
            &originals_for_format,
            &register_value,
            register_bytes,
            VectorLaneFormat::from_index(dropdown.selected()),
        );
    });

    let editor_for_apply = editor.clone();
    let register_name = register.name;
    apply.connect_clicked(move |_| {
        let format = VectorLaneFormat::from_index(interpretation.selected());
        let changes = entries
            .borrow()
            .iter()
            .zip(original_values.borrow().iter())
            .enumerate()
            .filter_map(|(index, (entry, original))| {
                let value = entry.text().trim().to_owned();
                (!value.is_empty() && value != *original).then_some((index, value))
            })
            .collect::<Vec<_>>();
        if !changes.is_empty()
            && let Some(handler) = handler.borrow().as_ref()
        {
            handler(register_name.clone(), format.field(register_bytes), changes);
        }
        editor_for_apply.close();
    });
    let editor_for_cancel = editor.clone();
    cancel.connect_clicked(move |_| editor_for_cancel.close());
    editor.present();
}

fn populate_vector_lane_grid(
    grid: &gtk::Grid,
    entries: &Rc<RefCell<Vec<gtk::Entry>>>,
    original_values: &Rc<RefCell<Vec<String>>>,
    register_value: &str,
    register_bytes: usize,
    format: VectorLaneFormat,
) {
    while let Some(child) = grid.first_child() {
        grid.remove(&child);
    }
    entries.borrow_mut().clear();
    original_values.borrow_mut().clear();
    let lane_count = register_bytes / format.lane_bytes();
    let field = format.field(register_bytes);
    let values = vector_field_values(register_value, &field, lane_count, format)
        .unwrap_or_else(|| vec![String::from("0"); lane_count]);
    let columns = if lane_count <= 8 { 2 } else { 4 };
    for (index, value) in values.into_iter().enumerate() {
        let group = index % columns;
        let row = index / columns;
        let label = gtk::Label::new(Some(&format!("[{index}]")));
        label.add_css_class("vector-lane-index");
        label.set_halign(gtk::Align::End);
        let entry = gtk::Entry::new();
        entry.set_text(&value);
        entry.set_hexpand(true);
        entry.set_tooltip_text(Some(&format!("${field}[{index}]")));
        grid.attach(&label, (group * 2) as i32, row as i32, 1, 1);
        grid.attach(&entry, (group * 2 + 1) as i32, row as i32, 1, 1);
        original_values.borrow_mut().push(value);
        entries.borrow_mut().push(entry);
    }
}

fn open_flag_editor(
    parent: &gtk::ApplicationWindow,
    register: Register,
    handler: Rc<RefCell<Option<VariableAssignmentHandler>>>,
) {
    let Some(original) = hex_value(&register.value) else {
        return;
    };
    let editor = gtk::Window::builder()
        .title(format!("Edit ${}", register.name))
        .transient_for(parent)
        .modal(true)
        .default_width(540)
        .build();
    editor.add_css_class("value-editor");
    let content = gtk::Box::new(gtk::Orientation::Vertical, 6);
    content.set_margin_top(10);
    content.set_margin_bottom(10);
    content.set_margin_start(10);
    content.set_margin_end(10);
    let heading = gtk::Label::new(Some(&format!(
        "${} = 0x{original:016x} · toggle individual flags",
        register.name
    )));
    heading.set_halign(gtk::Align::Start);
    heading.add_css_class("local-name");
    content.append(&heading);
    let flags = gtk::Grid::builder()
        .column_spacing(12)
        .row_spacing(3)
        .build();
    let mut toggles = Vec::new();
    for (index, (bit, name)) in FLAGS.iter().enumerate() {
        let toggle = gtk::CheckButton::with_label(&name.to_uppercase());
        toggle.set_active(original & (1_u64 << bit) != 0);
        toggle.set_tooltip_text(Some(&format!("Bit {bit}")));
        flags.attach(&toggle, (index % 2) as i32, (index / 2) as i32, 1, 1);
        toggles.push((toggle, *bit));
    }
    content.append(&flags);
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    actions.set_halign(gtk::Align::End);
    let cancel = gtk::Button::with_label("Cancel");
    let apply = gtk::Button::with_label("Apply flags");
    apply.add_css_class("primary-control");
    actions.append(&cancel);
    actions.append(&apply);
    content.append(&actions);
    editor.set_child(Some(&content));
    connect_escape_to_close(&editor);

    let editor_for_apply = editor.clone();
    let variable = Variable {
        name: format!("${}", register.name),
        value: register.value,
        type_name: Some(String::from("flags register")),
        varobj: None,
        num_children: 0,
        has_more: false,
    };
    apply.connect_clicked(move |_| {
        let mut value = original;
        for (toggle, bit) in &toggles {
            if toggle.is_active() {
                value |= 1_u64 << bit;
            } else {
                value &= !(1_u64 << bit);
            }
        }
        if value != original
            && let Some(handler) = handler.borrow().as_ref()
        {
            handler(variable.clone(), format!("0x{value:x}"));
        }
        editor_for_apply.close();
    });
    let editor_for_cancel = editor.clone();
    cancel.connect_clicked(move |_| editor_for_cancel.close());
    editor.present();
}

fn open_breakpoint_condition_editor(
    parent: &gtk::ApplicationWindow,
    breakpoint: Breakpoint,
    handler: Rc<RefCell<Option<BreakpointConditionHandler>>>,
) {
    let editor = gtk::Window::builder()
        .title(format!("Breakpoint #{} condition", breakpoint.number))
        .transient_for(parent)
        .modal(true)
        .default_width(620)
        .build();
    editor.add_css_class("value-editor");
    let content = gtk::Box::new(gtk::Orientation::Vertical, 6);
    content.set_margin_top(10);
    content.set_margin_bottom(10);
    content.set_margin_start(10);
    content.set_margin_end(10);

    let breakpoint_name = breakpoint
        .function
        .as_deref()
        .or(breakpoint.original_location.as_deref())
        .or(breakpoint.address.as_deref())
        .unwrap_or("unresolved");
    let expression = gtk::Label::new(Some(&format!("#{}  {breakpoint_name}", breakpoint.number)));
    expression.add_css_class("local-name");
    expression.set_halign(gtk::Align::Start);
    expression.set_ellipsize(pango::EllipsizeMode::End);
    content.append(&expression);
    let hint = gtk::Label::new(Some(
        "Stop only when this GDB expression is true. Leave it empty to clear.",
    ));
    hint.add_css_class("muted");
    hint.set_halign(gtk::Align::Start);
    hint.set_wrap(true);
    content.append(&hint);
    let entry = gtk::Entry::new();
    entry.set_text(breakpoint.condition.as_deref().unwrap_or(""));
    entry.set_hexpand(true);
    entry.set_tooltip_text(Some("Examples: count == 4, ptr != 0, $rax == 0x10"));
    content.append(&entry);

    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    actions.set_halign(gtk::Align::End);
    let cancel = gtk::Button::with_label("Cancel");
    let clear = gtk::Button::with_label("Clear");
    clear.set_sensitive(breakpoint.condition.is_some());
    let apply = gtk::Button::with_label("Set condition");
    apply.add_css_class("primary-control");
    actions.append(&cancel);
    actions.append(&clear);
    actions.append(&apply);
    content.append(&actions);
    editor.set_child(Some(&content));
    connect_escape_to_close(&editor);

    let number = breakpoint.command_number().to_owned();
    let original_condition = breakpoint.condition;
    let editor_for_submit = editor.clone();
    let submit = Rc::new(move |condition: Option<String>| {
        if condition != original_condition
            && let Some(handler) = handler.borrow().as_ref()
        {
            handler(number.clone(), condition);
        }
        editor_for_submit.close();
    });
    let entry_for_apply = entry.clone();
    let submit_for_apply = Rc::clone(&submit);
    apply.connect_clicked(move |_| {
        let condition = entry_for_apply.text().trim().to_owned();
        submit_for_apply((!condition.is_empty()).then_some(condition));
    });
    let entry_for_activate = entry.clone();
    let submit_for_activate = Rc::clone(&submit);
    entry.connect_activate(move |_| {
        let condition = entry_for_activate.text().trim().to_owned();
        submit_for_activate((!condition.is_empty()).then_some(condition));
    });
    clear.connect_clicked(move |_| submit(None));
    let editor_for_cancel = editor.clone();
    cancel.connect_clicked(move |_| editor_for_cancel.close());

    editor.present();
    entry.grab_focus();
    entry.select_region(0, -1);
}

fn connect_escape_to_close(window: &gtk::Window) {
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak_window = window.downgrade();
    keys.connect_key_pressed(move |_, key, _, _| {
        if key != gtk::gdk::Key::Escape {
            return gtk::glib::Propagation::Proceed;
        }
        if let Some(window) = weak_window.upgrade() {
            window.close();
        }
        gtk::glib::Propagation::Stop
    });
    window.add_controller(keys);
}

fn dynamic_list(empty_text: &str) -> gtk::Box {
    let list = gtk::Box::new(gtk::Orientation::Vertical, 1);
    list.append(&empty_label(empty_text));
    list
}

fn build_signal_grid(
    signals: &'static [(&'static str, &'static str)],
) -> (gtk::Grid, Vec<(gtk::Button, &'static str, &'static str)>) {
    let grid = gtk::Grid::builder()
        .column_homogeneous(true)
        .column_spacing(2)
        .row_spacing(2)
        .hexpand(true)
        .build();
    let buttons = signals
        .iter()
        .enumerate()
        .map(|(index, &(signal, description))| {
            let label = if signal == "all" {
                "ALL SIGNALS"
            } else {
                signal
            };
            let button = gtk::Button::with_label(label);
            button.add_css_class("signal-action");
            button.set_halign(gtk::Align::Fill);
            button.set_hexpand(true);
            button.set_tooltip_text(Some(&format!(
                "{description}\nClick to add a GDB signal catchpoint"
            )));
            grid.attach(&button, (index % 3) as i32, (index / 3) as i32, 1, 1);
            (button, signal, description)
        })
        .collect();
    (grid, buttons)
}

fn empty_label(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.add_css_class("muted");
    label.set_halign(gtk::Align::Start);
    label.set_wrap(true);
    label.set_margin_start(4);
    label.set_margin_top(3);
    label
}

fn clear_box(container: &gtk::Box) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
}

fn replace_boxed_store<T: 'static>(store: &gio::ListStore, values: impl IntoIterator<Item = T>) {
    let values = values
        .into_iter()
        .map(glib::BoxedAnyObject::new)
        .collect::<Vec<_>>();
    store.splice(0, store.n_items(), &values);
}

fn update_selected_frame_buttons(buttons: &[(u32, gtk::Button)], selected: u32) {
    for (level, button) in buttons {
        if *level == selected {
            button.add_css_class("current-debug-item");
        } else {
            button.remove_css_class("current-debug-item");
        }
    }
}

fn open_source_document(path: &Path, context: SourceOpenContext<'_>) -> Option<SourceDocument> {
    let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if let Some(document) = context
        .documents
        .borrow()
        .iter()
        .find(|document| document.path == path)
        .cloned()
    {
        if let Some(page) = context.notebook.page_num(&document.page) {
            context.notebook.set_current_page(Some(page));
        }
        document.view.grab_focus();
        return Some(document);
    }

    let contents = std::fs::read_to_string(&path).ok()?;
    let buffer = build_source_buffer(&contents, Some(&path), context.style_scheme);
    let view = build_source_view(&buffer);
    let breakpoint_renderer = build_breakpoint_gutter(
        &path,
        context.theme,
        context.breakpoints,
        context.insert_handler,
        context.delete_handler,
        context.enabled_handler,
    );
    sourceview5::prelude::ViewExt::gutter(&view, gtk::TextWindowType::Left)
        .insert(&breakpoint_renderer, -30);
    let page = gtk::ScrolledWindow::builder()
        .child(&view)
        .hexpand(true)
        .vexpand(true)
        .build();
    connect_breakpoint_gutter_context_click(&page, &view, &breakpoint_renderer);
    let tab = gtk::Box::new(gtk::Orientation::Horizontal, 3);
    tab.add_css_class("source-tab");
    let tab_label = gtk::Label::new(Some(&source_tab_title(&path)));
    tab_label.set_ellipsize(pango::EllipsizeMode::Middle);
    tab_label.set_width_chars(18);
    tab_label.set_max_width_chars(32);
    tab_label.set_tooltip_text(Some(&path.to_string_lossy()));
    let close = gtk::Button::from_icon_name("window-close-symbolic");
    close.add_css_class("source-tab-close");
    close.set_tooltip_text(Some("Close source tab"));
    tab.append(&tab_label);
    tab.append(&close);

    let document = SourceDocument {
        path: path.clone(),
        buffer,
        view,
        page,
        tab,
        tab_label,
        breakpoint_renderer,
    };
    connect_source_symbol_navigation(&document, context.symbol_handler);

    if context.documents.borrow().is_empty() {
        while context.notebook.n_pages() > 0 {
            context.notebook.remove_page(Some(0));
        }
    }
    let page_number = context
        .notebook
        .append_page(&document.page, Some(&document.tab));
    context.notebook.set_tab_reorderable(&document.page, true);
    context.documents.borrow_mut().push(document.clone());
    let notebook_for_close = context.notebook.clone();
    let documents_for_close = Rc::clone(context.documents);
    let path_for_close = path;
    let page_for_close = document.page.clone();
    let style_scheme_for_close = context.style_scheme.cloned();
    close.connect_clicked(move |_| {
        let Some(page_number) = notebook_for_close.page_num(&page_for_close) else {
            return;
        };
        notebook_for_close.remove_page(Some(page_number));
        let empty = {
            let mut documents = documents_for_close.borrow_mut();
            documents.retain(|document| document.path != path_for_close);
            documents.is_empty()
        };
        if empty {
            append_welcome_source(&notebook_for_close, style_scheme_for_close.as_ref());
        }
    });

    context.notebook.set_current_page(Some(page_number));
    document.view.grab_focus();
    Some(document)
}

fn build_breakpoint_gutter(
    path: &Path,
    theme: &Theme,
    breakpoints: &Rc<RefCell<Vec<Breakpoint>>>,
    insert_handler: &Rc<RefCell<Option<BreakpointInsertHandler>>>,
    delete_handler: &Rc<RefCell<Option<StringSelectionHandler>>>,
    enabled_handler: &Rc<RefCell<Option<BreakpointEnabledHandler>>>,
) -> BreakpointGutterRenderer {
    let path_for_data = path.to_path_buf();
    let breakpoints_for_data = Rc::clone(breakpoints);
    let inactive_foreground = gtk::gdk::RGBA::parse(theme.colors.muted).expect("theme color");
    let disabled_foreground = inactive_foreground;
    let disabled_background = gtk::gdk::RGBA::parse(theme.colors.raised).expect("theme color");
    let enabled_foreground = gtk::gdk::RGBA::parse(theme.colors.background).expect("theme color");
    let enabled_background = gtk::gdk::RGBA::parse(theme.colors.success).expect("theme color");
    let execution_foreground = gtk::gdk::RGBA::parse(theme.colors.warning).expect("theme color");
    let path = path.to_path_buf();
    let breakpoints = Rc::clone(breakpoints);
    let insert_handler = Rc::clone(insert_handler);
    let delete_handler = Rc::clone(delete_handler);
    let enabled_handler = Rc::clone(enabled_handler);
    let renderer = BreakpointGutterRenderer::new(
        move |buffer, line| {
            let source_line = line + 1;
            let executing = i32::try_from(line).ok().is_some_and(|line| {
                !buffer
                    .source_marks_at_line(line, Some(EXECUTION_CATEGORY))
                    .is_empty()
            });
            let breakpoint = breakpoints_for_data
                .borrow()
                .iter()
                .find(|breakpoint| {
                    breakpoint.line == Some(source_line)
                        && breakpoint
                            .source_path()
                            .is_some_and(|reported| source::paths_match(&path_for_data, reported))
                })
                .cloned();
            let text = if executing {
                format!("›{source_line:>3}")
            } else {
                format!("{source_line:>4}")
            };
            match breakpoint {
                Some(breakpoint) if breakpoint.enabled => LineStyle {
                    text,
                    foreground: enabled_foreground,
                    background: Some(enabled_background),
                },
                Some(_) => LineStyle {
                    text,
                    foreground: disabled_foreground,
                    background: Some(disabled_background),
                },
                None => LineStyle {
                    text,
                    foreground: if executing {
                        execution_foreground
                    } else {
                        inactive_foreground
                    },
                    background: None,
                },
            }
        },
        move |renderer, iter, area, button| {
            let line = u32::try_from(iter.line() + 1).ok();
            let existing = breakpoints
                .borrow()
                .iter()
                .find(|breakpoint| {
                    breakpoint.line == line
                        && breakpoint
                            .source_path()
                            .is_some_and(|reported| source::paths_match(&path, reported))
                })
                .cloned();
            match (button, existing) {
                (1, Some(breakpoint)) => {
                    if let Some(handler) = delete_handler.borrow().as_ref() {
                        handler(breakpoint.command_number().to_owned());
                    }
                }
                (1, None) => {
                    if let (Some(line), Some(handler)) = (line, insert_handler.borrow().as_ref()) {
                        handler(path.clone(), line);
                    }
                }
                (3, Some(breakpoint)) => {
                    open_breakpoint_gutter_menu(
                        renderer,
                        area,
                        breakpoint,
                        Rc::clone(&enabled_handler),
                        Rc::clone(&delete_handler),
                    );
                }
                _ => {}
            }
        },
    );
    renderer.set_tooltip_text(Some(
        "Left-click to add or delete a breakpoint · Right-click for more actions",
    ));
    renderer
}

fn connect_breakpoint_gutter_context_click(
    page: &gtk::ScrolledWindow,
    view: &sourceview5::View,
    renderer: &BreakpointGutterRenderer,
) {
    let right_click = gtk::GestureClick::new();
    right_click.set_button(3);
    right_click.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak_view = view.downgrade();
    let weak_renderer = renderer.downgrade();
    right_click.connect_pressed(move |gesture, _presses, x, y| {
        let (Some(view), Some(renderer)) = (weak_view.upgrade(), weak_renderer.upgrade()) else {
            return;
        };
        if x < 0.0 || x >= f64::from(renderer.width()) {
            return;
        }
        let (_, buffer_y) = view.window_to_buffer_coords(
            gtk::TextWindowType::Left,
            x.round() as i32,
            y.round() as i32,
        );
        let (iter, _) = view.line_at_y(buffer_y);
        let area = gtk::gdk::Rectangle::new(0, y.round() as i32, renderer.width(), 1);
        renderer.activate_at(&iter, &area, 3);
        gesture.set_state(gtk::EventSequenceState::Claimed);
    });
    page.add_controller(right_click);
}

fn open_breakpoint_gutter_menu(
    renderer: &BreakpointGutterRenderer,
    area: &gtk::gdk::Rectangle,
    breakpoint: Breakpoint,
    enabled_handler: Rc<RefCell<Option<BreakpointEnabledHandler>>>,
    delete_handler: Rc<RefCell<Option<StringSelectionHandler>>>,
) {
    let popover = gtk::Popover::builder()
        .has_arrow(false)
        .autohide(true)
        .build();
    let menu = gtk::Box::new(gtk::Orientation::Vertical, 1);
    menu.add_css_class("gutter-breakpoint-menu");
    let title = gtk::Label::new(Some(&format!(
        "BREAKPOINT #{}",
        breakpoint.command_number()
    )));
    title.add_css_class("section-title");
    title.set_halign(gtk::Align::Start);
    menu.append(&title);

    let toggle = gtk::Button::with_label(if breakpoint.enabled {
        "Disable"
    } else {
        "Enable"
    });
    let delete = gtk::Button::with_label("Delete");
    delete.add_css_class("danger-action");
    menu.append(&toggle);
    menu.append(&delete);
    popover.set_child(Some(&menu));
    popover.set_parent(renderer);
    popover.set_pointing_to(Some(area));

    let number = breakpoint.command_number().to_owned();
    let enable = !breakpoint.enabled;
    let popover_for_toggle = popover.clone();
    toggle.connect_clicked(move |_| {
        if let Some(handler) = enabled_handler.borrow().as_ref() {
            handler(number.clone(), enable);
        }
        popover_for_toggle.popdown();
    });
    let number = breakpoint.command_number().to_owned();
    let popover_for_delete = popover.clone();
    delete.connect_clicked(move |_| {
        if let Some(handler) = delete_handler.borrow().as_ref() {
            handler(number.clone());
        }
        popover_for_delete.popdown();
    });
    popover.connect_closed(|popover| popover.unparent());
    popover.popup();
}

fn connect_source_symbol_navigation(
    document: &SourceDocument,
    symbol_handler: &Rc<RefCell<Option<StringSelectionHandler>>>,
) {
    let link_tag = gtk::TextTag::builder()
        .name("ctrl-source-link")
        .underline(pango::Underline::Single)
        .build();
    document.buffer.tag_table().add(&link_tag);
    let highlighted_range = Rc::new(RefCell::new(None::<(i32, i32)>));
    let control_pressed = Rc::new(Cell::new(false));
    let pointer_position = Rc::new(Cell::new((0.0, 0.0)));

    let motion = gtk::EventControllerMotion::new();
    let view_for_motion = document.view.clone();
    let buffer_for_motion = document.buffer.clone();
    let tag_for_motion = link_tag.clone();
    let range_for_motion = Rc::clone(&highlighted_range);
    let control_for_motion = Rc::clone(&control_pressed);
    let position_for_motion = Rc::clone(&pointer_position);
    motion.connect_motion(move |controller, x, y| {
        position_for_motion.set((x, y));
        let active = control_for_motion.get()
            || controller
                .current_event_state()
                .contains(gtk::gdk::ModifierType::CONTROL_MASK);
        update_source_link_highlight(
            &view_for_motion,
            &buffer_for_motion,
            &tag_for_motion,
            &range_for_motion,
            x,
            y,
            active,
        );
    });
    let view_for_leave = document.view.clone();
    let buffer_for_leave = document.buffer.clone();
    let tag_for_leave = link_tag.clone();
    let range_for_leave = Rc::clone(&highlighted_range);
    motion.connect_leave(move |_| {
        clear_source_link_highlight(
            &view_for_leave,
            &buffer_for_leave,
            &tag_for_leave,
            &range_for_leave,
        );
    });
    document.view.add_controller(motion);

    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let view_for_press = document.view.clone();
    let buffer_for_press = document.buffer.clone();
    let tag_for_press = link_tag.clone();
    let range_for_press = Rc::clone(&highlighted_range);
    let control_for_press = Rc::clone(&control_pressed);
    let position_for_press = Rc::clone(&pointer_position);
    keys.connect_key_pressed(move |_, key, _, _| {
        if matches!(key, gtk::gdk::Key::Control_L | gtk::gdk::Key::Control_R) {
            control_for_press.set(true);
            let (x, y) = position_for_press.get();
            update_source_link_highlight(
                &view_for_press,
                &buffer_for_press,
                &tag_for_press,
                &range_for_press,
                x,
                y,
                true,
            );
        }
        gtk::glib::Propagation::Proceed
    });
    let view_for_release = document.view.clone();
    let buffer_for_release = document.buffer.clone();
    let tag_for_release = link_tag;
    let range_for_release = Rc::clone(&highlighted_range);
    keys.connect_key_released(move |_, key, _, _| {
        if matches!(key, gtk::gdk::Key::Control_L | gtk::gdk::Key::Control_R) {
            control_pressed.set(false);
            clear_source_link_highlight(
                &view_for_release,
                &buffer_for_release,
                &tag_for_release,
                &range_for_release,
            );
        }
    });
    document.view.add_controller(keys);

    let gesture = gtk::GestureClick::new();
    gesture.set_button(1);
    gesture.set_propagation_phase(gtk::PropagationPhase::Capture);
    let view = document.view.clone();
    let buffer = document.buffer.clone();
    let symbol_handler = Rc::clone(symbol_handler);
    gesture.connect_pressed(move |gesture, _, x, y| {
        if !gesture
            .current_event_state()
            .contains(gtk::gdk::ModifierType::CONTROL_MASK)
        {
            return;
        }
        let (x, y) = view.window_to_buffer_coords(
            gtk::TextWindowType::Widget,
            x.round() as i32,
            y.round() as i32,
        );
        let Some(iter) = view.iter_at_location(x, y) else {
            return;
        };
        let Some(symbol) = source_symbol_at_iter(&buffer, &iter) else {
            return;
        };
        gesture.set_state(gtk::EventSequenceState::Claimed);
        if let Some(handler) = symbol_handler.borrow().as_ref() {
            handler(symbol);
        }
    });
    document.view.add_controller(gesture);
}

fn source_symbol_at_iter(buffer: &sourceview5::Buffer, iter: &gtk::TextIter) -> Option<String> {
    source_symbol_span_at_iter(buffer, iter).map(|(symbol, _, _)| symbol)
}

fn source_symbol_span_at_iter(
    buffer: &sourceview5::Buffer,
    iter: &gtk::TextIter,
) -> Option<(String, usize, usize)> {
    if ["comment", "string"]
        .iter()
        .any(|context| buffer.iter_has_context_class(iter, context))
    {
        return None;
    }
    let start = buffer.iter_at_line(iter.line())?;
    let mut end = start;
    end.forward_to_line_end();
    let line = buffer.text(&start, &end, false);
    source_symbol_span_at_offset(&line, usize::try_from(iter.line_offset()).ok()?)
}

#[cfg(test)]
fn source_symbol_at_offset(line: &str, offset: usize) -> Option<String> {
    source_symbol_span_at_offset(line, offset).map(|(symbol, _, _)| symbol)
}

fn source_symbol_span_at_offset(line: &str, offset: usize) -> Option<(String, usize, usize)> {
    let characters = line.chars().collect::<Vec<_>>();
    let offset = offset.min(characters.len());
    let is_symbol_character =
        |character: char| character.is_alphanumeric() || matches!(character, '_' | ':' | '$' | '~');
    if offset < characters.len() && !is_symbol_character(characters[offset]) {
        return None;
    }
    let mut left = offset;
    while left > 0 && is_symbol_character(characters[left - 1]) {
        left -= 1;
    }
    let mut right = offset;
    while right < characters.len() && is_symbol_character(characters[right]) {
        right += 1;
    }
    let syntax_right = right;
    while left < right && characters[left] == ':' {
        left += 1;
    }
    while right > left && characters[right - 1] == ':' {
        right -= 1;
    }
    let symbol = characters[left..right].iter().collect::<String>();
    if !symbol
        .chars()
        .next()
        .is_some_and(|character| character.is_alphabetic() || matches!(character, '_' | '~'))
        || !is_callable_source_symbol(&symbol, &characters, syntax_right)
    {
        return None;
    }
    Some((symbol, left, right))
}

fn is_callable_source_symbol(symbol: &str, line: &[char], mut cursor: usize) -> bool {
    const NON_CALL_KEYWORDS: &[&str] = &[
        "if", "for", "while", "switch", "catch", "match", "loop", "sizeof", "alignof", "_Alignof",
        "typeof", "decltype", "typeid", "return",
    ];
    let name = symbol.rsplit("::").next().unwrap_or(symbol);
    if NON_CALL_KEYWORDS.contains(&name) {
        return false;
    }
    while line
        .get(cursor)
        .is_some_and(|character| character.is_whitespace())
    {
        cursor += 1;
    }
    if line.get(cursor) == Some(&'<') {
        let mut depth = 0_u32;
        while let Some(character) = line.get(cursor) {
            match character {
                '<' => depth += 1,
                '>' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        cursor += 1;
                        break;
                    }
                }
                _ => {}
            }
            cursor += 1;
        }
        if depth != 0 {
            return false;
        }
        while line
            .get(cursor)
            .is_some_and(|character| character.is_whitespace())
        {
            cursor += 1;
        }
    }
    line.get(cursor) == Some(&'(')
}

fn update_source_link_highlight(
    view: &sourceview5::View,
    buffer: &sourceview5::Buffer,
    tag: &gtk::TextTag,
    highlighted_range: &Rc<RefCell<Option<(i32, i32)>>>,
    x: f64,
    y: f64,
    active: bool,
) {
    clear_source_link_highlight(view, buffer, tag, highlighted_range);
    if !active {
        return;
    }
    let (x, y) = view.window_to_buffer_coords(
        gtk::TextWindowType::Widget,
        x.round() as i32,
        y.round() as i32,
    );
    let Some(iter) = view.iter_at_location(x, y) else {
        return;
    };
    let Some((_, start, end)) = source_symbol_span_at_iter(buffer, &iter) else {
        return;
    };
    let Some(line_start) = buffer.iter_at_line(iter.line()) else {
        return;
    };
    let Ok(start) = i32::try_from(start) else {
        return;
    };
    let Ok(end) = i32::try_from(end) else {
        return;
    };
    let start = line_start.offset() + start;
    let end = line_start.offset() + end;
    let start_iter = buffer.iter_at_offset(start);
    let end_iter = buffer.iter_at_offset(end);
    buffer.apply_tag(tag, &start_iter, &end_iter);
    highlighted_range.replace(Some((start, end)));
    view.set_cursor_from_name(Some("pointer"));
}

fn clear_source_link_highlight(
    view: &sourceview5::View,
    buffer: &sourceview5::Buffer,
    tag: &gtk::TextTag,
    highlighted_range: &Rc<RefCell<Option<(i32, i32)>>>,
) {
    if let Some((start, end)) = highlighted_range.take() {
        buffer.remove_tag(
            tag,
            &buffer.iter_at_offset(start),
            &buffer.iter_at_offset(end),
        );
    }
    view.set_cursor_from_name(None);
}

fn source_location_score(symbol: &str, location: &SourceLocation) -> u16 {
    let symbol = without_generic_arguments(symbol);
    let function = without_generic_arguments(&location.function);
    let mut score = if function == symbol {
        100
    } else if function == format!("__GI___libc_{symbol}") {
        98
    } else if function == format!("__GI_{symbol}") {
        97
    } else if function == format!("__libc_{symbol}") {
        95
    } else if function == format!("__{symbol}") {
        90
    } else if function.ends_with(&format!("::{symbol}")) {
        85
    } else if function.contains(&symbol) {
        40
    } else if symbol
        .rsplit("::")
        .next()
        .is_some_and(|name| function.ends_with(&format!("::{name}")))
    {
        30
    } else {
        0
    };
    let source_stem = Path::new(&location.file)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or_default();
    if source_stem == symbol || symbol.ends_with(&format!("::{source_stem}")) {
        score += 25;
    } else if source_stem.contains(&symbol) {
        score += 10;
    }
    score
}

fn without_generic_arguments(symbol: &str) -> String {
    let mut depth = 0_u32;
    symbol
        .chars()
        .filter(|character| match *character {
            '<' => {
                depth = depth.saturating_add(1);
                false
            }
            '>' if depth > 0 => {
                depth -= 1;
                false
            }
            _ => depth == 0,
        })
        .collect()
}

fn compact_function_name(symbol: &str) -> String {
    if symbol.chars().count() <= 56 || !symbol.contains(['<', '>']) {
        return symbol.to_owned();
    }

    let mut compact = String::with_capacity(symbol.len().min(80));
    let mut depth = 0_u32;
    for character in symbol.chars() {
        match character {
            '<' => {
                if depth == 0 {
                    compact.push('<');
                    compact.push('…');
                }
                depth = depth.saturating_add(1);
            }
            '>' if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    compact.push('>');
                }
            }
            _ if depth == 0 => compact.push(character),
            _ => {}
        }
    }
    if depth == 0 {
        compact
    } else {
        symbol.to_owned()
    }
}

fn scroll_source_document(document: &SourceDocument, line: u32) {
    let Ok(line) = i32::try_from(line.saturating_sub(1)) else {
        return;
    };
    let Some(mut iter) = document.buffer.iter_at_line(line) else {
        return;
    };
    document.buffer.place_cursor(&iter);
    let view = document.view.clone();
    gtk::glib::idle_add_local_once(move || {
        view.scroll_to_iter(&mut iter, 0.15, true, 0.0, 0.35);
    });
}

fn source_tab_title(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("source")
        .to_owned()
}

fn breakpoint_command_numbers(breakpoints: &[Breakpoint], watchpoints: bool) -> Vec<String> {
    let mut numbers = Vec::new();
    for breakpoint in breakpoints.iter().filter(|breakpoint| {
        if watchpoints {
            breakpoint.is_watchpoint()
        } else {
            !breakpoint.is_watchpoint()
                && !breakpoint.is_catchpoint()
                && !EventCatchpoint::ALL
                    .iter()
                    .any(|(event, _, _)| event.matches(breakpoint))
        }
    }) {
        let number = breakpoint.command_number();
        if !numbers.iter().any(|existing| existing == number) {
            numbers.push(number.to_owned());
        }
    }
    numbers
}

fn signal_catchpoint_command_numbers(breakpoints: &[Breakpoint]) -> Vec<String> {
    let mut numbers = Vec::new();
    for breakpoint in breakpoints
        .iter()
        .filter(|breakpoint| breakpoint.is_signal_catchpoint())
    {
        let number = breakpoint.command_number();
        if !numbers.iter().any(|existing| existing == number) {
            numbers.push(number.to_owned());
        }
    }
    numbers
}

fn event_catchpoint_command_numbers(breakpoints: &[Breakpoint]) -> Vec<String> {
    let mut numbers = Vec::new();
    for breakpoint in breakpoints.iter().filter(|breakpoint| {
        EventCatchpoint::ALL
            .iter()
            .any(|(event, _, _)| event.matches(breakpoint))
    }) {
        let number = breakpoint.command_number();
        if !numbers.iter().any(|existing| existing == number) {
            numbers.push(number.to_owned());
        }
    }
    numbers
}

fn event_catchpoint_command_number(
    breakpoints: &[Breakpoint],
    event: EventCatchpoint,
) -> Option<String> {
    breakpoints
        .iter()
        .find(|breakpoint| event.matches(breakpoint))
        .map(|breakpoint| breakpoint.command_number().to_owned())
}

fn breakpoint_command_number_at_address(
    breakpoints: &[Breakpoint],
    address: &str,
) -> Option<String> {
    breakpoints
        .iter()
        .find(|breakpoint| {
            !breakpoint.is_watchpoint()
                && breakpoint
                    .address
                    .as_deref()
                    .is_some_and(|candidate| addresses_equal(candidate, address))
        })
        .map(|breakpoint| breakpoint.command_number().to_owned())
}

fn normalized_signal_name(signal: &str) -> Option<String> {
    let signal = signal.trim().to_ascii_uppercase();
    if signal.is_empty()
        || !signal
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "_+-".contains(character))
    {
        return None;
    }
    if signal == "ALL" {
        Some(String::from("all"))
    } else if signal.starts_with("SIG")
        || signal
            .chars()
            .next()
            .is_some_and(|character| character.is_ascii_digit())
    {
        Some(signal)
    } else {
        Some(format!("SIG{signal}"))
    }
}

fn signal_catchpoint_command_number(breakpoints: &[Breakpoint], signal: &str) -> Option<String> {
    let signal = normalized_signal_name(signal)?;
    breakpoints
        .iter()
        .find(|breakpoint| {
            breakpoint.is_signal_catchpoint()
                && breakpoint
                    .original_location
                    .as_deref()
                    .is_some_and(|caught| {
                        if signal == "all" {
                            matches!(caught, "<any signal>" | "all")
                        } else {
                            caught.eq_ignore_ascii_case(&signal)
                        }
                    })
        })
        .map(|breakpoint| breakpoint.command_number().to_owned())
}

fn set_breakpoint_enabled(breakpoints: &mut [Breakpoint], number: &str, enabled: bool) -> bool {
    let mut changed = false;
    for breakpoint in breakpoints {
        if breakpoint.command_number() == number && breakpoint.enabled != enabled {
            breakpoint.enabled = enabled;
            changed = true;
        }
    }
    changed
}

fn remove_marks(buffer: &sourceview5::Buffer, category: &str) {
    let start = buffer.start_iter();
    let end = buffer.end_iter();
    buffer.remove_source_marks(&start, &end, Some(category));
}

fn addresses_equal(left: &str, right: &str) -> bool {
    fn normalized(address: &str) -> &str {
        address
            .strip_prefix("0x")
            .unwrap_or(address)
            .trim_start_matches('0')
    }
    normalized(left) == normalized(right)
}

fn connect_execution_button(
    button: &gtk::Button,
    ui: &Rc<Ui>,
    client: &Rc<MiClient>,
    command: &'static str,
    detail: &'static str,
) {
    let weak_ui = Rc::downgrade(ui);
    let client = Rc::clone(client);
    button.connect_clicked(move |_| {
        if let Some(ui) = weak_ui.upgrade() {
            issue_execution_command(&ui, &client, command, detail);
        }
    });
}

pub(crate) fn issue_execution_command(ui: &Ui, client: &MiClient, command: &str, detail: &str) {
    match client.send(command) {
        Ok(_) => {
            ui.set_command_pending(true);
            ui.set_status("Executing", detail, Some("status-running"));
        }
        Err(error) => ui.set_status("Command failed", &error.to_string(), Some("status-error")),
    }
}

fn request_signal_catchpoint_toggle(ui: &Ui, signal: &str) {
    let Some(signal) = normalized_signal_name(signal) else {
        ui.set_status(
            "Invalid signal",
            "Use a signal name such as SIGSEGV, RTMIN+1, or a signal number.",
            Some("status-error"),
        );
        return;
    };
    let existing = signal_catchpoint_command_number(&ui.breakpoints.borrow(), &signal);
    let progress = if existing.is_some() {
        format!("Removing the {signal} catchpoint…")
    } else {
        format!("Adding a {signal} catchpoint…")
    };
    ui.set_status("Updating signals", &progress, None);
    if let Some(handler) = ui.signal_catchpoint_handler.borrow().as_ref() {
        handler(signal, existing);
    } else {
        ui.set_status(
            "Catchpoint unavailable",
            "The debugger connection is not ready.",
            Some("status-error"),
        );
    }
}

fn set_status_widgets(
    status: &gtk::Label,
    detail_label: &gtk::Label,
    text: &str,
    detail: &str,
    class: Option<&str>,
) {
    for status_class in ["status-ready", "status-running", "status-error"] {
        status.remove_css_class(status_class);
    }
    if let Some(class) = class {
        status.add_css_class(class);
    }
    status.set_text(text);
    detail_label.set_text(detail);
    detail_label.set_tooltip_text(Some(detail));
}

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
