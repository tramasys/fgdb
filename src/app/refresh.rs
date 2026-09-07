//! Stop-refresh scheduling and the shared inputs used by its inspectors.

use super::*;

mod registers;
mod stack;
mod variables;

pub(super) use registers::*;
pub(super) use stack::*;
pub(super) use variables::*;

const MAX_POINTER_CHAIN_DEPTH: usize = 3;
const POINTER_STRING_PREVIEW_ELEMENTS: usize = 256;
const POINTER_ENRICHMENT_CONCURRENCY: usize = 4;

pub(super) struct StackInputs {
    ui: Weak<Ui>,
    requests: StopRequests,
    frames: Option<Vec<StackFrame>>,
    registers: Option<Vec<Register>>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct LazyStopNeeds {
    stack: bool,
    memory: bool,
    tls: bool,
}

#[derive(Default)]
struct StopProcessSnapshot {
    abi: Option<(TargetArchitecture, TargetEndian, u32)>,
    regions: Vec<MemoryRegion>,
}

impl LazyStopNeeds {
    fn for_visibility(stack: bool, memory: bool, tls: bool) -> Self {
        Self { stack, memory, tls }
    }

    fn any(self) -> bool {
        self.stack || self.memory || self.tls
    }
}

pub(crate) fn refresh_stopped_state(ui: &Weak<Ui>, client: &MiClient) {
    let Some(current_ui) = ui.upgrade() else {
        return;
    };

    if current_ui.model.inferior_is_running() {
        return;
    }

    replay::refresh(ui, client, false);

    let Some(context) = current_ui.begin_stop_refresh(client.transport_epoch()) else {
        current_ui.set_debug_state_stale(true);

        current_ui.set_status(
            "Waiting for a stopped thread",
            "GDB did not identify a safe thread context for inspection. Refreshing the thread list.",
            Some("status-ready"),
        );

        drop(current_ui);
        refresh_threads(ui, client);
        return;
    };

    let generation = context.generation();
    let Some(requests) = stop_requests(ui, client, generation) else {
        return;
    };
    client.cancel_stale_stop_requests(generation);
    drop(current_ui);
    let variable_update_batch = variable_update_batch(requests.clone(), 2);

    let stack_inputs = Rc::new(RefCell::new(StackInputs {
        ui: ui.clone(),
        requests: requests.clone(),
        frames: None,
        registers: None,
    }));

    let weak_ui = ui.clone();

    let stack_inputs_for_frames = Rc::clone(&stack_inputs);
    let frames_command = "-stack-list-frames 0 24";

    if requests
        .thread(frames_command)
        .request(move |client, record| {
            if record.class == "superseded" {
                return;
            }

            let frames = if record.is_done() {
                crate::debugger::stack_frames(&record)
            } else {
                Vec::new()
            };

            if let Some(ui) = weak_ui.upgrade() {
                ui.show_frames_for_refresh(generation, &frames);
            }

            stack_inputs_for_frames.borrow_mut().frames = Some(frames);
            start_stack_refresh_if_ready(&stack_inputs_for_frames, client);
        })
        .is_err()
    {
        if let Some(ui) = stack_inputs.borrow().ui.upgrade() {
            ui.show_frames_for_refresh(generation, &[]);
        }

        stack_inputs.borrow_mut().frames = Some(Vec::new());
        start_stack_refresh_if_ready(&stack_inputs, client);
    }

    let weak_ui = ui.clone();

    let frame_command = "-stack-info-frame";

    let _ = requests.frame(frame_command).request(move |_, record| {
        if record.class == "superseded" {
            return;
        }

        if let (Some(ui), Some(frame)) = (
            weak_ui.upgrade(),
            record
                .is_done()
                .then(|| crate::debugger::current_frame(&record))
                .flatten(),
        ) {
            ui.show_execution_location(&frame);
            let pc = frame.address.clone();
            let architecture = frame.architecture;
            ui.request_disassembly_for_stop(pc, architecture);
        } else if let Some(ui) = weak_ui.upgrade() {
            ui.clear_execution_location();
        }
    });

    let weak_ui = ui.clone();

    let variable_update_batch_for_locals = Rc::clone(&variable_update_batch);
    let variables_command = "-stack-list-variables --simple-values";

    if requests
        .frame(variables_command)
        .with_print_limit(AUTOMATIC_PRINT_ELEMENTS, move |client, record| {
            if record.is_done() {
                refresh_variable_objects(
                    weak_ui.clone(),
                    client,
                    generation,
                    crate::debugger::variables(&record),
                    variable_update_batch_for_locals,
                );
            } else {
                if record.class != "superseded"
                    && let Some(ui) = weak_ui.upgrade()
                {
                    ui.show_locals_refresh_error(
                        generation,
                        record
                            .error_message()
                            .unwrap_or("GDB could not load locals and arguments"),
                    );
                }

                variable_update_batch_ready(client, &variable_update_batch_for_locals, None);
            }
        })
        .is_err()
    {
        if let Some(ui) = ui.upgrade() {
            ui.show_locals_refresh_error(generation, "The locals request could not be submitted");
        }

        variable_update_batch_ready(client, &variable_update_batch, None);
    }

    refresh_registers(ui, client, generation, stack_inputs);

    refresh_expression_watches(
        ui.clone(),
        client,
        generation,
        Rc::clone(&variable_update_batch),
    );

    refresh_threads(ui, client);
}

/// Add the expensive pointer-chain details for an inspector page from the
/// current stop cache. Switching tabs must never invalidate and rebuild the
/// complete stopped state.
pub(crate) fn refresh_cached_inspector_details(ui: &Weak<Ui>, client: &MiClient, page: u32) {
    let Some(current_ui) = ui.upgrade() else {
        return;
    };

    let generation = current_ui.model.current_stop_refresh_generation();

    let Some(requests) = stop_requests(ui, client, generation) else {
        return;
    };

    match page {
        2 => {
            let Some(registers) = current_ui.model.registers_for_details(generation) else {
                return;
            };

            drop(current_ui);
            enrich_registers(ui.clone(), client, requests.clone(), registers);
        }
        3 | 4 | 7 => {
            let Some(registers) = current_ui.model.registers_for_details(generation) else {
                return;
            };

            let Some(frames) = current_ui.model.frames_for_details(generation) else {
                return;
            };

            drop(current_ui);
            refresh_visible_stop_details(ui.clone(), client, requests.clone(), registers, frames);
        }
        _ => {}
    }
}

pub(super) fn start_stack_refresh_if_ready(refresh: &Rc<RefCell<StackInputs>>, client: &MiClient) {
    let inputs = {
        let mut refresh = refresh.borrow_mut();
        match (refresh.frames.take(), refresh.registers.take()) {
            (Some(frames), Some(registers)) => Some((
                refresh.ui.clone(),
                refresh.requests.clone(),
                registers,
                frames,
            )),
            (frames, registers) => {
                refresh.frames = frames;
                refresh.registers = registers;
                None
            }
        }
    };
    let Some((ui, requests, registers, frames)) = inputs else {
        return;
    };
    let generation = requests.generation();
    if !requests.is_current() {
        return;
    }

    let cached = ui.upgrade().map(|ui| {
        (
            ui.model.inferior_pid(),
            ui.model.debugger_pid(),
            ui.model.selected_inferior_id(),
        )
    });
    if let Some((Some(pid), debugger_pid, _)) = cached.as_ref() {
        continue_stack_refresh(
            ui,
            client,
            requests.clone(),
            registers,
            frames,
            Some(*pid),
            *debugger_pid,
        );
        return;
    }

    let selected_inferior = cached.and_then(|(_, _, selected)| selected);
    let ui_for_request = ui.clone();

    let requests_for_response = requests.clone();
    if requests
        .unscoped("-list-thread-groups")
        .request(move |client, record| {
            if record.class == "superseded" {
                return;
            }

            let pid = selected_inferior
                .as_deref()
                .and_then(|id| crate::debugger::inferior_pid_for_group(&record, id))
                .or_else(|| crate::debugger::inferior_pid(&record));
            let debugger_pid = ui.upgrade().and_then(|ui| ui.model.debugger_pid());
            continue_stack_refresh(
                ui,
                client,
                requests_for_response,
                registers,
                frames,
                pid,
                debugger_pid,
            );
        })
        .is_err()
        && let Some(ui) = ui_for_request.upgrade()
    {
        let needs = LazyStopNeeds::for_visibility(
            ui.stack_details_visible(),
            ui.memory_details_visible(),
            ui.tls_details_visible(),
        );
        if needs.stack {
            ui.show_stack_for_refresh(generation, &[]);
        }

        if needs.any() {
            ui.show_memory_regions_for_refresh(generation, &[]);
        }

        if needs.memory && ui.model.claim_memory_watches_refresh(generation) {
            ui.refresh_memory_watches();
        }

        if needs.tls && ui.model.claim_tls_runtime_refresh(generation) {
            ui.show_tls_runtime_unavailable_for_refresh(
                generation,
                "The inferior process identity is unavailable",
            );
        }

        ui.refresh_kernel_after_stop();
        ui.refresh_misc_after_stop();
    }
}

fn continue_stack_refresh(
    ui: Weak<Ui>,
    client: &MiClient,
    requests: StopRequests,
    registers: Vec<Register>,
    frames: Vec<StackFrame>,
    pid: Option<u32>,
    debugger_pid: Option<u32>,
) {
    let generation = requests.generation();
    if !requests.is_current() {
        return;
    }

    if let Some(current_ui) = ui.upgrade() {
        current_ui.model.set_inferior_pid(pid);
        if pid.is_some() {
            current_ui.set_inferior_started(true);
        }

        current_ui.show_call_abi_for_refresh(generation, &frames);
        current_ui.refresh_kernel_after_stop();
        current_ui.refresh_misc_after_stop();
    }

    let Some((pid, debugger_pid)) = pid.zip(debugger_pid) else {
        refresh_visible_stop_details(ui, client, requests.clone(), registers, frames);
        return;
    };

    let (sender, receiver) = std::sync::mpsc::channel();
    if crate::background::submit_with_priority(crate::background::Priority::Critical, move || {
        let snapshot = StopProcessSnapshot {
            abi: crate::kernel::read_local_target_abi(pid, debugger_pid),
            regions: read_memory_regions(pid, debugger_pid),
        };
        let _ = sender.send(snapshot);
    })
    .is_err()
    {
        finish_stop_process_snapshot(
            ui,
            client,
            requests.clone(),
            registers,
            frames,
            StopProcessSnapshot::default(),
        );
        return;
    }

    let weak_client = client.weak();
    let started = std::time::Instant::now();
    gtk::glib::timeout_add_local(std::time::Duration::from_millis(15), move || {
        if !requests.is_current() {
            return gtk::glib::ControlFlow::Break;
        }

        let snapshot = match receiver.try_recv() {
            Ok(snapshot) => snapshot,
            Err(std::sync::mpsc::TryRecvError::Empty)
                if started.elapsed() < std::time::Duration::from_secs(5) =>
            {
                return gtk::glib::ControlFlow::Continue;
            }

            Err(
                std::sync::mpsc::TryRecvError::Empty | std::sync::mpsc::TryRecvError::Disconnected,
            ) => StopProcessSnapshot::default(),
        };
        let Some(client) = weak_client.upgrade() else {
            return gtk::glib::ControlFlow::Break;
        };
        finish_stop_process_snapshot(
            ui.clone(),
            &client,
            requests.clone(),
            registers.clone(),
            frames.clone(),
            snapshot,
        );
        gtk::glib::ControlFlow::Break
    });
}

fn finish_stop_process_snapshot(
    ui: Weak<Ui>,
    client: &MiClient,
    requests: StopRequests,
    registers: Vec<Register>,
    frames: Vec<StackFrame>,
    mut snapshot: StopProcessSnapshot,
) {
    let generation = requests.generation();
    if !requests.is_current() {
        return;
    }

    if let Some((architecture, endian, pointer_bits)) = snapshot.abi
        && let Some(current_ui) = ui.upgrade()
    {
        let previous = (
            current_ui.target_architecture(),
            current_ui.target_endian(),
            current_ui.target_pointer_bits(),
        );
        // An ELF class and byte order remain useful even when this fgdb build
        // does not recognize e_machine. Do not let a future machine erase a
        // more specific GDB result.
        if architecture != TargetArchitecture::Unknown {
            current_ui.set_target_architecture(architecture);
        }

        current_ui.set_target_endian(Some(endian));
        current_ui.set_target_pointer_bits(pointer_bits);
        let current = (
            current_ui.target_architecture(),
            current_ui.target_endian(),
            current_ui.target_pointer_bits(),
        );
        // Rebind only when ELF discovery actually refined the target. The
        // former unconditional pass rebuilt every register row on each stop.
        if previous != current
            && let Some(current_registers) = current_ui.model.registers_for_details(generation)
        {
            current_ui.show_registers_for_refresh(generation, &current_registers);
            // ABI discovery may make pointer enrichment runnable after the
            // initial register response. The generation claim prevents a
            // duplicate active or completed attempt.
            enrich_registers(ui.clone(), client, requests.clone(), current_registers);
        }
    }

    if let Some(current_ui) = ui.upgrade() {
        let architecture = current_ui.target_architecture();
        let architecture = if architecture == TargetArchitecture::Unknown {
            TargetArchitecture::infer_from_register_names_with_bits(
                registers.iter().map(|register| register.name.as_str()),
                Some(current_ui.target_pointer_bits()),
            )
        } else {
            architecture
        };
        annotate_memory_regions(&mut snapshot.regions, &registers, architecture);
        current_ui.show_memory_regions_for_refresh(generation, &snapshot.regions);
    }

    refresh_visible_stop_details(ui, client, requests.clone(), registers, frames);
}

fn refresh_visible_stop_details(
    ui: Weak<Ui>,
    client: &MiClient,
    requests: StopRequests,
    registers: Vec<Register>,
    frames: Vec<StackFrame>,
) {
    let generation = requests.generation();
    if !requests.is_current() {
        return;
    }

    let Some(current_ui) = ui.upgrade() else {
        return;
    };
    let needs = LazyStopNeeds::for_visibility(
        current_ui.stack_details_visible(),
        current_ui.memory_details_visible(),
        current_ui.tls_details_visible(),
    );
    if !needs.any() {
        return;
    }

    let architecture = current_ui.target_architecture();
    let architecture = if architecture == TargetArchitecture::Unknown {
        TargetArchitecture::infer_from_register_names_with_bits(
            registers.iter().map(|register| register.name.as_str()),
            Some(current_ui.target_pointer_bits()),
        )
    } else {
        architecture
    };
    let regions = memory_regions_for_stop(&ui, generation);

    if needs.memory && current_ui.model.claim_memory_watches_refresh(generation) {
        current_ui.refresh_memory_watches();
    }

    if needs.tls && current_ui.model.claim_tls_runtime_refresh(generation) {
        drop(current_ui);
        request_tls_runtime(&ui, requests.clone(), &registers, &regions, architecture);
    } else {
        drop(current_ui);
    }

    if !needs.stack {
        return;
    }

    let Some(current_ui) = ui.upgrade() else {
        return;
    };
    if let Some(entries) = current_ui.model.stack_for_details(generation) {
        let Some(stack_register) =
            architecture.stack_pointer(registers.iter().map(|register| register.name.as_str()))
        else {
            return;
        };
        let Some(endian) = current_ui.target_endian() else {
            return;
        };
        let word_size = usize::try_from(current_ui.target_pointer_bits() / 8)
            .unwrap_or(8)
            .clamp(4, 8);
        drop(current_ui);
        enrich_stack(
            ui,
            client,
            requests.clone(),
            entries,
            stack_register,
            word_size,
            endian,
        );
    } else if current_ui.model.claim_stack_memory_refresh(generation) {
        drop(current_ui);
        request_stack_memory(ui, requests.clone(), registers, frames, regions);
    }
}

fn memory_regions_for_stop(ui: &Weak<Ui>, generation: u64) -> Vec<MemoryRegion> {
    let Some(current_ui) = ui.upgrade() else {
        return Vec::new();
    };
    current_ui
        .model
        .memory_regions_for_details(generation)
        .unwrap_or_default()
}

fn request_tls_runtime(
    ui: &Weak<Ui>,
    requests: StopRequests,
    registers: &[Register],
    regions: &[MemoryRegion],
    architecture: TargetArchitecture,
) {
    let generation = requests.generation();
    const TLS_READ_BYTES: usize = 80;
    let Some((register, base)) = architecture
        .thread_pointer_candidates()
        .iter()
        .copied()
        .find_map(|name| {
            registers
                .iter()
                .find(|register| register.name == name)
                .and_then(|register| pointer_address(&register.value))
                .filter(|address| *address != 0)
                .map(|address| (name, address))
        })
    else {
        if let Some(ui) = ui.upgrade() {
            ui.show_tls_runtime_unavailable_for_refresh(
                generation,
                "This target did not expose a supported non-zero thread-pointer register",
            );
        }

        return;
    };
    let mapping = memory_region_for_address(regions, base).map(MemoryRegion::description);
    let command = format!("-data-read-memory-bytes ${register} {TLS_READ_BYTES}");
    let ui_for_response = ui.clone();

    let ui_for_error = ui.clone();
    let mapping_for_response = mapping.clone();
    if requests
        .frame(&command)
        .request(move |_, record| {
            if record.class == "superseded" {
                return;
            }

            let memory = record
                .is_done()
                .then(|| crate::debugger::memory_block(&record))
                .flatten();
            if let Some(ui) = ui_for_response.upgrade() {
                if let Some(memory) = memory.as_ref() {
                    ui.show_tls_runtime_for_refresh(
                        generation,
                        (architecture, ui.target_endian(), ui.target_pointer_bits()),
                        register,
                        base,
                        mapping_for_response.as_deref(),
                        Ok(memory),
                    );
                } else {
                    ui.show_tls_runtime_for_refresh(
                        generation,
                        (architecture, ui.target_endian(), ui.target_pointer_bits()),
                        register,
                        base,
                        mapping_for_response.as_deref(),
                        Err(record
                            .error_message()
                            .unwrap_or("GDB could not read the live TLS block")),
                    );
                }
            }
        })
        .is_err()
        && let Some(ui) = ui_for_error.upgrade()
    {
        ui.show_tls_runtime_for_refresh(
            generation,
            (architecture, ui.target_endian(), ui.target_pointer_bits()),
            register,
            base,
            mapping.as_deref(),
            Err("The MI channel is unavailable"),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::LazyStopNeeds;

    #[test]
    fn hidden_inspectors_schedule_no_memory_heavy_stop_work() {
        let needs = LazyStopNeeds::for_visibility(false, false, false);
        assert!(!needs.any());
        assert!(!needs.stack);
        assert!(!needs.memory);
        assert!(!needs.tls);
    }

    #[test]
    fn each_visible_inspector_requests_only_its_lazy_stop_work() {
        assert_eq!(
            LazyStopNeeds::for_visibility(true, false, false),
            LazyStopNeeds {
                stack: true,
                memory: false,
                tls: false,
            }
        );

        assert_eq!(
            LazyStopNeeds::for_visibility(false, true, false),
            LazyStopNeeds {
                stack: false,
                memory: true,
                tls: false,
            }
        );

        assert_eq!(
            LazyStopNeeds::for_visibility(false, false, true),
            LazyStopNeeds {
                stack: false,
                memory: false,
                tls: true,
            }
        );
    }
}
