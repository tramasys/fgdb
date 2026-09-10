use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, TryRecvError},
    },
    time::{Duration, Instant},
};

use super::*;

mod heap;
pub(super) mod locks;
pub(super) use heap::request_heap_inspection;

fn allocator_python_expression(script: &str) -> String {
    format!(
        "python exec(bytes.fromhex(\"{}\").decode(), {{}})",
        super::type_metadata::hex(script.as_bytes())
    )
}

static MISC_READER_ACTIVE: AtomicBool = AtomicBool::new(false);
struct MiscWorkerGuard;

impl Drop for MiscWorkerGuard {
    fn drop(&mut self) {
        MISC_READER_ACTIVE.store(false, Ordering::Release);
    }
}

struct AllocatorProbeDiscovery {
    ui: Weak<Ui>,
    requests: StopRequests,
    generation: u64,
    pid: u32,
    debugger_pid: u32,
    include_locks: bool,
    next: usize,
    deadline: Instant,
    probe: crate::misc::AllocatorProbe,
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
    let selected_group = current_ui.model.selected_inferior_id();
    let stop_generation = current_ui.model.current_stop_refresh_generation();
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

    let Some(requests) = stop_requests(&ui, &client, stop_generation) else {
        if let Some(ui) = ui.upgrade() {
            ui.finish_stale_misc_refresh();
        }

        return;
    };

    if let Err(error) = requests
        .unscoped("-list-thread-groups")
        .request(move |client, record| {
            let Some(current_ui) = ui_for_response.upgrade() else {
                return;
            };

            if !current_ui.misc_refresh_is_current(generation) {
                current_ui.finish_stale_misc_refresh();
                return;
            }

            let debugger_pid = current_ui.model.debugger_pid();

            let Some(pid) = selected_group
                .as_deref()
                .and_then(|group| crate::debugger::inferior_pid_for_group(&record, group))
            else {
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
        })
    {
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
    let Some(current_ui) = ui.upgrade() else {
        return;
    };

    let Some(requests) = stop_requests(
        &ui,
        client,
        current_ui.model.current_stop_refresh_generation(),
    ) else {
        current_ui.finish_stale_misc_refresh();
        return;
    };

    let discovery = Rc::new(RefCell::new(AllocatorProbeDiscovery {
        ui,
        requests: requests.clone(),
        generation,
        pid,
        debugger_pid,
        include_locks,
        next: 0,
        deadline: Instant::now() + Duration::from_secs(2),
        probe: crate::misc::AllocatorProbe::default(),
    }));

    if client.capabilities().supports("python") {
        let python = allocator_python_expression(&crate::misc::allocator_probe_script());

        let command = format!(
            "-interpreter-exec --language c console {}",
            crate::debugger::quote(&python)
        );
        let guard = Rc::clone(&discovery);
        let response = Rc::clone(&discovery);

        let result = requests
            .frame(&command)
            .when(move || {
                guard
                    .borrow()
                    .ui
                    .upgrade()
                    .is_some_and(|ui| ui.misc_refresh_is_current(generation))
            })
            .capture(move |client, record, output| {
                let state = response.borrow();

                if !state.requests.is_current()
                    || !state
                        .ui
                        .upgrade()
                        .is_some_and(|ui| ui.misc_refresh_is_current(generation))
                {
                    if let Some(ui) = state.ui.upgrade()
                        && ui.misc_refresh_is_current(generation)
                    {
                        ui.finish_stale_misc_refresh();
                    }

                    return;
                }

                if record.is_done()
                    && let Some(probe) = crate::misc::parse_allocator_probe(&output)
                {
                    read_live_misc(
                        state.ui.clone(),
                        generation,
                        pid,
                        debugger_pid,
                        include_locks,
                        probe,
                    );
                    return;
                }

                drop(state);
                probe_next_allocator_symbol(client, response);
            });

        if result.is_ok() {
            return;
        }
    }

    probe_next_allocator_symbol(client, discovery);
}

fn probe_next_allocator_symbol(
    _client: &MiClient,
    discovery: Rc<RefCell<AllocatorProbeDiscovery>>,
) {
    loop {
        let probe_spec = {
            let mut state = discovery.borrow_mut();

            let Some(ui) = state.ui.upgrade() else {
                return;
            };

            if !state.requests.is_current() || !ui.misc_refresh_is_current(state.generation) {
                if ui.misc_refresh_is_current(state.generation) {
                    ui.finish_stale_misc_refresh();
                }

                return;
            }

            let probe_spec = crate::misc::ALLOCATOR_PROBE_SPECS
                .get(state.next)
                .copied()
                .filter(|_| Instant::now() < state.deadline);

            let Some(probe_spec) = probe_spec else {
                state.probe.dispatch_failures += crate::misc::ALLOCATOR_PROBE_SPECS
                    .len()
                    .saturating_sub(state.next);

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
            "-data-evaluate-expression --language c {}",
            crate::debugger::quote(&expression)
        );

        let state_for_guard = Rc::clone(&discovery);
        let state_for_response = Rc::clone(&discovery);

        let requests = discovery.borrow().requests.clone();

        if requests
            .frame(&command)
            .when(move || {
                let state = state_for_guard.borrow();

                state
                    .ui
                    .upgrade()
                    .is_some_and(|ui| ui.misc_refresh_is_current(state.generation))
            })
            .request(move |client, record| {
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
            })
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

    if !ui
        .upgrade()
        .is_some_and(|ui| ui.misc_refresh_is_current(generation))
    {
        return;
    }

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
