use std::{
    borrow::Cow,
    cell::{Cell, RefCell},
    cmp::Reverse,
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use gtk::{gio, glib, pango, prelude::*};
use sourceview5::prelude::*;
use vte4::prelude::*;

mod actions;
mod configuration;
mod debug_data;
pub(crate) use debug_data::DebugDataAction;
mod debugger_state;
mod domain;
use domain::{
    LocalVariableCatalog, MemoryRefreshBatch, TerminalSynchronization, VariableNodeIndex,
};
mod layout;
mod source_navigation;
mod value;

pub use actions::*;
pub(crate) use debugger_state::{DebuggerState, DebuggerStateDelta, TargetConnection};

#[cfg(test)]
use value::IntegerFormat;
pub(crate) use value::StringAssignmentKind;
use value::{
    FloatRepresentation, IntegerRadix, StringStorage, canonical_gdb_float, canonical_gdb_integer,
    format_character_value, format_float_value, format_integer_value, format_string_bytes,
    integer_decimal_value, is_rust_string, parse_character_input, parse_float_value,
    parse_integer_input, parse_string_input, register_integer_format, string_edit,
    variable_boolean_value, variable_character_format, variable_float_edit,
    variable_integer_format, variable_is_address,
};

use crate::{
    breakpoint_gutter::{BreakpointGutterRenderer, LineStyle},
    config::{ConfigurationReport, DebugSession, LaunchConfig},
    debug_info::ModuleDebugMetadata,
    debugger::{
        Breakpoint, GdbCapabilities, InferiorInfo, InferiorState, Instruction, MemoryBlock,
        MemoryKind, MiClient, Register, SharedLibrary, SourceFile, SourceLocation, StackEntry,
        StackFrame, TargetArchitecture, TargetEndian, ThreadInfo, ValueTypeKind, ValueTypeMetadata,
        Variable, VariableUpdate,
        context::{MemoryRegion, memory_region_for_address},
    },
    kernel::{
        KernelBaseline, KernelFileDescriptor, KernelLimit, KernelMapping, KernelMappingChange,
        KernelMemoryCategory, KernelProcess, KernelSignal, KernelSnapshot, KernelThread,
        KernelTlsModule, KernelTlsSymbol, ProcessArgument, ProcessEnvironment,
        ProcessStartupSnapshot,
    },
    misc::{
        AllocatorRegion, AllocatorSnapshot, AuxvEntry, CallAbiFact, CallAbiPhase, CallAbiRegister,
        CallAbiSnapshot, CoreDumpSnapshot, CoreMappedFile, CoreNote, HeapInspectionRow,
        HeapInspectionSnapshot, LiveMiscSnapshot, LockDependency, LockSnapshot, LockWait,
    },
    source,
    theme::Theme,
};

const EXECUTION_CATEGORY: &str = "execution";
const MAX_EXPRESSION_WATCHES: usize = 256;
const MAX_MEMORY_WATCHES: usize = 256;
const DISCLOSURE_EXPANDED_ICON: &str = "▾";
const DISCLOSURE_COLLAPSED_ICON: &str = "›";

fn known_gdb_prompt(text: &str) -> bool {
    matches!(text.trim(), "(gdb)" | "(rr)" | "gef➤" | "gef>" | "pwndbg>")
}

fn set_execution_sensitive<W: IsA<gtk::Widget>>(widget: &W, sensitive: bool, busy: bool) {
    let widget = widget.upcast_ref::<gtk::Widget>();

    if !busy || sensitive {
        if widget.has_css_class("execution-interlocked") {
            widget.remove_css_class("execution-interlocked");
        }
    } else if widget.is_sensitive() && !widget.has_css_class("execution-interlocked") {
        // Preserve the appearance only when this execution transition is what
        // made an otherwise available control insensitive.
        widget.add_css_class("execution-interlocked");
    }

    if widget.is_sensitive() != sensitive {
        widget.set_sensitive(sensitive);
    }
}

/// Keep a previously available action visually stable while a short execution
/// command is in flight. The corresponding signal handlers still validate
/// debugger state before issuing a command, so durable unavailable states
/// remain insensitive.
fn set_transient_execution_sensitive<W: IsA<gtk::Widget>>(widget: &W, sensitive: bool, busy: bool) {
    let widget = widget.upcast_ref::<gtk::Widget>();

    if widget.has_css_class("execution-interlocked") {
        widget.remove_css_class("execution-interlocked");
    }

    if (!busy || sensitive || !widget.is_sensitive()) && widget.is_sensitive() != sensitive {
        widget.set_sensitive(sensitive);
    }
}

fn set_label_text(label: &gtk::Label, text: &str) {
    if label.text().as_str() != text {
        label.set_text(text);
    }
}

fn set_css_class(widget: &impl IsA<gtk::Widget>, class: &str, enabled: bool) {
    if enabled && !widget.has_css_class(class) {
        widget.add_css_class(class);
    } else if !enabled && widget.has_css_class(class) {
        widget.remove_css_class(class);
    }
}

fn configured_target_can_start(
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ControlState {
    busy: bool,
    run: bool,
    pause: bool,
    move_target: bool,
    inspect: bool,
    syntax: bool,
    gef_tools: bool,
    heap_inspector_in_flight: bool,
    heap_action_visibility: u64,
    edit_local: bool,
    manage_watches: bool,
    add_watch: bool,
    remove_watch: bool,
    add_memory: bool,
    session: bool,
    new_session: bool,
    restart_session: bool,
    kill_session: bool,
    detach_session: bool,
    restart_gdb: bool,
    resynchronize: bool,
    edit_stop_points: bool,
    add_signal: bool,
    delete_signal_catchpoints: bool,
    delete_event_catchpoints: bool,
    delete_breakpoints: bool,
    delete_watchpoints: bool,
}

#[derive(Clone, PartialEq, Eq)]
struct ThreadRenderState {
    source_threads: Vec<ThreadInfo>,
    rendered_threads: Vec<ThreadInfo>,
    stop_reason: Option<String>,
    executable_name: Option<String>,
    query: String,
    state_filter: u32,
    sort: u32,
}

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
type VariableEditorHandler = Rc<dyn Fn(Variable)>;
type FloatAssignmentHandler = Rc<dyn Fn(Variable, Vec<u8>)>;
type VariableChildrenHandler = Rc<dyn Fn(Variable, usize)>;
type VariableViewerHandler = Rc<dyn Fn(VariableViewerRequest)>;
type ExpressionWatchRefreshHandler = Rc<dyn Fn()>;
type StringAssignmentHandler = Rc<dyn Fn(Variable, Vec<u8>, StringAssignmentKind)>;
type VectorAssignmentHandler = Rc<dyn Fn(String, String, Vec<(usize, String)>)>;
type BreakpointConditionHandler = Rc<dyn Fn(String, Option<String>)>;
type BreakpointEditorHandler = Rc<dyn Fn(BreakpointEditRequest)>;
type BreakpointEnabledHandler = Rc<dyn Fn(String, bool)>;
type BreakpointBulkDeleteHandler = Rc<dyn Fn(Vec<String>)>;
type BreakpointInsertHandler = Rc<dyn Fn(PathBuf, u32)>;
type SourceJumpHandler = Rc<dyn Fn(PathBuf, u32)>;
type SourceDiscoveryHandler = Rc<dyn Fn(SourceDiscoveryRequest)>;
type SourceTreePathHandler = Rc<dyn Fn(PathBuf)>;
type SourceTreeRefreshHandler = Rc<dyn Fn()>;
type DebugDataActionHandler = Rc<dyn Fn(debug_data::DebugDataAction)>;
type InferiorActionHandler = Rc<dyn Fn(InferiorAction)>;
type ThreadActionHandler = Rc<dyn Fn(ThreadAction)>;
type SignalCatchpointHandler = Rc<dyn Fn(String, Option<String>)>;
type EventCatchpointHandler = Rc<dyn Fn(EventCatchpoint, Option<String>)>;
type WatchpointInsertHandler = Rc<dyn Fn(WatchpointRequest)>;
type FilteredCatchpointHandler = Rc<dyn Fn(FilteredCatchpointRequest)>;
type MemoryWatchHandler = Rc<dyn Fn(u64, String, usize)>;
type InstructionMemoryHandler = Rc<dyn Fn(String)>;
type DisassemblyHandler = Rc<dyn Fn(DisassemblyRequest)>;
type DisassemblySourceCache =
    Rc<RefCell<crate::performance::BoundedLruCache<PathBuf, Rc<Vec<Rc<str>>>>>>;
type KernelRefreshHandler = Rc<dyn Fn()>;
type MiscRefreshHandler = Rc<dyn Fn()>;
type HeapInspectionHandler = Rc<dyn Fn(HeapInspectionRequest)>;
type KernelSectionHandler = Rc<dyn Fn(&str, bool)>;
type DebugSessionHandler = Rc<dyn Fn(DebugSession)>;
type SessionActionHandler = Rc<dyn Fn(SessionAction)>;
type UntilActionHandler = Rc<dyn Fn(UntilAction)>;
type UntilCancelHandler = Rc<dyn Fn()>;
type UntilAbortHandler = Rc<dyn Fn()>;
type UntilStopHandler = Rc<dyn Fn(Option<&str>, Option<&str>, Option<&str>) -> bool>;

struct KernelViewBindings<'a> {
    refresh_handler: &'a Rc<RefCell<Option<KernelRefreshHandler>>>,
    remembered_disclosures: &'a HashMap<String, bool>,
    section_handler: &'a Rc<RefCell<Option<KernelSectionHandler>>>,
}

struct MiscViewBindings<'a> {
    refresh_handler: &'a Rc<RefCell<Option<MiscRefreshHandler>>>,
}

struct InspectorBindings<'a> {
    theme: &'a Theme,
    variable_children_handler: &'a Rc<RefCell<Option<VariableChildrenHandler>>>,
    variable_viewer_handler: &'a Rc<RefCell<Option<VariableViewerHandler>>>,
    variable_viewers: &'a Rc<VariableViewerRegistry>,
    target_pointer_bits: &'a Rc<Cell<u32>>,
    kernel: KernelViewBindings<'a>,
    misc: MiscViewBindings<'a>,
}

#[derive(Clone)]
struct ValueEditorHandlers {
    stop_generation: Rc<Cell<u64>>,
    can_edit: Rc<dyn Fn() -> bool>,
    assignment: Rc<RefCell<Option<VariableAssignmentHandler>>>,
    float: Rc<RefCell<Option<FloatAssignmentHandler>>>,
    string: Rc<RefCell<Option<StringAssignmentHandler>>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct VariableEditorRequest {
    pub(crate) generation: u64,
    id: u64,
}

#[derive(Default)]
struct RefreshGate {
    in_flight: Cell<bool>,
    queued: Cell<bool>,
}

impl RefreshGate {
    fn begin(&self) -> bool {
        if self.in_flight.replace(true) {
            self.queued.set(true);

            false
        } else {
            true
        }
    }

    fn finish(&self) -> bool {
        self.in_flight.set(false);

        self.queued.replace(false)
    }

    fn invalidate(&self) {
        if self.in_flight.get() {
            self.queued.set(true);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EventCatchpoint {
    CxxThrow,
    CxxCatch,
    CxxRethrow,
    RustPanic,
    Exec,
    Fork,
    Vfork,
    Syscall,
    LibraryLoad,
    LibraryUnload,
}

impl EventCatchpoint {
    const ALL: [(Self, &'static str, &'static str); 10] = [
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
            Self::CxxRethrow,
            "C++ rethrow",
            "Stop when a C++ exception is rethrown",
        ),
        (
            Self::RustPanic,
            "Rust panic",
            "Stop at Rust's panic runtime entry point",
        ),
        (Self::Exec, "exec", "Stop when the inferior calls exec"),
        (Self::Fork, "fork", "Stop when the inferior forks"),
        (Self::Vfork, "vfork", "Stop when the inferior calls vfork"),
        (
            Self::Syscall,
            "syscall",
            "Stop at every system call. This can trigger very frequently",
        ),
        (
            Self::LibraryLoad,
            "library load",
            "Stop when any shared library is loaded",
        ),
        (
            Self::LibraryUnload,
            "library unload",
            "Stop when any shared library is unloaded",
        ),
    ];

    pub(crate) const fn command(self) -> &'static str {
        match self {
            Self::CxxThrow => "catch throw",
            Self::CxxCatch => "catch catch",
            Self::CxxRethrow => "catch rethrow",
            Self::RustPanic => "break rust_panic",
            Self::Exec => "catch exec",
            Self::Fork => "catch fork",
            Self::Vfork => "catch vfork",
            Self::Syscall => "catch syscall",
            Self::LibraryLoad => "catch load",
            Self::LibraryUnload => "catch unload",
        }
    }

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::CxxThrow => "C++ throw",
            Self::CxxCatch => "C++ catch",
            Self::CxxRethrow => "C++ rethrow",
            Self::RustPanic => "Rust panic",
            Self::Exec => "exec",
            Self::Fork => "fork",
            Self::Vfork => "vfork",
            Self::Syscall => "syscall",
            Self::LibraryLoad => "library load",
            Self::LibraryUnload => "library unload",
        }
    }

    fn matches(self, breakpoint: &Breakpoint) -> bool {
        match self {
            Self::CxxThrow => breakpoint.catch_type.as_deref() == Some("throw"),
            Self::CxxCatch => breakpoint.catch_type.as_deref() == Some("catch"),
            Self::CxxRethrow => breakpoint.catch_type.as_deref() == Some("rethrow"),
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
            Self::Vfork => breakpoint.catch_type.as_deref() == Some("vfork"),
            Self::Syscall => {
                breakpoint.catch_type.as_deref() == Some("syscall")
                    && breakpoint.original_location.as_deref() == Some("<any syscall>")
            }
            Self::LibraryLoad => {
                breakpoint.catch_type.as_deref() == Some("load")
                    && breakpoint.original_location.as_deref() == Some("load of library")
            }
            Self::LibraryUnload => {
                breakpoint.catch_type.as_deref() == Some("unload")
                    && breakpoint.original_location.as_deref() == Some("unload of library")
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FilteredCatchpointKind {
    Syscall,
    LibraryLoad,
    LibraryUnload,
}

impl FilteredCatchpointKind {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Syscall => "syscall",
            Self::LibraryLoad => "library load",
            Self::LibraryUnload => "library unload",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FilteredCatchpointRequest {
    pub kind: FilteredCatchpointKind,
    pub filter: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum WatchpointRequest {
    Standard {
        expression: String,
        access: WatchpointAccess,
    },
    Masked {
        expression: String,
        mask: String,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct StopPointMetadata {
    group: Option<String>,
    tags: Vec<String>,
}

#[derive(Clone)]
struct StopPointFilterRow {
    widgets: Vec<gtk::Widget>,
    number: String,
    searchable: String,
    status: gtk::Label,
    hardware: bool,
    watchpoint: bool,
    catchpoint: bool,
    enabled: bool,
}

#[derive(Clone)]
struct StopPointFilterControls {
    search: gtk::SearchEntry,
    kind: gtk::DropDown,
    empty: gtk::Label,
}

#[derive(Clone)]
struct FilteredCatchpointControls {
    kind: gtk::DropDown,
    filter: gtk::Entry,
    add: gtk::Button,
}

#[derive(Clone, PartialEq, Eq)]
struct InstructionRowData {
    instruction: Instruction,
    current: bool,
    pointer_bits: u32,
    source_text: Option<Rc<str>>,
}

#[derive(Clone)]
struct CallAbiInstructionContext {
    current: Instruction,
    previous: Option<Instruction>,
    target_resolution: Option<CallAbiTargetResolution>,
    pending_target: Option<String>,
}

#[derive(Clone)]
struct CallAbiTargetResolution {
    expression: String,
    display: String,
}

#[derive(Clone)]
pub(crate) struct CallAbiTargetRequest {
    pub generation: u64,
    pub instruction_address: String,
    pub expression: String,
}

#[derive(Clone)]
struct DisassemblyControls {
    back: gtk::Button,
    forward: gtk::Button,
    previous_function: gtk::Button,
    next_function: gtk::Button,
    location: gtk::Entry,
    go: gtk::Button,
    current_pc: gtk::Button,
    mixed: gtk::ToggleButton,
    syntax_intel: gtk::ToggleButton,
    syntax_att: gtk::ToggleButton,
    follow: gtk::Button,
    open_memory: gtk::Button,
    range: gtk::Label,
    source_column: gtk::ColumnViewColumn,
    scrolled: gtk::ScrolledWindow,
    scroll_generation: Rc<Cell<u64>>,
    loading: Rc<Cell<bool>>,
    syntax_applicable: Rc<Cell<bool>>,
    setting_syntax: Rc<Cell<bool>>,
}

#[derive(Clone, PartialEq, Eq)]
struct RegisterRowData {
    register: Register,
    changed: bool,
    ring: Option<u64>,
    architecture: TargetArchitecture,
    endian: Option<TargetEndian>,
    pointer_bits: u32,
}

#[derive(Clone)]
struct VariableNode {
    variable: Variable,
    search_text: Rc<str>,
    children: gio::ListStore,
    children_loaded: Rc<Cell<bool>>,
    children_loading: Rc<Cell<bool>>,
    expanded: Rc<Cell<bool>>,
    changed: bool,
    load_more: Option<(Variable, usize)>,
    placeholder: bool,
}

impl VariableNode {
    fn new(variable: Variable) -> Self {
        let search_text = variable_search_text(&variable).into();

        Self {
            variable,
            search_text,
            children: gio::ListStore::new::<glib::BoxedAnyObject>(),
            children_loaded: Rc::new(Cell::new(false)),
            children_loading: Rc::new(Cell::new(false)),
            expanded: Rc::new(Cell::new(false)),
            changed: false,
            load_more: None,
            placeholder: false,
        }
    }

    fn placeholder(name: &str, value: &str) -> Self {
        let variable = Variable {
            local_index: None,
            name: name.to_owned(),
            value: value.to_owned(),
            type_name: None,
            argument: false,
            varobj: None,
            num_children: 0,
            has_more: false,
            display_hint: None,
            dynamic: false,
        };

        Self {
            search_text: variable_search_text(&variable).into(),
            variable,
            children: gio::ListStore::new::<glib::BoxedAnyObject>(),
            children_loaded: Rc::new(Cell::new(true)),
            children_loading: Rc::new(Cell::new(false)),
            expanded: Rc::new(Cell::new(false)),
            changed: false,
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

        let variable = Variable {
            local_index: None,
            name: String::from("Load more…"),
            value: detail,
            type_name: None,
            argument: false,
            varobj: None,
            num_children: 0,
            has_more: false,
            display_hint: None,
            dynamic: false,
        };

        Self {
            search_text: variable_search_text(&variable).into(),
            variable,
            children: gio::ListStore::new::<glib::BoxedAnyObject>(),
            children_loaded: Rc::new(Cell::new(true)),
            children_loading: Rc::new(Cell::new(false)),
            expanded: Rc::new(Cell::new(false)),
            changed: false,
            load_more: Some((parent, next)),
            placeholder: true,
        }
    }

    fn load_more_error(parent: Variable, next: usize, error: &str) -> Self {
        let mut node = Self::load_more(parent, next);
        node.variable.name = String::from("Retry loading more…");
        node.variable.value = error.to_owned();

        node
    }

    fn retry_expansion(parent: Variable, error: &str) -> Self {
        let mut node = Self::load_more(parent, 0);
        node.variable.name = String::from("Retry expansion…");
        node.variable.value = error.to_owned();

        node
    }

    fn updated(&self, variable: Variable, mark_changed: bool) -> Self {
        let structure_unchanged = self.variable.varobj == variable.varobj
            && self.variable.type_name == variable.type_name
            && self.variable.num_children == variable.num_children
            && self.variable.has_more == variable.has_more
            && self.variable.dynamic == variable.dynamic;

        Self {
            changed: if mark_changed {
                self.variable.value != variable.value
            } else {
                self.changed
            },
            search_text: variable_search_text(&variable).into(),
            variable,
            children: if structure_unchanged {
                self.children.clone()
            } else {
                gio::ListStore::new::<glib::BoxedAnyObject>()
            },
            children_loaded: if structure_unchanged {
                Rc::clone(&self.children_loaded)
            } else {
                Rc::new(Cell::new(false))
            },
            children_loading: if structure_unchanged {
                Rc::clone(&self.children_loading)
            } else {
                Rc::new(Cell::new(false))
            },
            expanded: Rc::clone(&self.expanded),
            load_more: None,
            placeholder: false,
        }
    }

    fn has_changes(&self) -> bool {
        if self.changed {
            return true;
        }

        let mut pending = vec![self.children.clone()];

        while let Some(store) = pending.pop() {
            for position in 0..store.n_items() {
                let Some(item) = store.item(position).and_downcast::<glib::BoxedAnyObject>() else {
                    continue;
                };

                let node = item.borrow::<VariableNode>();

                if node.changed {
                    return true;
                }

                pending.push(node.children.clone());
            }
        }

        false
    }

    fn without_change_marker(&self) -> Self {
        Self {
            variable: self.variable.clone(),
            search_text: Rc::clone(&self.search_text),
            children: self.children.clone(),
            children_loaded: Rc::clone(&self.children_loaded),
            children_loading: Rc::clone(&self.children_loading),
            expanded: Rc::clone(&self.expanded),
            changed: false,
            load_more: self.load_more.clone(),
            placeholder: self.placeholder,
        }
    }

    fn rebound(&self) -> Self {
        self.clone()
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

#[derive(Clone, Debug, PartialEq, Eq)]
struct SourceNavigationLocation {
    path: PathBuf,
    line: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ClosedSourceTab {
    path: PathBuf,
    line: u32,
}

#[derive(Clone)]
struct SourceNavigationControls {
    back: gtk::Button,
    forward: gtk::Button,
    quick_open: gtk::Button,
    open_file: gtk::Button,
    find: gtk::Button,
    go_to_line: gtk::Button,
    symbols: gtk::Button,
    loaded_search: gtk::Button,
    tree_search: gtk::Button,
    reopen_closed: gtk::Button,
    find_bar: gtk::Box,
    find_entry: gtk::Entry,
    find_count: gtk::Label,
    find_previous: gtk::Button,
    find_next: gtk::Button,
    find_case: gtk::ToggleButton,
    find_close: gtk::Button,
}

struct SourceEditorPanel {
    root: gtk::Box,
    navigation: SourceNavigationControls,
}

struct SourceFindState {
    path: PathBuf,
    context: sourceview5::SearchContext,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SourceSearchMode {
    Files,
    Symbols,
    LoadedText,
    Tree,
}

#[derive(Clone, Debug)]
pub(crate) enum SourceDiscoveryRequest {
    LoadedFiles(u64),
    Symbols { query: String, generation: u64 },
}

struct SourcePalette {
    window: gtk::Window,
    mode: SourceSearchMode,
    entry: gtk::Entry,
    results: gtk::Box,
    status: gtk::Label,
    loaded_files: Arc<Vec<PathBuf>>,
    loaded_search: Arc<source::SourceSearchIndex>,
    loaded_files_ready: bool,
    tree_files: Arc<Vec<PathBuf>>,
    tree_search: Arc<source::SourceSearchIndex>,
    scope: Option<PathBuf>,
}

#[derive(Clone)]
struct SourceTreeNode {
    data: Arc<source::SourceTreeNodeData>,
}

#[derive(Clone)]
struct SourceTreeControls {
    root: gtk::Box,
    search: gtk::Entry,
    status: gtk::Label,
    roots: gio::ListStore,
    model: gtk::TreeListModel,
    selection: gtk::SingleSelection,
    view: gtk::ListView,
    file_routes: Rc<RefCell<HashMap<source::SourceId, Box<[u32]>>>>,
    open_handler: Rc<RefCell<Option<SourceTreePathHandler>>>,
    search_handler: Rc<RefCell<Option<SourceTreePathHandler>>>,
    refresh_handler: Rc<RefCell<Option<SourceTreeRefreshHandler>>>,
}

#[derive(Clone)]
struct InferiorControls {
    summary: gtk::Box,
    page: gtk::Box,
    selector: gtk::DropDown,
    selector_model: gtk::StringList,
    selector_ids: Rc<RefCell<Vec<String>>>,
    selector_updating: Rc<Cell<bool>>,
    selected_state: gtk::Label,
    stop_owner: gtk::Label,
    list: gtk::Box,
    cards: Rc<RefCell<Vec<(String, InferiorCardControls)>>>,
    follow_parent: gtk::ToggleButton,
    follow_child: gtk::ToggleButton,
    detach_on_fork: gtk::CheckButton,
    switch_parent: gtk::Button,
    switch_child: gtk::Button,
    refresh: gtk::Button,
    action_handler: Rc<RefCell<Option<InferiorActionHandler>>>,
}

#[derive(Clone)]
struct InferiorCardControls {
    root: gtk::Box,
    name: gtk::Label,
    state: gtk::Label,
    facts: gtk::Label,
    relationship: gtk::Label,
    select: gtk::Button,
    execution: gtk::Button,
    execution_action: Rc<RefCell<Option<InferiorAction>>>,
}

#[derive(Clone)]
struct ThreadControls {
    root: gtk::Box,
    list: gtk::Box,
    summary: gtk::Label,
    search: gtk::SearchEntry,
    state_filter: gtk::DropDown,
    sort: gtk::DropDown,
    scheduler_locking: gtk::DropDown,
    scheduler_updating: Rc<Cell<bool>>,
    non_stop: gtk::CheckButton,
    mode_note: gtk::Label,
    refresh: gtk::Button,
    run_only: gtk::Button,
    freeze: gtk::Button,
    thaw: gtk::Button,
    backtraces: gtk::Button,
    compare: gtk::Button,
    compare_left: gtk::DropDown,
    compare_right: gtk::DropDown,
    compare_left_model: gtk::StringList,
    compare_right_model: gtk::StringList,
    compare_ids: Rc<RefCell<Vec<String>>>,
    compare_updating: Rc<Cell<bool>>,
    action_handler: Rc<RefCell<Option<ThreadActionHandler>>>,
    action_pending: Rc<Cell<Option<ThreadActionPending>>>,
    analysis_generation: Rc<Cell<u64>>,
    analysis_window: Rc<RefCell<Option<gtk::Window>>>,
    analysis_content: Rc<RefCell<Option<gtk::Box>>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MemoryWatchFormat {
    Bytes,
    U16,
    U32,
    U64,
    F32,
    F64,
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
    page: gtk::Box,
    page_offset: Rc<Cell<i64>>,
    status: gtk::Label,
    range: gtk::Label,
    offset: gtk::Label,
    store: gio::ListStore,
    selection: gtk::SingleSelection,
    follow_button: gtk::Button,
    previous_begin: Rc<Cell<Option<u64>>>,
    previous_bytes: Rc<RefCell<Vec<u8>>>,
}

#[derive(Clone)]
struct MemoryWatchContainer {
    notebook: gtk::Notebook,
    empty: gtk::Label,
    refresh_all: gtk::Button,
    clear_all: gtk::Button,
    refresh_batch: Rc<RefCell<MemoryRefreshBatch>>,
    commands_available: Rc<Cell<bool>>,
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

#[derive(Clone)]
struct KernelView {
    root: gtk::Box,
    wide_subtabs: gtk::Box,
    compact_subtabs: gtk::Box,
    pages: gtk::Stack,
    active: Rc<Cell<bool>>,
    in_flight: Rc<Cell<bool>>,
    needs_refresh: Rc<Cell<bool>>,
    tls_requested: Rc<Cell<bool>>,
    metadata_only_refresh: Rc<Cell<bool>>,
    warnings: gtk::Box,
    previous_snapshot: Rc<RefCell<Option<KernelBaseline>>>,
    overview_store: gio::ListStore,
    resource_store: gio::ListStore,
    tls_runtime_store: gio::ListStore,
    tls_runtime: Rc<RefCell<KernelTlsRuntime>>,
    tls_module_store: gio::ListStore,
    tls_module_count: gtk::Label,
    tls_modules_empty: gtk::Label,
    tls_symbol_store: gio::ListStore,
    tls_symbol_count: gtk::Label,
    tls_symbols_empty: gtk::Label,
    tls_metadata: gtk::Stack,
    change_store: gio::ListStore,
    mapping_change_store: gio::ListStore,
    mapping_change_count: gtk::Label,
    mapping_changes_empty: gtk::Label,
    changes_split: gtk::Paned,
    memory_store: gio::ListStore,
    private_mapping_store: gio::ListStore,
    memory_summary: KernelMemorySummaryView,
    memory_empty: gtk::Label,
    private_mapping_empty: gtk::Label,
    thread_store: gio::ListStore,
    thread_count: gtk::Label,
    threads_empty: gtk::Label,
    signal_store: gio::ListStore,
    signal_count: gtk::Label,
    signals_empty: gtk::Label,
    mapping_store: gio::ListStore,
    mapping_count: gtk::Label,
    mappings_empty: gtk::Label,
    descriptor_store: gio::ListStore,
    descriptor_count: gtk::Label,
    descriptors_empty: gtk::Label,
    limit_store: gio::ListStore,
    limit_count: gtk::Label,
    limits_empty: gtk::Label,
    process_store: gio::ListStore,
    process_count: gtk::Label,
    processes_empty: gtk::Label,
}

#[derive(Clone)]
struct MiscStartupSummary {
    argc: gtk::Label,
    argv: gtk::Label,
    envp: gtk::Label,
    env: gtk::Label,
}

#[derive(Clone)]
struct MiscView {
    root: gtk::Box,
    wide_subtabs: gtk::Box,
    compact_subtabs: gtk::Box,
    active: Rc<Cell<bool>>,
    in_flight: Rc<Cell<bool>>,
    needs_refresh: Rc<Cell<bool>>,
    pages: gtk::Stack,
    cfg: CfgView,
    allocator_requested: Rc<Cell<bool>>,
    allocator_probe_fresh: Rc<Cell<bool>>,
    allocator_probe_cache: Rc<RefCell<Option<crate::misc::AllocatorProbe>>>,
    locks_requested: Rc<Cell<bool>>,
    summary: MiscStartupSummary,
    warning: gtk::Label,
    arguments_store: gio::ListStore,
    arguments_empty: gtk::Label,
    environment_store: gio::ListStore,
    environment_empty: gtk::Label,
    startup_split: gtk::Paned,
    auxv_summary: gtk::Label,
    auxv_store: gio::ListStore,
    auxv_empty: gtk::Label,
    call_abi_summary: gtk::Label,
    call_abi_context: gtk::Label,
    call_abi_register_store: gio::ListStore,
    call_abi_register_empty: gtk::Label,
    call_abi_contract_store: gio::ListStore,
    call_abi_split: gtk::Paned,
    allocator_implementation: gtk::Label,
    allocator_basis: gtk::Label,
    allocator_bindings: gtk::Label,
    allocator_runtimes: gtk::Label,
    allocator_frontends: gtk::Label,
    allocator_evidence: gtk::Label,
    allocator_safety: gtk::Label,
    allocator_heap_bytes: gtk::Label,
    allocator_anonymous_bytes: gtk::Label,
    allocator_mapping_count: gtk::Label,
    allocator_store: gio::ListStore,
    allocator_empty: gtk::Label,
    heap_inspector_actions: Vec<(gtk::Button, HeapInspectionAction)>,
    heap_inspector_expression: gtk::Entry,
    heap_inspector_status: gtk::Label,
    heap_inspector_command: gtk::Label,
    heap_inspector_store: gio::ListStore,
    heap_inspector_empty: gtk::Label,
    heap_inspector_in_flight: Rc<Cell<bool>>,
    lock_summary: gtk::Label,
    lock_note: gtk::Label,
    lock_store: gio::ListStore,
    lock_empty: gtk::Label,
    lock_graph_summary: gtk::Label,
    lock_dependency_store: gio::ListStore,
    lock_graph_empty: gtk::Label,
    lock_split: gtk::Paned,
    core_summary: gtk::Label,
    core_warning: gtk::Label,
    core_note_store: gio::ListStore,
    core_file_store: gio::ListStore,
    core_empty: gtk::Label,
    core_split: gtk::Paned,
}

#[derive(Clone, PartialEq, Eq)]
struct KernelOverviewRow {
    section: bool,
    section_key: String,
    label: String,
    value: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct KernelTlsRuntime {
    thread: Option<String>,
    architecture: TargetArchitecture,
    endian: Option<TargetEndian>,
    pointer_bits: u32,
    register: Option<String>,
    base: Option<u64>,
    mapping: Option<String>,
    bytes: Vec<u8>,
    error: Option<String>,
}

#[derive(Clone, PartialEq, Eq)]
struct KernelTlsSymbolRow {
    module: Rc<str>,
    path: Rc<str>,
    symbol: KernelTlsSymbol,
}

#[derive(Clone, PartialEq, Eq)]
struct KernelMemoryRow {
    category: KernelMemoryCategory,
    page_size: u64,
    total_unique: u64,
}

#[derive(Clone, PartialEq, Eq)]
struct KernelPrivateMappingRow {
    mapping: Rc<KernelMapping>,
    page_size: u64,
    total_unique: u64,
}

#[derive(Clone)]
struct KernelMemorySummaryView {
    meta: gtk::Label,
    rows: Vec<KernelMemoryUnitRow>,
    private_summary: KernelPrivateSummaryView,
}

#[derive(Clone)]
struct KernelPrivateSummaryView {
    total: gtk::Label,
    clean: gtk::Label,
    dirty: gtk::Label,
    mappings: gtk::Label,
}

#[derive(Clone)]
struct KernelMemoryUnitRow {
    kib: gtk::Label,
    mib: gtk::Label,
    gib: gtk::Label,
    pages: gtk::Label,
}

impl MemoryWatchFormat {
    const fn label(self) -> &'static str {
        match self {
            Self::Bytes => "HEX BYTES",
            Self::U16 => "16-BIT VALUES",
            Self::U32 => "32-BIT VALUES",
            Self::U64 => "64-BIT VALUES",
            Self::F32 => "32-BIT FLOATS",
            Self::F64 => "64-BIT FLOATS",
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
    source_index: &'a Rc<RefCell<Option<Arc<source::SourceIndex>>>>,
    insert_handler: &'a Rc<RefCell<Option<BreakpointInsertHandler>>>,
    jump_handler: &'a Rc<RefCell<Option<SourceJumpHandler>>>,
    delete_handler: &'a Rc<RefCell<Option<StringSelectionHandler>>>,
    enabled_handler: &'a Rc<RefCell<Option<BreakpointEnabledHandler>>>,
    symbol_handler: &'a Rc<RefCell<Option<StringSelectionHandler>>>,
    closed_tabs: &'a Rc<RefCell<Vec<ClosedSourceTab>>>,
    reopen_closed: &'a gtk::Button,
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
    Registers,
    Path,
}

pub(crate) const GEF_COMMAND_CAPABILITIES: &[&str] = &[
    "context off",
    "context on",
    "gef config context.enable",
    "xinfo",
    "ii",
    "registers",
    "telescope",
    "dumpargs",
    "syscall-args",
    "future-calls",
    "stack-frame",
    "vmmap",
    "proc-info",
    "xfiles",
    "argv",
    "envp",
    "fds",
    "auxv",
    "errno",
    "tls",
    "follow",
    "checksec",
    "elf-info",
    "got",
    "got-all",
    "canary",
    "dwarf-exception-handler",
    "dynamic",
    "link-map",
    "dt",
];

#[derive(Clone)]
struct GefCapabilityControl {
    widget: gtk::Widget,
    capability: &'static str,
}

#[derive(Clone)]
struct GefCapabilityGroup {
    widget: gtk::Widget,
    capabilities: Vec<&'static str>,
}

struct GefToolsMenu {
    button: gtk::ToggleButton,
    content: gtk::Box,
    controls: Vec<GefCapabilityControl>,
    groups: Vec<GefCapabilityGroup>,
}

const INITIAL_SOURCE: &str = r#"// fgdb is connected to a real GDB terminal.
//
// Source opens automatically at the first source-backed stop.
// Use “Open file…” in the source toolbar to keep several files in tabs.
//
// F5        run / continue       F6        pause
// F10       step over            F11       step into
// Ctrl+F10  next instruction     Ctrl+F11  step instruction
// Shift+F11 finish function
//
// Ctrl+P    quick open source    Ctrl+O     open files from disk
// Ctrl+F    find in source       Ctrl+G     go to source line
// Ctrl+Shift+O search symbols
// Ctrl+Shift+F search source tree
// Alt+Left / Alt+Right navigate source history
//
// Ctrl+hover underlines navigable symbols. Ctrl+click opens definitions.
// Double-click an instruction to toggle an address breakpoint.
"#;

#[derive(Clone)]
pub struct Ui {
    pub window: gtk::ApplicationWindow,
    pub terminal: vte4::Terminal,
    session_button: gtk::ToggleButton,
    session_popover: gtk::Popover,
    session_kind_label: gtk::Label,
    session_target_label: gtk::Label,
    new_session_button: gtk::Button,
    restart_session_button: gtk::Button,
    kill_session_button: gtk::Button,
    detach_session_button: gtk::Button,
    restart_gdb_button: gtk::Button,
    resynchronize_button: gtk::Button,
    configuration_button: gtk::Button,
    gdb_capabilities_label: gtk::Label,
    target_label: gtk::Label,
    terminal_toggle_button: gtk::ToggleButton,
    debug_data_button: gtk::Button,
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
    gef_tools_content: gtk::Box,
    gef_tool_controls: Vec<GefCapabilityControl>,
    gef_tool_groups: Vec<GefCapabilityGroup>,
    pub status_label: gtk::Label,
    pub status_detail: gtk::Label,
    status_visual_generation: Rc<Cell<u64>>,
    pause_visual_generation: Rc<Cell<u64>>,
    inspector_notebook: gtk::Notebook,
    source_notebook: gtk::Notebook,
    source_documents: Rc<RefCell<Vec<SourceDocument>>>,
    source_navigation: SourceNavigationControls,
    source_tree: SourceTreeControls,
    source_back_history: Rc<RefCell<Vec<SourceNavigationLocation>>>,
    source_forward_history: Rc<RefCell<Vec<SourceNavigationLocation>>>,
    closed_source_tabs: Rc<RefCell<Vec<ClosedSourceTab>>>,
    source_find_state: Rc<RefCell<Option<SourceFindState>>>,
    source_palette: Rc<RefCell<Option<SourcePalette>>>,
    source_palette_generation: Arc<AtomicU64>,
    source_loaded_generation: Arc<AtomicU64>,
    source_loaded_cache: Rc<RefCell<Option<Arc<Vec<PathBuf>>>>>,
    source_loaded_search: Rc<RefCell<Option<Arc<source::SourceSearchIndex>>>>,
    loaded_source_files: Rc<RefCell<Vec<SourceFile>>>,
    source_tree_base_roots: Vec<PathBuf>,
    source_tree_roots: Rc<RefCell<Vec<PathBuf>>>,
    source_tree_cache: Rc<RefCell<Option<Arc<Vec<PathBuf>>>>>,
    source_tree_search: Rc<RefCell<Option<Arc<source::SourceSearchIndex>>>>,
    source_index: Rc<RefCell<Option<Arc<source::SourceIndex>>>>,
    source_tree_indexing: Rc<Cell<bool>>,
    source_tree_generation: Arc<AtomicU64>,
    source_tree_render_generation: Arc<AtomicU64>,
    source_tree_initialized: Rc<Cell<bool>>,
    execution_source_path: Rc<RefCell<Option<PathBuf>>>,
    execution_source_line: Rc<Cell<Option<u32>>>,
    source_theme: Theme,
    source_style_scheme: Option<sourceview5::StyleScheme>,
    resolved_source_paths:
        Rc<RefCell<crate::performance::BoundedLruCache<String, Option<PathBuf>>>>,
    call_stack_list: gtk::Box,
    frame_buttons: Rc<RefCell<Vec<(u32, gtk::Button)>>>,
    latest_frames: Rc<RefCell<Vec<StackFrame>>>,
    latest_frames_generation: Rc<Cell<Option<u64>>>,
    selected_frame_level: Rc<Cell<u32>>,
    threads_list: gtk::Box,
    thread_controls: ThreadControls,
    thread_buttons: Rc<RefCell<Vec<(String, gtk::Button)>>>,
    latest_threads: Rc<RefCell<Option<ThreadRenderState>>>,
    selected_thread_id: Rc<RefCell<Option<String>>>,
    scheduler_locking: Rc<Cell<Option<SchedulerLockingMode>>>,
    non_stop_mode: Rc<Cell<Option<bool>>>,
    thread_policy_generation: Rc<Cell<u64>>,
    modules_list: gtk::Box,
    latest_modules: Rc<RefCell<Vec<SharedLibrary>>>,
    module_debug_metadata: Rc<RefCell<HashMap<PathBuf, ModuleDebugMetadata>>>,
    module_debug_generation: Arc<AtomicU64>,
    module_debug_worker_active: Arc<AtomicBool>,
    module_debug_force_pending: Arc<AtomicBool>,
    inferior_controls: InferiorControls,
    inferiors: Rc<RefCell<Vec<InferiorInfo>>>,
    thread_inferior_ids: Rc<RefCell<HashMap<String, String>>>,
    selected_inferior_id: Rc<RefCell<Option<String>>>,
    stop_owner_inferior_id: Rc<RefCell<Option<String>>>,
    stop_owner_thread_id: Rc<RefCell<Option<String>>>,
    inferior_parents: Rc<RefCell<HashMap<String, String>>>,
    pending_fork_parents: Rc<RefCell<HashMap<u32, String>>>,
    inferior_refresh_generation: Rc<Cell<u64>>,
    execution_context_visual_generation: Rc<Cell<u64>>,
    execution_context_visual_pending: Rc<Cell<bool>>,
    fork_policy_generation: Rc<Cell<u64>>,
    fork_follow_mode: Rc<Cell<Option<ForkFollowMode>>>,
    detach_on_fork: Rc<Cell<Option<bool>>>,
    inferior_action_pending: Rc<Cell<Option<InferiorActionPending>>>,
    inferior_execution_generation: Rc<Cell<u64>>,
    pending_execution_inferior: Rc<RefCell<Option<String>>>,
    active_thread_execution: Rc<RefCell<Option<String>>>,
    thread_execution_exit_candidate: Rc<RefCell<Option<String>>>,
    locals_store: gio::ListStore,
    locals_selection: gtk::SingleSelection,
    variable_node_index: Rc<RefCell<VariableNodeIndex>>,
    local_variables: Rc<RefCell<LocalVariableCatalog>>,
    locals_render_limit: Rc<Cell<usize>>,
    locals_generation: Rc<Cell<Option<u64>>>,
    locals_view: gtk::ColumnView,
    locals_empty: gtk::Label,
    locals_summary: gtk::Label,
    locals_edit_button: gtk::Button,
    locals_more_button: gtk::Button,
    locals_filter: gtk::Entry,
    expression_watches_store: gio::ListStore,
    expression_watches_selection: gtk::SingleSelection,
    expression_watches_view: gtk::ColumnView,
    expression_watches_empty: gtk::Label,
    expression_watches: Rc<RefCell<Vec<String>>>,
    deferred_variable_object_deletions: Rc<RefCell<HashSet<String>>>,
    pending_local_variable_objects: Rc<RefCell<HashSet<(u64, usize)>>>,
    expression_watch_entry: gtk::Entry,
    expression_watch_add_button: gtk::Button,
    expression_watch_remove_button: gtk::Button,
    target_pointer_bits: Rc<Cell<u32>>,
    target_pointer_bits_known: Rc<Cell<bool>>,
    target_architecture: Rc<Cell<TargetArchitecture>>,
    target_endian: Rc<Cell<Option<TargetEndian>>>,
    current_source_is_rust: Rc<Cell<bool>>,
    instructions_title: gtk::Label,
    instructions_store: gio::ListStore,
    instructions_selection: gtk::SingleSelection,
    instructions_view: gtk::ColumnView,
    instructions_empty: gtk::Label,
    instruction_flow: gtk::Label,
    instruction_arguments: gtk::Label,
    instruction_memory: gtk::Label,
    disassembly_controls: DisassemblyControls,
    current_instruction: Rc<RefCell<Option<Instruction>>>,
    call_abi_instruction: Rc<RefCell<Option<CallAbiInstructionContext>>>,
    call_abi_instruction_generation: Rc<Cell<Option<u64>>>,
    current_instruction_memory_expression: Rc<RefCell<Option<String>>>,
    latest_registers: Rc<RefCell<Vec<Register>>>,
    latest_registers_generation: Rc<Cell<Option<u64>>>,
    register_details_generation: Rc<Cell<Option<u64>>>,
    instruction_memory_handler: Rc<RefCell<Option<InstructionMemoryHandler>>>,
    disassembly_handler: Rc<RefCell<Option<DisassemblyHandler>>>,
    disassembly_source_cache: DisassemblySourceCache,
    register_groups: Vec<RegisterGroupView>,
    registers_empty: gtk::Label,
    stack_store: gio::ListStore,
    latest_stack: Rc<RefCell<Vec<StackEntry>>>,
    displayed_stack: Rc<RefCell<Vec<StackEntry>>>,
    latest_stack_generation: Rc<Cell<Option<u64>>>,
    stack_memory_refresh_generation: Rc<Cell<Option<u64>>>,
    stack_details_generation: Rc<Cell<Option<u64>>>,
    stack_empty: gtk::Label,
    breakpoints_list: gtk::Box,
    stop_point_filter: StopPointFilterControls,
    add_breakpoint_button: gtk::Button,
    delete_all_breakpoints_button: gtk::Button,
    delete_all_watchpoints_button: gtk::Button,
    delete_all_catchpoints_button: gtk::Button,
    event_catchpoint_buttons: Vec<(gtk::Button, EventCatchpoint)>,
    watchpoint_expression: gtk::Entry,
    watchpoint_access: gtk::DropDown,
    watchpoint_mask: gtk::Entry,
    watchpoint_add_button: gtk::Button,
    filtered_catchpoint: FilteredCatchpointControls,
    signal_detail: gtk::Label,
    signal_buttons: Vec<(gtk::Button, &'static str, &'static str)>,
    signal_entry: gtk::Entry,
    signal_add_button: gtk::Button,
    delete_all_signal_catchpoints_button: gtk::Button,
    until_actions: Vec<(gtk::Button, UntilAction)>,
    until_condition_entry: gtk::Entry,
    until_condition_button: gtk::Button,
    memory_region_store: gio::ListStore,
    memory_regions_view: gtk::ColumnView,
    memory_regions_empty: gtk::Label,
    memory_regions: Rc<RefCell<Vec<MemoryRegion>>>,
    memory_regions_generation: Rc<Cell<Option<u64>>>,
    memory_watches_refresh_generation: Rc<Cell<Option<u64>>>,
    tls_runtime_refresh_generation: Rc<Cell<Option<u64>>>,
    memory_watches: Rc<RefCell<Vec<MemoryWatchView>>>,
    memory_watch_container: MemoryWatchContainer,
    memory_address_entry: gtk::Entry,
    memory_size: gtk::SpinButton,
    memory_format: gtk::DropDown,
    memory_add_button: gtk::Button,
    memory_watch_handler: Rc<RefCell<Option<MemoryWatchHandler>>>,
    kernel_view: KernelView,
    kernel_refresh_handler: Rc<RefCell<Option<KernelRefreshHandler>>>,
    kernel_refresh_generation: Rc<Cell<u64>>,
    misc_view: MiscView,
    misc_refresh_handler: Rc<RefCell<Option<MiscRefreshHandler>>>,
    misc_refresh_generation: Rc<Cell<u64>>,
    debugger_pid: Rc<Cell<Option<u32>>>,
    inferior_pid: Rc<Cell<Option<u32>>>,
    layout: layout::Persistence,
    breakpoints: Rc<RefCell<Vec<Breakpoint>>>,
    stop_point_filter_rows: Rc<RefCell<Vec<StopPointFilterRow>>>,
    stop_point_metadata: Rc<RefCell<HashMap<String, StopPointMetadata>>>,
    previous_registers: Rc<RefCell<HashMap<String, String>>>,
    cached_register_names: Rc<RefCell<Option<Rc<Vec<String>>>>>,
    stop_refresh_generation: Rc<Cell<u64>>,
    variable_editor_request: Cell<u64>,
    active_stop_context: Rc<RefCell<Option<crate::debugger::StopContext>>>,
    thread_refresh_generation: Rc<Cell<u64>>,
    breakpoint_refresh_generation: Rc<Cell<u64>>,
    breakpoint_refresh_gate: Rc<RefreshGate>,
    module_refresh_gate: Rc<RefreshGate>,
    modules_dirty: Rc<Cell<bool>>,
    command_pending: Rc<Cell<bool>>,
    debugger_state: Rc<Cell<DebuggerState>>,
    execution_transition_generation: Rc<Cell<u64>>,
    session_pending: Rc<Cell<bool>>,
    applied_control_state: Rc<RefCell<Option<ControlState>>>,
    gef_available: Rc<Cell<bool>>,
    gef_capabilities: Rc<RefCell<HashSet<&'static str>>>,
    gef_context_control: Rc<Cell<GefContextControl>>,
    gef_context_visible: bool,
    gef_context_hidden_by_fgdb: Rc<Cell<bool>>,
    heap_inspection_handler: Rc<RefCell<Option<HeapInspectionHandler>>>,
    source_roots: Rc<RefCell<Vec<PathBuf>>>,
    source_base_roots: Vec<PathBuf>,
    current_session: Rc<RefCell<Option<DebugSession>>>,
    gdb_capabilities: Rc<RefCell<GdbCapabilities>>,
    gdb_recovery_available: Rc<Cell<bool>>,
    configuration_report: ConfigurationReport,
    configuration_dialog: Rc<RefCell<Option<gtk::Window>>>,
    debug_data_view: Rc<RefCell<Option<debug_data::DebugDataView>>>,
    debug_data_state: Rc<RefCell<debug_data::DebugDataState>>,
    performance_notice_times: Rc<RefCell<HashMap<String, Instant>>>,
    adaptive_render_budgets: Rc<RefCell<crate::performance::AdaptiveRenderBudgets>>,
    terminal_synchronization: Rc<RefCell<TerminalSynchronization>>,
    debug_data_generation: Rc<Cell<u64>>,
    debug_data_action_handler: Rc<RefCell<Option<DebugDataActionHandler>>>,
    session_handler: Rc<RefCell<Option<DebugSessionHandler>>>,
    session_action_handler: Rc<RefCell<Option<SessionActionHandler>>>,
    until_action_handler: Rc<RefCell<Option<UntilActionHandler>>>,
    until_cancel_handler: Rc<RefCell<Option<UntilCancelHandler>>>,
    until_abort_handler: Rc<RefCell<Option<UntilAbortHandler>>>,
    until_stop_handler: Rc<RefCell<Option<UntilStopHandler>>>,
    native_until_active: Rc<Cell<bool>>,
    frame_selection_handler: Rc<RefCell<Option<FrameSelectionHandler>>>,
    thread_selection_handler: Rc<RefCell<Option<StringSelectionHandler>>>,
    instruction_handler: Rc<RefCell<Option<StringSelectionHandler>>>,
    variable_editor_handler: Rc<RefCell<Option<VariableEditorHandler>>>,
    variable_assignment_handler: Rc<RefCell<Option<VariableAssignmentHandler>>>,
    float_assignment_handler: Rc<RefCell<Option<FloatAssignmentHandler>>>,
    string_assignment_handler: Rc<RefCell<Option<StringAssignmentHandler>>>,
    variable_children_handler: Rc<RefCell<Option<VariableChildrenHandler>>>,
    variable_viewer_handler: Rc<RefCell<Option<VariableViewerHandler>>>,
    variable_viewer_windows: Rc<RefCell<Vec<gtk::Window>>>,
    expression_watch_refresh_handler: Rc<RefCell<Option<ExpressionWatchRefreshHandler>>>,
    vector_assignment_handler: Rc<RefCell<Option<VectorAssignmentHandler>>>,
    breakpoint_insert_handler: Rc<RefCell<Option<BreakpointInsertHandler>>>,
    source_jump_handler: Rc<RefCell<Option<SourceJumpHandler>>>,
    breakpoint_delete_handler: Rc<RefCell<Option<StringSelectionHandler>>>,
    breakpoint_condition_handler: Rc<RefCell<Option<BreakpointConditionHandler>>>,
    breakpoint_editor_handler: Rc<RefCell<Option<BreakpointEditorHandler>>>,
    breakpoint_enabled_handler: Rc<RefCell<Option<BreakpointEnabledHandler>>>,
    breakpoint_bulk_delete_handler: Rc<RefCell<Option<BreakpointBulkDeleteHandler>>>,
    signal_catchpoint_handler: Rc<RefCell<Option<SignalCatchpointHandler>>>,
    event_catchpoint_handler: Rc<RefCell<Option<EventCatchpointHandler>>>,
    filtered_catchpoint_handler: Rc<RefCell<Option<FilteredCatchpointHandler>>>,
    watchpoint_insert_handler: Rc<RefCell<Option<WatchpointInsertHandler>>>,
    source_symbol_handler: Rc<RefCell<Option<StringSelectionHandler>>>,
    source_discovery_handler: Rc<RefCell<Option<SourceDiscoveryHandler>>>,
    thread_stop_reason: Rc<RefCell<Option<String>>>,
    debugger_ready: Rc<Cell<bool>>,
}

struct Topbar {
    root: gtk::HeaderBar,
    session_button: gtk::ToggleButton,
    session_popover: gtk::Popover,
    session_kind_label: gtk::Label,
    session_target_label: gtk::Label,
    new_session_button: gtk::Button,
    restart_session_button: gtk::Button,
    kill_session_button: gtk::Button,
    detach_session_button: gtk::Button,
    restart_gdb_button: gtk::Button,
    resynchronize_button: gtk::Button,
    configuration_button: gtk::Button,
    gdb_capabilities_label: gtk::Label,
    target_label: gtk::Label,
    debug_data_button: gtk::Button,
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
    gef_tools_content: gtk::Box,
    gef_tool_controls: Vec<GefCapabilityControl>,
    gef_tool_groups: Vec<GefCapabilityGroup>,
    until_actions: Vec<(gtk::Button, UntilAction)>,
    until_condition_entry: gtk::Entry,
    until_condition_button: gtk::Button,
    status_label: gtk::Label,
}

struct Workspace {
    root: gtk::Paned,
    layout_panes: Vec<layout::Pane>,
    terminal_panel: gtk::Box,
    status_detail: gtk::Label,
    source_navigation: SourceNavigationControls,
    source_tree: SourceTreeControls,
    left_navigation: gtk::Notebook,
    inspector_notebook: gtk::Notebook,
    call_stack_list: gtk::Box,
    threads_list: gtk::Box,
    thread_controls: ThreadControls,
    modules_list: gtk::Box,
    inferior_controls: InferiorControls,
    locals_store: gio::ListStore,
    locals_selection: gtk::SingleSelection,
    locals_view: gtk::ColumnView,
    locals_empty: gtk::Label,
    locals_summary: gtk::Label,
    locals_edit_button: gtk::Button,
    locals_more_button: gtk::Button,
    locals_filter: gtk::Entry,
    expression_watches_store: gio::ListStore,
    expression_watches_selection: gtk::SingleSelection,
    expression_watches_view: gtk::ColumnView,
    expression_watches_empty: gtk::Label,
    expression_watch_entry: gtk::Entry,
    expression_watch_add_button: gtk::Button,
    expression_watch_remove_button: gtk::Button,
    instructions_title: gtk::Label,
    instructions_store: gio::ListStore,
    instructions_selection: gtk::SingleSelection,
    instructions_view: gtk::ColumnView,
    instructions_empty: gtk::Label,
    instruction_flow: gtk::Label,
    instruction_arguments: gtk::Label,
    instruction_memory: gtk::Label,
    disassembly_controls: DisassemblyControls,
    register_groups: Vec<RegisterGroupView>,
    registers_empty: gtk::Label,
    stack_store: gio::ListStore,
    stack_empty: gtk::Label,
    breakpoints_list: gtk::Box,
    stop_point_filter: StopPointFilterControls,
    add_breakpoint_button: gtk::Button,
    delete_all_breakpoints_button: gtk::Button,
    delete_all_watchpoints_button: gtk::Button,
    delete_all_catchpoints_button: gtk::Button,
    event_catchpoint_buttons: Vec<(gtk::Button, EventCatchpoint)>,
    watchpoint_expression: gtk::Entry,
    watchpoint_access: gtk::DropDown,
    watchpoint_mask: gtk::Entry,
    watchpoint_add_button: gtk::Button,
    filtered_catchpoint: FilteredCatchpointControls,
    signal_detail: gtk::Label,
    signal_buttons: Vec<(gtk::Button, &'static str, &'static str)>,
    signal_entry: gtk::Entry,
    signal_add_button: gtk::Button,
    delete_all_signal_catchpoints_button: gtk::Button,
    memory_region_store: gio::ListStore,
    memory_regions_view: gtk::ColumnView,
    memory_regions_empty: gtk::Label,
    memory_watch_container: MemoryWatchContainer,
    memory_address_entry: gtk::Entry,
    memory_size: gtk::SpinButton,
    memory_format: gtk::DropDown,
    memory_add_button: gtk::Button,
    kernel_view: KernelView,
    misc_view: MiscView,
}

struct Inspector {
    root: gtk::Box,
    notebook: gtk::Notebook,
    compact_tabs: gtk::Box,
    context_split: gtk::Paned,
    status_detail: gtk::Label,
    locals_store: gio::ListStore,
    locals_selection: gtk::SingleSelection,
    locals_view: gtk::ColumnView,
    locals_empty: gtk::Label,
    locals_summary: gtk::Label,
    locals_edit_button: gtk::Button,
    locals_more_button: gtk::Button,
    locals_filter: gtk::Entry,
    expression_watches_store: gio::ListStore,
    expression_watches_selection: gtk::SingleSelection,
    expression_watches_view: gtk::ColumnView,
    expression_watches_empty: gtk::Label,
    expression_watch_entry: gtk::Entry,
    expression_watch_add_button: gtk::Button,
    expression_watch_remove_button: gtk::Button,
    instructions_title: gtk::Label,
    instructions_store: gio::ListStore,
    instructions_selection: gtk::SingleSelection,
    instructions_view: gtk::ColumnView,
    instructions_empty: gtk::Label,
    instruction_flow: gtk::Label,
    instruction_arguments: gtk::Label,
    instruction_memory: gtk::Label,
    disassembly_controls: DisassemblyControls,
    register_groups: Vec<RegisterGroupView>,
    registers_empty: gtk::Label,
    stack_store: gio::ListStore,
    stack_empty: gtk::Label,
    breakpoints_list: gtk::Box,
    stop_point_filter: StopPointFilterControls,
    add_breakpoint_button: gtk::Button,
    delete_all_breakpoints_button: gtk::Button,
    delete_all_watchpoints_button: gtk::Button,
    delete_all_catchpoints_button: gtk::Button,
    event_catchpoint_buttons: Vec<(gtk::Button, EventCatchpoint)>,
    watchpoint_expression: gtk::Entry,
    watchpoint_access: gtk::DropDown,
    watchpoint_mask: gtk::Entry,
    watchpoint_add_button: gtk::Button,
    filtered_catchpoint: FilteredCatchpointControls,
    signal_detail: gtk::Label,
    signal_buttons: Vec<(gtk::Button, &'static str, &'static str)>,
    signal_entry: gtk::Entry,
    signal_add_button: gtk::Button,
    delete_all_signal_catchpoints_button: gtk::Button,
    memory_region_store: gio::ListStore,
    memory_regions_view: gtk::ColumnView,
    memory_regions_empty: gtk::Label,
    memory_watch_container: MemoryWatchContainer,
    memory_split: gtk::Paned,
    memory_address_entry: gtk::Entry,
    memory_size: gtk::SpinButton,
    memory_format: gtk::DropDown,
    memory_add_button: gtk::Button,
    kernel_view: KernelView,
    misc_view: MiscView,
}

struct LeftSidebar {
    root: gtk::Box,
    navigation: gtk::Notebook,
    call_stack_list: gtk::Box,
    threads_list: gtk::Box,
    thread_controls: ThreadControls,
    modules_list: gtk::Box,
    source_tree: SourceTreeControls,
    inferior_controls: InferiorControls,
}

mod build;
mod cfg_view;
pub(crate) mod controls;
mod debug_state;
mod dialogs;
pub(crate) mod formatting;
mod inferiors;
mod kernel_view;
mod memory_view;
mod misc_view;
mod session;
mod source_actions;
mod source_view;
mod state;
mod threads;
mod variable_viewers;
mod views;
mod watches;

pub(crate) use variable_viewers::{
    VariableViewerPlan, VariableViewerRegistry, VariableViewerRequest, VariableViewerRow,
    VariableViewerSession,
};
pub(crate) use views::compact_variable_type;

use build::*;
use cfg_view::*;
use controls::*;
use dialogs::*;
use formatting::*;
use kernel_view::*;
use memory_view::*;
use misc_view::*;
use source_view::*;
use threads::*;
use views::*;

#[cfg(test)]
mod tests {
    use std::{collections::HashSet, path::Path};

    use super::{
        DebugSession, EventCatchpoint, GEF_COMMAND_CAPABILITIES, IntegerFormat, IntegerRadix,
        RefreshGate, StringStorage, TargetConnection, TerminalClipboardAction, UntilAction,
        VariableNode, VectorLaneFormat, breakpoint_command_number_at_address,
        breakpoint_command_numbers, call_abi_phase, compact_function_name, compact_variable_type,
        conditional_branch_taken, configured_target_can_start, event_catchpoint_command_number,
        event_catchpoint_command_numbers, flags_markup, format_register_value,
        format_register_value_for_architecture, format_register_value_for_target, full_address,
        instruction_arguments_description, instruction_flow_description, instruction_flow_target,
        instruction_matches_until, instruction_memory_expression, integer_decimal_value,
        normalized_signal_name, parse_character_input, parse_integer_input, parse_string_input,
        register_details, register_integer_format, register_value_css, set_breakpoint_enabled,
        signal_catchpoint_command_number, signal_catchpoint_command_numbers, source_location_score,
        source_symbol_at_offset, source_tab_title, stop_reason_label, string_edit,
        terminal_clipboard_action, thread_os_id, variable_boolean_value, variable_character_format,
        variable_details, variable_integer_format, variable_is_address,
        variable_node_matches_filter, variable_search_text, variable_value_parts,
        vector_field_values, without_generic_arguments,
    };
    use crate::debugger::{
        Breakpoint, Instruction, Register, SourceLocation, TargetArchitecture, TargetEndian,
        Variable,
    };
    use crate::misc::CallAbiPhase;

    #[test]
    fn gef_capability_probes_are_unique() {
        let unique = GEF_COMMAND_CAPABILITIES
            .iter()
            .copied()
            .collect::<HashSet<_>>();

        assert_eq!(unique.len(), GEF_COMMAND_CAPABILITIES.len());
    }

    #[test]
    fn configured_remote_session_cannot_start_without_a_live_connection() {
        let session = DebugSession::Remote {
            endpoint: String::from("host:1234"),
            executable: None,
            extended: true,
            remote_executable: Some(String::from("/srv/app")),
        };

        assert!(!configured_target_can_start(
            Some(&session),
            TargetConnection::None
        ));

        assert!(configured_target_can_start(
            Some(&session),
            TargetConnection::Remote
        ));
    }

    #[test]
    fn terminal_clipboard_shortcuts_preserve_gdb_interrupts() {
        use gtk::gdk::{Key, ModifierType};

        let control = ModifierType::CONTROL_MASK;
        let shift = ModifierType::SHIFT_MASK;

        assert_eq!(
            terminal_clipboard_action(Key::v, control, false),
            Some(TerminalClipboardAction::Paste)
        );

        assert_eq!(
            terminal_clipboard_action(Key::V, control | shift, false),
            Some(TerminalClipboardAction::Paste)
        );

        assert_eq!(
            terminal_clipboard_action(Key::Insert, shift, false),
            Some(TerminalClipboardAction::Paste)
        );

        assert_eq!(
            terminal_clipboard_action(Key::KP_Insert, control, false),
            Some(TerminalClipboardAction::Copy)
        );

        assert_eq!(
            terminal_clipboard_action(Key::c, control, true),
            Some(TerminalClipboardAction::Copy)
        );

        assert_eq!(terminal_clipboard_action(Key::c, control, false), None);

        assert_eq!(
            terminal_clipboard_action(Key::C, control | shift, false),
            Some(TerminalClipboardAction::Copy)
        );

        assert_eq!(
            terminal_clipboard_action(Key::v, control | ModifierType::ALT_MASK, false),
            None
        );
    }

    #[test]
    fn classifies_native_until_instruction_events_across_syntaxes() {
        let instruction = |text: &str| Instruction {
            address: String::from("0x401000"),
            function: String::from("main"),
            offset: String::from("0"),
            opcodes: None,
            text: text.to_owned(),
            source: None,
        };

        assert!(instruction_matches_until(
            &UntilAction::NextCall,
            &instruction("call 0x402000 <worker>"),
            TargetArchitecture::X86_64,
        ));

        assert!(instruction_matches_until(
            &UntilAction::NextIndirectBranch,
            &instruction("call *%rax"),
            TargetArchitecture::X86_64,
        ));

        assert!(!instruction_matches_until(
            &UntilAction::NextIndirectBranch,
            &instruction("call 0x402000 <worker>"),
            TargetArchitecture::X86_64,
        ));

        assert!(instruction_matches_until(
            &UntilAction::MemoryAccess,
            &instruction("mov 0x10(%rsp),%rax"),
            TargetArchitecture::X86_64,
        ));

        assert!(instruction_matches_until(
            &UntilAction::NextSyscall,
            &instruction("svc #0"),
            TargetArchitecture::AArch64,
        ));

        assert!(instruction_matches_until(
            &UntilAction::NextControlFlow,
            &instruction("jalr ra,a0,0"),
            TargetArchitecture::RiscV64,
        ));
    }

    #[test]
    fn coalesces_bursty_model_refreshes() {
        let gate = RefreshGate::default();
        assert!(gate.begin());
        assert!(!gate.begin());
        assert!(!gate.begin());
        assert!(gate.finish());
        assert!(gate.begin());
        assert!(!gate.finish());
        assert!(gate.begin());
        gate.invalidate();
        assert!(gate.finish());
    }

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
            local_index: None,
            name: String::from("value"),
            value: value.to_owned(),
            type_name: Some(type_name.to_owned()),
            argument: false,
            varobj: None,
            num_children: 0,
            has_more: false,
            display_hint: None,
            dynamic: false,
        };

        let details = |variable: &Variable, value: &str, annotation: &str| {
            variable_details(variable, value, annotation, 64)
        };

        assert_eq!(details(&integer("int", "0x2a"), "0x2a", ""), "42");

        assert_eq!(
            details(&integer("char", "0x41 'A'"), "0x41", "'A'"),
            "65  ·  'A'"
        );

        assert_eq!(details(&integer("pid_t", "-0x1"), "-0x1", ""), "-1");

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
    fn compacts_cpp_and_rust_debug_types_without_losing_user_types() {
        assert_eq!(
            compact_variable_type(
                "const std::__cxx11::basic_string<char, std::char_traits<char>, std::allocator<char> >"
            ),
            "const std::string"
        );

        assert_eq!(
            compact_variable_type(
                "core::option::Option<alloc::boxed::Box<demo::Node, alloc::alloc::Global>>"
            ),
            "Option<Box<demo::Node>>"
        );

        assert_eq!(
            compact_variable_type(
                "std::collections::hash::map::HashMap<alloc::string::String, usize, std::hash::random::RandomState, alloc::alloc::Global>"
            ),
            "HashMap<String, usize>"
        );
    }

    #[test]
    fn filters_variables_across_scope_type_and_pretty_value() {
        let variable = Variable {
            local_index: None,
            name: String::from("state"),
            value: String::from("PacketKind::Payload"),
            type_name: Some(String::from("core::option::Option<demo::PacketKind>")),
            argument: true,
            varobj: None,
            num_children: 0,
            has_more: false,
            display_hint: None,
            dynamic: false,
        };

        let search_text = variable_search_text(&variable);
        assert!(search_text.contains("state"));
        assert!(search_text.contains("payload"));
        assert!(search_text.contains("option"));
        assert!(search_text.contains("packet"));
        assert!(search_text.contains("argument"));
        assert!(!search_text.contains("vector"));

        let root = VariableNode::new(Variable {
            local_index: None,
            name: String::from("fixture"),
            value: String::from("{...}"),
            type_name: Some(String::from("struct Fixture")),
            argument: false,
            varobj: Some(String::from("var1")),
            num_children: 1,
            has_more: false,
            display_hint: None,
            dynamic: false,
        });

        root.children
            .append(&gtk::glib::BoxedAnyObject::new(VariableNode::new(variable)));

        assert!(variable_node_matches_filter(&root, "packet payload"));
    }

    #[test]
    fn decodes_rust_c_and_cpp_integer_types() {
        let decimal = |type_name: &str, value: &str, pointer_bits| {
            let variable = Variable {
                local_index: None,
                name: String::from("value"),
                value: value.to_owned(),
                type_name: Some(type_name.to_owned()),
                argument: false,
                varobj: None,
                num_children: 0,
                has_more: false,
                display_hint: None,
                dynamic: false,
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
    }

    #[test]
    fn parses_and_converts_type_aware_editor_values() {
        let signed = IntegerFormat::signed(32);
        let unsigned = IntegerFormat::unsigned(16);

        assert_eq!(
            parse_integer_input("-1", signed, IntegerRadix::Decimal),
            Ok(0xffff_ffff)
        );

        assert_eq!(
            parse_integer_input("0xffffffff", signed, IntegerRadix::Hexadecimal),
            Ok(0xffff_ffff)
        );

        assert_eq!(
            parse_integer_input("1111_1111", unsigned, IntegerRadix::Binary),
            Ok(255)
        );

        assert_eq!(
            parse_integer_input("0o177", unsigned, IntegerRadix::Decimal),
            Ok(127)
        );

        assert!(
            parse_integer_input("32768", IntegerFormat::signed(16), IntegerRadix::Decimal).is_err()
        );

        assert!(parse_integer_input("-1", unsigned, IntegerRadix::Decimal).is_err());
        assert_eq!(parse_character_input("'A'", unsigned), Ok(65));
        assert_eq!(parse_character_input("\\n", unsigned), Ok(10));
        assert!(parse_character_input("AB", unsigned).is_err());

        assert_eq!(
            parse_string_input(r"line\nA\101\x42\\"),
            Ok(b"line\nAAB\\".to_vec())
        );
    }

    #[test]
    fn chooses_safe_editor_semantics_from_type_and_register_role() {
        let variable = |name: &str, type_name: &str, value: &str| Variable {
            local_index: None,
            name: name.to_owned(),
            value: value.to_owned(),
            type_name: Some(type_name.to_owned()),
            argument: false,
            varobj: None,
            num_children: 0,
            has_more: false,
            display_hint: None,
            dynamic: false,
        };

        assert_eq!(
            variable_integer_format(&variable("count", "std::uint32_t", "0x2a"), 64, None),
            Some(IntegerFormat::unsigned(32))
        );

        assert_eq!(
            variable_character_format(
                &variable("separator", "char16_t", "65 'A'"),
                64,
                false,
                None,
            ),
            Some(IntegerFormat::unsigned(16))
        );

        assert_eq!(
            variable_character_format(&variable("letter", "char", "'🦀'"), 64, true, None),
            Some(IntegerFormat::unsigned(32))
        );

        assert!(variable_is_address(
            &variable("data", "char *", "0x1000 \"x\""),
            TargetArchitecture::X86_64,
        ));

        assert!(register_integer_format("$rax", 64, TargetArchitecture::X86_64).is_some());

        assert_eq!(
            register_integer_format("$rax", 32, TargetArchitecture::X86_64),
            Some(IntegerFormat::unsigned(64))
        );

        assert_eq!(
            register_integer_format("$a0", 32, TargetArchitecture::Mips64),
            Some(IntegerFormat::unsigned(64))
        );

        assert!(register_integer_format("$rsp", 64, TargetArchitecture::X86_64).is_none());
        assert!(register_integer_format("$r29", 32, TargetArchitecture::Mips32).is_none());

        assert_eq!(
            variable_boolean_value(&variable("enabled", "bool", "true"), None),
            Some(true)
        );

        assert_eq!(
            variable_boolean_value(&variable("enabled", "const _Bool", "0"), None),
            Some(false)
        );

        assert_eq!(
            variable_boolean_value(&variable("enabled", "core::ffi::c_bool", "0x1"), None),
            Some(true)
        );

        let c_buffer = string_edit(&variable("text", "char[8]", r#""hello""#)).unwrap();

        assert_eq!(
            c_buffer.storage,
            StringStorage::Buffer {
                capacity: 7,
                pointer: false
            }
        );

        let cpp = string_edit(&variable("text", "std::string &", r#""hello""#)).unwrap();
        assert_eq!(cpp.storage, StringStorage::CppString);
        let rust = string_edit(&variable("text", "alloc::string::String", r#""hello""#)).unwrap();
        assert_eq!(rust.storage, StringStorage::RustString { length: 5 });
        assert!(string_edit(&variable("wide", "char32_t[8]", r#"U"hello""#)).is_none());
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
            register_value_css(
                &Register {
                    name: String::from("ymm1"),
                    value: zero_ymm.to_owned(),
                    pointer_chain: Vec::new(),
                },
                TargetArchitecture::X86_64,
                Some(TargetEndian::Little),
                64,
            ),
            "register-zero"
        );

        let mixed_ymm = "{v4_int64 = {[0x0] = 0x0 <repeats 3 times>, [0x3] = 0x1}}";

        assert_eq!(
            register_value_css(
                &Register {
                    name: String::from("ymm2"),
                    value: mixed_ymm.to_owned(),
                    pointer_chain: Vec::new(),
                },
                TargetArchitecture::X86_64,
                Some(TargetEndian::Little),
                64,
            ),
            "memory-none"
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

        let oversized_repeat = "{v4_int64 = {0 <repeats 1000000 times>}}";

        assert_eq!(
            vector_field_values(oversized_repeat, "v4_int64", 4, VectorLaneFormat::Int64,).unwrap(),
            ["0x0000000000000000"; 4]
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
        assert_eq!(full_address("0x55555555516f", 64), "0x000055555555516f");
        assert_eq!(full_address("0x8048123", 32), "0x08048123");

        let register = |name: &str, chain: &[&str]| Register {
            name: name.to_owned(),
            value: String::from("0x7fffffffcf40"),
            pointer_chain: chain.iter().map(|value| (*value).to_owned()).collect(),
        };

        assert_eq!(
            register_value_css(
                &register("rip", &[]),
                TargetArchitecture::X86_64,
                Some(TargetEndian::Little),
                64,
            ),
            "memory-code"
        );

        assert_eq!(
            register_value_css(
                &register("rsp", &[]),
                TargetArchitecture::X86_64,
                Some(TargetEndian::Little),
                64,
            ),
            "memory-stack"
        );

        assert_eq!(
            register_value_css(
                &register("rsi", &["0x1", "0x61732f656d6f682f"]),
                TargetArchitecture::X86_64,
                Some(TargetEndian::Little),
                64,
            ),
            "memory-string"
        );

        assert_eq!(
            register_details(
                &register("rax", &["0x123456789", "0x8048123"]),
                TargetArchitecture::X86_64,
                Some(TargetEndian::Little),
                32,
            ),
            "0x08048123"
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
            source: None,
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
            instruction_flow_description(&instruction, &registers, TargetArchitecture::X86_64,),
            "CALL  ▶  0x402000 <mmap@plt>"
        );

        let arguments =
            instruction_arguments_description(&instruction, &registers, TargetArchitecture::X86_64);

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
            instruction_memory_expression(
                &memory_instruction,
                &with_rbp,
                TargetArchitecture::X86_64,
            )
            .as_deref(),
            Some("($rbp-0x10)")
        );
    }

    #[test]
    fn classifies_live_call_abi_boundaries_without_guessing_sequential_state() {
        let call = Instruction {
            address: String::from("0x401000"),
            function: String::from("main"),
            offset: String::from("16"),
            opcodes: Some(String::from("e8 00 00 00 00")),
            text: String::from("call 0x402000 <worker>"),
            source: None,
        };

        assert_eq!(
            call_abi_phase(&call, None, TargetArchitecture::X86_64),
            CallAbiPhase::OutgoingCall {
                target: Some(String::from("0x402000")),
            }
        );

        let entry = Instruction {
            address: String::from("0x402000"),
            function: String::from("worker"),
            offset: String::from("0"),
            opcodes: None,
            text: String::from("push rbp"),
            source: None,
        };

        assert_eq!(
            call_abi_phase(&entry, None, TargetArchitecture::X86_64),
            CallAbiPhase::IncomingEntry {
                function: String::from("worker"),
            }
        );

        let after_call = Instruction {
            address: String::from("0x401005"),
            function: String::from("main"),
            offset: String::from("21"),
            opcodes: None,
            text: String::from("mov rbx,rax"),
            source: None,
        };

        assert_eq!(
            call_abi_phase(&after_call, Some(&call), TargetArchitecture::X86_64),
            CallAbiPhase::Returned {
                target: Some(String::from("0x402000")),
            }
        );

        assert_eq!(
            call_abi_phase(
                &Instruction {
                    text: String::from("ret"),
                    ..after_call
                },
                None,
                TargetArchitecture::X86_64,
            ),
            CallAbiPhase::Returning
        );
    }

    #[test]
    fn resolves_direct_symbol_and_register_flow_targets() {
        let instruction = Instruction {
            address: String::from("0x401000"),
            function: String::from("main"),
            offset: String::from("12"),
            opcodes: None,
            text: String::from("call 0x402000 <worker>"),
            source: None,
        };

        assert_eq!(
            instruction_flow_target(&instruction, TargetArchitecture::X86_64).as_deref(),
            Some("0x402000")
        );

        assert_eq!(
            instruction_flow_target(
                &Instruction {
                    text: String::from("call rax"),
                    ..instruction.clone()
                },
                TargetArchitecture::X86_64,
            )
            .as_deref(),
            Some("$rax")
        );

        assert_eq!(
            instruction_flow_target(
                &Instruction {
                    text: String::from("bl worker"),
                    ..instruction.clone()
                },
                TargetArchitecture::AArch64,
            )
            .as_deref(),
            Some("worker")
        );

        assert!(
            instruction_flow_target(
                &Instruction {
                    text: String::from("mov rax,rbx"),
                    ..instruction
                },
                TargetArchitecture::X86_64,
            )
            .is_none()
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
            source: None,
        };

        let flags = Register {
            name: String::from("eflags"),
            value: String::from("0x246"),
            pointer_chain: Vec::new(),
        };

        assert_eq!(
            instruction_flow_description(
                &branch,
                std::slice::from_ref(&flags),
                TargetArchitecture::X86_64,
            ),
            "BRANCH · NOT TAKEN  ▶  0x40100c <main+0x1c>"
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

        let arguments =
            instruction_arguments_description(&syscall, &registers, TargetArchitecture::X86_64);

        assert!(arguments.starts_with("SYSCALL  #1 write("));
        assert!(arguments.contains("fd=0x0000000000000002"));
        assert!(arguments.contains("count=0x0000000000000020"));

        let carry = Register {
            name: String::from("eflags"),
            value: String::from("0x1"),
            pointer_chain: Vec::new(),
        };

        for (mnemonic, expected) in [("jbe 0x10", Some(true)), ("ja 0x10", Some(false))] {
            let conditional = Instruction {
                text: mnemonic.to_owned(),
                ..syscall.clone()
            };

            assert_eq!(
                conditional_branch_taken(
                    &conditional,
                    std::slice::from_ref(&carry),
                    TargetArchitecture::X86,
                ),
                expected,
                "{mnemonic}"
            );
        }

        let i386_registers = [
            ("eax", "0x4"),
            ("ebx", "0x1"),
            ("ecx", "0x8049000"),
            ("edx", "0x4"),
        ]
        .map(|(name, value)| Register {
            name: name.to_owned(),
            value: value.to_owned(),
            pointer_chain: Vec::new(),
        });

        let int80 = Instruction {
            text: String::from("int 0x80"),
            ..syscall
        };

        let arguments =
            instruction_arguments_description(&int80, &i386_registers, TargetArchitecture::X86);

        assert!(arguments.starts_with("SYSCALL  #4 write("));
        assert!(arguments.contains("fd=0x00000001"));

        let arguments =
            instruction_arguments_description(&int80, &i386_registers, TargetArchitecture::X86_64);

        assert!(arguments.starts_with("SYSCALL  #4 write("));

        let trap = Instruction {
            text: String::from("int3"),
            ..int80
        };

        assert!(
            !instruction_flow_description(&trap, &i386_registers, TargetArchitecture::X86_64,)
                .contains("SYSCALL")
        );

        assert!(
            instruction_arguments_description(&trap, &i386_registers, TargetArchitecture::X86_64,)
                .is_empty()
        );
    }

    #[test]
    fn applies_arm_and_riscv_abis_to_instruction_insight() {
        assert_eq!(
            format_register_value_for_architecture("r8", "0x1234", false, TargetArchitecture::Arm,),
            "0x00001234"
        );

        assert_eq!(
            format_register_value_for_architecture(
                "r8",
                "0x1234",
                false,
                TargetArchitecture::X86_64,
            ),
            "0x0000000000001234"
        );

        let svc = Instruction {
            address: String::from("0x4000"),
            function: String::from("write_one"),
            offset: String::from("4"),
            opcodes: None,
            text: String::from("svc #0"),
            source: None,
        };

        let aarch64_registers = [
            ("x8", "0x40"),
            ("x0", "0x1"),
            ("x1", "0x8000"),
            ("x2", "0x4"),
        ]
        .map(|(name, value)| Register {
            name: name.to_owned(),
            value: value.to_owned(),
            pointer_chain: Vec::new(),
        });

        let arguments = instruction_arguments_description(
            &svc,
            &aarch64_registers,
            TargetArchitecture::AArch64,
        );

        assert!(arguments.starts_with("SYSCALL  #64 write("));
        assert!(arguments.contains("fd=0x0000000000000001"));

        let branch = Instruction {
            text: String::from("beq a0,a1,0x1010"),
            ..svc.clone()
        };

        let riscv_registers = [("a0", "0x2a"), ("a1", "0x2a")].map(|(name, value)| Register {
            name: name.to_owned(),
            value: value.to_owned(),
            pointer_chain: Vec::new(),
        });

        assert_eq!(
            instruction_flow_description(&branch, &riscv_registers, TargetArchitecture::RiscV32,),
            "BRANCH · TAKEN  ▶  a0,a1,0x1010"
        );

        let load = Instruction {
            text: String::from("ldr x3,[x0, #0x10]"),
            ..svc
        };

        assert_eq!(
            instruction_memory_expression(&load, &aarch64_registers, TargetArchitecture::AArch64,)
                .as_deref(),
            Some("($x0 + 0x10)")
        );

        let arm_return = Instruction {
            text: String::from("bx lr"),
            ..load.clone()
        };

        assert_eq!(
            instruction_flow_description(&arm_return, &[], TargetArchitecture::Arm),
            "RETURN  ▶  return to caller"
        );

        let aarch64_return = Instruction {
            text: String::from("br x30"),
            ..load.clone()
        };

        assert_eq!(
            instruction_flow_description(&aarch64_return, &[], TargetArchitecture::AArch64),
            "RETURN  ▶  return to caller"
        );

        let s390_svc = Instruction {
            text: String::from("svc 4"),
            ..load
        };

        let s390_registers =
            [("r2", "0x1"), ("r3", "0x2000"), ("r4", "0x8")].map(|(name, value)| Register {
                name: name.to_owned(),
                value: value.to_owned(),
                pointer_chain: Vec::new(),
            });

        assert!(
            instruction_arguments_description(
                &s390_svc,
                &s390_registers,
                TargetArchitecture::S390x,
            )
            .starts_with("SYSCALL  #4 write(")
        );
    }

    #[test]
    fn formats_non_native_register_values_without_host_assumptions() {
        assert_eq!(
            format_register_value_for_target(
                "r3",
                "0x54455854",
                true,
                TargetArchitecture::PowerPc32,
                Some(TargetEndian::Big),
                32,
            ),
            "0x54455854 'TEXT…'"
        );

        let vector = format_register_value_for_target(
            "v0",
            "{ uint128 = 0x1, u = { 0x1, 0x2 } }",
            false,
            TargetArchitecture::AArch64,
            Some(TargetEndian::Little),
            64,
        );

        assert!(vector.contains("uint128 = 0x1"));
        assert_ne!(vector, "{");
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
            disposition: Some(String::from("keep")),
            hit_count: 0,
            ignore_count: 0,
            thread: None,
            inferior: None,
            pending: None,
            commands: Vec::new(),
            parent_number: None,
            location_count: 0,
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
            Breakpoint {
                catch_type: Some(String::from("syscall")),
                original_location: Some(String::from("openat, read")),
                ..stop_point("6", "catchpoint")
            },
            Breakpoint {
                catch_type: Some(String::from("syscall")),
                original_location: Some(String::from("<any syscall>")),
                ..stop_point("7", "catchpoint")
            },
        ];

        assert_eq!(breakpoint_command_numbers(&stop_points, false), ["1"]);
        assert_eq!(breakpoint_command_numbers(&stop_points, true), ["2"]);
        assert_eq!(signal_catchpoint_command_numbers(&stop_points), ["3"]);

        assert_eq!(
            event_catchpoint_command_numbers(&stop_points),
            ["4", "5", "6", "7"]
        );

        assert_eq!(
            event_catchpoint_command_number(&stop_points, EventCatchpoint::CxxThrow).as_deref(),
            Some("4")
        );

        assert_eq!(
            event_catchpoint_command_number(&stop_points, EventCatchpoint::RustPanic).as_deref(),
            Some("5")
        );

        assert_eq!(
            event_catchpoint_command_number(&stop_points, EventCatchpoint::Syscall).as_deref(),
            Some("7")
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
        assert!(set_breakpoint_enabled(&mut stop_points, "1.1", true));
        assert!(stop_points[0].enabled);
        assert!(!stop_points[1].enabled);
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
