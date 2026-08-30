use std::cell::Cell;
use std::collections::HashMap;
use std::time::Duration;

use super::*;
use crate::ui::controls::issue_execution_command;

const MAX_UNTIL_EXPRESSION_BYTES: usize = 4096;
const DISASSEMBLY_LOOKAHEAD_BYTES: u64 = 32;
const MAX_TRACKED_UNTIL_PCS: usize = 8192;

#[derive(Clone, Debug, PartialEq, Eq)]
struct MappingIdentity {
    start: u64,
    end: u64,
}

#[derive(Clone, Debug)]
struct UntilRun {
    action: UntilAction,
    steps: u64,
    current_address: Option<u64>,
    /// Addresses observed on the live execution path. The value records whether
    /// the instruction was already disassembled and found not to match.
    observed_addresses: HashMap<u64, bool>,
    observed_addresses_capped: bool,
    repeated_steps: u64,
    address_space: Option<crate::misc::ProcessAddressSpace>,
    initial_mapping: Option<MappingIdentity>,
    context_control: GefContextControl,
    cancel_requested: bool,
    step_in_flight: bool,
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
        self.state.replace(Some(UntilRun {
            action: action.clone(),
            steps: 0,
            current_address: None,
            observed_addresses: HashMap::new(),
            observed_addresses_capped: false,
            repeated_steps: 0,
            address_space: None,
            initial_mapping: None,
            context_control: GefContextControl::None,
            cancel_requested: false,
            step_in_flight: false,
        }));
        ui.set_native_until_active(true);
        ui.set_debug_state_stale(true);
        ui.start_stop_refresh();
        ui.start_thread_refresh();
        ui.invalidate_kernel_refresh();
        ui.invalidate_misc_refresh();
        ui.set_status(
            "Running until",
            &format!(
                "Following live execution for {}…",
                action_description(&action)
            ),
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
            .is_some_and(|run| run.step_in_flight);
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
            issue_execution_command(
                &ui,
                &self.client,
                "-exec-interrupt --all",
                "Cancelling the active Until operation…",
            );
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

    pub(super) fn on_stopped(self: &Rc<Self>, reason: Option<&str>, address: Option<&str>) -> bool {
        if self.state.borrow().is_none() {
            return false;
        }
        if let Some(run) = self.state.borrow_mut().as_mut() {
            run.step_in_flight = false;
        }
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
        if !is_internal_step_stop(reason) {
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
            state.steps = state.steps.saturating_add(1);
            if state.steps.is_multiple_of(64)
                && let Some(ui) = self.ui.upgrade()
            {
                ui.set_status(
                    "Running until",
                    &progress_detail(state, false),
                    Some("status-running"),
                );
            }
        }
        if let Some(address) = address.and_then(parse_address) {
            self.observe_address(address);
            self.inspect_stop(address, generation);
        } else {
            self.request_pc(generation, false);
        }
        true
    }

    fn prepare_address_space(self: &Rc<Self>, generation: u64, required: bool) {
        let controller = Rc::clone(self);
        if let Err(error) = self
            .client
            .request("-list-thread-groups", move |_, record| {
                if !controller.is_current(generation) {
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
                let address_space = match crate::misc::read_process_address_space(pid, debugger_pid)
                {
                    Ok(address_space) => address_space,
                    Err(error) => {
                        controller.address_space_unavailable(generation, required, &error);
                        return;
                    }
                };
                let action = controller
                    .state
                    .borrow()
                    .as_ref()
                    .map(|state| state.action.clone());
                let Some(action) = action else {
                    return;
                };
                if address_space.capped {
                    controller.address_space_unavailable(
                        generation,
                        required,
                        "The process has more mappings than fgdb can safely scan",
                    );
                    return;
                }
                let target_available = match action {
                    UntilAction::UserCode => address_space
                        .mappings
                        .iter()
                        .any(|mapping| is_user_code_mapping(mapping, &address_space)),
                    UntilAction::LibcCode => {
                        address_space.mappings.iter().any(is_libc_code_mapping)
                    }
                    UntilAction::RegionChange => true,
                    _ => true,
                };
                if !target_available {
                    controller.fail(
                        generation,
                        match action {
                            UntilAction::UserCode => {
                                "No executable mapping for the main program is available"
                            }
                            UntilAction::LibcCode => "No executable libc mapping is loaded",
                            _ => "The requested mapping class is unavailable",
                        },
                    );
                    return;
                }
                if let Some(state) = controller.state.borrow_mut().as_mut() {
                    state.address_space = Some(address_space);
                }
                if action == UntilAction::RegionChange {
                    controller.request_pc(generation, true);
                } else {
                    controller.begin_stepping(generation);
                }
            })
        {
            self.address_space_unavailable(generation, required, &error.to_string());
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
            let (detail, cancel_requested) = {
                let state = controller.state.borrow();
                let Some(state) = state.as_ref() else {
                    return gtk::glib::ControlFlow::Break;
                };
                (
                    progress_detail(state, state.step_in_flight),
                    state.cancel_requested,
                )
            };
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
        let action = self
            .state
            .borrow()
            .as_ref()
            .map(|state| state.action.clone());
        let Some(action) = action else {
            return;
        };
        match action {
            UntilAction::Expression(expression) => {
                self.evaluate_condition(expression, generation);
            }
            UntilAction::UserCode | UntilAction::LibcCode | UntilAction::RegionChange => {
                let matched = {
                    let state = self.state.borrow();
                    let Some(state) = state.as_ref() else {
                        return;
                    };
                    match action {
                        UntilAction::UserCode => {
                            state.address_space.as_ref().is_some_and(|space| {
                                mapping_at(&space.mappings, address)
                                    .is_some_and(|mapping| is_user_code_mapping(mapping, space))
                            })
                        }
                        UntilAction::LibcCode => {
                            state.address_space.as_ref().is_some_and(|space| {
                                mapping_at(&space.mappings, address)
                                    .is_some_and(is_libc_code_mapping)
                            })
                        }
                        UntilAction::RegionChange => state
                            .initial_mapping
                            .as_ref()
                            .is_some_and(|mapping| !mapping_contains(mapping, address)),
                        _ => false,
                    }
                };
                if matched {
                    self.complete(generation);
                } else {
                    self.issue_step(generation);
                }
            }
            _ if self.instruction_was_checked(address) => self.issue_step(generation),
            _ => self.request_instruction(address, generation, DISASSEMBLY_LOOKAHEAD_BYTES),
        }
    }

    fn request_instruction(self: &Rc<Self>, address: u64, generation: u64, bytes: u64) {
        let end = address.saturating_add(bytes.max(1));
        let command = format!("-data-disassemble -s 0x{address:x} -e 0x{end:x} -- 0");
        let controller = Rc::clone(self);
        if let Err(error) = self.client.request(&command, move |_, record| {
            if !controller.is_current(generation) {
                return;
            }
            let instructions = if record.is_done() {
                crate::debugger::instructions(&record)
            } else {
                Vec::new()
            };
            let instruction = instructions
                .iter()
                .find(|instruction| parse_address(&instruction.address) == Some(address))
                .or_else(|| instructions.first());
            let Some(instruction) = instruction else {
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
            };
            let (action, architecture) = match controller.state.borrow().as_ref() {
                Some(state) => (
                    state.action.clone(),
                    controller
                        .ui
                        .upgrade()
                        .map_or(TargetArchitecture::Unknown, |ui| ui.target_architecture()),
                ),
                None => return,
            };
            if crate::ui::formatting::instruction_matches_until(&action, instruction, architecture)
            {
                controller.complete(generation);
            } else {
                controller.mark_instruction_checked(address);
                controller.issue_step(generation);
            }
        }) {
            self.fail(generation, &error.to_string());
        }
    }

    fn evaluate_condition(self: &Rc<Self>, expression: String, generation: u64) {
        let command = format!(
            "-data-evaluate-expression {}",
            crate::debugger::quote(&expression)
        );
        let controller = Rc::clone(self);
        if let Err(error) = self.client.request(&command, move |_, record| {
            if !controller.is_current(generation) {
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
        }) {
            self.fail(generation, &error.to_string());
        }
    }

    fn issue_step(self: &Rc<Self>, generation: u64) {
        if !self.is_current(generation) {
            return;
        }
        if let Some(run) = self.state.borrow_mut().as_mut() {
            run.step_in_flight = true;
        }
        let controller = Rc::clone(self);
        if let Err(error) = self
            .client
            .request("-exec-step-instruction", move |_, record| {
                if controller.is_current(generation) && !record.is_success() {
                    controller.fail(
                        generation,
                        record
                            .error_message()
                            .unwrap_or("GDB rejected instruction stepping"),
                    );
                }
            })
        {
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
            state.observed_addresses.insert(address, false);
        } else {
            state.observed_addresses_capped = true;
        }
    }

    fn instruction_was_checked(&self, address: u64) -> bool {
        self.state.borrow().as_ref().is_some_and(|state| {
            instruction_is_cacheable(state, address)
                && state.observed_addresses.get(&address) == Some(&true)
        })
    }

    fn mark_instruction_checked(&self, address: u64) {
        let mut state = self.state.borrow_mut();
        let Some(state) = state.as_mut() else {
            return;
        };
        if instruction_is_cacheable(state, address)
            && let Some(checked) = state.observed_addresses.get_mut(&address)
        {
            *checked = true;
        }
    }

    fn restore_context(&self, control: GefContextControl, render_current_stop: bool) {
        let Some(command) = context_restore_command(control, render_current_stop) else {
            return;
        };
        let _ = self.client.send(&command);
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
    Some(format!(
        "-interpreter-exec console {}",
        crate::debugger::quote(python)
    ))
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
    Some(format!(
        "-interpreter-exec console {}",
        crate::debugger::quote(&python)
    ))
}

fn is_internal_step_stop(reason: Option<&str>) -> bool {
    reason == Some("end-stepping-range")
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

fn instruction_is_cacheable(run: &UntilRun, address: u64) -> bool {
    run.address_space
        .as_ref()
        .and_then(|space| mapping_at(&space.mappings, address))
        .is_some_and(|mapping| {
            mapping.permissions.contains('x') && !mapping.permissions.contains('w')
        })
}

fn progress_detail(run: &UntilRun, instruction_in_flight: bool) -> String {
    let verb = if instruction_in_flight {
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
        "{verb} · {}{unique_suffix} unique PCs",
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

fn validate_until_expression(expression: &str) -> Result<(), &'static str> {
    let expression = expression.trim();
    if expression.is_empty() {
        return Err("Enter a GDB expression to evaluate after each instruction.");
    }
    if expression.len() > MAX_UNTIL_EXPRESSION_BYTES {
        return Err("The Until expression is too large.");
    }
    if contains_assignment(expression) {
        return Err(
            "Assignments are not allowed in an Until expression. Use == to compare values.",
        );
    }
    Ok(())
}

fn contains_assignment(expression: &str) -> bool {
    let bytes = expression.as_bytes();
    bytes.iter().enumerate().any(|(index, byte)| {
        if *byte != b'=' {
            return false;
        }
        let previous = index.checked_sub(1).and_then(|index| bytes.get(index));
        let next = bytes.get(index + 1);
        !matches!(previous, Some(b'=' | b'!' | b'<' | b'>')) && next != Some(&b'=')
    })
}

fn parse_condition_value(value: &str) -> Option<bool> {
    let value = value.trim();
    if value.eq_ignore_ascii_case("true") {
        return Some(true);
    }
    if value.eq_ignore_ascii_case("false") {
        return Some(false);
    }
    let value = value.split_whitespace().next()?.trim_matches(['(', ')']);
    let (negative, digits) = value
        .strip_prefix('-')
        .map_or((false, value), |digits| (true, digits));
    let number = digits
        .strip_prefix("0x")
        .or_else(|| digits.strip_prefix("0X"))
        .map_or_else(
            || digits.parse::<u128>().ok(),
            |digits| u128::from_str_radix(digits, 16).ok(),
        )?;
    Some(negative || number != 0)
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
    mappings
        .iter()
        .find(|mapping| mapping.start <= address && address < mapping.end)
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

    #[test]
    fn distinguishes_internal_steps_from_real_debugger_stops() {
        assert!(is_internal_step_stop(Some("end-stepping-range")));
        assert!(!is_internal_step_stop(None));
        assert!(!is_internal_step_stop(Some("breakpoint-hit")));
        assert!(!is_internal_step_stop(Some("signal-received")));
        assert!(!is_internal_step_stop(Some("watchpoint-trigger")));
    }

    #[test]
    fn validates_side_effect_free_until_expressions() {
        assert!(validate_until_expression("$rax == 0").is_ok());
        assert!(validate_until_expression("*(int*)$rbx != 4").is_ok());
        assert!(validate_until_expression("$rax = 0").is_err());
        assert!(validate_until_expression("value += 1").is_err());
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
    fn classifies_main_and_libc_executable_mappings() {
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
            mappings: vec![main.clone(), libc.clone()],
            capped: false,
        };
        assert!(is_user_code_mapping(&main, &space));
        assert!(!is_user_code_mapping(&libc, &space));
        assert!(is_libc_code_mapping(&libc));
        assert!(!is_libc_code_mapping(&main));

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
}
