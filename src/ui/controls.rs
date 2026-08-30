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

pub(crate) fn issue_execution_command(ui: &Ui, client: &MiClient, command: &str, detail: &str) {
    match client.send(command) {
        Ok(_) => {
            ui.set_command_pending(true);
            ui.set_execution_status("Executing", detail);
        }
        Err(error) => ui.set_status("Command failed", &error.to_string(), Some("status-error")),
    }
}

pub(super) fn request_signal_catchpoint_toggle(ui: &Ui, signal: &str) {
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
    use super::addresses_equal;

    #[test]
    fn compares_only_valid_normalized_addresses() {
        assert!(addresses_equal("0x0000AB", "ab"));
        assert!(addresses_equal("0", "0x000"));
        assert!(!addresses_equal("", "0"));
        assert!(!addresses_equal("0x", "0"));
        assert!(!addresses_equal("not-an-address", "not-an-address"));
    }
}
