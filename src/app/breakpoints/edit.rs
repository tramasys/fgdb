//! Acknowledged edits retain drafts and clean up only breakpoints created here.

use super::*;
use gtk::glib;

struct Edit {
    ui: Weak<Ui>,
    client: Rc<MiClient>,
    epoch: u64,
    operation: crate::model::CommandOperationId,
    applied: Vec<String>,
    created: Vec<String>,
    uncertain_creation: bool,
}

pub(in crate::app) fn edit_breakpoint(
    ui: Weak<Ui>,
    client: Rc<MiClient>,
    request: BreakpointEditRequest,
    complete: crate::ui::BreakpointEditCompletion,
) {
    let operation = ui.upgrade().and_then(|ui| {
        (client.is_ready() && crate::ui::stop_point_actions_available(&ui.model))
            .then(|| ui.begin_command_operation())
            .flatten()
    });

    let Some(operation) = operation else {
        complete(
            Err(String::from(
                "Wait until the debugger is ready for this edit",
            )),
            true,
        );

        return;
    };

    let mut edit = Edit {
        ui,
        epoch: client.transport_epoch(),
        client,
        operation,
        applied: Vec::new(),
        created: Vec::new(),
        uncertain_creation: false,
    };

    glib::spawn_future_local(async move {
        let mut result = edit.apply(request).await;
        let mut retryable = edit.current() && !edit.uncertain_creation;

        if let Err(error) = &mut result {
            if edit.uncertain_creation {
                error.push_str("\nGDB may have created breakpoints without confirming their identities. Review the breakpoint list before retrying.");
            }

            if !edit.created.is_empty() {
                let command = format!("-break-delete {}", edit.created.join(" "));

                match edit.request(&command).await {
                    Ok(_) => error.push_str(
                        "\nNew breakpoints were removed. Existing breakpoints were left in place.",
                    ),
                    Err(cleanup) => {
                        retryable = false;

                        error.push_str(&format!(
                            "\nCould not remove new breakpoints {}. Review them before retrying. {cleanup}",
                            edit.created.join(", ")
                        ));
                    }
                }
            } else if !edit.applied.is_empty() {
                error.push_str(&format!(
                    "\nAlready applied\n{}\nRetry uses the current breakpoint settings.",
                    edit.applied.join("\n")
                ));
            }
        }

        if let Some(ui) = edit.ui.upgrade() {
            let current = edit.current();
            ui.finish_command_operation(edit.operation);

            if current {
                match &result {
                    Ok(()) => ui.set_status(
                        "Paused",
                        "Breakpoint settings applied",
                        Some("status-ready"),
                    ),
                    Err(error) => {
                        ui.set_status("Breakpoint update failed", error, Some("status-error"))
                    }
                }

                refresh_breakpoints(&edit.ui, &edit.client);
            }
        }

        complete(result, retryable);
    });
}

impl Edit {
    fn current(&self) -> bool {
        self.client.is_ready()
            && self.client.transport_epoch() == self.epoch
            && self.ui.upgrade().is_some_and(|ui| {
                ui.model.command_operation_is_current(self.operation)
                    && !ui.model.inferior_is_running()
                    && !ui.model.terminal_pending()
                    && !ui.model.execution().session_pending
            })
    }

    async fn request(&self, command: &str) -> Result<MiRecord, String> {
        if !self.current() {
            return Err(String::from(
                "The debugger context changed. Reopen the editor.",
            ));
        }

        let (sender, receiver) = futures_channel::oneshot::channel();

        self.client
            .request(command, move |_, record| {
                let _ = sender.send(record);
            })
            .map_err(|error| error.to_string())?;

        let record = receiver
            .await
            .map_err(|_| String::from("The debugger disconnected"))?;

        if !self.current() {
            return Err(String::from(
                "The debugger context changed. Reopen the editor.",
            ));
        }

        if !record.is_done() {
            return Err(record
                .error_message()
                .unwrap_or("GDB rejected this setting")
                .to_owned());
        }

        Ok(record)
    }

    async fn commands(&mut self, commands: impl IntoIterator<Item = String>) -> Result<(), String> {
        for command in commands {
            self.request(&command).await?;

            let summary = command
                .split_whitespace()
                .take(2)
                .collect::<Vec<_>>()
                .join(" ");

            self.applied.push(summary);
        }

        Ok(())
    }

    async fn apply(&mut self, mut request: BreakpointEditRequest) -> Result<(), String> {
        if request.spec.regex {
            return self.regex(request.spec).await;
        }

        if let Some(original) = &request.original {
            let record = self.request("-break-list").await?;

            request.original = Some(
                crate::debugger::breakpoints(&record)
                    .into_iter()
                    .find(|breakpoint| breakpoint.number == original.number)
                    .ok_or_else(|| {
                        String::from("The breakpoint no longer exists. Reopen the editor.")
                    })?,
            );
        }

        if let Some(original) = &request.original
            && (original.is_watchpoint() || original.is_catchpoint())
        {
            return self
                .request(&breakpoint_condition_command(
                    original.command_number(),
                    request.spec.condition.as_deref(),
                ))
                .await
                .map(|_| ());
        }

        if let Some(original) = &request.original
            && !breakpoint_needs_recreation(original, &request.spec)
        {
            let commands =
                mutable_breakpoint_commands(original.command_number(), original, &request.spec);

            return self.commands(commands).await;
        }

        let enabled = request.spec.enabled;
        request.spec.enabled = false;
        let record = self
            .request(&breakpoint_insert_command(&request.spec))
            .await?;

        self.created = crate::debugger::inserted_breakpoints(&record)
            .into_iter()
            .filter(|breakpoint| !breakpoint.is_location())
            .map(|breakpoint| breakpoint.number)
            .collect();

        self.uncertain_creation = self.created.is_empty();

        let number = self.created.first().cloned().ok_or_else(|| {
            String::from("GDB omitted the created breakpoint identity. Review the breakpoint list before retrying.")
        })?;

        let commands = request.spec.effective_commands();

        if !commands.is_empty() {
            self.request(&breakpoint_commands_command(&number, &commands))
                .await?;
        }

        if enabled {
            self.request(&format!("-break-enable {number}")).await?;
        }

        if let Some(original) = request.original {
            self.request(&format!("-break-delete {}", original.command_number()))
                .await?;

            if let Some(ui) = self.ui.upgrade() {
                ui.move_stop_point_metadata(original.command_number(), &number);
            }
        }

        Ok(())
    }

    async fn regex(&mut self, spec: BreakpointSpec) -> Result<(), String> {
        let command = crate::debugger::CliCommandBuilder::new("rbreak")
            .verbatim_tail(&spec.location)
            .map_err(str::to_owned)?
            .finish();

        let record = self.request("-break-list").await?;

        let before = crate::debugger::breakpoints(&record)
            .into_iter()
            .map(|breakpoint| breakpoint.number)
            .collect::<HashSet<_>>();

        self.uncertain_creation = true;
        let inserted = self.request(&command).await;
        let record = self.request("-break-list").await?;

        self.created = crate::debugger::breakpoints(&record)
            .into_iter()
            .filter(|breakpoint| !breakpoint.is_location() && !before.contains(&breakpoint.number))
            .map(|breakpoint| breakpoint.number)
            .collect();

        self.uncertain_creation = false;
        inserted?;

        if self.created.is_empty() {
            return Err(String::from(
                "No currently loaded function matched that regular expression",
            ));
        }

        let mut commands = Vec::new();
        let effective = spec.effective_commands();

        for number in &self.created {
            if let Some(condition) = &spec.condition {
                commands.push(format!(
                    "-break-condition {number} {}",
                    crate::debugger::quote(condition)
                ));
            }

            if spec.stop_after > 1 {
                commands.push(format!("-break-after {number} {}", spec.stop_after - 1));
            }

            if !effective.is_empty() {
                commands.push(breakpoint_commands_command(number, &effective));
            }
        }

        if spec.temporary {
            commands.push(crate::debugger::console_command(&format!(
                "enable delete {}",
                self.created.join(" ")
            )));
        }

        if !spec.enabled {
            commands.push(format!("-break-disable {}", self.created.join(" ")));
        }

        self.commands(commands).await
    }
}
