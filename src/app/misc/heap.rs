use super::*;
use crate::ui::HeapInspectionId;

static HEAP_READER_ACTIVE: AtomicBool = AtomicBool::new(false);

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
    generation: HeapInspectionId,
    requests: StopRequests,
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

pub(in crate::app) fn request_heap_inspection(
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

    let command = if request.action == HeapInspectionAction::Backend {
        request.backend.map_or_else(
            || String::from("Allocator state"),
            |backend| format!("{} state", backend.name()),
        )
    } else {
        heap_action_title(request.action)
    };

    let Some(generation) = current_ui.begin_heap_inspection(&command) else {
        return;
    };

    let Some(backend) = request.backend else {
        current_ui.show_heap_inspection_error(
            generation,
            &command,
            "No supported allocator runtime was detected. Pause the target and refresh detection",
        );
        return;
    };

    let Some(requests) = stop_requests(&ui, &client, generation.stop_generation) else {
        current_ui.finish_heap_inspection(generation);
        return;
    };

    if backend != crate::misc::HeapBackend::Glibc {
        if request.action == HeapInspectionAction::Backend {
            request_typed_allocator(ui, requests, generation, backend);
        } else {
            current_ui.show_heap_inspection_error(
                generation,
                &command,
                "Chunk and bin actions require the glibc decoder",
            );
        }

        return;
    }

    let target_expression = match request.action {
        HeapInspectionAction::Chunk => match validated_heap_expression(&request.expression) {
            Ok(expression) => Some(expression.to_owned()),
            Err(error) => {
                current_ui.show_heap_inspection_error(generation, &command, &error);
                return;
            }
        },
        _ => None,
    };

    let selected_group = current_ui.model.selected_inferior_id();
    drop(current_ui);
    let requests_for_response = requests.clone();
    let ui_for_response = ui.clone();
    let response_command = command.clone();
    let request_action = request.action;

    if let Err(error) = requests
        .unscoped("-list-thread-groups")
        .request(move |client, record| {
            let Some(current_ui) = ui_for_response.upgrade() else {
                return;
            };

            if !requests_for_response.is_current()
                || !current_ui.heap_inspection_is_current(generation)
            {
                current_ui.show_heap_inspection_error(
                    generation,
                    &response_command,
                    "Heap inspection was superseded by a newer stop",
                );

                return;
            }

            let Some(pid) = selected_group
                .as_deref()
                .and_then(|group| crate::debugger::inferior_pid_for_group(&record, group))
            else {
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
                requests: requests_for_response,
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

fn request_typed_allocator(
    ui: Weak<Ui>,
    requests: StopRequests,
    generation: HeapInspectionId,
    backend: crate::misc::HeapBackend,
) {
    let Some(script) = crate::misc::allocator_inspection_script(backend) else {
        return;
    };

    let command = crate::debugger::console_command(&allocator_python_expression(&script));
    let ui_for_guard = ui.clone();
    let ui_for_response = ui.clone();
    let requests_for_response = requests.clone();

    let result = requests
        .frame(&command)
        .when(move || {
            ui_for_guard
                .upgrade()
                .is_some_and(|ui| ui.heap_inspection_is_current(generation))
        })
        .capture(move |_, record, output| {
            let Some(ui) = ui_for_response.upgrade() else {
                return;
            };

            if !requests_for_response.is_current() || !ui.heap_inspection_is_current(generation) {
                ui.finish_heap_inspection(generation);
                return;
            }

            let snapshot = if record.is_done() {
                crate::misc::parse_allocator_inspection(backend, &output)
            } else {
                Err(record
                    .error_message()
                    .unwrap_or("GDB Python is required for this allocator's typed state reader")
                    .to_owned())
            };

            match snapshot {
                Ok(snapshot) => ui.show_heap_inspection(generation, snapshot),
                Err(error) => ui.show_heap_inspection_error(generation, backend.name(), &error),
            }
        });

    if let Err(error) = result
        && let Some(ui) = ui.upgrade()
    {
        ui.show_heap_inspection_error(generation, backend.name(), &error.to_string());
    }
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

fn probe_next_heap_metadata(_client: &MiClient, state: Rc<RefCell<HeapDiscoveryState>>) {
    loop {
        let probe = {
            let mut state = state.borrow_mut();

            let Some(ui) = state.ui.upgrade() else {
                return;
            };

            if !state.requests.is_current() || !ui.heap_inspection_is_current(state.generation) {
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
            "-data-evaluate-expression --language c {}",
            crate::debugger::quote(&probe.expression)
        );

        let state_for_guard = Rc::clone(&state);
        let state_for_response = Rc::clone(&state);

        let requests = state.borrow().requests.clone();

        if requests
            .frame(&command)
            .when(move || {
                let state = state_for_guard.borrow();

                state
                    .ui
                    .upgrade()
                    .is_some_and(|ui| ui.heap_inspection_is_current(state.generation))
            })
            .request(move |client, record| {
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
            })
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
    let requests = state.requests.clone();
    let budget = crate::misc::HeapReadBudget::new();

    let request = crate::misc::NativeHeapReadRequest {
        pid: state.pid,
        debugger_pid: state.debugger_pid,
        architecture: state.architecture,
        endian: state.endian,
        pointer_bits: state.pointer_bits,
        query,
        discovery: state.discovery.clone(),
        budget: budget.clone(),
    };

    drop(state);

    if HEAP_READER_ACTIVE.swap(true, Ordering::AcqRel) {
        if let Some(ui) = ui.upgrade() {
            ui.show_heap_inspection_error(
                generation,
                query.title(),
                "A previous native heap reader is still finishing",
            );
        }

        return;
    }

    let (sender, receiver) = futures_channel::oneshot::channel();

    let worker = std::thread::Builder::new()
        .name(String::from("fgdb-native-heap"))
        .spawn(move || {
            let result = {
                let _guard = HeapWorkerGuard;
                crate::misc::inspect_native_heap(request)
            };

            let _ = sender.send(result);
        });

    if let Err(error) = worker {
        HEAP_READER_ACTIVE.store(false, Ordering::Release);

        if let Some(ui) = ui.upgrade() {
            ui.show_heap_inspection_error(
                generation,
                query.title(),
                &format!("Cannot start the native heap reader: {error}"),
            );
        }

        return;
    }

    gtk::glib::MainContext::default().spawn_local(async move {
        let result = crate::background::receive_current(receiver, READ_TIMEOUT, || {
            requests.is_current()
                && ui
                    .upgrade()
                    .is_some_and(|ui| ui.heap_inspection_is_current(generation))
        })
        .await;

        budget.cancel();

        let Some(ui) = ui.upgrade() else {
            return;
        };

        match result {
            Ok(Ok(snapshot)) => ui.show_heap_inspection(generation, snapshot),
            Ok(Err(error)) => ui.show_heap_inspection_error(generation, query.title(), &error),
            Err(crate::background::CompletionError::Superseded) => {
                ui.finish_heap_inspection(generation);
            }
            Err(crate::background::CompletionError::TimedOut) => ui.show_heap_inspection_error(
                generation,
                query.title(),
                "Native heap inspection exceeded eight seconds",
            ),
            Err(crate::background::CompletionError::Disconnected) => ui.show_heap_inspection_error(
                generation,
                query.title(),
                "The native heap reader stopped before returning data",
            ),
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
