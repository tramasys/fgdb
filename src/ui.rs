mod actions;
mod build;
mod cfg_view;
mod components;
mod configuration;
pub(crate) mod controls;
mod debug_data;
mod debug_state;
mod dialogs;
mod domain;
mod editor;
mod formatting;
mod inferiors;
pub(crate) mod investigation;
mod kernel_view;
mod keybindings;
mod layout;
mod lifecycle;
mod log_view;
mod memory_search;
mod memory_view;
mod misc_view;
mod modules;
mod replay;
mod session;
mod settings;
mod simd;
mod state;
mod syscall_view;
mod threads;
mod value;
mod variable_presentation;
mod variable_viewers;
mod variables;
mod views;
mod watches;
mod workspace;

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

use components::{
    clear_box, dynamic_list, empty_label, replace_boxed_store, replace_boxed_store_if_changed,
    section_title,
};
pub(crate) use debug_data::DebugDataAction;
use debug_state::update_selected_frame_buttons;
use domain::{
    LocalVariableCatalog, MemoryRefreshBatch, TerminalSynchronization, VariableNodeIndex,
    local_refresh_indices,
};
use log_view::{ApplicationLog, LogLevel};
pub(crate) use syscall_view::SyscallAction;
use variables::VariableNode;
pub(crate) use workspace::PanelId;

use crate::model::DebuggerStateDelta;
#[cfg(test)]
use crate::model::TargetConnection;
use crate::model::{RefreshGate, configured_target_can_start};
pub(crate) use actions::*;

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
    config::{ConfigurationReport, DebugSession, LaunchConfig},
    debug_info::ModuleDebugMetadata,
    debugger::{
        Breakpoint, GdbCapabilities, InferiorInfo, InferiorState, Instruction, MemoryBlock,
        MemoryFormat as MemoryWatchFormat, MemoryKind, MiClient, Register, SharedLibrary,
        SourceFile, SourceLocation, StackEntry, StackFrame, TargetArchitecture, TargetEndian,
        ThreadInfo, ValueTypeKind, ValueTypeMetadata, Variable, VariableUpdate,
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

pub(crate) use variable_viewers::{
    VariableViewerPlan, VariableViewerRegistry, VariableViewerRequest, VariableViewerRow,
    VariableViewerSession,
};
pub(crate) use views::compact_variable_type;

use build::*;
use cfg_view::*;
use controls::*;
use dialogs::*;
use editor::*;
use formatting::*;
use kernel_view::*;
use memory_view::*;
use misc_view::*;
use modules::ModuleControls;
use simd::{VectorControls, VectorDisplay, open_vector_editor};
use threads::*;
use views::*;

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

fn stop_point_actions_available(model: &crate::model::DebuggerModel) -> bool {
    model.stop_point_commands_available()
        && model.execution().inferior_action_pending.is_none()
        && model.execution().thread_action_pending.is_none()
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ControlState {
    busy: bool,
    stop_point_busy: bool,
    run: bool,
    pause: bool,
    move_target: bool,
    until: bool,
    inspect: bool,
    syntax: bool,
    gef_tools: bool,
    heap_inspector_in_flight: bool,
    heap_action_visibility: u64,
    edit_local: bool,
    inspect_locals: bool,
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
    source_threads: Rc<[ThreadInfo]>,
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
type VariableEditorHandler = Rc<dyn Fn(Variable, PanelId)>;
type FloatAssignmentHandler = Rc<dyn Fn(Variable, Vec<u8>)>;
type VariableChildrenHandler = Rc<dyn Fn(Variable, usize)>;
type VariableViewerHandler = Rc<dyn Fn(VariableViewerRequest, gtk::Widget)>;
type ExpressionWatchRefreshHandler = Rc<dyn Fn()>;
type StringAssignmentHandler = Rc<dyn Fn(Variable, Vec<u8>, StringAssignmentKind)>;
pub(crate) type VectorWriteCompletion = Box<dyn FnOnce(Result<(), String>)>;
type VectorAssignmentHandler =
    Rc<dyn Fn(crate::debugger::vector::VectorWrite, VectorWriteCompletion)>;
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
type MemoryWatchHandler = Rc<dyn Fn(MemoryWatchRequest)>;
type InstructionMemoryHandler = Rc<dyn Fn(String)>;
type DisassemblyHandler = Rc<dyn Fn(DisassemblyRequest)>;
type DisassemblySourceCache =
    Rc<RefCell<crate::performance::BoundedLruCache<PathBuf, (u64, Option<source::CachedSource>)>>>;
type KernelRefreshHandler = Rc<dyn Fn()>;
type MiscRefreshHandler = Rc<dyn Fn()>;
type HeapInspectionHandler = Rc<dyn Fn(HeapInspectionRequest)>;
type KernelSectionHandler = Rc<dyn Fn(&str, bool)>;
type DebugSessionHandler = Rc<dyn Fn(crate::session_request::SessionRequest)>;
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
    variable_presentation: &'a Rc<variable_presentation::VariablePresentation>,
    variable_locations: &'a Rc<variables::locations::Locations>,
    kernel: KernelViewBindings<'a>,
    misc: MiscViewBindings<'a>,
}

#[derive(Clone)]
struct ValueEditorHandlers {
    model: Rc<crate::model::DebuggerModel>,
    assignment: Rc<RefCell<Option<VariableAssignmentHandler>>>,
    float: Rc<RefCell<Option<FloatAssignmentHandler>>>,
    string: Rc<RefCell<Option<StringAssignmentHandler>>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct VariableEditorRequest {
    pub(crate) generation: u64,
    id: u64,
    origin: PanelId,
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
        (Self::Exec, "Exec", "Stop when the inferior calls exec"),
        (Self::Fork, "Fork", "Stop when the inferior forks"),
        (Self::Vfork, "Vfork", "Stop when the inferior calls vfork"),
        (
            Self::Syscall,
            "Syscall",
            "Stop at every system call. This can trigger very frequently",
        ),
        (
            Self::LibraryLoad,
            "Library load",
            "Stop when any shared library is loaded",
        ),
        (
            Self::LibraryUnload,
            "Library unload",
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
    number: String,
    status: gtk::Label,
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
    source_text: Option<source::SourceLine>,
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
    columns: InstructionColumns,
    scrolled: gtk::ScrolledWindow,
    scroll_generation: Rc<Cell<u64>>,
    loading: Rc<Cell<bool>>,
    syntax_applicable: Rc<Cell<bool>>,
    setting_syntax: Rc<Cell<bool>>,
}

#[derive(Clone)]
struct InstructionColumns {
    bytes: gtk::ColumnViewColumn,
    symbols: gtk::ColumnViewColumn,
    source: gtk::ColumnViewColumn,
}

#[derive(Clone, PartialEq, Eq)]
struct RegisterRowData {
    register: Register,
    changed: bool,
    ring: Option<u64>,
    architecture: TargetArchitecture,
    endian: Option<TargetEndian>,
    pointer_bits: u32,
    vector_display: VectorDisplay,
}

#[derive(Clone)]
struct RegisterGroupView {
    kind: RegisterGroupKind,
    store: gio::ListStore,
    view: RegisterGroupWidget,
    panel: gtk::Box,
    vector_controls: Option<VectorControls>,
}

#[derive(Clone)]
enum RegisterGroupWidget {
    Table(gtk::ColumnView),
    Vector(simd::VectorRegisterList),
}

impl RegisterGroupWidget {
    fn connect_activate(&self, store: &gio::ListStore, action: impl Fn(u32) + 'static) {
        match self {
            Self::Table(view) => {
                view.connect_activate(move |_, position| action(position));
            }
            Self::Vector(view) => {
                view.connect_activate(store, action);
            }
        }
    }
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
    page: gtk::Box,
    freshness: Rc<editor::freshness::SourceFreshness>,
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
    analysis_window: Rc<RefCell<Option<gtk::Window>>>,
    analysis_content: Rc<RefCell<Option<gtk::Box>>>,
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
    byte_offset: Rc<Cell<i64>>,
    request_revision: Rc<Cell<u64>>,
    navigation: MemoryNavigation,
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
    root: workspace::ResponsiveBox,
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
    root: workspace::ResponsiveBox,
    active: Rc<Cell<bool>>,
    in_flight: Rc<Cell<bool>>,
    needs_refresh: Rc<Cell<bool>>,
    pages: gtk::Stack,
    cfg: CfgView,
    syscalls: syscall_view::SyscallView,
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
    heap_inspector_in_flight: Rc<Cell<Option<HeapInspectionId>>>,
    heap_inspector_serial: Cell<u64>,
    heap_inspector_snapshot_stop: Cell<Option<u64>>,
    heap_selection: Rc<misc_view::AllocatorSelection>,
    heap_backend_selector: gtk::DropDown,
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
    summary: MemorySummary,
    private_summary: KernelPrivateSummaryView,
}

#[derive(Clone)]
struct KernelPrivateSummaryView {
    total: gtk::Label,
    clean: gtk::Label,
    dirty: gtk::Label,
    mappings: gtk::Label,
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
    breakpoint_index: &'a Rc<RefCell<source::SourceBreakpointIndex>>,
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
// Open Settings > Keybindings to inspect or change shortcuts.
// Menus and tooltips show the active bindings.
//
// Ctrl+hover underlines navigable symbols. Ctrl+click opens definitions.
// Double-click an instruction to toggle an address breakpoint.
"#;

#[derive(Clone)]
pub struct Ui {
    investigation: Rc<investigation::Workspace>,
    replay_controls: replay::ReplayControls,
    pub(crate) model: Rc<crate::model::DebuggerModel>,
    self_weak: Rc<RefCell<std::rc::Weak<Ui>>>,
    source_open_generation: Arc<AtomicU64>,
    source_annotation_epoch: Arc<AtomicU64>,
    disassembly_source_pending: Rc<RefCell<HashMap<PathBuf, Arc<AtomicBool>>>>,
    pub window: gtk::ApplicationWindow,
    pub terminal: vte4::Terminal,
    application_log: ApplicationLog,
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
    log_toggle_button: gtk::ToggleButton,
    debug_data_button: gtk::Button,
    pub run_button: gtk::Button,
    restart_button: gtk::Button,
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
    panels: Rc<workspace::Panels>,
    panel_hosts: Rc<workspace::Hosts>,
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
    source_breakpoint_index: Rc<RefCell<source::SourceBreakpointIndex>>,
    source_breakpoint_refresh: Rc<RefCell<editor::SourceBreakpointRefresh>>,
    source_tree_indexing: Rc<Cell<bool>>,
    source_tree_generation: Arc<AtomicU64>,
    source_tree_render_generation: Arc<AtomicU64>,
    source_tree_initialized: Rc<Cell<bool>>,
    execution_source_path: Rc<RefCell<Option<PathBuf>>>,
    execution_source_line: Rc<Cell<Option<u32>>>,
    source_theme: Theme,
    source_style_scheme: Option<sourceview5::StyleScheme>,
    resolved_source_paths: Rc<RefCell<crate::performance::BoundedLruCache<String, PathBuf>>>,
    call_stack_list: gtk::Box,
    frame_buttons: Rc<RefCell<Vec<(u32, gtk::Button)>>>,
    displayed_frames: Rc<RefCell<Rc<[StackFrame]>>>,
    threads_list: gtk::Box,
    thread_controls: ThreadControls,
    thread_buttons: Rc<RefCell<Vec<(String, gtk::Button)>>>,
    latest_threads: Rc<RefCell<Option<ThreadRenderState>>>,
    module_controls: ModuleControls,
    latest_modules: Rc<RefCell<Vec<SharedLibrary>>>,
    module_debug_metadata: Rc<RefCell<HashMap<PathBuf, ModuleDebugMetadata>>>,
    module_debug_generation: Arc<AtomicU64>,
    module_debug_worker_active: Arc<AtomicBool>,
    module_debug_force_pending: Arc<AtomicBool>,
    inferior_controls: InferiorControls,
    execution_context_visual_generation: Rc<Cell<u64>>,
    execution_context_visual_pending: Rc<Cell<bool>>,
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
    local_symbol_revision: Cell<u64>,
    watch_symbol_revision: Cell<u64>,
    pending_local_variable_objects: Rc<RefCell<HashSet<(u64, usize)>>>,
    expression_watch_entry: gtk::Entry,
    expression_watch_add_button: gtk::Button,
    expression_watch_remove_button: gtk::Button,
    target_pointer_bits: Rc<Cell<u32>>,
    target_pointer_bits_known: Rc<Cell<bool>>,
    target_architecture: Rc<Cell<TargetArchitecture>>,
    target_endian: Rc<Cell<Option<TargetEndian>>>,
    current_source_language: Rc<Cell<crate::language::Language>>,
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
    instruction_memory_handler: Rc<RefCell<Option<InstructionMemoryHandler>>>,
    disassembly_handler: Rc<RefCell<Option<DisassemblyHandler>>>,
    disassembly_source_cache: DisassemblySourceCache,
    register_groups: Vec<RegisterGroupView>,
    register_render_context: Rc<RefCell<Option<crate::debugger::StopContext>>>,
    registers_empty: gtk::Label,
    stack_store: gio::ListStore,
    displayed_stack: Rc<RefCell<Vec<StackEntry>>>,
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
    memory_search: Rc<memory_search::MemorySearchView>,
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
    layout: layout::Persistence,
    breakpoints: Rc<RefCell<Vec<Breakpoint>>>,
    stop_point_filter_rows: Rc<RefCell<Vec<StopPointFilterRow>>>,
    stop_point_metadata: Rc<RefCell<HashMap<String, StopPointMetadata>>>,
    variable_editor_request: Cell<u64>,
    breakpoint_refresh_generation: Rc<Cell<u64>>,
    breakpoint_refresh_gate: Rc<RefreshGate>,
    module_refresh_gate: Rc<RefreshGate>,
    modules_dirty: Rc<Cell<bool>>,
    applied_control_state: Rc<RefCell<Option<ControlState>>>,
    gef_available: Rc<Cell<bool>>,
    gef_capabilities: Rc<RefCell<HashSet<&'static str>>>,
    gef_context_control: Rc<Cell<GefContextControl>>,
    gef_context_visible: bool,
    gef_context_hidden_by_fgdb: Rc<Cell<bool>>,
    heap_inspection_handler: Rc<RefCell<Option<HeapInspectionHandler>>>,
    source_roots: Rc<RefCell<Vec<PathBuf>>>,
    source_base_roots: Vec<PathBuf>,
    configuration_report: ConfigurationReport,
    settings: Rc<settings::Settings>,
    variable_presentation: Rc<variable_presentation::VariablePresentation>,
    variable_locations: Rc<variables::locations::Locations>,
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
}

struct Topbar {
    replay_controls: replay::ReplayControls,
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
    restart_button: gtk::Button,
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
    panels: workspace::Panels,
    layout_panes: Vec<layout::Pane>,
    console: gtk::Stack,
    application_log: ApplicationLog,
    status_detail: gtk::Label,
    source_navigation: SourceNavigationControls,
    source_tree: SourceTreeControls,
    left_navigation: gtk::Notebook,
    inspector_navigation: gtk::Notebook,
    call_stack_list: gtk::Box,
    threads_list: gtk::Box,
    thread_controls: ThreadControls,
    module_controls: ModuleControls,
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
    memory_search: Rc<memory_search::MemorySearchView>,
    memory_watch_container: MemoryWatchContainer,
    memory_address_entry: gtk::Entry,
    memory_size: gtk::SpinButton,
    memory_format: gtk::DropDown,
    memory_add_button: gtk::Button,
    kernel_view: KernelView,
    misc_view: MiscView,
}

struct Inspector {
    root: workspace::ResponsiveBox,
    notebook: gtk::Notebook,
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
    memory_search: Rc<memory_search::MemorySearchView>,
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
    module_controls: ModuleControls,
    source_tree: SourceTreeControls,
    inferior_controls: InferiorControls,
}

#[cfg(test)]
mod tests;
