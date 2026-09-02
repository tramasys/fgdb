use std::cell::Cell;
use std::collections::HashMap;
use std::time::{Duration, Instant};

use super::*;
use crate::debugger::Instruction;
use crate::ui::controls::issue_execution_command;

mod expression;

use expression::{parse_value as parse_condition_value, validate as validate_until_expression};

const DISASSEMBLY_LOOKAHEAD_BYTES: u64 = 512;
const MAX_UNTIL_LOOKAHEAD_INSTRUCTIONS: usize = 256;
const MAX_TRACKED_UNTIL_PCS: usize = 8192;
const MIN_NATIVE_ADVANCE_STEPS: u16 = 4;
const EXECUTION_TRANSITION_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Clone, Debug, PartialEq, Eq)]
struct MappingIdentity {
    start: u64,
    end: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StopDecision {
    EvaluateCondition,
    Complete,
    Step,
    InspectInstruction,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StepCommand {
    Single,
    Counted,
    NativeAdvance,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct ObservedInstruction {
    checked: bool,
    safe_steps: u16,
    advance_target: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LookaheadInstruction {
    address: u64,
    matched: bool,
    safe_steps: u16,
    advance_target: Option<u64>,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct InstructionWindow {
    current_matched: bool,
    safe_steps: u16,
    advance_target: Option<u64>,
    lookahead: Vec<LookaheadInstruction>,
}

#[derive(Clone, Debug)]
struct UntilRun {
    action: UntilAction,
    steps: u64,
    current_address: Option<u64>,
    /// Addresses observed on the live execution path. The value records whether
    /// the instruction was already disassembled and found not to match.
    observed_addresses: HashMap<u64, ObservedInstruction>,
    /// Already classified instructions returned alongside the current PC by
    /// GDB's bounded disassembly response. The bounded window is replaced on
    /// the next cache miss.
    instruction_lookahead: Vec<LookaheadInstruction>,
    observed_addresses_capped: bool,
    repeated_steps: u64,
    address_space: Option<crate::misc::ProcessAddressSpace>,
    initial_mapping: Option<MappingIdentity>,
    context_control: GefContextControl,
    cancel_requested: bool,
    pending_steps: u64,
    pending_since: Option<Instant>,
    pending_location: Option<u64>,
    counted_steps_supported: bool,
    native_advance_supported: bool,
    thread_id: Option<Rc<str>>,
    condition_command: Option<Rc<str>>,
}

pub(super) struct NativeUntilController {
    ui: Weak<Ui>,
    client: Rc<MiClient>,
    state: RefCell<Option<UntilRun>>,
    generation: Cell<u64>,
}

impl NativeUntilController {
    pub(super) fn new(ui: Weak<Ui>, client: Rc<MiClient>) -> Rc<Self> {
        Rc::new(Self {
            ui,
            client,
            state: RefCell::new(None),
            generation: Cell::new(0),
        })
    }

    pub(super) fn start(self: &Rc<Self>, action: UntilAction) {
        let Some(ui) = self.ui.upgrade() else {
            return;
        };

        if ui.native_until_active() || !ui.movement_commands_available() {
            return;
        }

        match action {
            UntilAction::CurrentLine => {
                issue_execution_command(
                    &ui,
                    &self.client,
                    "-exec-until",
                    "Running until the current source line is left…",
                );

                return;
            }
            UntilAction::FunctionReturns => {
                issue_execution_command(
                    &ui,
                    &self.client,
                    "-exec-finish",
                    "Running until the current function returns…",
                );

                return;
            }
            UntilAction::Expression(ref expression) => {
                if let Err(message) = validate_until_expression(expression) {
                    ui.set_status("Invalid expression", message, Some("status-error"));
                    return;
                }
            }
            _ => {}
        }

        let generation = self.next_generation();

        let requires_address_space = matches!(
            action,
            UntilAction::UserCode | UntilAction::LibcCode | UntilAction::RegionChange
        );

        let can_use_address_space = requires_address_space || searches_instructions(&action);
        let description = action_description(&action);

        let thread_id = ui
            .current_thread_id()
            .and_then(|id| crate::debugger::thread_id_argument(&id).map(str::to_owned))
            .map(Rc::<str>::from);

        let condition_command = if let UntilAction::Expression(expression) = &action {
            Some(
                format!(
                    "-data-evaluate-expression {}",
                    crate::debugger::quote(expression)
                )
                .into(),
            )
        } else {
            None
        };

        self.state.replace(Some(UntilRun {
            action,
            steps: 0,
            current_address: None,
            observed_addresses: HashMap::new(),
            instruction_lookahead: Vec::new(),
            observed_addresses_capped: false,
            repeated_steps: 0,
            address_space: None,
            initial_mapping: None,
            context_control: GefContextControl::None,
            cancel_requested: false,
            pending_steps: 0,
            pending_since: None,
            pending_location: None,
            counted_steps_supported: true,
            native_advance_supported: true,
            thread_id,
            condition_command,
        }));

        ui.set_native_until_active(true);
        ui.set_debug_state_stale(true);
        ui.start_stop_refresh();
        ui.start_thread_refresh();
        ui.invalidate_kernel_refresh();
        ui.invalidate_misc_refresh();

        ui.set_status(
            "Running until",
            &format!("Following live execution for {}…", description),
            Some("status-running"),
        );

        drop(ui);
        self.start_progress_updates(generation);

        if can_use_address_space {
            self.prepare_address_space(generation, requires_address_space);
        } else {
            self.begin_stepping(generation);
        }
    }

    pub(super) fn cancel(self: &Rc<Self>) {
        let Some(ui) = self.ui.upgrade() else {
            return;
        };

        if self.state.borrow().is_none() {
            return;
        }

        let execution_pending = self
            .state
            .borrow()
            .as_ref()
            .is_some_and(|run| run.pending_steps > 0);

        if ui.inferior_is_running() || execution_pending {
            if self
                .state
                .borrow()
                .as_ref()
                .is_some_and(|run| run.cancel_requested)
            {
                return;
            }

            if let Some(run) = self.state.borrow_mut().as_mut() {
                run.cancel_requested = true;
            }

            if !issue_execution_command(
                &ui,
                &self.client,
                "-exec-interrupt --all",
                "Cancelling the active Until operation…",
            ) {
                if let Some(run) = self.state.borrow_mut().as_mut() {
                    run.cancel_requested = false;
                }

                ui.set_status(
                    "Until cancel failed",
                    "Could not queue the interrupt. The Until operation remains active. Retry Cancel or restart GDB if no stop arrives.",
                    Some("status-error"),
                );
            }
        } else {
            let Some(run) = self.state.borrow_mut().take() else {
                return;
            };

            self.next_generation();
            self.restore_context(run.context_control, true);
            ui.set_native_until_active(false);
            drop(ui);

            finish_stopped_state(
                &self.ui,
                &self.client,
                Some(String::from("until-cancelled")),
                None,
                None,
                Some(String::from("Until operation cancelled")),
            );
        }
    }

    pub(super) fn abort(&self) {
        let run = self.state.borrow_mut().take();

        if run.is_none() {
            return;
        }

        self.next_generation();

        if let Some(run) = run {
            self.restore_context(run.context_control, false);
        }

        if let Some(ui) = self.ui.upgrade() {
            ui.set_native_until_active(false);
        }
    }

    pub(super) fn on_stopped(
        self: &Rc<Self>,
        reason: Option<&str>,
        address: Option<&str>,
        thread_id: Option<&str>,
    ) -> bool {
        if self.state.borrow().is_none() {
            return false;
        }

        let parsed_address = address.and_then(parse_address);

        let (completed_steps, pending_location, expected_thread) = {
            let mut state = self.state.borrow_mut();

            let Some(run) = state.as_mut() else {
                return false;
            };

            run.pending_since.take();

            (
                std::mem::take(&mut run.pending_steps),
                run.pending_location.take(),
                run.thread_id.clone(),
            )
        };

        if self
            .state
            .borrow()
            .as_ref()
            .is_some_and(|run| run.cancel_requested)
        {
            let run = self.state.borrow_mut().take();
            self.next_generation();

            if let Some(run) = run {
                self.restore_context(run.context_control, true);
            }

            if let Some(ui) = self.ui.upgrade() {
                ui.set_native_until_active(false);
            }

            return false;
        }

        if !is_internal_until_stop(
            reason,
            parsed_address,
            thread_id,
            completed_steps,
            pending_location,
            expected_thread.as_deref(),
        ) {
            let run = self.state.borrow_mut().take();
            self.next_generation();

            if let Some(run) = run {
                let render_context = reason.is_none_or(|reason| !reason.starts_with("exited"));
                self.restore_context(run.context_control, render_context);
            }

            if let Some(ui) = self.ui.upgrade() {
                ui.set_native_until_active(false);
            }

            return false;
        }

        let generation = self.generation.get();

        {
            let mut state = self.state.borrow_mut();

            let Some(state) = state.as_mut() else {
                return false;
            };

            state.steps = state.steps.saturating_add(completed_steps.max(1));

            if state.steps.is_multiple_of(64)
                && let Some(ui) = self.ui.upgrade()
            {
                ui.set_status(
                    "Running until",
                    &progress_detail(state, 0),
                    Some("status-running"),
                );
            }
        }

        if let Some(address) = parsed_address {
            self.observe_address(address);
            self.inspect_stop(address, generation);
        } else {
            self.request_pc(generation, false);
        }

        true
    }

    fn prepare_address_space(self: &Rc<Self>, generation: u64, required: bool) {
        let cached_process = self
            .ui
            .upgrade()
            .and_then(|ui| ui.inferior_pid().zip(ui.debugger_pid()));

        if let Some((pid, debugger_pid)) = cached_process {
            self.finish_address_space_preparation(generation, required, pid, debugger_pid);
            return;
        }

        let controller = Rc::clone(self);

        if let Err(error) = self
            .client
            .request("-list-thread-groups", move |_, record| {
                if !controller.is_current(generation) {
                    return;
                }

                if controller.recover_timed_out_request(&record) {
                    return;
                }

                let Some(pid) = crate::debugger::inferior_pid(&record) else {
                    controller.address_space_unavailable(
                        generation,
                        required,
                        record
                            .error_message()
                            .unwrap_or("GDB did not report a live inferior process ID"),
                    );

                    return;
                };

                let Some(debugger_pid) = controller.ui.upgrade().and_then(|ui| ui.debugger_pid())
                else {
                    controller.address_space_unavailable(
                        generation,
                        required,
                        "The local GDB process identity is unavailable",
                    );

                    return;
                };

                if let Some(ui) = controller.ui.upgrade() {
                    ui.set_inferior_pid(Some(pid));
                }

                controller.finish_address_space_preparation(
                    generation,
                    required,
                    pid,
                    debugger_pid,
                );
            })
        {
            self.address_space_unavailable(generation, required, &error.to_string());
        }
    }

    fn finish_address_space_preparation(
        self: &Rc<Self>,
        generation: u64,
        required: bool,
        pid: u32,
        debugger_pid: u32,
    ) {
        if !self.is_current(generation) {
            return;
        }

        let address_space = match crate::misc::read_process_address_space(pid, debugger_pid) {
            Ok(address_space) => address_space,
            Err(error) => {
                self.address_space_unavailable(generation, required, &error);
                return;
            }
        };

        if address_space.capped {
            self.address_space_unavailable(
                generation,
                required,
                "The process has more mappings than fgdb can safely scan",
            );

            return;
        }

        let (unavailable, region_change) = {
            let state = self.state.borrow();

            let Some(state) = state.as_ref() else {
                return;
            };

            let unavailable = match &state.action {
                UntilAction::UserCode
                    if !address_space
                        .mappings
                        .iter()
                        .any(|mapping| is_user_code_mapping(mapping, &address_space)) =>
                {
                    Some("No executable mapping for the main program is available")
                }

                UntilAction::LibcCode
                    if !address_space.mappings.iter().any(is_libc_code_mapping) =>
                {
                    Some("No executable libc mapping is loaded")
                }
                _ => None,
            };

            (unavailable, state.action == UntilAction::RegionChange)
        };

        if let Some(message) = unavailable {
            self.fail(generation, message);
            return;
        }

        if let Some(state) = self.state.borrow_mut().as_mut() {
            state.address_space = Some(address_space);
        }

        if region_change {
            self.request_pc(generation, true);
        } else {
            self.begin_stepping(generation);
        }
    }

    fn address_space_unavailable(self: &Rc<Self>, generation: u64, required: bool, message: &str) {
        if required {
            self.fail(generation, message);
        } else {
            self.begin_stepping(generation);
        }
    }

    fn start_progress_updates(self: &Rc<Self>, generation: u64) {
        let controller = Rc::downgrade(self);

        gtk::glib::timeout_add_local(Duration::from_secs(1), move || {
            let Some(controller) = controller.upgrade() else {
                return gtk::glib::ControlFlow::Break;
            };

            if !controller.is_current(generation) {
                return gtk::glib::ControlFlow::Break;
            }

            let Some(ui) = controller.ui.upgrade() else {
                return gtk::glib::ControlFlow::Break;
            };

            let (detail, cancel_requested, transition_stalled) = {
                let state = controller.state.borrow();

                let Some(state) = state.as_ref() else {
                    return gtk::glib::ControlFlow::Break;
                };

                (
                    progress_detail(state, state.pending_steps),
                    state.cancel_requested,
                    state.pending_steps > 0
                        && !ui.inferior_is_running()
                        && state.pending_since.is_some_and(|started| {
                            started.elapsed() >= EXECUTION_TRANSITION_TIMEOUT
                        }),
                )
            };

            if transition_stalled {
                controller.client.quarantine(
                    "GDB accepted an Until execution step but did not report a running or stopped transition within 15 seconds. Restart GDB from the Session menu.",
                );

                return gtk::glib::ControlFlow::Break;
            }

            let (title, detail) = if cancel_requested {
                (
                    "Cancelling until",
                    String::from("Waiting for GDB to interrupt the inferior…"),
                )
            } else {
                ("Running until", detail)
            };

            ui.set_status(title, &detail, Some("status-running"));

            gtk::glib::ControlFlow::Continue
        });
    }

    fn request_pc(self: &Rc<Self>, generation: u64, preparing_region: bool) {
        let controller = Rc::clone(self);

        if let Err(error) =
            self.client
                .request("-data-evaluate-expression $pc", move |_, record| {
                    if !controller.is_current(generation) {
                        return;
                    }

                    if controller.recover_timed_out_request(&record) {
                        return;
                    }

                    let address = record
                        .is_done()
                        .then(|| crate::debugger::evaluated_value(&record))
                        .flatten()
                        .as_deref()
                        .and_then(parse_address);

                    let Some(address) = address else {
                        controller.fail(
                            generation,
                            record
                                .error_message()
                                .unwrap_or("GDB did not return the program counter"),
                        );

                        return;
                    };

                    if preparing_region {
                        controller.finish_region_preparation(address, generation);
                    } else {
                        controller.observe_address(address);
                        controller.inspect_stop(address, generation);
                    }
                })
        {
            self.fail(generation, &error.to_string());
        }
    }

    fn finish_region_preparation(self: &Rc<Self>, address: u64, generation: u64) {
        let identity = self
            .state
            .borrow()
            .as_ref()
            .and_then(|state| state.address_space.as_ref())
            .and_then(|space| mapping_at(&space.mappings, address))
            .map(mapping_identity);

        let Some(identity) = identity else {
            self.fail(
                generation,
                "The current program counter is not inside a known process mapping",
            );

            return;
        };

        if let Some(state) = self.state.borrow_mut().as_mut() {
            state.initial_mapping = Some(identity);
        }

        self.begin_stepping(generation);
    }

    fn inspect_stop(self: &Rc<Self>, address: u64, generation: u64) {
        let decision = {
            let state = self.state.borrow();

            let Some(state) = state.as_ref() else {
                return;
            };

            let advance_or_step = || {
                if instruction_is_cacheable(state, address) {
                    StopDecision::InspectInstruction
                } else {
                    StopDecision::Step
                }
            };

            match &state.action {
                UntilAction::Expression(_) => StopDecision::EvaluateCondition,
                UntilAction::UserCode => {
                    state
                        .address_space
                        .as_ref()
                        .map_or(StopDecision::Step, |space| {
                            if mapping_at(&space.mappings, address)
                                .is_some_and(|mapping| is_user_code_mapping(mapping, space))
                            {
                                StopDecision::Complete
                            } else {
                                advance_or_step()
                            }
                        })
                }
                UntilAction::LibcCode => {
                    state
                        .address_space
                        .as_ref()
                        .map_or(StopDecision::Step, |space| {
                            if mapping_at(&space.mappings, address)
                                .is_some_and(is_libc_code_mapping)
                            {
                                StopDecision::Complete
                            } else {
                                advance_or_step()
                            }
                        })
                }
                UntilAction::RegionChange => {
                    if state
                        .initial_mapping
                        .as_ref()
                        .is_some_and(|mapping| !mapping_contains(mapping, address))
                    {
                        StopDecision::Complete
                    } else {
                        advance_or_step()
                    }
                }
                _ => StopDecision::InspectInstruction,
            }
        };

        match decision {
            StopDecision::EvaluateCondition => self.evaluate_condition(generation),
            StopDecision::Complete => self.complete(generation),
            StopDecision::Step => self.issue_step(generation),
            StopDecision::InspectInstruction
                if let Some((safe_steps, advance_target)) =
                    self.cached_instruction_advance(address) =>
            {
                self.issue_steps(generation, safe_steps, advance_target);
            }
            StopDecision::InspectInstruction => {
                if let Some(instruction) = self.take_instruction_lookahead(address) {
                    if instruction.matched {
                        self.complete(generation);
                    } else {
                        self.mark_instruction_checked(
                            address,
                            instruction.safe_steps,
                            instruction.advance_target,
                        );

                        self.issue_steps(
                            generation,
                            u64::from(instruction.safe_steps),
                            instruction.advance_target,
                        );
                    }
                } else {
                    self.request_instruction(address, generation, DISASSEMBLY_LOOKAHEAD_BYTES);
                }
            }
        }
    }

    fn request_instruction(self: &Rc<Self>, address: u64, generation: u64, bytes: u64) {
        if let Some(state) = self.state.borrow_mut().as_mut() {
            state.instruction_lookahead.clear();
        }

        let end = self.state.borrow().as_ref().map_or_else(
            || address.saturating_add(bytes.max(1)),
            |state| disassembly_end(address, bytes, state.address_space.as_ref()),
        );

        let command = format!("-data-disassemble -s 0x{address:x} -e 0x{end:x} -- 0");
        let controller = Rc::clone(self);

        if let Err(error) = self.client.request(&command, move |_, record| {
            if !controller.is_current(generation) {
                return;
            }

            if controller.recover_timed_out_request(&record) {
                return;
            }

            let instructions = if record.is_done() {
                crate::debugger::instructions(&record)
            } else {
                Vec::new()
            };

            let instruction_index = instructions
                .iter()
                .position(|instruction| parse_address(&instruction.address) == Some(address))
                .unwrap_or(0);

            if instructions.get(instruction_index).is_none() {
                if bytes > 1 {
                    controller.request_instruction(address, generation, bytes.div_ceil(2));
                } else {
                    controller.fail(
                        generation,
                        record
                            .error_message()
                            .unwrap_or("GDB could not disassemble the current instruction"),
                    );
                }

                return;
            }

            let architecture = controller
                .ui
                .upgrade()
                .map_or(TargetArchitecture::Unknown, |ui| ui.target_architecture());

            let InstructionWindow {
                current_matched,
                safe_steps,
                advance_target,
                lookahead,
            } = {
                let state = controller.state.borrow();

                let Some(state) = state.as_ref() else {
                    return;
                };

                classify_instruction_window(
                    &state.action,
                    &instructions,
                    instruction_index,
                    architecture,
                    state.address_space.as_ref(),
                )
            };

            if let Some(state) = controller.state.borrow_mut().as_mut() {
                state.instruction_lookahead = lookahead;
            }

            if current_matched {
                controller.complete(generation);
            } else {
                controller.mark_instruction_checked(address, safe_steps, advance_target);
                controller.issue_steps(generation, u64::from(safe_steps), advance_target);
            }
        }) {
            self.fail(generation, &error.to_string());
        }
    }

    fn evaluate_condition(self: &Rc<Self>, generation: u64) {
        let command = self
            .state
            .borrow()
            .as_ref()
            .and_then(|state| state.condition_command.clone());

        let Some(command) = command else {
            self.fail(generation, "The Until expression is unavailable");
            return;
        };

        let controller = Rc::clone(self);

        let request = self.client.request(&command, move |_, record| {
            if !controller.is_current(generation) {
                return;
            }

            if controller.recover_timed_out_request(&record) {
                return;
            }

            if !record.is_done() {
                controller.fail(
                    generation,
                    record
                        .error_message()
                        .unwrap_or("GDB could not evaluate the Until expression"),
                );

                return;
            }

            let result = crate::debugger::evaluated_value(&record)
                .as_deref()
                .and_then(parse_condition_value);

            match result {
                Some(true) => controller.complete(generation),
                Some(false) => controller.issue_step(generation),
                None => controller.fail(
                    generation,
                    "The Until expression must produce a scalar integer or boolean value",
                ),
            }
        });

        if let Err(error) = request {
            self.fail(generation, &error.to_string());
        }
    }

    fn issue_step(self: &Rc<Self>, generation: u64) {
        self.issue_steps(generation, 1, None);
    }

    fn issue_steps(
        self: &Rc<Self>,
        generation: u64,
        requested_steps: u64,
        advance_target: Option<u64>,
    ) {
        if !self.is_current(generation) {
            return;
        }

        let (steps, command_kind, native_target, thread_id) = {
            let mut state = self.state.borrow_mut();

            let Some(run) = state.as_mut() else {
                return;
            };

            let requested_steps = requested_steps.max(1);

            let native_target = advance_target.filter(|_| {
                requested_steps >= u64::from(MIN_NATIVE_ADVANCE_STEPS)
                    && run.native_advance_supported
                    && run.thread_id.is_some()
                    && native_advance_is_safe(
                        run.address_space.as_ref(),
                        run.current_address,
                        advance_target,
                    )
            });

            let (steps, command_kind) = if native_target.is_some() {
                (requested_steps, StepCommand::NativeAdvance)
            } else if requested_steps == 1 {
                (1, StepCommand::Single)
            } else if run.counted_steps_supported {
                (requested_steps, StepCommand::Counted)
            } else {
                (1, StepCommand::Single)
            };

            let thread_id = run.thread_id.clone();
            run.pending_location = native_target;
            run.pending_steps = steps;
            run.pending_since = Some(Instant::now());

            (steps, command_kind, native_target, thread_id)
        };

        let command = match command_kind {
            StepCommand::Single => String::from("-exec-step-instruction"),
            StepCommand::Counted => crate::debugger::console_command(&format!("stepi {steps}")),
            StepCommand::NativeAdvance => {
                let (Some(target), Some(thread_id)) = (native_target, thread_id.as_deref()) else {
                    if let Some(run) = self.state.borrow_mut().as_mut() {
                        run.pending_steps = 0;
                        run.pending_since = None;
                        run.pending_location = None;
                    }

                    self.fail(generation, "The native Until target became unavailable");
                    return;
                };

                format!("-exec-until --thread {thread_id} *0x{target:x}")
            }
        };

        if let Some(ui) = self.ui.upgrade() {
            ui.set_active_thread_execution(thread_id.as_deref().map(str::to_owned));
            ui.set_thread_execution_exit_candidate(None);
        }

        let controller = Rc::clone(self);

        if let Err(error) = self.client.request(&command, move |_, record| {
            if !controller.is_current(generation) {
                return;
            }

            if controller.recover_timed_out_request(&record) || record.is_success() {
                return;
            }

            if let Some(ui) = controller.ui.upgrade() {
                ui.set_active_thread_execution(None);
                ui.set_thread_execution_exit_candidate(None);
            }

            match command_kind {
                StepCommand::NativeAdvance => {
                    if let Some(run) = controller.state.borrow_mut().as_mut() {
                        run.pending_steps = 0;
                        run.pending_since = None;
                        run.pending_location = None;
                        run.native_advance_supported = false;
                    }

                    controller.issue_steps(generation, steps, native_target);
                }
                StepCommand::Counted => {
                    if let Some(run) = controller.state.borrow_mut().as_mut() {
                        run.pending_steps = 0;
                        run.pending_since = None;
                        run.pending_location = None;
                        run.counted_steps_supported = false;
                    }

                    controller.issue_step(generation);
                }
                StepCommand::Single => controller.fail(
                    generation,
                    record
                        .error_message()
                        .unwrap_or("GDB rejected instruction stepping"),
                ),
            }
        }) {
            if let Some(ui) = self.ui.upgrade() {
                ui.set_active_thread_execution(None);
                ui.set_thread_execution_exit_candidate(None);
            }

            if let Some(run) = self.state.borrow_mut().as_mut() {
                run.pending_steps = 0;
                run.pending_since = None;
                run.pending_location = None;
            }

            self.fail(generation, &error.to_string());
        }
    }

    fn begin_stepping(self: &Rc<Self>, generation: u64) {
        let context_control = self
            .ui
            .upgrade()
            .map_or(GefContextControl::None, |ui| ui.gef_context_control());

        let Some(command) = context_suppression_command(context_control) else {
            self.issue_step(generation);
            return;
        };

        let controller = Rc::clone(self);

        if self
            .client
            .request(&command, move |_, record| {
                if !controller.is_current(generation) {
                    if record.is_success() {
                        let render_context = controller
                            .ui
                            .upgrade()
                            .is_some_and(|ui| !ui.inferior_is_running());

                        controller.restore_context(context_control, render_context);
                    }

                    return;
                }

                if controller.recover_timed_out_request(&record) {
                    return;
                }

                if record.is_success()
                    && let Some(state) = controller.state.borrow_mut().as_mut()
                {
                    state.context_control = context_control;
                }

                controller.issue_step(generation);
            })
            .is_err()
        {
            self.issue_step(generation);
        }
    }

    fn observe_address(&self, address: u64) {
        let mut state = self.state.borrow_mut();

        let Some(state) = state.as_mut() else {
            return;
        };

        state.current_address = Some(address);

        if state.observed_addresses.contains_key(&address) {
            state.repeated_steps = state.repeated_steps.saturating_add(1);
        } else if state.observed_addresses.len() < MAX_TRACKED_UNTIL_PCS {
            state
                .observed_addresses
                .insert(address, ObservedInstruction::default());
        } else {
            state.observed_addresses_capped = true;
        }
    }

    fn cached_instruction_advance(&self, address: u64) -> Option<(u64, Option<u64>)> {
        let state = self.state.borrow();
        let state = state.as_ref()?;

        if !instruction_is_cacheable(state, address) {
            return None;
        }

        let instruction = state.observed_addresses.get(&address)?;

        instruction.checked.then_some((
            u64::from(instruction.safe_steps.max(1)),
            instruction.advance_target,
        ))
    }

    fn mark_instruction_checked(&self, address: u64, safe_steps: u16, advance_target: Option<u64>) {
        let mut state = self.state.borrow_mut();

        let Some(state) = state.as_mut() else {
            return;
        };

        if instruction_is_cacheable(state, address)
            && let Some(instruction) = state.observed_addresses.get_mut(&address)
        {
            instruction.checked = true;
            instruction.safe_steps = safe_steps.max(1);
            instruction.advance_target = advance_target;
        }
    }

    fn take_instruction_lookahead(&self, address: u64) -> Option<LookaheadInstruction> {
        let state = self.state.borrow();
        let lookahead = &state.as_ref()?.instruction_lookahead;

        lookahead
            .binary_search_by_key(&address, |instruction| instruction.address)
            .ok()
            .map(|index| lookahead[index])
    }

    fn restore_context(&self, control: GefContextControl, render_current_stop: bool) {
        let Some(command) = context_restore_command(control, render_current_stop) else {
            return;
        };

        let _ = self.client.request(&command, |_, _| {});
    }

    fn complete(self: &Rc<Self>, generation: u64) {
        if !self.is_current(generation) {
            return;
        }

        let Some(run) = self.state.borrow_mut().take() else {
            return;
        };

        self.next_generation();
        self.restore_context(run.context_control, true);

        if let Some(ui) = self.ui.upgrade() {
            ui.set_native_until_active(false);
        }

        finish_stopped_state(
            &self.ui,
            &self.client,
            Some(String::from("until-reached")),
            None,
            None,
            Some(format!(
                "Reached {} after executing {} instruction{}",
                action_description(&run.action),
                run.steps,
                if run.steps == 1 { "" } else { "s" }
            )),
        );
    }

    fn fail(self: &Rc<Self>, generation: u64, message: &str) {
        if !self.is_current(generation) {
            return;
        }

        let run = self.state.borrow_mut().take();
        self.next_generation();

        if let Some(run) = run {
            self.restore_context(run.context_control, true);
        }

        if let Some(ui) = self.ui.upgrade() {
            ui.set_native_until_active(false);
        }

        finish_stopped_state(
            &self.ui,
            &self.client,
            Some(String::from("until-error")),
            None,
            None,
            Some(String::from("Until operation stopped")),
        );

        if let Some(ui) = self.ui.upgrade() {
            ui.set_status("Until failed", message, Some("status-error"));
        }
    }

    fn is_current(&self, generation: u64) -> bool {
        self.generation.get() == generation
            && self.state.borrow().is_some()
            && self.ui.upgrade().is_some_and(|ui| ui.native_until_active())
    }

    fn recover_timed_out_request(&self, record: &MiRecord) -> bool {
        match record.class.as_str() {
            "timeout" => {
                self.client.quarantine(
                    "GDB stopped answering while Until was controlling execution. The target state can no longer be determined safely.",
                );

                true
            }
            "unavailable" => {
                self.abort();

                true
            }
            _ => false,
        }
    }

    fn next_generation(&self) -> u64 {
        let generation = self.generation.get().wrapping_add(1);
        self.generation.set(generation);

        generation
    }
}

fn context_suppression_command(control: GefContextControl) -> Option<String> {
    let python = match control {
        GefContextControl::None => return None,
        GefContextControl::ContextCommand => {
            "python globals()['_fgdb_context_hidden_before_until'] = ContextCommand.context_hidden; ContextCommand.hide_context()"
        }
        GefContextControl::OriginalGef => {
            "python globals()['_fgdb_context_hidden_before_until'] = gef.ui.context_hidden; hide_context()"
        }
    };

    Some(crate::debugger::console_command(python))
}

fn context_restore_command(
    control: GefContextControl,
    render_current_stop: bool,
) -> Option<String> {
    let render = if render_current_stop { "True" } else { "False" };

    let python = match control {
        GefContextControl::None => return None,
        GefContextControl::ContextCommand => format!(
            "python _fgdb_hidden = globals().pop('_fgdb_context_hidden_before_until', False); ContextCommand.context_hidden = _fgdb_hidden; gdb.execute('context') if {render} and not _fgdb_hidden else None; del _fgdb_hidden"
        ),
        GefContextControl::OriginalGef => format!(
            "python _fgdb_hidden = globals().pop('_fgdb_context_hidden_before_until', False); gef.ui.context_hidden = _fgdb_hidden; gdb.execute('context') if {render} and not _fgdb_hidden else None; del _fgdb_hidden"
        ),
    };

    Some(crate::debugger::console_command(&python))
}

fn is_internal_until_stop(
    reason: Option<&str>,
    address: Option<u64>,
    stopped_thread: Option<&str>,
    pending_steps: u64,
    pending_location: Option<u64>,
    expected_thread: Option<&str>,
) -> bool {
    if pending_steps == 0 {
        return false;
    }

    if expected_thread.is_some_and(|expected| stopped_thread != Some(expected)) {
        return false;
    }

    match pending_location {
        Some(location) => reason == Some("location-reached") && address == Some(location),
        None => reason == Some("end-stepping-range"),
    }
}

fn searches_instructions(action: &UntilAction) -> bool {
    matches!(
        action,
        UntilAction::NextCall
            | UntilAction::NextReturn
            | UntilAction::NextSyscall
            | UntilAction::NextIndirectBranch
            | UntilAction::NextControlFlow
            | UntilAction::MemoryAccess
    )
}

fn classify_instruction_window(
    action: &UntilAction,
    instructions: &[Instruction],
    current_index: usize,
    architecture: TargetArchitecture,
    address_space: Option<&crate::misc::ProcessAddressSpace>,
) -> InstructionWindow {
    let window = instructions
        .get(current_index..)
        .unwrap_or_default()
        .iter()
        .take(MAX_UNTIL_LOOKAHEAD_INSTRUCTIONS.saturating_add(1));

    let can_advance = matches!(
        architecture,
        TargetArchitecture::X86 | TargetArchitecture::X86_64
    );

    let classified = window
        .map(|instruction| {
            let address = parse_address(&instruction.address);

            let matched =
                crate::ui::formatting::instruction_matches_until(action, instruction, architecture);

            let ends_linear_flow =
                crate::ui::formatting::instruction_ends_linear_flow(instruction, architecture);

            let cacheable =
                address.is_some_and(|address| address_is_cacheable(address_space, address));

            (address, matched, ends_linear_flow, cacheable)
        })
        .collect::<Vec<_>>();

    let Some((_, current_matched, _, _)) = classified.first().copied() else {
        return InstructionWindow::default();
    };

    // Work backwards so every cached address knows the farthest instruction
    // that can be reached without crossing a match or a control-flow edge.
    // This makes branch targets inside the same disassembly window just as
    // cheap as its first instruction instead of reverting to one-step MI
    // round trips after a jump.
    let mut advances = vec![(1_u16, None); classified.len()];

    for index in (0..classified.len()).rev() {
        let (Some(address), matched, ends_linear_flow, cacheable) = classified[index] else {
            continue;
        };

        if !can_advance || matched || ends_linear_flow || !cacheable {
            continue;
        }

        let Some(&(Some(next_address), next_matched, next_ends_flow, next_cacheable)) =
            classified.get(index + 1)
        else {
            continue;
        };

        if !next_cacheable || next_address <= address {
            continue;
        }

        advances[index] = if !next_matched && !next_ends_flow {
            match advances[index + 1].1 {
                Some(target) => (advances[index + 1].0.saturating_add(1), Some(target)),
                None => (1, Some(next_address)),
            }
        } else {
            (1, Some(next_address))
        };
    }

    let (safe_steps, advance_target) = advances[0];
    let mut lookahead = Vec::with_capacity(classified.len().saturating_sub(1));

    for (index, &(address, matched, _, cacheable)) in classified.iter().enumerate().skip(1) {
        if let (Some(address), true) = (address, cacheable) {
            let (safe_steps, advance_target) = advances[index];

            lookahead.push(LookaheadInstruction {
                address,
                matched,
                safe_steps,
                advance_target,
            });
        }
    }

    lookahead.sort_unstable_by_key(|instruction| instruction.address);
    lookahead.dedup_by_key(|instruction| instruction.address);

    InstructionWindow {
        current_matched,
        safe_steps,
        advance_target,
        lookahead,
    }
}

fn disassembly_end(
    address: u64,
    bytes: u64,
    address_space: Option<&crate::misc::ProcessAddressSpace>,
) -> u64 {
    let requested_end = address.saturating_add(bytes.max(1));

    address_space
        .and_then(|space| mapping_at(&space.mappings, address))
        .map_or(requested_end, |mapping| requested_end.min(mapping.end))
}

fn instruction_is_cacheable(run: &UntilRun, address: u64) -> bool {
    address_is_cacheable(run.address_space.as_ref(), address)
}

fn native_advance_is_safe(
    address_space: Option<&crate::misc::ProcessAddressSpace>,
    current_address: Option<u64>,
    target_address: Option<u64>,
) -> bool {
    let (Some(address_space), Some(current_address), Some(target_address)) =
        (address_space, current_address, target_address)
    else {
        return false;
    };

    let Some(mapping) = mapping_at(&address_space.mappings, current_address) else {
        return false;
    };

    target_address > current_address
        && target_address < mapping.end
        && mapping.permissions.contains('x')
        && !mapping.permissions.contains('w')
        && !is_dynamic_linker_mapping(mapping, address_space)
}

fn address_is_cacheable(
    address_space: Option<&crate::misc::ProcessAddressSpace>,
    address: u64,
) -> bool {
    address_space
        .and_then(|space| mapping_at(&space.mappings, address))
        .is_some_and(|mapping| {
            mapping.permissions.contains('x') && !mapping.permissions.contains('w')
        })
}

fn progress_detail(run: &UntilRun, pending_steps: u64) -> String {
    let verb = if pending_steps > 1 {
        format!(
            "Executing up to {pending_steps} instructions from instruction {}",
            run.steps.saturating_add(1)
        )
    } else if pending_steps == 1 {
        format!("Executing instruction {}", run.steps.saturating_add(1))
    } else {
        format!("Executed {} instructions", run.steps)
    };

    let unique_suffix = if run.observed_addresses_capped {
        "+"
    } else {
        ""
    };

    let mut detail = format!(
        "{verb} · {}{unique_suffix} observed PCs",
        run.observed_addresses.len()
    );

    if let Some(address) = run.current_address {
        detail.push_str(&format!(" · PC 0x{address:x}"));
    }

    if run.repeated_steps >= 64 && run.repeated_steps.saturating_mul(4) >= run.steps {
        detail.push_str(" · repeating control-flow path");
    }

    detail.push_str(&format!(
        " · looking for {} · Pause cancels",
        action_description(&run.action)
    ));

    detail
}

fn parse_address(value: &str) -> Option<u64> {
    let value = value.trim();

    let digits = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))?
        .split(|character: char| !character.is_ascii_hexdigit())
        .next()?;

    (!digits.is_empty())
        .then(|| u64::from_str_radix(digits, 16).ok())
        .flatten()
}

fn mapping_at(
    mappings: &[crate::misc::ProcessMapping],
    address: u64,
) -> Option<&crate::misc::ProcessMapping> {
    let index = mappings
        .partition_point(|mapping| mapping.start <= address)
        .checked_sub(1)?;

    mappings.get(index).filter(|mapping| address < mapping.end)
}

fn mapping_identity(mapping: &crate::misc::ProcessMapping) -> MappingIdentity {
    MappingIdentity {
        start: mapping.start,
        end: mapping.end,
    }
}

fn mapping_contains(mapping: &MappingIdentity, address: u64) -> bool {
    mapping.start <= address && address < mapping.end
}

fn normalized_mapping_path(path: &str) -> &str {
    path.strip_suffix(" (deleted)").unwrap_or(path)
}

fn is_user_code_mapping(
    mapping: &crate::misc::ProcessMapping,
    address_space: &crate::misc::ProcessAddressSpace,
) -> bool {
    mapping.permissions.contains('x')
        && address_space
            .executable
            .as_deref()
            .is_some_and(|executable| {
                normalized_mapping_path(&mapping.path) == normalized_mapping_path(executable)
            })
}

fn is_dynamic_linker_mapping(
    mapping: &crate::misc::ProcessMapping,
    address_space: &crate::misc::ProcessAddressSpace,
) -> bool {
    if address_space
        .interpreter
        .as_deref()
        .is_some_and(|interpreter| {
            normalized_mapping_path(&mapping.path) == normalized_mapping_path(interpreter)
        })
    {
        return true;
    }

    let name = std::path::Path::new(normalized_mapping_path(&mapping.path))
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    name == "ld.so"
        || name.starts_with("ld.so.")
        || name.starts_with("ld-linux")
        || name.starts_with("ld-musl-")
        || name.starts_with("ld-uclibc")
        || (name.starts_with("ld-") && name.contains(".so"))
        || name == "linker"
        || name == "linker64"
}

fn is_libc_code_mapping(mapping: &crate::misc::ProcessMapping) -> bool {
    if !mapping.permissions.contains('x') {
        return false;
    }

    let name = std::path::Path::new(normalized_mapping_path(&mapping.path))
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    name == "libc.so"
        || name.starts_with("libc.so.")
        || name.starts_with("libc-")
        || name.starts_with("libc.musl-")
        || (name.starts_with("ld-musl-") && name.contains(".so"))
        || name.starts_with("libuclibc-")
}

fn action_description(action: &UntilAction) -> &'static str {
    match action {
        UntilAction::CurrentLine => "the current source line",
        UntilAction::FunctionReturns => "the current function return",
        UntilAction::NextCall => "the next call instruction",
        UntilAction::NextReturn => "the next return instruction",
        UntilAction::NextSyscall => "the next syscall instruction",
        UntilAction::NextIndirectBranch => "the next indirect branch",
        UntilAction::NextControlFlow => "the next call, jump, or return",
        UntilAction::MemoryAccess => "the next memory-accessing instruction",
        UntilAction::UserCode => "the next instruction in the main executable",
        UntilAction::LibcCode => "the next instruction in libc",
        UntilAction::RegionChange => "a different virtual-memory mapping",
        UntilAction::Expression(_) => "the expression to become true",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn instruction(address: &str, text: &str) -> Instruction {
        Instruction {
            address: address.to_owned(),
            function: String::from("test"),
            offset: String::from("0"),
            opcodes: None,
            text: text.to_owned(),
            source: None,
        }
    }

    #[test]
    fn distinguishes_internal_execution_from_real_debugger_stops() {
        assert!(is_internal_until_stop(
            Some("end-stepping-range"),
            Some(0x1001),
            Some("1"),
            1,
            None,
            Some("1"),
        ));

        assert!(is_internal_until_stop(
            Some("location-reached"),
            Some(0x1010),
            Some("1"),
            8,
            Some(0x1010),
            Some("1"),
        ));

        assert!(!is_internal_until_stop(
            Some("breakpoint-hit"),
            Some(0x1010),
            Some("1"),
            8,
            Some(0x1010),
            Some("1"),
        ));

        assert!(!is_internal_until_stop(
            Some("location-reached"),
            Some(0x1010),
            Some("2"),
            8,
            Some(0x1010),
            Some("1"),
        ));

        assert!(!is_internal_until_stop(
            Some("end-stepping-range"),
            Some(0x1001),
            Some("2"),
            1,
            None,
            Some("1"),
        ));

        assert!(!is_internal_until_stop(
            Some("location-reached"),
            Some(0x1011),
            Some("1"),
            8,
            Some(0x1010),
            Some("1"),
        ));

        assert!(!is_internal_until_stop(
            Some("end-stepping-range"),
            Some(0x1001),
            Some("1"),
            0,
            None,
            Some("1"),
        ));
    }

    #[test]
    fn validates_side_effect_free_until_expressions() {
        assert!(validate_until_expression("$rax == 0").is_ok());
        assert!(validate_until_expression("*(int*)$rbx != 4").is_ok());
        assert!(validate_until_expression("c == '='").is_ok());
        assert!(validate_until_expression("strcmp(s, \"=\") == 0").is_ok());
        assert!(validate_until_expression("foo(\"a=b\") == 1").is_ok());
        assert!(validate_until_expression("foo(\"a=\\\"b\") == 1").is_ok());
        assert!(validate_until_expression("$rax = 0").is_err());
        assert!(validate_until_expression("value += 1").is_err());
        assert!(validate_until_expression("value -= 1").is_err());
        assert!(validate_until_expression("value *= 2").is_err());
        assert!(validate_until_expression("value /= 2").is_err());
        assert!(validate_until_expression("value %= 2").is_err());
        assert!(validate_until_expression("value <<= 1").is_err());
        assert!(validate_until_expression("value >>= 1").is_err());
        assert!(validate_until_expression("value &= mask").is_err());
        assert!(validate_until_expression("value |= mask").is_err());
        assert!(validate_until_expression("value ^= mask").is_err());
        assert!(validate_until_expression("  ").is_err());
    }

    #[test]
    fn parses_gdb_boolean_and_integer_results() {
        assert_eq!(parse_condition_value("true"), Some(true));
        assert_eq!(parse_condition_value("false"), Some(false));
        assert_eq!(parse_condition_value("0x0"), Some(false));
        assert_eq!(parse_condition_value("0x10"), Some(true));
        assert_eq!(parse_condition_value("-1"), Some(true));
        assert_eq!(parse_condition_value("{value = 1}"), None);
    }

    #[test]
    fn allows_main_and_shared_library_code_but_rejects_the_loader() {
        let main = crate::misc::ProcessMapping {
            start: 0x400000,
            end: 0x401000,
            permissions: String::from("r-xp"),
            path: String::from("/tmp/target"),
        };

        let libc = crate::misc::ProcessMapping {
            start: 0x7f00_0000,
            end: 0x7f10_0000,
            permissions: String::from("r-xp"),
            path: String::from("/usr/lib/libc.so.6"),
        };

        let space = crate::misc::ProcessAddressSpace {
            executable: Some(String::from("/tmp/target")),
            interpreter: Some(String::from("/usr/lib/ld-linux-x86-64.so.2")),
            mappings: vec![main.clone(), libc.clone()],
            capped: false,
        };

        assert!(is_user_code_mapping(&main, &space));
        assert!(!is_user_code_mapping(&libc, &space));
        assert!(is_libc_code_mapping(&libc));
        assert!(!is_libc_code_mapping(&main));

        assert!(native_advance_is_safe(
            Some(&space),
            Some(0x400100),
            Some(0x400180),
        ));

        assert!(native_advance_is_safe(
            Some(&space),
            Some(0x7f00_0100),
            Some(0x7f00_0180),
        ));

        assert!(!native_advance_is_safe(
            Some(&space),
            Some(0x400100),
            Some(0x7f00_0180),
        ));

        let loader = crate::misc::ProcessMapping {
            start: 0x7f20_0000,
            end: 0x7f30_0000,
            permissions: String::from("r-xp"),
            path: String::from("/usr/lib/ld-linux-x86-64.so.2"),
        };

        assert!(is_dynamic_linker_mapping(&loader, &space));

        assert!(!native_advance_is_safe(
            Some(&space),
            Some(0x7f20_0100),
            Some(0x7f20_0180),
        ));

        let unknown_interpreter_space = crate::misc::ProcessAddressSpace {
            interpreter: None,
            ..space.clone()
        };

        assert!(is_dynamic_linker_mapping(
            &loader,
            &unknown_interpreter_space
        ));

        for path in [
            "/lib/libc.so.0",
            "/lib/ld-musl-x86_64.so.1",
            "/lib/libuClibc-1.0.so",
        ] {
            let mapping = crate::misc::ProcessMapping {
                start: 0x7f20_0000,
                end: 0x7f30_0000,
                permissions: String::from("r-xp"),
                path: path.to_owned(),
            };

            assert!(is_libc_code_mapping(&mapping), "{path}");
        }
    }

    #[test]
    fn reuses_bounded_disassembly_lookahead_for_read_only_code() {
        let address_space = crate::misc::ProcessAddressSpace {
            executable: Some(String::from("/tmp/target")),
            interpreter: None,
            mappings: vec![crate::misc::ProcessMapping {
                start: 0x1000,
                end: 0x2000,
                permissions: String::from("r-xp"),
                path: String::from("/tmp/target"),
            }],
            capped: false,
        };

        let instructions = [
            instruction("0x1000", "mov eax, ebx"),
            instruction("0x1002", "nop"),
            instruction("0x1003", "call 0x1800"),
        ];

        let window = classify_instruction_window(
            &UntilAction::NextCall,
            &instructions,
            0,
            TargetArchitecture::X86_64,
            Some(&address_space),
        );

        assert!(!window.current_matched);
        assert_eq!(window.safe_steps, 2);
        assert_eq!(window.advance_target, Some(0x1003));

        assert_eq!(
            window.lookahead,
            vec![
                LookaheadInstruction {
                    address: 0x1002,
                    matched: false,
                    safe_steps: 1,
                    advance_target: Some(0x1003),
                },
                LookaheadInstruction {
                    address: 0x1003,
                    matched: true,
                    safe_steps: 1,
                    advance_target: None,
                },
            ]
        );

        let mut writable = address_space;
        writable.mappings[0].permissions = String::from("rwxp");

        let window = classify_instruction_window(
            &UntilAction::NextCall,
            &instructions,
            0,
            TargetArchitecture::X86_64,
            Some(&writable),
        );

        assert_eq!(window.safe_steps, 1);
        assert_eq!(window.advance_target, None);
        assert!(window.lookahead.is_empty());
    }

    #[test]
    fn counted_steps_stop_before_nonmatching_control_flow() {
        let address_space = crate::misc::ProcessAddressSpace {
            executable: Some(String::from("/tmp/target")),
            interpreter: None,
            mappings: vec![crate::misc::ProcessMapping {
                start: 0x1000,
                end: 0x2000,
                permissions: String::from("r-xp"),
                path: String::from("/tmp/target"),
            }],
            capped: false,
        };

        let instructions = [
            instruction("0x1000", "mov eax, ebx"),
            instruction("0x1002", "nop"),
            instruction("0x1003", "jne 0x1100"),
            instruction("0x1005", "nop"),
            instruction("0x1006", "call 0x1800"),
        ];

        let window = classify_instruction_window(
            &UntilAction::NextCall,
            &instructions,
            0,
            TargetArchitecture::X86_64,
            Some(&address_space),
        );

        assert_eq!(window.safe_steps, 2);

        assert_eq!(
            window
                .lookahead
                .binary_search_by_key(&0x1006, |instruction| instruction.address)
                .ok()
                .map(|index| window.lookahead[index].matched),
            Some(true)
        );

        let prefixed_return = instruction("0x1100", "repz retq");

        assert!(crate::ui::formatting::instruction_matches_until(
            &UntilAction::NextReturn,
            &prefixed_return,
            TargetArchitecture::X86_64,
        ));

        assert!(crate::ui::formatting::instruction_ends_linear_flow(
            &instruction("0x1101", "ds ljmp $0x8,$0x1200"),
            TargetArchitecture::X86_64,
        ));
    }

    #[test]
    fn finds_mapping_boundaries_with_sorted_lookup() {
        let mappings = [
            crate::misc::ProcessMapping {
                start: 0x1000,
                end: 0x1800,
                permissions: String::from("r-xp"),
                path: String::new(),
            },
            crate::misc::ProcessMapping {
                start: 0x2000,
                end: 0x2800,
                permissions: String::from("rw-p"),
                path: String::new(),
            },
        ];

        assert_eq!(
            mapping_at(&mappings, 0x1000).map(|map| map.start),
            Some(0x1000)
        );

        assert_eq!(
            mapping_at(&mappings, 0x17ff).map(|map| map.start),
            Some(0x1000)
        );

        assert!(mapping_at(&mappings, 0x1800).is_none());
        assert!(mapping_at(&mappings, 0x1fff).is_none());

        assert_eq!(
            mapping_at(&mappings, 0x2000).map(|map| map.start),
            Some(0x2000)
        );

        assert!(mapping_at(&mappings, 0x2800).is_none());

        let address_space = crate::misc::ProcessAddressSpace {
            executable: None,
            interpreter: None,
            mappings: mappings.to_vec(),
            capped: false,
        };

        assert_eq!(disassembly_end(0x17f0, 128, Some(&address_space)), 0x1800);
    }
}
