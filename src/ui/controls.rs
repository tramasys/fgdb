use super::*;

pub(super) fn breakpoint_command_numbers(
    breakpoints: &[Breakpoint],
    watchpoints: bool,
) -> Vec<String> {
    let mut numbers = Vec::new();
    let mut seen = HashSet::new();
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
        if seen.insert(number) {
            numbers.push(number.to_owned());
        }
    }
    numbers
}

pub(super) fn signal_catchpoint_command_numbers(breakpoints: &[Breakpoint]) -> Vec<String> {
    let mut numbers = Vec::new();
    let mut seen = HashSet::new();
    for breakpoint in breakpoints
        .iter()
        .filter(|breakpoint| breakpoint.is_signal_catchpoint())
    {
        let number = breakpoint.command_number();
        if seen.insert(number) {
            numbers.push(number.to_owned());
        }
    }
    numbers
}

pub(super) fn event_catchpoint_command_numbers(breakpoints: &[Breakpoint]) -> Vec<String> {
    let mut numbers = Vec::new();
    let mut seen = HashSet::new();
    for breakpoint in breakpoints.iter().filter(|breakpoint| {
        EventCatchpoint::ALL
            .iter()
            .any(|(event, _, _)| event.matches(breakpoint))
    }) {
        let number = breakpoint.command_number();
        if seen.insert(number) {
            numbers.push(number.to_owned());
        }
    }
    numbers
}

pub(super) fn event_catchpoint_command_number(
    breakpoints: &[Breakpoint],
    event: EventCatchpoint,
) -> Option<String> {
    breakpoints
        .iter()
        .find(|breakpoint| event.matches(breakpoint))
        .map(|breakpoint| breakpoint.command_number().to_owned())
}

pub(super) fn breakpoint_command_number_at_address(
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

pub(super) fn normalized_signal_name(signal: &str) -> Option<String> {
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

pub(super) fn signal_catchpoint_command_number(
    breakpoints: &[Breakpoint],
    signal: &str,
) -> Option<String> {
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

pub(super) fn set_breakpoint_enabled(
    breakpoints: &mut [Breakpoint],
    number: &str,
    enabled: bool,
) -> bool {
    let mut changed = false;
    let location_only = number.contains('.');
    for breakpoint in breakpoints {
        let matches = if location_only {
            breakpoint.number == number
        } else {
            breakpoint.command_number() == number
        };
        if matches && breakpoint.enabled != enabled {
            breakpoint.enabled = enabled;
            changed = true;
        }
    }
    changed
}

pub(super) fn remove_marks(buffer: &sourceview5::Buffer, category: &str) {
    let start = buffer.start_iter();
    let end = buffer.end_iter();
    buffer.remove_source_marks(&start, &end, Some(category));
}

pub(super) fn addresses_equal(left: &str, right: &str) -> bool {
    fn normalized(address: &str) -> Option<&str> {
        let address = address.trim();
        let digits = address
            .strip_prefix("0x")
            .or_else(|| address.strip_prefix("0X"))
            .unwrap_or(address);
        if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return None;
        }
        let digits = digits.trim_start_matches('0');
        Some(if digits.is_empty() { "0" } else { digits })
    }
    normalized(left)
        .zip(normalized(right))
        .is_some_and(|(left, right)| left.eq_ignore_ascii_case(right))
}

pub(super) fn connect_execution_button(
    button: &gtk::Button,
    ui: &Rc<Ui>,
    client: &Rc<MiClient>,
    command: &'static str,
    detail: &'static str,
) {
    let weak_ui = Rc::downgrade(ui);
    let client = Rc::clone(client);
    button.connect_clicked(move |_| {
        if let Some(ui) = weak_ui.upgrade()
            && ui.movement_commands_available()
        {
            issue_execution_command(&ui, &client, command, detail);
        }
    });
}

pub(crate) fn issue_execution_command(
    ui: &Rc<Ui>,
    client: &MiClient,
    command: &str,
    detail: &str,
) -> bool {
    let interrupt = execution_interrupt(command);
    let previous_active_thread = ui.active_thread_execution();
    let targeted_thread = execution_thread(command).map(str::to_owned);
    let pending_group = execution_thread_group(command)
        .map(str::to_owned)
        .or_else(|| {
            execution_targets_selected_group(command)
                .then(|| ui.selected_inferior_id())
                .flatten()
        });
    ui.set_pending_execution_inferior(pending_group);
    if interrupt {
        if previous_active_thread.is_none() {
            ui.set_active_thread_execution(targeted_thread);
        }
    } else {
        ui.set_thread_execution_exit_candidate(None);
        ui.set_active_thread_execution(targeted_thread.or_else(|| {
            selected_thread_execution(command)
                .then(|| ui.current_thread_id())
                .flatten()
        }));
    }
    match client.send(command) {
        Ok(_) => {
            ui.set_command_pending(true);
            let generation = ui.begin_execution_transition();
            let weak_ui = Rc::downgrade(ui);
            let weak_client = client.weak();
            gtk::glib::timeout_add_local_once(Duration::from_secs(15), move || {
                let Some(ui) = weak_ui.upgrade() else {
                    return;
                };
                if ui.execution_transition_is_pending(generation) {
                    let message = "GDB accepted an execution command but did not report a running or stopped transition within 15 seconds. Restart GDB from the Session menu.";
                    if let Some(client) = weak_client.upgrade() {
                        client.quarantine(message);
                    } else {
                        ui.require_gdb_recovery("GDB recovery required", message);
                    }
                }
            });
            ui.set_execution_status("Executing", detail);
            true
        }
        Err(error) => {
            ui.set_pending_execution_inferior(None);
            ui.set_active_thread_execution(previous_active_thread);
            ui.set_status("Command failed", &error.to_string(), Some("status-error"));
            false
        }
    }
}

fn execution_resumes(command: &str) -> bool {
    matches!(
        command.split_whitespace().next(),
        Some(
            "-exec-run"
                | "-exec-continue"
                | "-exec-next"
                | "-exec-step"
                | "-exec-next-instruction"
                | "-exec-step-instruction"
                | "-exec-finish"
                | "-exec-until"
        )
    )
}

fn execution_interrupt(command: &str) -> bool {
    command.split_whitespace().next() == Some("-exec-interrupt")
}

fn execution_targets_selected_group(command: &str) -> bool {
    let explicitly_scoped = command
        .split_whitespace()
        .any(|word| matches!(word, "--thread" | "--thread-group" | "--all"));
    (execution_resumes(command) || execution_interrupt(command))
        && !selected_thread_execution(command)
        && !explicitly_scoped
}

fn execution_thread(command: &str) -> Option<&str> {
    let mut words = command.split_whitespace();
    while let Some(word) = words.next() {
        if word == "--thread" {
            return words.next();
        }
    }
    None
}

pub(super) fn execution_event_matches_thread(
    active: Option<&str>,
    reported: Option<&str>,
    all_stopped: bool,
) -> bool {
    all_stopped || active.is_none() || matches!(reported, None | Some("all")) || active == reported
}

fn selected_thread_execution(command: &str) -> bool {
    matches!(
        command.split_whitespace().next(),
        Some(
            "-exec-next"
                | "-exec-step"
                | "-exec-next-instruction"
                | "-exec-step-instruction"
                | "-exec-finish"
                | "-exec-until"
        )
    )
}

fn execution_thread_group(command: &str) -> Option<&str> {
    let mut arguments = command.split_whitespace();
    while let Some(argument) = arguments.next() {
        if argument == "--thread-group" {
            return arguments.next();
        }
    }
    None
}

pub(super) fn request_signal_catchpoint_toggle(ui: &Ui, signal: &str) {
    if !ui.stop_point_commands_available() {
        return;
    }
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

pub(super) fn set_status_widgets(
    status: &gtk::Label,
    detail_label: &gtk::Label,
    text: &str,
    detail: &str,
    class: Option<&str>,
) {
    for status_class in ["status-ready", "status-running", "status-error"] {
        if Some(status_class) != class && status.has_css_class(status_class) {
            status.remove_css_class(status_class);
        }
    }
    if let Some(class) = class
        && !status.has_css_class(class)
    {
        status.add_css_class(class);
    }
    if status.text().as_str() != text {
        status.set_text(text);
    }
    if detail_label.text().as_str() != detail {
        detail_label.set_text(detail);
    }
    if detail_label.tooltip_text().as_deref() != Some(detail) {
        detail_label.set_tooltip_text(Some(detail));
    }
}

#[cfg(test)]
mod tests {
    use super::{
        addresses_equal, execution_event_matches_thread, execution_interrupt, execution_resumes,
        execution_targets_selected_group, execution_thread, execution_thread_group,
        selected_thread_execution,
    };

    #[test]
    fn compares_only_valid_normalized_addresses() {
        assert!(addresses_equal("0x0000AB", "ab"));
        assert!(addresses_equal("0", "0x000"));
        assert!(!addresses_equal("", "0"));
        assert!(!addresses_equal("0x", "0"));
        assert!(!addresses_equal("not-an-address", "not-an-address"));
    }

    #[test]
    fn identifies_a_targeted_execution_group() {
        assert_eq!(
            execution_thread_group("-exec-continue --thread-group i2"),
            Some("i2")
        );
        assert_eq!(execution_thread_group("-exec-next"), None);
        assert_eq!(
            execution_thread_group("-exec-interrupt --thread-group"),
            None
        );
    }

    #[test]
    fn identifies_execution_that_can_be_orphaned_by_the_selected_thread() {
        for command in [
            "-exec-next",
            "-exec-step --thread 2",
            "-exec-next-instruction",
            "-exec-step-instruction",
            "-exec-finish",
            "-exec-until main.c:42",
        ] {
            assert!(selected_thread_execution(command), "{command}");
        }
        assert!(!selected_thread_execution("-exec-continue"));
        assert!(!selected_thread_execution("-exec-interrupt --all"));
    }

    #[test]
    fn distinguishes_resume_and_interrupt_transitions() {
        for command in [
            "-exec-run",
            "-exec-continue --thread-group i2",
            "-exec-step --thread 4",
            "-exec-until *0x401000",
        ] {
            assert!(execution_resumes(command), "{command}");
            assert!(!execution_interrupt(command), "{command}");
        }
        assert!(execution_interrupt("-exec-interrupt --thread 4"));
        assert!(!execution_resumes("-exec-interrupt --thread 4"));
        assert_eq!(execution_thread("-exec-continue --thread 4"), Some("4"));
        assert_eq!(execution_thread("-exec-interrupt --thread 12"), Some("12"));
        assert_eq!(execution_thread("-exec-continue --thread-group i2"), None);
        assert_eq!(execution_thread("-exec-step --thread"), None);
    }

    #[test]
    fn distinguishes_thread_group_and_global_execution_targets() {
        for command in ["-exec-run", "-exec-continue", "-exec-interrupt"] {
            assert!(execution_targets_selected_group(command), "{command}");
        }
        for command in [
            "-exec-next",
            "-exec-finish --thread 4",
            "-exec-continue --thread 4",
            "-exec-continue --thread-group i2",
            "-exec-continue --all",
            "-exec-interrupt --all",
        ] {
            assert!(!execution_targets_selected_group(command), "{command}");
        }
    }

    #[test]
    fn correlates_targeted_thread_transitions_without_accepting_unrelated_events() {
        assert!(execution_event_matches_thread(Some("4"), Some("4"), false));
        assert!(execution_event_matches_thread(
            Some("4"),
            Some("all"),
            false
        ));
        assert!(execution_event_matches_thread(Some("4"), Some("2"), true));
        assert!(!execution_event_matches_thread(Some("4"), Some("2"), false));
        assert!(execution_event_matches_thread(None, Some("2"), false));
    }
}
