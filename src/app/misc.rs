use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, TryRecvError},
    },
    time::{Duration, Instant},
};

use super::*;

static MISC_READER_ACTIVE: AtomicBool = AtomicBool::new(false);
static HEAP_READER_ACTIVE: AtomicBool = AtomicBool::new(false);
struct MiscWorkerGuard;

impl Drop for MiscWorkerGuard {
    fn drop(&mut self) {
        MISC_READER_ACTIVE.store(false, Ordering::Release);
    }
}

struct HeapWorkerGuard;

impl Drop for HeapWorkerGuard {
    fn drop(&mut self) {
        HEAP_READER_ACTIVE.store(false, Ordering::Release);
    }
}

#[derive(Clone, Copy)]
enum HeapProbeKind {
    MainArena,
    MallocHook,
    Tcache,
    Tls,
    Target,
}

struct HeapProbeSpec {
    kind: HeapProbeKind,
    expression: String,
}

struct HeapDiscoveryState {
    ui: Weak<Ui>,
    generation: u64,
    pid: u32,
    debugger_pid: u32,
    architecture: TargetArchitecture,
    endian: TargetEndian,
    pointer_bits: u32,
    action: HeapInspectionAction,
    next: usize,
    probes: Vec<HeapProbeSpec>,
    discovery: crate::misc::HeapDiscovery,
    target: Option<u64>,
}

struct AllocatorProbeDiscovery {
    ui: Weak<Ui>,
    generation: u64,
    pid: u32,
    debugger_pid: u32,
    include_locks: bool,
    next: usize,
    probe: crate::misc::AllocatorProbe,
}

pub(super) fn request_heap_inspection(
    ui: Weak<Ui>,
    client: Rc<MiClient>,
    request: HeapInspectionRequest,
) {
    let Some(current_ui) = ui.upgrade() else {
        return;
    };

    if !current_ui.model.stopped_inspection_available() {
        return;
    }

    let target_expression = match request.action {
        HeapInspectionAction::Chunk => match validated_heap_expression(&request.expression) {
            Ok(expression) => Some(expression.to_owned()),
            Err(error) => {
                let generation = current_ui.model.current_stop_refresh_generation();
                current_ui.show_heap_inspection_error(generation, "heap chunk", &error);
                return;
            }
        },
        _ => None,
    };

    if request.action == HeapInspectionAction::Backend {
        let allocator = current_ui.allocator_identity();
        let normalized = allocator.to_ascii_lowercase();

        if !normalized.contains("glibc") && !normalized.contains("ptmalloc") {
            let generation = current_ui.model.current_stop_refresh_generation();

            current_ui.show_heap_inspection_error(
                generation,
                "native allocator structures",
                &format!(
                    "fgdb does not yet have a verified native structure decoder for `{allocator}`. It will not fall back to a GEF command"
                ),
            );

            return;
        }
    }

    let command = heap_action_title(request.action);

    let Some(generation) = current_ui.begin_heap_inspection(&command) else {
        return;
    };

    current_ui.set_command_pending(true);
    drop(current_ui);
    let ui_for_response = ui.clone();
    let response_command = command.clone();
    let request_action = request.action;

    if let Err(error) =
        client.request("-list-thread-groups", move |client, record| {
            let Some(current_ui) = ui_for_response.upgrade() else {
                return;
            };

            if !current_ui.heap_inspection_is_current(generation) {
                current_ui.set_command_pending(false);

                current_ui.show_heap_inspection_error(
                    generation,
                    &response_command,
                    "Heap inspection was superseded by a newer stop",
                );

                return;
            }

            let Some(pid) = crate::debugger::inferior_pid(&record) else {
                current_ui.set_command_pending(false);

                current_ui.show_heap_inspection_error(
                    generation,
                    &response_command,
                    record
                        .error_message()
                        .unwrap_or("GDB did not report a live inferior process ID"),
                );

                return;
            };

            let Some(debugger_pid) = current_ui.model.debugger_pid() else {
                current_ui.set_command_pending(false);

                current_ui.show_heap_inspection_error(
                    generation,
                    &response_command,
                    "The local GDB process identity is unavailable",
                );

                return;
            };

            let architecture = current_ui.target_architecture();

            let Some(endian) = current_ui
                .target_endian()
                .or_else(|| architecture.default_endian())
            else {
                current_ui.set_command_pending(false);

                current_ui.show_heap_inspection_error(
                    generation,
                    &response_command,
                    "The target byte order is not known",
                );

                return;
            };

            let pointer_bits = current_ui.target_pointer_bits();
            drop(current_ui);

            let mut probes = vec![
                HeapProbeSpec {
                    kind: HeapProbeKind::MainArena,
                    expression: String::from("(void *)&main_arena"),
                },
                HeapProbeSpec {
                    kind: HeapProbeKind::MallocHook,
                    expression: String::from("(void *)&__malloc_hook"),
                },
                HeapProbeSpec {
                    kind: HeapProbeKind::Tcache,
                    expression: String::from("(void *)tcache"),
                },
            ];

            probes.extend(heap_tls_expressions(architecture).iter().map(|expression| {
                HeapProbeSpec {
                    kind: HeapProbeKind::Tls,
                    expression: (*expression).to_owned(),
                }
            }));

            if let Some(expression) = target_expression.as_ref() {
                probes.push(HeapProbeSpec {
                    kind: HeapProbeKind::Target,
                    expression: format!("(void *)({expression})"),
                });
            }

            let discovery = Rc::new(RefCell::new(HeapDiscoveryState {
                ui: ui_for_response.clone(),
                generation,
                pid,
                debugger_pid,
                architecture,
                endian,
                pointer_bits,
                action: request_action,
                next: 0,
                probes,
                discovery: crate::misc::HeapDiscovery::default(),
                target: None,
            }));

            probe_next_heap_metadata(client, discovery);
        })
        && let Some(current_ui) = ui.upgrade()
    {
        current_ui.set_command_pending(false);
        current_ui.show_heap_inspection_error(generation, &command, &error.to_string());
    }
}

fn heap_action_title(action: HeapInspectionAction) -> String {
    match action {
        HeapInspectionAction::Arenas | HeapInspectionAction::Backend => "heap arenas",
        HeapInspectionAction::Arena => "heap arena",
        HeapInspectionAction::Top => "heap top",
        HeapInspectionAction::Chunks => "heap chunks",
        HeapInspectionAction::Parsed => "heap parse",
        HeapInspectionAction::CompactBins => "heap bins compact",
        HeapInspectionAction::AllBins => "heap bins",
        HeapInspectionAction::TcacheBins => "heap bins tcache",
        HeapInspectionAction::FastBins => "heap bins fast",
        HeapInspectionAction::UnsortedBin => "heap bins unsorted",
        HeapInspectionAction::SmallBins => "heap bins small",
        HeapInspectionAction::LargeBins => "heap bins large",
        HeapInspectionAction::Chunk => "heap chunk",
    }
    .to_owned()
}

fn heap_tls_expressions(architecture: TargetArchitecture) -> &'static [&'static str] {
    match architecture {
        TargetArchitecture::X86_64 => &["(void *)$fs_base"],
        TargetArchitecture::X86 => &["(void *)$gs_base"],
        TargetArchitecture::AArch64 => &["(void *)$tpidr_el0", "(void *)$tpidr"],
        TargetArchitecture::Arm => &["(void *)$TPIDRURO"],
        TargetArchitecture::RiscV32 | TargetArchitecture::RiscV64 => &["(void *)$tp"],
        TargetArchitecture::Mips32 | TargetArchitecture::Mips64 => &[
            "(void *)($tp - 0x7000)",
            "(void *)($thread_pointer - 0x7000)",
        ],
        TargetArchitecture::PowerPc32 => &["(void *)($r2 - 0x7000)"],
        TargetArchitecture::PowerPc64 => &["(void *)($r13 - 0x7000)"],
        TargetArchitecture::S390 | TargetArchitecture::S390x => {
            &["(void *)(((unsigned long long)$acr0 << 32) | $acr1)"]
        }
        TargetArchitecture::LoongArch64 => &["(void *)$tp"],
        _ => &[],
    }
}

fn probe_next_heap_metadata(client: &MiClient, state: Rc<RefCell<HeapDiscoveryState>>) {
    loop {
        let probe = {
            let mut state = state.borrow_mut();

            let Some(ui) = state.ui.upgrade() else {
                return;
            };

            if !ui.heap_inspection_is_current(state.generation) {
                ui.set_command_pending(false);

                ui.show_heap_inspection_error(
                    state.generation,
                    &heap_action_title(state.action),
                    "Heap inspection was superseded by a newer stop",
                );

                return;
            }

            let Some(probe) = state.probes.get(state.next) else {
                drop(ui);
                start_native_heap_reader(state);
                return;
            };

            let probe = HeapProbeSpec {
                kind: probe.kind,
                expression: probe.expression.clone(),
            };

            state.next += 1;

            probe
        };

        let command = format!(
            "-data-evaluate-expression {}",
            crate::debugger::quote(&probe.expression)
        );

        let state_for_guard = Rc::clone(&state);
        let state_for_response = Rc::clone(&state);

        if client
            .request_when(
                &command,
                move || {
                    let state = state_for_guard.borrow();

                    state
                        .ui
                        .upgrade()
                        .is_some_and(|ui| ui.heap_inspection_is_current(state.generation))
                },
                move |client, record| {
                    if record.is_done()
                        && let Some(value) = crate::debugger::evaluated_value(&record)
                        && let Some(address) = pointer_address(&value)
                    {
                        let mut state = state_for_response.borrow_mut();

                        match probe.kind {
                            HeapProbeKind::MainArena if address != 0 => {
                                state.discovery.main_arena = Some(address);
                            }
                            HeapProbeKind::MallocHook if address != 0 => {
                                state.discovery.malloc_hook = Some(address);
                            }
                            HeapProbeKind::Tcache => state.discovery.tcache = Some(address),
                            HeapProbeKind::Tls if address != 0 => {
                                if !state.discovery.tls_bases.contains(&address) {
                                    state.discovery.tls_bases.push(address);
                                }
                            }
                            HeapProbeKind::Target => state.target = Some(address),
                            _ => {}
                        }
                    }

                    probe_next_heap_metadata(client, state_for_response);
                },
            )
            .is_ok()
        {
            return;
        }

        // Optional metadata probes are deliberately skippable: the native
        // reader has independent, validated fallbacks for stripped glibc.
    }
}

fn start_native_heap_reader(state: std::cell::RefMut<'_, HeapDiscoveryState>) {
    const READ_TIMEOUT: Duration = Duration::from_secs(8);

    let query = match native_heap_query(state.action, state.target) {
        Ok(query) => query,
        Err(error) => {
            if let Some(ui) = state.ui.upgrade() {
                ui.set_command_pending(false);

                ui.show_heap_inspection_error(
                    state.generation,
                    &heap_action_title(state.action),
                    &error,
                );
            }

            return;
        }
    };

    let ui = state.ui.clone();
    let generation = state.generation;

    let request = crate::misc::NativeHeapReadRequest {
        pid: state.pid,
        debugger_pid: state.debugger_pid,
        architecture: state.architecture,
        endian: state.endian,
        pointer_bits: state.pointer_bits,
        query,
        discovery: state.discovery.clone(),
    };

    drop(state);

    if HEAP_READER_ACTIVE.swap(true, Ordering::AcqRel) {
        if let Some(ui) = ui.upgrade() {
            ui.set_command_pending(false);

            ui.show_heap_inspection_error(
                generation,
                query.title(),
                "A previous native heap reader is still finishing",
            );
        }

        return;
    }

    let (sender, receiver) = mpsc::channel();

    let worker = std::thread::Builder::new()
        .name(String::from("fgdb-native-heap"))
        .spawn(move || {
            let _guard = HeapWorkerGuard;
            let _ = sender.send(crate::misc::inspect_native_heap(request));
        });

    if let Err(error) = worker {
        HEAP_READER_ACTIVE.store(false, Ordering::Release);

        if let Some(ui) = ui.upgrade() {
            ui.set_command_pending(false);

            ui.show_heap_inspection_error(
                generation,
                query.title(),
                &format!("Cannot start the native heap reader: {error}"),
            );
        }

        return;
    }

    let started = Instant::now();

    gtk::glib::timeout_add_local(Duration::from_millis(20), move || {
        match receiver.try_recv() {
            Ok(Ok(snapshot)) => {
                if let Some(ui) = ui.upgrade() {
                    ui.set_command_pending(false);
                    ui.show_heap_inspection(generation, snapshot);
                }

                gtk::glib::ControlFlow::Break
            }
            Ok(Err(error)) => {
                if let Some(ui) = ui.upgrade() {
                    ui.set_command_pending(false);
                    ui.show_heap_inspection_error(generation, query.title(), &error);
                }

                gtk::glib::ControlFlow::Break
            }

            Err(TryRecvError::Empty)
                if ui.strong_count() > 0 && started.elapsed() < READ_TIMEOUT =>
            {
                gtk::glib::ControlFlow::Continue
            }
            Err(TryRecvError::Empty) if ui.strong_count() > 0 => {
                if let Some(ui) = ui.upgrade() {
                    ui.set_command_pending(false);

                    ui.show_heap_inspection_error(
                        generation,
                        query.title(),
                        "Native heap inspection exceeded eight seconds",
                    );
                }

                gtk::glib::ControlFlow::Break
            }
            Err(TryRecvError::Disconnected) => {
                if let Some(ui) = ui.upgrade() {
                    ui.set_command_pending(false);

                    ui.show_heap_inspection_error(
                        generation,
                        query.title(),
                        "The native heap reader stopped before returning data",
                    );
                }

                gtk::glib::ControlFlow::Break
            }
            Err(TryRecvError::Empty) => gtk::glib::ControlFlow::Break,
        }
    });
}

fn native_heap_query(
    action: HeapInspectionAction,
    target: Option<u64>,
) -> Result<crate::misc::NativeHeapQuery, String> {
    use crate::misc::NativeHeapQuery;

    Ok(match action {
        HeapInspectionAction::Arenas | HeapInspectionAction::Backend => NativeHeapQuery::Arenas,
        HeapInspectionAction::Arena => NativeHeapQuery::Arena,
        HeapInspectionAction::Top => NativeHeapQuery::Top,
        HeapInspectionAction::Chunks => NativeHeapQuery::Chunks,
        HeapInspectionAction::Parsed => NativeHeapQuery::Parsed,
        HeapInspectionAction::CompactBins => NativeHeapQuery::CompactBins,
        HeapInspectionAction::AllBins => NativeHeapQuery::AllBins,
        HeapInspectionAction::TcacheBins => NativeHeapQuery::TcacheBins,
        HeapInspectionAction::FastBins => NativeHeapQuery::FastBins,
        HeapInspectionAction::UnsortedBin => NativeHeapQuery::UnsortedBin,
        HeapInspectionAction::SmallBins => NativeHeapQuery::SmallBins,
        HeapInspectionAction::LargeBins => NativeHeapQuery::LargeBins,
        HeapInspectionAction::Chunk => {
            NativeHeapQuery::Chunk(target.ok_or_else(|| {
                String::from("GDB could not resolve the chunk address expression")
            })?)
        }
    })
}

fn validated_heap_expression(expression: &str) -> Result<&str, String> {
    let expression = expression.trim();

    if expression.is_empty() {
        return Err(String::from("Enter a chunk address or expression first"));
    }

    if expression.len() > 256 {
        return Err(String::from("The heap expression exceeds 256 bytes"));
    }

    if expression.chars().any(|character| {
        character.is_control()
            || !matches!(
                character,
                'a'..='z'
                    | 'A'..='Z'
                    | '0'..='9'
                    | '_'
                    | '$'
                    | '&'
                    | '*'
                    | '+'
                    | '-'
                    | '.'
                    | '/'
                    | ':'
                    | '['
                    | ']'
                    | '('
                    | ')'
                    | '<'
                    | '>'
                    | '%'
                    | '@'
                    | ' '
                    | '\t'
            )
    }) {
        return Err(String::from(
            "The heap expression contains unsupported or command-separator characters",
        ));
    }

    let mut previous = None;

    for character in expression.chars() {
        if character == '('
            && previous.is_some_and(|previous: char| {
                previous.is_ascii_alphanumeric() || matches!(previous, '_' | ')' | ']')
            })
        {
            return Err(String::from(
                "Function calls are not allowed in read-only heap expressions",
            ));
        }

        if !character.is_whitespace() {
            previous = Some(character);
        }
    }

    Ok(expression)
}

pub(super) fn request_misc_refresh(ui: Weak<Ui>, client: Rc<MiClient>) {
    let Some(current_ui) = ui.upgrade() else {
        return;
    };

    let Some(generation) = current_ui.begin_misc_refresh() else {
        return;
    };

    let session = current_ui.model.current_session();
    let cached_pid = current_ui.model.inferior_pid();
    let debugger_pid = current_ui.model.debugger_pid();
    drop(current_ui);

    if let Some(DebugSession::CoreDump { core_dump, .. }) = session {
        read_core_dump(ui, generation, core_dump);
        return;
    }

    if let (Some(pid), Some(debugger_pid)) = (cached_pid, debugger_pid) {
        continue_live_misc_refresh(&ui, &client, generation, pid, debugger_pid);
        return;
    }

    let ui_for_response = ui.clone();

    if let Err(error) = client.request("-list-thread-groups", move |client, record| {
        let Some(current_ui) = ui_for_response.upgrade() else {
            return;
        };

        if !current_ui.misc_refresh_is_current(generation) {
            current_ui.finish_stale_misc_refresh();
            return;
        }

        let debugger_pid = current_ui.model.debugger_pid();

        let Some(pid) = crate::debugger::inferior_pid(&record) else {
            show_misc_error(
                &ui_for_response,
                generation,
                record
                    .error_message()
                    .unwrap_or("GDB did not report a live inferior process ID"),
            );

            return;
        };

        current_ui.model.set_inferior_pid(Some(pid));
        drop(current_ui);

        let Some(debugger_pid) = debugger_pid else {
            show_misc_error(
                &ui_for_response,
                generation,
                "The local GDB process identity is unavailable",
            );

            return;
        };

        continue_live_misc_refresh(&ui_for_response, client, generation, pid, debugger_pid);
    }) {
        show_misc_error(&ui, generation, &error.to_string());
    }
}

fn continue_live_misc_refresh(
    ui: &Weak<Ui>,
    client: &MiClient,
    generation: u64,
    pid: u32,
    debugger_pid: u32,
) {
    let Some(current_ui) = ui.upgrade() else {
        return;
    };

    if !current_ui.misc_refresh_is_current(generation) {
        current_ui.finish_stale_misc_refresh();
        return;
    }

    let include_locks = current_ui.misc_locks_requested();
    let allocator_requested = current_ui.misc_allocator_requested();

    let cached_allocator_probe = allocator_requested
        .then(|| current_ui.cached_allocator_probe())
        .flatten();

    drop(current_ui);

    if let Some(probe) = cached_allocator_probe {
        read_live_misc(
            ui.clone(),
            generation,
            pid,
            debugger_pid,
            include_locks,
            probe,
        );
    } else if allocator_requested {
        probe_allocator(
            ui.clone(),
            client,
            generation,
            pid,
            debugger_pid,
            include_locks,
        );
    } else {
        read_live_misc(
            ui.clone(),
            generation,
            pid,
            debugger_pid,
            include_locks,
            crate::misc::AllocatorProbe::default(),
        );
    }
}

fn probe_allocator(
    ui: Weak<Ui>,
    client: &MiClient,
    generation: u64,
    pid: u32,
    debugger_pid: u32,
    include_locks: bool,
) {
    let discovery = Rc::new(RefCell::new(AllocatorProbeDiscovery {
        ui,
        generation,
        pid,
        debugger_pid,
        include_locks,
        next: 0,
        probe: crate::misc::AllocatorProbe::default(),
    }));

    probe_next_allocator_symbol(client, discovery);
}

fn probe_next_allocator_symbol(client: &MiClient, discovery: Rc<RefCell<AllocatorProbeDiscovery>>) {
    loop {
        let probe_spec = {
            let mut state = discovery.borrow_mut();

            let Some(ui) = state.ui.upgrade() else {
                return;
            };

            if !ui.misc_refresh_is_current(state.generation) {
                ui.finish_stale_misc_refresh();
                return;
            }

            let Some(&probe_spec) = crate::misc::ALLOCATOR_PROBE_SPECS.get(state.next) else {
                state.probe.complete = true;
                let ui = state.ui.clone();
                let generation = state.generation;
                let pid = state.pid;
                let debugger_pid = state.debugger_pid;
                let include_locks = state.include_locks;
                let probe = std::mem::take(&mut state.probe);
                drop(state);
                read_live_misc(ui, generation, pid, debugger_pid, include_locks, probe);
                return;
            };

            state.next += 1;

            probe_spec
        };

        let expression = format!("(void *) {}", probe_spec.expression);

        let command = format!(
            "-data-evaluate-expression {}",
            crate::debugger::quote(&expression)
        );

        let state_for_guard = Rc::clone(&discovery);
        let state_for_response = Rc::clone(&discovery);

        if client
            .request_when(
                &command,
                move || {
                    let state = state_for_guard.borrow();

                    state
                        .ui
                        .upgrade()
                        .is_some_and(|ui| ui.misc_refresh_is_current(state.generation))
                },
                move |client, record| {
                    if record.is_done()
                        && let Some(value) = crate::debugger::evaluated_value(&record)
                        && let Some(address) = pointer_address(&value)
                        && address != 0
                    {
                        state_for_response.borrow_mut().probe.symbols.push(
                            crate::misc::AllocatorProbeSymbol {
                                name: probe_spec.name.to_owned(),
                                address,
                                indirect: crate::misc::allocator_probe_value_is_indirect(&value),
                            },
                        );
                    }

                    probe_next_allocator_symbol(client, state_for_response);
                },
            )
            .is_ok()
        {
            return;
        }

        // A saturated or disconnected MI client rejected this optional probe.
        // Skip it and retain the process mapping fallback.
        discovery.borrow_mut().probe.dispatch_failures += 1;
    }
}

fn read_live_misc(
    ui: Weak<Ui>,
    generation: u64,
    pid: u32,
    debugger_pid: u32,
    include_locks: bool,
    allocator_probe: crate::misc::AllocatorProbe,
) {
    const READ_TIMEOUT: Duration = Duration::from_secs(5);

    if MISC_READER_ACTIVE.swap(true, Ordering::AcqRel) {
        show_misc_error(
            &ui,
            generation,
            "A previous Misc data reader is still finishing",
        );

        return;
    }

    if allocator_probe.complete
        && let Some(current_ui) = ui.upgrade()
    {
        current_ui.cache_allocator_probe(allocator_probe.clone());
    }

    let (sender, receiver) = mpsc::channel();

    let worker = std::thread::Builder::new()
        .name(String::from("fgdb-misc-live"))
        .spawn(move || {
            let _guard = MiscWorkerGuard;

            let _ = sender.send(crate::misc::read_live_misc(
                pid,
                debugger_pid,
                include_locks,
                allocator_probe,
            ));
        });

    if let Err(error) = worker {
        MISC_READER_ACTIVE.store(false, Ordering::Release);

        show_misc_error(
            &ui,
            generation,
            &format!("Cannot start the Misc data reader: {error}"),
        );

        return;
    }

    let started = Instant::now();

    gtk::glib::timeout_add_local(Duration::from_millis(20), move || {
        match receiver.try_recv() {
            Ok(Ok(snapshot)) => {
                if let Some(ui) = ui.upgrade() {
                    ui.show_misc_snapshot(generation, snapshot);
                }

                gtk::glib::ControlFlow::Break
            }
            Ok(Err(error)) => {
                show_misc_error(&ui, generation, &error);

                gtk::glib::ControlFlow::Break
            }

            Err(TryRecvError::Empty)
                if ui.strong_count() > 0 && started.elapsed() < READ_TIMEOUT =>
            {
                gtk::glib::ControlFlow::Continue
            }
            Err(TryRecvError::Empty) if ui.strong_count() > 0 => {
                show_misc_error(
                    &ui,
                    generation,
                    "Reading bounded Misc process data exceeded five seconds",
                );

                gtk::glib::ControlFlow::Break
            }
            Err(TryRecvError::Disconnected) => {
                show_misc_error(
                    &ui,
                    generation,
                    "The Misc data reader stopped before returning data",
                );

                gtk::glib::ControlFlow::Break
            }
            Err(TryRecvError::Empty) => gtk::glib::ControlFlow::Break,
        }
    });
}

fn read_core_dump(ui: Weak<Ui>, generation: u64, path: std::path::PathBuf) {
    const READ_TIMEOUT: Duration = Duration::from_secs(5);

    if MISC_READER_ACTIVE.swap(true, Ordering::AcqRel) {
        show_misc_error(
            &ui,
            generation,
            "A previous Misc data reader is still finishing",
        );

        return;
    }

    let (sender, receiver) = mpsc::channel();

    let worker = std::thread::Builder::new()
        .name(String::from("fgdb-misc-core"))
        .spawn(move || {
            let _guard = MiscWorkerGuard;
            let _ = sender.send(crate::misc::read_core_dump(&path));
        });

    if let Err(error) = worker {
        MISC_READER_ACTIVE.store(false, Ordering::Release);

        show_misc_error(
            &ui,
            generation,
            &format!("Cannot start the core-note reader: {error}"),
        );

        return;
    }

    let started = Instant::now();

    gtk::glib::timeout_add_local(Duration::from_millis(20), move || {
        match receiver.try_recv() {
            Ok(Ok(snapshot)) => {
                if let Some(ui) = ui.upgrade() {
                    ui.show_misc_core_snapshot(generation, snapshot);
                }

                gtk::glib::ControlFlow::Break
            }
            Ok(Err(error)) => {
                show_misc_error(&ui, generation, &error);

                gtk::glib::ControlFlow::Break
            }

            Err(TryRecvError::Empty)
                if ui.strong_count() > 0 && started.elapsed() < READ_TIMEOUT =>
            {
                gtk::glib::ControlFlow::Continue
            }
            Err(TryRecvError::Empty) if ui.strong_count() > 0 => {
                show_misc_error(
                    &ui,
                    generation,
                    "Reading bounded core metadata exceeded five seconds",
                );

                gtk::glib::ControlFlow::Break
            }
            Err(TryRecvError::Disconnected) => {
                show_misc_error(
                    &ui,
                    generation,
                    "The core-note reader stopped before returning data",
                );

                gtk::glib::ControlFlow::Break
            }
            Err(TryRecvError::Empty) => gtk::glib::ControlFlow::Break,
        }
    });
}

fn show_misc_error(ui: &Weak<Ui>, generation: u64, error: &str) {
    if let Some(ui) = ui.upgrade() {
        ui.show_misc_error(generation, error);
    }
}

#[cfg(test)]
mod heap_inspection_tests {
    use super::validated_heap_expression;

    #[test]
    fn accepts_side_effect_free_heap_expressions() {
        assert_eq!(
            validated_heap_expression(" $rax + 0x20 "),
            Ok("$rax + 0x20")
        );

        assert_eq!(
            validated_heap_expression("(void *)$rax + 8"),
            Ok("(void *)$rax + 8")
        );
    }

    #[test]
    fn rejects_console_injection_and_function_calls() {
        assert!(validated_heap_expression("$rax\ncontinue").is_err());
        assert!(validated_heap_expression("$rax; continue").is_err());
        assert!(validated_heap_expression("malloc(32)").is_err());
        assert!(validated_heap_expression("").is_err());
    }
}
