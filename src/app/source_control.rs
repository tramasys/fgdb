use super::*;

#[derive(Clone, Copy)]
pub(super) enum SourceLineOperation {
    Breakpoint,
    RunTo,
}

impl SourceLineOperation {
    fn unavailable_title(self) -> &'static str {
        match self {
            Self::Breakpoint => "Breakpoint unavailable",
            Self::RunTo => "Jump unavailable",
        }
    }

    fn rejected_title(self) -> &'static str {
        match self {
            Self::Breakpoint => "No breakpoint added",
            Self::RunTo => "Jump unavailable",
        }
    }
}

pub(super) struct ResolvedSourceLine {
    pub path: PathBuf,
    pub line: u32,
    pub location: String,
}

pub(super) fn run_to_source_line(ui: Weak<Ui>, client: &MiClient, path: PathBuf, line: u32) {
    let Some(current_ui) = ui.upgrade() else {
        return;
    };
    if !current_ui.movement_commands_available() {
        current_ui.set_status(
            "Jump unavailable",
            "The inferior must be paused before execution can run to a source line.",
            Some("status-error"),
        );
        return;
    }
    drop(current_ui);

    resolve_executable_source_line(
        ui,
        client,
        path,
        line,
        SourceLineOperation::RunTo,
        |ui, client, source| {
            let command = format!("-exec-until {}", crate::debugger::quote(&source.location));
            if let Some(ui) = ui.upgrade() {
                crate::ui::controls::issue_execution_command(
                    &ui,
                    client,
                    &command,
                    &format!("Running to {}", source.location),
                );
            }
        },
    );
}

pub(super) fn resolve_executable_source_line(
    ui: Weak<Ui>,
    client: &MiClient,
    path: PathBuf,
    line: u32,
    operation: SourceLineOperation,
    on_resolved: impl FnOnce(Weak<Ui>, &MiClient, ResolvedSourceLine) + 'static,
) {
    if !client.is_ready() {
        if let Some(ui) = ui.upgrade() {
            ui.set_status(
                operation.unavailable_title(),
                "Wait for the GDB/MI channel to become ready.",
                Some("status-error"),
            );
        }
        return;
    }
    let location = format!("{}:{line}", path.display());
    if let Some(ui) = ui.upgrade() {
        ui.set_command_pending(true);
        ui.set_status(
            "Checking source line",
            &format!("Looking for executable code at {location}"),
            None,
        );
    }
    let command = format!(
        "-symbol-list-lines {}",
        crate::debugger::quote(&path.to_string_lossy())
    );
    let ui_for_response = ui.clone();
    if let Err(error) = client.request(&command, move |client, record| {
        if !record.is_done() {
            if let Some(ui) = ui_for_response.upgrade() {
                ui.set_command_pending(false);
                ui.set_status(
                    operation.unavailable_title(),
                    record
                        .error_message()
                        .unwrap_or("GDB could not inspect executable lines for this source file"),
                    Some("status-error"),
                );
            }
            return;
        }
        if !crate::debugger::executable_source_lines(&record).contains(&line) {
            if let Some(ui) = ui_for_response.upgrade() {
                ui.set_command_pending(false);
                ui.set_status(
                    operation.rejected_title(),
                    &format!("{location} contains no executable code"),
                    None,
                );
            }
            return;
        }
        on_resolved(
            ui_for_response,
            client,
            ResolvedSourceLine {
                path,
                line,
                location,
            },
        );
    }) && let Some(ui) = ui.upgrade()
    {
        ui.set_command_pending(false);
        ui.set_status(
            operation.unavailable_title(),
            &error.to_string(),
            Some("status-error"),
        );
    }
}
