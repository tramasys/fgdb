use super::*;

const MAX_BACKTRACE_THREADS: usize = 256;
const MAX_BACKTRACE_FRAME: u32 = 31;
const MAX_CONCURRENT_BACKTRACES: usize = 6;
type ThreadActionContinuation = Box<dyn FnOnce(Weak<Ui>, Rc<MiClient>)>;

struct BacktraceCollection {
    ui: Weak<Ui>,
    generation: u64,
    pending: VecDeque<ThreadInfo>,
    in_flight: usize,
    finished: bool,
    traces: Vec<ThreadBacktrace>,
}

#[derive(Default)]
struct ComparisonResults {
    left_frames: Option<Result<Vec<StackFrame>, String>>,
    right_frames: Option<Result<Vec<StackFrame>, String>>,
    left_registers: Option<Result<Vec<Register>, String>>,
    right_registers: Option<Result<Vec<Register>, String>>,
}

struct ComparisonCollection {
    ui: Weak<Ui>,
    generation: u64,
    left: ThreadInfo,
    right: ThreadInfo,
    results: ComparisonResults,
}

struct ThreadPolicyRefresh {
    ui: Weak<Ui>,
    generation: u64,
    remaining: u8,
    scheduler: Option<SchedulerLockingMode>,
    non_stop: Option<bool>,
}

pub(super) fn refresh_thread_policy(ui: &Weak<Ui>, client: &MiClient) {
    let Some(current_ui) = ui.upgrade() else {
        return;
    };

    let generation = current_ui.model.start_thread_policy_refresh();
    drop(current_ui);

    let refresh = Rc::new(RefCell::new(ThreadPolicyRefresh {
        ui: ui.clone(),
        generation,
        remaining: 2,
        scheduler: None,
        non_stop: None,
    }));

    let refresh_for_response = Rc::clone(&refresh);

    if client
        .request("-gdb-show scheduler-locking", move |_, record| {
            let value = record
                .is_done()
                .then(|| record.field("value"))
                .flatten()
                .and_then(|value| value.as_const())
                .and_then(parse_scheduler_locking);

            complete_thread_policy_refresh(&refresh_for_response, Some(value), None);
        })
        .is_err()
    {
        complete_thread_policy_refresh(&refresh, Some(None), None);
    }

    let refresh_for_response = Rc::clone(&refresh);

    if client
        .request("-gdb-show non-stop", move |_, record| {
            let value = record
                .is_done()
                .then(|| record.field("value"))
                .flatten()
                .and_then(|value| value.as_const())
                .and_then(parse_on_off);

            complete_thread_policy_refresh(&refresh_for_response, None, Some(value));
        })
        .is_err()
    {
        complete_thread_policy_refresh(&refresh, None, Some(None));
    }
}

fn complete_thread_policy_refresh(
    refresh: &Rc<RefCell<ThreadPolicyRefresh>>,
    scheduler: Option<Option<SchedulerLockingMode>>,
    non_stop: Option<Option<bool>>,
) {
    let finished = {
        let mut refresh = refresh.borrow_mut();

        if let Some(scheduler) = scheduler {
            refresh.scheduler = scheduler;
        }

        if let Some(non_stop) = non_stop {
            refresh.non_stop = non_stop;
        }

        refresh.remaining = refresh.remaining.saturating_sub(1);

        (refresh.remaining == 0).then(|| {
            (
                refresh.ui.clone(),
                refresh.generation,
                refresh.scheduler,
                refresh.non_stop,
            )
        })
    };

    let Some((ui, generation, scheduler, non_stop)) = finished else {
        return;
    };

    if let Some(ui) = ui
        .upgrade()
        .filter(|ui| ui.model.is_thread_policy_refresh_current(generation))
    {
        ui.set_thread_control_policy(scheduler, non_stop);
    }
}

pub(super) fn handle_thread_action(ui: Weak<Ui>, client: Rc<MiClient>, action: ThreadAction) {
    if !ui
        .upgrade()
        .is_some_and(|ui| ui.model.thread_action_can_dispatch(&action))
    {
        return;
    }

    match action {
        ThreadAction::Refresh => {
            refresh_thread_policy(&ui, &client);
            refresh_inferiors(&ui, &client);

            if ui
                .upgrade()
                .is_some_and(|ui| !ui.model.inferior_is_running())
            {
                refresh_threads(&ui, &client);
            }
        }
        ThreadAction::SetSchedulerLocking(mode) => {
            set_scheduler_locking(ui, client, mode, None);
        }
        ThreadAction::SetNonStop(enabled) => set_non_stop(ui, client, enabled),
        ThreadAction::RunOnly(id) => run_only(ui, client, id),
        ThreadAction::Freeze(id) => control_non_stop_thread(ui, client, id, false),
        ThreadAction::Thaw(id) => control_non_stop_thread(ui, client, id, true),
        ThreadAction::Backtraces { generation } => {
            request_all_backtraces(ui, client, generation);
        }

        ThreadAction::Compare {
            generation,
            left,
            right,
        } => request_thread_comparison(ui, client, generation, left, right),
        ThreadAction::SelectFrame { thread, frame } => {
            select_thread_frame(ui, client, thread, frame);
        }
    }
}

fn set_scheduler_locking(
    ui: Weak<Ui>,
    client: Rc<MiClient>,
    mode: SchedulerLockingMode,
    then: Option<ThreadActionContinuation>,
) {
    let Some(current_ui) = ui.upgrade() else {
        return;
    };

    current_ui.set_thread_action_pending(Some(ThreadActionPending::Setting));
    drop(current_ui);
    let command = format!("-gdb-set scheduler-locking {}", mode.gdb_value());
    let weak_ui = ui.clone();
    let weak_ui_for_error = ui;
    let client_for_response = Rc::clone(&client);

    if client
        .request(&command, move |_, record| {
            let Some(current_ui) = weak_ui.upgrade() else {
                return;
            };

            if !record.is_done() {
                current_ui.clear_thread_action_pending();

                current_ui.set_thread_control_policy(
                    current_ui.model.scheduler_locking_mode(),
                    current_ui.model.non_stop_mode(),
                );

                current_ui.set_status(
                    "Scheduler locking failed",
                    record
                        .error_message()
                        .unwrap_or("GDB rejected the scheduler-locking mode"),
                    Some("status-error"),
                );

                return;
            }

            current_ui.set_thread_control_policy(Some(mode), current_ui.model.non_stop_mode());

            if let Some(next) = then {
                drop(current_ui);
                next(weak_ui.clone(), Rc::clone(&client_for_response));
            } else {
                current_ui.clear_thread_action_pending();

                current_ui.set_status(
                    "Scheduler locking updated",
                    &format!("Scheduler locking is now {}", mode.gdb_value()),
                    Some("status-ready"),
                );
            }
        })
        .is_err()
        && let Some(ui) = weak_ui_for_error.upgrade()
    {
        ui.clear_thread_action_pending();

        ui.set_status(
            "Scheduler locking failed",
            "Could not queue the GDB setting command",
            Some("status-error"),
        );
    }
}

fn set_non_stop(ui: Weak<Ui>, client: Rc<MiClient>, enabled: bool) {
    let Some(current_ui) = ui.upgrade() else {
        return;
    };

    if current_ui.model.inferior_has_started() {
        current_ui.set_thread_control_policy(
            current_ui.model.scheduler_locking_mode(),
            current_ui.model.non_stop_mode(),
        );

        current_ui.set_status(
            "Non-stop mode unchanged",
            "GDB can change non-stop mode only before starting or attaching to a target",
            Some("status-error"),
        );

        return;
    }

    current_ui.set_thread_action_pending(Some(ThreadActionPending::Setting));
    drop(current_ui);
    let command = format!("-gdb-set non-stop {}", if enabled { "on" } else { "off" });
    let weak_ui = ui.clone();
    let weak_ui_for_error = ui;

    if client
        .request(&command, move |_, record| {
            let Some(ui) = weak_ui.upgrade() else {
                return;
            };

            ui.clear_thread_action_pending();

            if record.is_done() {
                ui.set_thread_control_policy(ui.model.scheduler_locking_mode(), Some(enabled));

                ui.set_status(
                    "Thread-control mode updated",
                    if enabled {
                        "The next target will use non-stop mode when supported"
                    } else {
                        "The next target will use all-stop mode"
                    },
                    Some("status-ready"),
                );
            } else {
                ui.set_thread_control_policy(
                    ui.model.scheduler_locking_mode(),
                    ui.model.non_stop_mode(),
                );

                ui.set_status(
                    "Thread-control mode failed",
                    record
                        .error_message()
                        .unwrap_or("GDB rejected the non-stop setting"),
                    Some("status-error"),
                );
            }
        })
        .is_err()
        && let Some(ui) = weak_ui_for_error.upgrade()
    {
        ui.clear_thread_action_pending();
        ui.set_thread_control_policy(ui.model.scheduler_locking_mode(), ui.model.non_stop_mode());

        ui.set_status(
            "Thread-control mode failed",
            "Could not queue the GDB non-stop setting",
            Some("status-error"),
        );
    }
}

fn run_only(ui: Weak<Ui>, client: Rc<MiClient>, id: String) {
    let Some(current_ui) = ui.upgrade() else {
        return;
    };

    let Some(thread) = crate::debugger::thread_id_argument(&id).map(str::to_owned) else {
        current_ui.set_status(
            "Thread execution unavailable",
            &format!("GDB reported an unsupported thread identifier: {id}"),
            Some("status-error"),
        );

        return;
    };

    if current_ui.model.non_stop_mode() == Some(true)
        || current_ui.model.scheduler_locking_mode() == Some(SchedulerLockingMode::On)
    {
        drop(current_ui);
        resume_thread(ui, client, thread, "Running only the selected thread");
        return;
    }

    drop(current_ui);
    let thread_for_resume = thread;

    set_scheduler_locking(
        ui,
        client,
        SchedulerLockingMode::On,
        Some(Box::new(move |ui, client| {
            resume_thread(
                ui,
                client,
                thread_for_resume,
                "Running only the selected thread with scheduler locking",
            );
        })),
    );
}

fn resume_thread(ui: Weak<Ui>, client: Rc<MiClient>, id: String, detail: &'static str) {
    let Some(current_ui) = ui.upgrade() else {
        return;
    };

    current_ui.set_thread_action_pending(Some(ThreadActionPending::Execution));

    if !crate::ui::controls::issue_execution_command(
        &current_ui,
        &client,
        &format!("-exec-continue --thread {id}"),
        detail,
    ) {
        current_ui.clear_thread_action_pending();
    }
}

fn control_non_stop_thread(ui: Weak<Ui>, client: Rc<MiClient>, id: String, resume: bool) {
    let Some(current_ui) = ui.upgrade() else {
        return;
    };

    if current_ui.model.non_stop_mode() != Some(true) {
        current_ui.set_status(
            "Individual thread control unavailable",
            "Freeze and thaw require GDB non-stop mode, which must be enabled before the target starts",
            Some("status-error"),
        );

        return;
    }

    let Some(thread) = crate::debugger::thread_id_argument(&id) else {
        current_ui.set_status(
            "Individual thread control unavailable",
            &format!("GDB reported an unsupported thread identifier: {id}"),
            Some("status-error"),
        );

        return;
    };

    current_ui.set_thread_action_pending(Some(ThreadActionPending::Execution));

    let command = format!(
        "{} --thread {thread}",
        if resume {
            "-exec-continue"
        } else {
            "-exec-interrupt"
        }
    );

    if !crate::ui::controls::issue_execution_command(
        &current_ui,
        &client,
        &command,
        if resume {
            "Thawing the selected thread"
        } else {
            "Freezing the selected thread"
        },
    ) {
        current_ui.clear_thread_action_pending();
    }
}

fn request_all_backtraces(ui: Weak<Ui>, client: Rc<MiClient>, generation: u64) {
    let Some(current_ui) = ui.upgrade() else {
        return;
    };

    let threads = current_ui
        .thread_snapshot()
        .into_iter()
        .filter(|thread| thread.state == "stopped")
        .take(MAX_BACKTRACE_THREADS)
        .collect::<Vec<_>>();

    if threads.is_empty() {
        current_ui.show_thread_analysis_error(
            generation,
            "No stopped threads are available for backtracing",
        );

        return;
    }

    current_ui.set_thread_action_pending(Some(ThreadActionPending::Analysis));
    drop(current_ui);
    let thread_count = threads.len();

    let collection = Rc::new(RefCell::new(BacktraceCollection {
        ui,
        generation,
        pending: threads.into(),
        in_flight: 0,
        finished: false,
        traces: Vec::with_capacity(thread_count),
    }));

    pump_backtraces(&collection, &client);
}

fn pump_backtraces(collection: &Rc<RefCell<BacktraceCollection>>, client: &MiClient) {
    loop {
        let thread = {
            let mut state = collection.borrow_mut();

            let current = state.ui.upgrade().is_some_and(|ui| {
                ui.model.is_thread_analysis_current(state.generation)
                    && !ui.model.inferior_is_running()
            });

            if !current {
                state.pending.clear();
                return;
            }

            if state.in_flight >= MAX_CONCURRENT_BACKTRACES {
                return;
            }

            let Some(thread) = state.pending.pop_front() else {
                drop(state);
                finish_backtraces(collection);
                return;
            };

            state.in_flight += 1;

            thread
        };

        let Some(id) = crate::debugger::thread_id_argument(&thread.id).map(str::to_owned) else {
            record_backtrace(
                collection,
                ThreadBacktrace {
                    thread,
                    frames: Vec::new(),
                    error: Some(String::from("Unsupported GDB thread identifier")),
                },
            );

            continue;
        };

        let command = format!("-stack-list-frames --thread {id} 0 {MAX_BACKTRACE_FRAME}");
        let collection_for_response = Rc::clone(collection);
        let thread_for_response = thread.clone();
        let collection_for_guard = Rc::clone(collection);

        if client
            .request_when(
                &command,
                move || {
                    let collection = collection_for_guard.borrow();

                    collection.ui.upgrade().is_some_and(|ui| {
                        ui.model.is_thread_analysis_current(collection.generation)
                            && !ui.model.inferior_is_running()
                    })
                },
                move |client, record| {
                    let trace = if record.is_done() {
                        ThreadBacktrace {
                            thread: thread_for_response,
                            frames: crate::debugger::stack_frames(&record),
                            error: None,
                        }
                    } else {
                        ThreadBacktrace {
                            thread: thread_for_response,
                            frames: Vec::new(),
                            error: Some(
                                record
                                    .error_message()
                                    .unwrap_or("GDB could not read this thread stack")
                                    .to_owned(),
                            ),
                        }
                    };

                    complete_backtrace(&collection_for_response, client, trace);
                },
            )
            .is_err()
        {
            record_backtrace(
                collection,
                ThreadBacktrace {
                    thread,
                    frames: Vec::new(),
                    error: Some(String::from("Could not queue the stack query")),
                },
            );
        }
    }
}

fn complete_backtrace(
    collection: &Rc<RefCell<BacktraceCollection>>,
    client: &MiClient,
    trace: ThreadBacktrace,
) {
    record_backtrace(collection, trace);
    pump_backtraces(collection, client);
}

fn record_backtrace(collection: &Rc<RefCell<BacktraceCollection>>, trace: ThreadBacktrace) {
    {
        let mut collection = collection.borrow_mut();
        collection.traces.push(trace);
        collection.in_flight = collection.in_flight.saturating_sub(1);
    }
}

fn finish_backtraces(collection: &Rc<RefCell<BacktraceCollection>>) {
    let ready = {
        let collection = collection.borrow();

        !collection.finished && collection.pending.is_empty() && collection.in_flight == 0
    };

    if !ready {
        return;
    }

    let (ui, generation, mut traces) = {
        let mut collection = collection.borrow_mut();
        collection.finished = true;

        (
            collection.ui.clone(),
            collection.generation,
            std::mem::take(&mut collection.traces),
        )
    };

    traces.sort_by(|left, right| {
        crate::debugger::compare_thread_ids(&left.thread.id, &right.thread.id)
    });

    if let Some(ui) = ui
        .upgrade()
        .filter(|ui| ui.finish_thread_analysis_action(generation))
    {
        ui.show_thread_backtraces(generation, traces);
    }
}

fn request_thread_comparison(
    ui: Weak<Ui>,
    client: Rc<MiClient>,
    generation: u64,
    left_id: String,
    right_id: String,
) {
    let Some(current_ui) = ui.upgrade() else {
        return;
    };

    let threads = current_ui.thread_snapshot();
    let left = threads.iter().find(|thread| thread.id == left_id).cloned();
    let right = threads.iter().find(|thread| thread.id == right_id).cloned();

    let (Some(left), Some(right)) = (left, right) else {
        current_ui.show_thread_analysis_error(generation, "The selected threads no longer exist");
        return;
    };

    if left.state != "stopped" || right.state != "stopped" {
        current_ui.show_thread_analysis_error(
            generation,
            "Both threads must be stopped before frames and registers can be compared",
        );

        return;
    }

    current_ui.set_thread_action_pending(Some(ThreadActionPending::Analysis));

    if let Some(names) = current_ui.model.cached_register_names() {
        drop(current_ui);
        start_thread_comparison(ui, client, generation, left, right, names);
        return;
    }

    drop(current_ui);
    let weak_ui = ui.clone();
    let weak_ui_for_error = ui;
    let client_for_response = Rc::clone(&client);

    if client
        .request("-data-list-register-names", move |_, record| {
            if !record.is_done() {
                if let Some(ui) = weak_ui
                    .upgrade()
                    .filter(|ui| ui.finish_thread_analysis_action(generation))
                {
                    ui.show_thread_analysis_error(
                        generation,
                        record
                            .error_message()
                            .unwrap_or("GDB could not list target registers"),
                    );
                }

                return;
            }

            let names = Rc::new(crate::debugger::register_names(&record));

            if let Some(ui) = weak_ui
                .upgrade()
                .filter(|ui| ui.model.is_thread_analysis_current(generation))
            {
                ui.model.cache_register_names(Rc::clone(&names));
            } else {
                return;
            }

            start_thread_comparison(
                weak_ui.clone(),
                Rc::clone(&client_for_response),
                generation,
                left,
                right,
                names,
            );
        })
        .is_err()
        && let Some(ui) = weak_ui_for_error.upgrade()
        && ui.finish_thread_analysis_action(generation)
    {
        ui.show_thread_analysis_error(generation, "Could not queue the register-name query");
    }
}

fn start_thread_comparison(
    ui: Weak<Ui>,
    client: Rc<MiClient>,
    generation: u64,
    left: ThreadInfo,
    right: ThreadInfo,
    names: Rc<Vec<String>>,
) {
    let collection = Rc::new(RefCell::new(ComparisonCollection {
        ui,
        generation,
        left: left.clone(),
        right: right.clone(),
        results: ComparisonResults::default(),
    }));

    request_comparison_frames(&client, &collection, left, true);
    request_comparison_frames(&client, &collection, right, false);

    let architecture = collection
        .borrow()
        .ui
        .upgrade()
        .map_or(TargetArchitecture::Unknown, |ui| ui.target_architecture());

    let numbers = crate::debugger::compact_register_numbers(&names, architecture);
    request_comparison_registers(&client, &collection, names.clone(), numbers.clone(), true);
    request_comparison_registers(&client, &collection, names, numbers, false);
}

fn request_comparison_frames(
    client: &MiClient,
    collection: &Rc<RefCell<ComparisonCollection>>,
    thread: ThreadInfo,
    left: bool,
) {
    let result = crate::debugger::thread_id_argument(&thread.id)
        .map(|id| format!("-stack-list-frames --thread {id} 0 {MAX_BACKTRACE_FRAME}"));

    let Some(command) = result else {
        complete_comparison_frames(
            collection,
            left,
            Err(String::from("Unsupported GDB thread identifier")),
        );

        return;
    };

    let collection_for_response = Rc::clone(collection);

    if client
        .request(&command, move |_, record| {
            let frames = if record.is_done() {
                Ok(crate::debugger::stack_frames(&record))
            } else {
                Err(record
                    .error_message()
                    .unwrap_or("GDB could not read the thread stack")
                    .to_owned())
            };

            complete_comparison_frames(&collection_for_response, left, frames);
        })
        .is_err()
    {
        complete_comparison_frames(
            collection,
            left,
            Err(String::from("Could not queue the stack query")),
        );
    }
}

fn complete_comparison_frames(
    collection: &Rc<RefCell<ComparisonCollection>>,
    left: bool,
    result: Result<Vec<StackFrame>, String>,
) {
    if left {
        collection.borrow_mut().results.left_frames = Some(result);
    } else {
        collection.borrow_mut().results.right_frames = Some(result);
    }

    finish_comparison_if_ready(collection);
}

fn request_comparison_registers(
    client: &MiClient,
    collection: &Rc<RefCell<ComparisonCollection>>,
    names: Rc<Vec<String>>,
    numbers: Vec<usize>,
    left: bool,
) {
    let thread = if left {
        collection.borrow().left.id.clone()
    } else {
        collection.borrow().right.id.clone()
    };

    let Some(thread) = crate::debugger::thread_id_argument(&thread).map(str::to_owned) else {
        complete_comparison_registers(
            collection,
            left,
            Err(String::from("Unsupported GDB thread identifier")),
        );

        return;
    };

    if numbers.is_empty() {
        complete_comparison_registers(collection, left, Ok(Vec::new()));
        return;
    }

    let mut command = format!("-data-list-register-values --thread {thread} x");

    for number in numbers {
        let _ = write!(command, " {number}");
    }

    let collection_for_response = Rc::clone(collection);

    if client
        .request(&command, move |_, record| {
            let registers = if record.is_done() {
                Ok(crate::debugger::registers(&record, &names))
            } else {
                Err(record
                    .error_message()
                    .unwrap_or("GDB could not read the thread registers")
                    .to_owned())
            };

            complete_comparison_registers(&collection_for_response, left, registers);
        })
        .is_err()
    {
        complete_comparison_registers(
            collection,
            left,
            Err(String::from("Could not queue the register query")),
        );
    }
}

fn complete_comparison_registers(
    collection: &Rc<RefCell<ComparisonCollection>>,
    left: bool,
    result: Result<Vec<Register>, String>,
) {
    if left {
        collection.borrow_mut().results.left_registers = Some(result);
    } else {
        collection.borrow_mut().results.right_registers = Some(result);
    }

    finish_comparison_if_ready(collection);
}

fn finish_comparison_if_ready(collection: &Rc<RefCell<ComparisonCollection>>) {
    let ready = {
        let collection = collection.borrow();

        collection.results.left_frames.is_some()
            && collection.results.right_frames.is_some()
            && collection.results.left_registers.is_some()
            && collection.results.right_registers.is_some()
    };

    if !ready {
        return;
    }

    let (ui, generation, left, right, results) = {
        let mut collection = collection.borrow_mut();

        (
            collection.ui.clone(),
            collection.generation,
            collection.left.clone(),
            collection.right.clone(),
            std::mem::take(&mut collection.results),
        )
    };

    let ComparisonResults {
        left_frames: Some(left_frames),
        right_frames: Some(right_frames),
        left_registers: Some(left_registers),
        right_registers: Some(right_registers),
    } = results
    else {
        if let Some(ui) = ui
            .upgrade()
            .filter(|ui| ui.finish_thread_analysis_action(generation))
        {
            ui.show_thread_analysis_error(
                generation,
                "Thread comparison completed with an incomplete result",
            );
        }

        return;
    };

    let mut warnings = Vec::new();
    let left_frames = unwrap_comparison_result(left_frames, "Left stack", &mut warnings);
    let right_frames = unwrap_comparison_result(right_frames, "Right stack", &mut warnings);
    let left_registers = unwrap_comparison_result(left_registers, "Left registers", &mut warnings);

    let right_registers =
        unwrap_comparison_result(right_registers, "Right registers", &mut warnings);

    let comparison = ThreadComparison {
        left,
        right,
        frames: compare_frames(&left_frames, &right_frames),
        registers: compare_registers(&left_registers, &right_registers),
        warnings,
    };

    if let Some(ui) = ui
        .upgrade()
        .filter(|ui| ui.finish_thread_analysis_action(generation))
    {
        ui.show_thread_comparison(generation, comparison);
    }
}

fn unwrap_comparison_result<T>(
    result: Result<Vec<T>, String>,
    label: &str,
    warnings: &mut Vec<String>,
) -> Vec<T> {
    match result {
        Ok(values) => values,
        Err(error) => {
            warnings.push(format!("{label}: {error}"));

            Vec::new()
        }
    }
}

fn compare_frames(left: &[StackFrame], right: &[StackFrame]) -> Vec<ThreadComparisonRow> {
    let count = left.len().max(right.len());

    (0..count)
        .map(|index| {
            let left_frame = left.get(index);
            let right_frame = right.get(index);
            let left = left_frame.map_or_else(|| String::from("<no frame>"), format_frame);
            let right = right_frame.map_or_else(|| String::from("<no frame>"), format_frame);

            ThreadComparisonRow {
                item: format!("Frame #{index}"),
                different: left != right,
                left,
                right,
            }
        })
        .collect()
}

fn format_frame(frame: &StackFrame) -> String {
    let location = frame.source_path().zip(frame.line).map_or_else(
        || frame.address.clone(),
        |(path, line)| format!("{path}:{line}"),
    );

    format!("{} at {location}", frame.function)
}

fn compare_registers(left: &[Register], right: &[Register]) -> Vec<ThreadComparisonRow> {
    let left = left
        .iter()
        .map(|register| (register.name.as_str(), register.value.as_str()))
        .collect::<HashMap<_, _>>();

    let right = right
        .iter()
        .map(|register| (register.name.as_str(), register.value.as_str()))
        .collect::<HashMap<_, _>>();

    let mut names = left.keys().chain(right.keys()).copied().collect::<Vec<_>>();
    names.sort_unstable();
    names.dedup();

    names
        .into_iter()
        .map(|name| {
            let left = left.get(name).copied().unwrap_or("<unavailable>");
            let right = right.get(name).copied().unwrap_or("<unavailable>");

            ThreadComparisonRow {
                item: name.to_owned(),
                left: left.to_owned(),
                right: right.to_owned(),
                different: left != right,
            }
        })
        .collect()
}

fn select_thread_frame(ui: Weak<Ui>, client: Rc<MiClient>, thread: String, frame: u32) {
    let Some(current_ui) = ui.upgrade() else {
        return;
    };

    if !current_ui.model.thread_is_stopped(&thread) {
        current_ui.set_status(
            "Frame selection unavailable",
            "The thread is no longer stopped",
            Some("status-error"),
        );

        return;
    }

    let Some(thread) = crate::debugger::thread_id_argument(&thread).map(str::to_owned) else {
        return;
    };

    current_ui.set_thread_action_pending(Some(ThreadActionPending::Setting));
    drop(current_ui);
    let weak_ui = ui.clone();
    let weak_ui_for_error = ui;

    if client
        .request(
            &format!("-thread-select {thread}"),
            move |client, record| {
                if !record.is_done() {
                    if let Some(ui) = weak_ui.upgrade() {
                        ui.clear_thread_action_pending();

                        ui.set_status(
                            "Thread selection failed",
                            record.error_message().unwrap_or("GDB rejected the thread"),
                            Some("status-error"),
                        );
                    }

                    return;
                }

                let weak_ui_for_frame = weak_ui.clone();
                let weak_ui_for_frame_error = weak_ui.clone();

                if client
                    .request(
                        &format!("-stack-select-frame {frame}"),
                        move |client, record| {
                            if let Some(ui) = weak_ui_for_frame.upgrade() {
                                ui.clear_thread_action_pending();
                            }

                            if record.is_done() {
                                refresh_stopped_state(&weak_ui_for_frame, client);
                            } else if let Some(ui) = weak_ui_for_frame.upgrade() {
                                ui.set_status(
                                    "Frame selection failed",
                                    record.error_message().unwrap_or("GDB rejected the frame"),
                                    Some("status-error"),
                                );
                            }
                        },
                    )
                    .is_err()
                    && let Some(ui) = weak_ui_for_frame_error.upgrade()
                {
                    ui.clear_thread_action_pending();

                    ui.set_status(
                        "Frame selection failed",
                        "Could not queue the frame-selection command",
                        Some("status-error"),
                    );
                }
            },
        )
        .is_err()
        && let Some(ui) = weak_ui_for_error.upgrade()
    {
        ui.clear_thread_action_pending();

        ui.set_status(
            "Thread selection failed",
            "Could not queue the thread-selection command",
            Some("status-error"),
        );
    }
}

fn parse_scheduler_locking(value: &str) -> Option<SchedulerLockingMode> {
    match value {
        "off" => Some(SchedulerLockingMode::Off),
        "on" => Some(SchedulerLockingMode::On),
        "step" => Some(SchedulerLockingMode::Step),
        "replay" => Some(SchedulerLockingMode::Replay),
        _ => None,
    }
}

fn parse_on_off(value: &str) -> Option<bool> {
    match value {
        "on" => Some(true),
        "off" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(level: u32, function: &str, address: &str) -> StackFrame {
        StackFrame {
            level,
            address: address.to_owned(),
            function: function.to_owned(),
            architecture: None,
            file: None,
            fullname: None,
            line: None,
        }
    }

    fn register(name: &str, value: &str) -> Register {
        Register {
            name: name.to_owned(),
            value: value.to_owned(),
            pointer_chain: Vec::new(),
        }
    }

    #[test]
    fn comparisons_keep_equal_and_changed_frames_visible() {
        let rows = compare_frames(
            &[frame(0, "worker", "0x10"), frame(1, "main", "0x20")],
            &[frame(0, "worker", "0x10"), frame(1, "poll", "0x30")],
        );

        assert_eq!(rows.len(), 2);
        assert!(!rows[0].different);
        assert!(rows[1].different);
    }

    #[test]
    fn register_comparison_is_name_aligned_and_marks_missing_values() {
        let rows = compare_registers(
            &[register("pc", "0x10"), register("sp", "0x20")],
            &[register("pc", "0x11"), register("fp", "0x30")],
        );

        assert_eq!(rows.len(), 3);
        assert!(rows.iter().all(|row| row.different));
        assert_eq!(rows[0].item, "fp");
    }

    #[test]
    fn parses_only_documented_thread_policy_values() {
        assert_eq!(
            parse_scheduler_locking("step"),
            Some(SchedulerLockingMode::Step)
        );

        assert_eq!(parse_scheduler_locking("invalid"), None);
        assert_eq!(parse_on_off("on"), Some(true));
    }
}
