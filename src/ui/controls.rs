use super::*;
#[cfg(test)]
pub(super) use crate::model::execution_event_matches_thread;

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
        (breakpoint.is_catchpoint() && !breakpoint.is_signal_catchpoint())
            || EventCatchpoint::RustPanic.matches(breakpoint)
    }) {
        let number = breakpoint.command_number();

        if seen.insert(number) {
            numbers.push(number.to_owned());
        }
    }

    numbers
}

pub(super) fn stop_point_search_text(breakpoint: &Breakpoint) -> String {
    format!(
        "{} {} {} {} {} {} {} {} {}",
        breakpoint.number,
        breakpoint.kind,
        breakpoint.original_location.as_deref().unwrap_or_default(),
        breakpoint.function.as_deref().unwrap_or_default(),
        breakpoint.source_path().unwrap_or_default(),
        breakpoint.condition.as_deref().unwrap_or_default(),
        breakpoint.catch_type.as_deref().unwrap_or_default(),
        breakpoint.commands.join(" "),
        if breakpoint.enabled {
            "enabled"
        } else {
            "disabled"
        },
    )
    .to_ascii_lowercase()
}

pub(super) fn stop_point_matches(
    breakpoint: &Breakpoint,
    metadata: Option<&StopPointMetadata>,
    terms: &[&str],
    kind: u32,
) -> bool {
    let kind_matches = match kind {
        1 => !breakpoint.is_watchpoint() && !breakpoint.is_catchpoint(),
        2 => breakpoint.is_hardware_breakpoint(),
        3 => breakpoint.is_watchpoint(),
        4 => breakpoint.is_catchpoint(),
        5 => !breakpoint.enabled,
        _ => true,
    };

    if !kind_matches || terms.is_empty() {
        return kind_matches;
    }

    let searchable = stop_point_search_text(breakpoint);
    let metadata = metadata
        .map(stop_point_metadata_text)
        .unwrap_or_default()
        .to_ascii_lowercase();
    terms
        .iter()
        .all(|term| searchable.contains(term) || metadata.contains(term))
}

pub(super) fn normalized_stop_point_metadata(group: &str, tags: &str) -> StopPointMetadata {
    let group = group.trim();
    let group = (!group.is_empty()).then(|| group.to_owned());
    let mut normalized_tags = Vec::new();

    for tag in tags.split(',').map(str::trim).filter(|tag| !tag.is_empty()) {
        if !normalized_tags
            .iter()
            .any(|existing: &String| existing.eq_ignore_ascii_case(tag))
        {
            normalized_tags.push(tag.to_owned());
        }
    }

    StopPointMetadata {
        group,
        tags: normalized_tags,
    }
}

pub(super) fn stop_point_metadata_text(metadata: &StopPointMetadata) -> String {
    let mut parts = Vec::new();

    if let Some(group) = metadata.group.as_deref() {
        parts.push(format!("GROUP {group}"));
    }

    parts.extend(metadata.tags.iter().map(|tag| format!("#{tag}")));

    parts.join("  ")
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

pub(super) fn request_signal_catchpoint_toggle(ui: &Ui, signal: &str) {
    if !ui.model.stop_point_commands_available() {
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
    let handler = ui.signal_catchpoint_handler.borrow().clone();

    if let Some(handler) = handler {
        handler(signal, existing);
    } else {
        ui.set_status(
            "Catchpoint unavailable",
            "The debugger connection is not ready.",
            Some("status-error"),
        );
    }
}

pub(super) fn execution_status(
    model: &crate::model::DebuggerModel,
) -> (&'static str, &'static str) {
    let execution = model.execution();

    if model.gdb_recovery_required() || (!execution.ready && model.debugger_pid().is_none()) {
        ("Disconnected", "status-error")
    } else if !execution.ready {
        ("Starting GDB", "status-running")
    } else if execution.state.inferior_running() {
        ("Running", "status-running")
    } else if model.terminal_pending() {
        ("GDB busy", "status-running")
    } else if execution.state.resynchronizing() {
        ("Synchronizing", "status-running")
    } else if execution.command_pending || execution.session_pending {
        ("Busy", "status-running")
    } else if !execution.state.inferior_started() {
        ("Ready", "status-ready")
    } else {
        ("Stopped", "status-ready")
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
    use super::{addresses_equal, execution_event_matches_thread};

    #[test]
    fn terminal_work_gates_mutations_but_keeps_interrupt_and_authoritative_status() {
        use crate::model::{DebuggerModel, DebuggerStateDelta, TargetConnection};

        let model = DebuggerModel::new(None);
        assert_eq!(super::execution_status(&model).0, "Disconnected");
        model.set_debugger_pid(Some(1));
        assert_eq!(super::execution_status(&model).0, "Starting GDB");
        model.set_controls_ready(true);
        model.set_debug_state_stale(false);
        assert_eq!(super::execution_status(&model).0, "Ready");
        model.apply_debugger_state_delta(DebuggerStateDelta::establish_stopped_target(
            TargetConnection::Local,
        ));

        model.set_terminal_pending(true);
        model.set_controls_running(true);
        assert_eq!(super::execution_status(&model).0, "Running");
        model.set_controls_running(false);
        model.set_terminal_pending(false);
        model.set_debug_state_stale(false);
        model.set_current_thread_id(Some("1"));
        model.start_stop_refresh();
        let context = model.bind_stop_context(1).unwrap();
        assert!(model.stopped_inspection_available());
        model.set_terminal_pending(true);
        assert_eq!(super::execution_status(&model).0, "GDB busy");
        assert!(!model.movement_commands_available());
        assert!(!model.stop_point_commands_available());
        assert!(!model.stopped_inspection_available());
        assert!(!model.is_stop_context_current(&context));
        assert!(model.begin_command_operation().is_none());
        assert!(model.pause_available());
        model.set_command_pending(true);
        model.set_command_pending(false);
        assert!(model.terminal_pending());
        model.set_terminal_pending(false);
        assert!(model.stopped_inspection_available());
        assert_eq!(super::execution_status(&model).0, "Stopped");
        model.set_terminal_pending(true);
        model.set_controls_ready(false);
        assert!(!model.terminal_pending());
    }

    #[test]
    fn compares_only_valid_normalized_addresses() {
        assert!(addresses_equal("0x0000AB", "ab"));
        assert!(addresses_equal("0", "0x000"));
        assert!(!addresses_equal("", "0"));
        assert!(!addresses_equal("0x", "0"));
        assert!(!addresses_equal("not-an-address", "not-an-address"));
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
