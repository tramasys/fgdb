use super::*;
use crate::debugger::{CliCommandBuilder, MiCommandBuilder, console_command};
use crate::ui::TargetConnection;

use std::cell::Cell;

pub(super) struct SessionController {
    ui: Weak<Ui>,
    client: Rc<MiClient>,
    configured_environment: RefCell<HashSet<String>>,
    busy: Cell<bool>,
}

enum SequenceCompletion {
    Configure(DebugSession),
    Kill,
    Detach,
}

struct CommandSequence {
    controller: Rc<SessionController>,
    commands: RefCell<VecDeque<SessionCommand>>,
    completion: RefCell<Option<SequenceCompletion>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SessionCommand {
    text: String,
    connection_after: Option<TargetConnection>,
}

impl SessionCommand {
    fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            connection_after: None,
        }
    }

    fn with_connection(text: impl Into<String>, connection: TargetConnection) -> Self {
        Self {
            text: text.into(),
            connection_after: Some(connection),
        }
    }
}

impl SessionController {
    pub fn new(ui: Weak<Ui>, client: Rc<MiClient>) -> Rc<Self> {
        Rc::new(Self {
            ui,
            client,
            configured_environment: RefCell::new(HashSet::new()),
            busy: Cell::new(false),
        })
    }

    pub fn configure(self: &Rc<Self>, session: DebugSession) {
        let Some(ui) = self.ui.upgrade() else {
            return;
        };
        if self.busy.replace(true) {
            return;
        }
        let mut commands = cleanup_commands(
            ui.current_session().as_ref(),
            ui.target_connection(),
            ui.inferior_has_started(),
        );
        let Ok(setup_commands) = session_commands(&session, &self.configured_environment.borrow())
        else {
            self.fail_invalid_configuration(&ui);
            return;
        };
        commands.extend(setup_commands);
        ui.set_session_pending(true);
        ui.set_status(
            "Configuring session",
            &format!("Preparing {} target…", session.kind_label().to_lowercase()),
            None,
        );
        self.run_sequence(commands, SequenceCompletion::Configure(session));
    }

    pub fn configure_initial(self: &Rc<Self>, session: DebugSession) {
        let Some(ui) = self.ui.upgrade() else {
            return;
        };
        if self.busy.replace(true) {
            return;
        }
        self.configured_environment.borrow_mut().clear();
        ui.set_session_pending(true);
        ui.set_status(
            "Configuring startup session",
            &format!("Preparing {} target…", session.kind_label().to_lowercase()),
            None,
        );
        let Ok(commands) = session_commands(&session, &HashSet::new()) else {
            self.fail_invalid_configuration(&ui);
            return;
        };
        self.run_sequence(commands, SequenceCompletion::Configure(session));
    }

    pub fn restore(self: &Rc<Self>, session: DebugSession) {
        let Some(ui) = self.ui.upgrade() else {
            return;
        };
        if self.busy.replace(true) {
            return;
        }
        self.configured_environment.borrow_mut().clear();
        ui.set_session_pending(true);
        ui.set_status(
            "Restoring session",
            &format!(
                "Reconnecting the {} target…",
                session.kind_label().to_lowercase()
            ),
            None,
        );
        let Ok(commands) = session_commands(&session, &HashSet::new()) else {
            self.fail_invalid_configuration(&ui);
            return;
        };
        self.run_sequence(commands, SequenceCompletion::Configure(session));
    }

    pub fn action(self: &Rc<Self>, action: SessionAction) {
        let Some(ui) = self.ui.upgrade() else {
            return;
        };
        if self.busy.replace(true) {
            return;
        }
        if action == SessionAction::Restart {
            ui.set_session_pending(true);
            crate::ui::controls::issue_execution_command(
                &ui,
                &self.client,
                "-exec-run",
                "Restarting the configured inferior",
            );
            self.busy.set(false);
            ui.set_session_pending(false);
            return;
        }
        let (commands, completion, title, detail) = match action {
            SessionAction::Kill => (
                vec![SessionCommand::new(console_command("kill"))],
                SequenceCompletion::Kill,
                "Killing inferior",
                "Terminating the inferior without closing GDB…",
            ),
            SessionAction::Detach => (
                vec![if ui.target_connection() == TargetConnection::Remote {
                    SessionCommand::with_connection("-target-disconnect", TargetConnection::None)
                } else {
                    SessionCommand::new("-target-detach")
                }],
                SequenceCompletion::Detach,
                "Detaching",
                "Releasing and resuming the inferior…",
            ),
            SessionAction::Restart => unreachable!(),
        };
        ui.set_session_pending(true);
        ui.set_status(title, detail, None);
        self.run_sequence(commands, completion);
    }

    fn run_sequence(
        self: &Rc<Self>,
        commands: Vec<SessionCommand>,
        completion: SequenceCompletion,
    ) {
        let sequence = Rc::new(CommandSequence {
            controller: Rc::clone(self),
            commands: RefCell::new(commands.into()),
            completion: RefCell::new(Some(completion)),
        });
        run_next(sequence);
    }

    fn fail(&self, message: &str) {
        self.busy.set(false);
        if let Some(ui) = self.ui.upgrade() {
            ui.set_session_pending(false);
            ui.set_status("Session command failed", message, Some("status-error"));
        }
    }

    fn fail_invalid_configuration(&self, ui: &Ui) {
        self.busy.set(false);
        ui.set_session_pending(false);
        ui.set_status(
            "Invalid session value",
            "A GDB CLI setting contained a NUL or line break and was not sent.",
            Some("status-error"),
        );
    }

    fn finish(&self, completion: SequenceCompletion) {
        self.busy.set(false);
        let Some(ui) = self.ui.upgrade() else {
            return;
        };
        ui.set_session_pending(false);
        match completion {
            SequenceCompletion::Configure(session) => {
                let environment = match &session {
                    DebugSession::Launch { environment, .. } => {
                        environment.iter().map(|(name, _)| name.clone()).collect()
                    }
                    DebugSession::Attach { .. }
                    | DebugSession::CoreDump { .. }
                    | DebugSession::Remote { .. } => HashSet::new(),
                };
                self.configured_environment.replace(environment);
                ui.set_controls_running(false);
                ui.set_inferior_started(false);
                ui.reset_target_abi();
                ui.clear_inferiors();
                ui.clear_debugger_state();
                ui.set_current_session(session.clone());
                match session {
                    DebugSession::Launch { .. } => ui.set_status(
                        "Ready to launch",
                        "The executable, arguments, environment, and working directory are configured.",
                        Some("status-ready"),
                    ),
                    DebugSession::Attach { .. }
                    | DebugSession::CoreDump { .. }
                    | DebugSession::Remote { .. } => {
                        ui.set_status(
                            session.kind_label(),
                            "Refreshing target state…",
                            Some("status-ready"),
                        );
                        request_initial_source(&self.ui, &self.client);
                        refresh_breakpoints(&self.ui, &self.client);
                        refresh_inferiors(&self.ui, &self.client);
                        refresh_fork_policy(&self.ui, &self.client);
                        refresh_thread_policy(&self.ui, &self.client);
                        refresh_modules(&self.ui, &self.client);
                        establish_session_target(&self.ui, &self.client, session.kind_label());
                    }
                }
            }
            SequenceCompletion::Kill => {
                ui.set_controls_running(false);
                ui.set_inferior_started(false);
                ui.set_thread_stop_reason(None);
                ui.clear_debugger_state();
                refresh_inferiors(&self.ui, &self.client);
                ui.set_status(
                    "Inferior terminated",
                    "The debugged process was killed. The session remains configured and can be run again.",
                    Some("status-ready"),
                );
            }
            SequenceCompletion::Detach => {
                ui.set_controls_running(false);
                ui.set_inferior_started(false);
                ui.set_thread_stop_reason(None);
                ui.clear_debugger_state();
                refresh_inferiors(&self.ui, &self.client);
                ui.set_status(
                    "Detached",
                    "GDB released the process. It normally continues running outside fgdb.",
                    Some("status-ready"),
                );
            }
        }
    }
}

fn establish_session_target(ui: &Weak<Ui>, client: &MiClient, kind: &'static str) {
    let weak_ui = ui.clone();
    if client
        .request("-thread-info", move |client, record| {
            let stopped = record.is_done()
                && crate::debugger::threads(&record)
                    .iter()
                    .any(|thread| thread.state == "stopped");
            let Some(ui) = weak_ui.upgrade() else {
                return;
            };
            ui.set_controls_running(false);
            ui.set_inferior_started(stopped);
            if stopped {
                ui.set_thread_stop_reason(Some("stopped"));
                ui.set_status(
                    kind,
                    "The target is stopped and ready for inspection.",
                    Some("status-ready"),
                );
            } else {
                ui.set_thread_stop_reason(None);
                ui.set_status(
                    kind,
                    "Connected without a stopped inferior. Extended-remote targets can be started with Run when a remote executable is configured.",
                    Some("status-ready"),
                );
            }
            detect_target_abi(&weak_ui, client);
        })
        .is_err()
        && let Some(ui) = ui.upgrade()
    {
        ui.set_status(
            "Session refresh failed",
            "Could not query the target threads",
            Some("status-error"),
        );
    }
}

fn run_next(sequence: Rc<CommandSequence>) {
    let Some(command) = sequence.commands.borrow_mut().pop_front() else {
        if let Some(completion) = sequence.completion.borrow_mut().take() {
            sequence.controller.finish(completion);
        }
        return;
    };
    let sequence_for_response = Rc::clone(&sequence);
    let connection_after = command.connection_after;
    if let Err(error) = sequence
        .controller
        .client
        .request(&command.text, move |client, record| {
            if record.is_success() {
                if let Some(connection) = connection_after
                    && let Some(ui) = sequence_for_response.controller.ui.upgrade()
                {
                    ui.set_target_connection(connection);
                }
                run_next(sequence_for_response);
            } else if record.class == "timeout" {
                sequence_for_response.controller.busy.set(false);
                client.quarantine(
                    "GDB did not answer a session command within 30 seconds. The target and session state can no longer be determined safely.",
                );
            } else {
                sequence_for_response.controller.fail(
                    record
                        .error_message()
                        .unwrap_or("GDB rejected the session command"),
                );
            }
        })
    {
        sequence
            .controller
            .fail(&format!("Could not queue a GDB command: {error}"));
    }
}

fn cleanup_commands(
    session: Option<&DebugSession>,
    connection: TargetConnection,
    inferior_started: bool,
) -> Vec<SessionCommand> {
    let command = match connection {
        TargetConnection::Remote => Some(String::from("-target-disconnect")),
        TargetConnection::Core => Some(console_command("core-file")),
        TargetConnection::Local => match session {
            Some(DebugSession::Launch { .. }) if inferior_started => Some(console_command("kill")),
            Some(DebugSession::Attach { .. }) if inferior_started => {
                Some(String::from("-target-detach"))
            }
            Some(DebugSession::Launch { .. })
            | Some(DebugSession::Attach { .. })
            | Some(DebugSession::CoreDump { .. })
            | Some(DebugSession::Remote { .. })
            | None => None,
        },
        TargetConnection::None => None,
    };
    command.map_or_else(Vec::new, |command| {
        vec![SessionCommand::with_connection(
            command,
            TargetConnection::None,
        )]
    })
}

pub(super) fn shutdown_cleanup_command(
    session: Option<&DebugSession>,
    connection: TargetConnection,
    inferior_started: bool,
) -> Option<String> {
    match connection {
        TargetConnection::Remote => Some(String::from("-target-disconnect")),
        TargetConnection::Local => match session {
            Some(DebugSession::Launch { .. }) if inferior_started => Some(console_command("kill")),
            Some(DebugSession::Attach { .. }) if inferior_started => {
                Some(String::from("-target-detach"))
            }
            Some(DebugSession::Launch { .. })
            | Some(DebugSession::Attach { .. })
            | Some(DebugSession::CoreDump { .. })
            | Some(DebugSession::Remote { .. })
            | None => None,
        },
        TargetConnection::Core | TargetConnection::None => None,
    }
}

fn session_commands(
    session: &DebugSession,
    old_environment: &HashSet<String>,
) -> Result<Vec<SessionCommand>, &'static str> {
    let mut commands = Vec::new();
    for name in old_environment {
        commands.push(SessionCommand::new(
            CliCommandBuilder::new("unset")
                .keyword("environment")
                .verbatim_tail(name)?
                .finish(),
        ));
    }
    match session {
        DebugSession::Launch {
            executable,
            arguments,
            environment,
            working_directory,
        } => {
            commands.push(SessionCommand::new(
                MiCommandBuilder::new("-environment-cd")
                    .argument(&working_directory.to_string_lossy())
                    .finish(),
            ));
            commands.push(SessionCommand::with_connection(
                file_command(Some(executable)),
                TargetConnection::Local,
            ));
            let argument_command = arguments
                .iter()
                .fold(
                    MiCommandBuilder::new("-exec-arguments"),
                    |command, argument| command.argument(argument),
                )
                .finish();
            commands.push(SessionCommand::new(argument_command));
            for (name, value) in environment {
                // GDB's `set environment` treats the remainder of the line as
                // the value. Quoting it would preserve quote characters in the
                // inferior environment. Session creation validates names and
                // obtains values one text line at a time; `console_command`
                // still performs the required MI transport escaping.
                let assignment = format!("{name}={value}");
                commands.push(SessionCommand::new(
                    CliCommandBuilder::new("set")
                        .keyword("environment")
                        .verbatim_tail(&assignment)?
                        .finish(),
                ));
            }
        }
        DebugSession::Attach { pid, executable } => {
            commands.push(SessionCommand::new(file_command(executable.as_deref())));
            commands.push(SessionCommand::with_connection(
                MiCommandBuilder::new("-target-attach").number(pid).finish(),
                TargetConnection::Local,
            ));
        }
        DebugSession::CoreDump {
            executable,
            core_dump,
        } => {
            commands.push(SessionCommand::new(file_command(Some(executable))));
            commands.push(SessionCommand::with_connection(
                MiCommandBuilder::new("-target-select")
                    .keyword("core")
                    .argument(&core_dump.to_string_lossy())
                    .finish(),
                TargetConnection::Core,
            ));
        }
        DebugSession::Remote {
            endpoint,
            executable,
            extended,
            remote_executable,
        } => {
            commands.push(SessionCommand::new(file_command(executable.as_deref())));
            if let Some(remote_executable) = remote_executable {
                commands.push(SessionCommand::new(
                    CliCommandBuilder::new("set")
                        .keyword("remote")
                        .keyword("exec-file")
                        .verbatim_tail(remote_executable)?
                        .finish(),
                ));
            }
            commands.push(SessionCommand::with_connection(
                MiCommandBuilder::new("-target-select")
                    .keyword(if *extended {
                        "extended-remote"
                    } else {
                        "remote"
                    })
                    .argument(endpoint)
                    .finish(),
                TargetConnection::Remote,
            ));
        }
    }
    Ok(commands)
}

fn file_command(path: Option<&std::path::Path>) -> String {
    path.map_or_else(
        || String::from("-file-exec-and-symbols"),
        |path| {
            MiCommandBuilder::new("-file-exec-and-symbols")
                .argument(&path.to_string_lossy())
                .finish()
        },
    )
}

#[cfg(test)]
mod tests {
    use super::{SessionCommand, cleanup_commands, session_commands, shutdown_cleanup_command};
    use crate::config::DebugSession;
    use crate::ui::TargetConnection;
    use std::{collections::HashSet, path::PathBuf};

    fn command_texts(commands: Vec<SessionCommand>) -> Vec<String> {
        commands.into_iter().map(|command| command.text).collect()
    }

    #[test]
    fn launch_configuration_preserves_argument_and_environment_boundaries() {
        let session = DebugSession::Launch {
            executable: PathBuf::from("/tmp/app with spaces"),
            arguments: vec![String::from("first value"), String::from("--mode=test")],
            environment: vec![(String::from("MODE"), String::from("debug build"))],
            working_directory: PathBuf::from("/tmp/project"),
        };
        let commands = command_texts(
            session_commands(&session, &HashSet::from([String::from("OLD")])).unwrap(),
        );
        assert!(
            commands
                .iter()
                .any(|command| command == "-environment-cd \"/tmp/project\"")
        );
        assert!(
            commands
                .iter()
                .any(|command| command == "-file-exec-and-symbols \"/tmp/app with spaces\"")
        );
        assert!(
            commands
                .iter()
                .any(|command| command == "-exec-arguments \"first value\" \"--mode=test\"")
        );
        assert!(
            commands
                .iter()
                .any(|command| command.contains("unset environment OLD"))
        );
        assert!(
            commands
                .iter()
                .any(|command| command.contains("MODE=debug build"))
        );
    }

    #[test]
    fn builds_native_attach_core_and_remote_commands() {
        let attach = DebugSession::Attach {
            pid: 42,
            executable: None,
        };
        assert_eq!(
            command_texts(session_commands(&attach, &HashSet::new()).unwrap()),
            ["-file-exec-and-symbols", "-target-attach 42"]
        );
        let core = DebugSession::CoreDump {
            executable: PathBuf::from("/tmp/app"),
            core_dump: PathBuf::from("/tmp/core file"),
        };
        assert_eq!(
            session_commands(&core, &HashSet::new()).unwrap()[1].text,
            "-target-select core \"/tmp/core file\""
        );
        let remote = DebugSession::Remote {
            endpoint: String::from("localhost:1234"),
            executable: None,
            extended: true,
            remote_executable: Some(String::from("/srv/app")),
        };
        let commands = command_texts(session_commands(&remote, &HashSet::new()).unwrap());
        assert!(
            commands
                .iter()
                .any(|command| command.contains("set remote exec-file"))
        );
        assert_eq!(
            commands.last().unwrap(),
            "-target-select extended-remote \"localhost:1234\""
        );
    }

    #[test]
    fn remote_session_quotes_endpoints_and_remote_executable_paths() {
        let remote_path = "/srv/app \"debug\"\\bin";
        let session = DebugSession::Remote {
            endpoint: String::from("host name:1234"),
            executable: Some(PathBuf::from("/tmp/local \"symbols\"")),
            extended: true,
            remote_executable: Some(remote_path.to_owned()),
        };
        let commands = command_texts(session_commands(&session, &HashSet::new()).unwrap());
        let cli = format!("set remote exec-file {remote_path}");
        assert_eq!(
            commands[0],
            "-file-exec-and-symbols \"/tmp/local \\\"symbols\\\"\""
        );
        assert_eq!(commands[1], crate::debugger::console_command(&cli));
        assert_eq!(
            commands[2],
            "-target-select extended-remote \"host name:1234\""
        );
    }

    #[test]
    fn switching_away_from_live_targets_uses_safe_cleanup() {
        let attach = DebugSession::Attach {
            pid: 42,
            executable: None,
        };
        assert_eq!(
            command_texts(cleanup_commands(
                Some(&attach),
                TargetConnection::Local,
                true,
            )),
            ["-target-detach"]
        );
        let remote = DebugSession::Remote {
            endpoint: String::from("host:1"),
            executable: None,
            extended: false,
            remote_executable: None,
        };
        assert_eq!(
            command_texts(cleanup_commands(
                Some(&remote),
                TargetConnection::Remote,
                true,
            )),
            ["-target-disconnect"]
        );
    }

    #[test]
    fn switching_from_connected_extended_remote_without_inferior_disconnects_first() {
        let remote = DebugSession::Remote {
            endpoint: String::from("host:1"),
            executable: None,
            extended: true,
            remote_executable: None,
        };
        let launch = DebugSession::Launch {
            executable: PathBuf::from("/tmp/next"),
            arguments: Vec::new(),
            environment: Vec::new(),
            working_directory: PathBuf::from("/tmp"),
        };
        let mut commands = cleanup_commands(Some(&remote), TargetConnection::Remote, false);
        commands.extend(session_commands(&launch, &HashSet::new()).unwrap());
        let texts = command_texts(commands);

        assert_eq!(
            texts.first().map(String::as_str),
            Some("-target-disconnect")
        );
        assert!(
            texts
                .iter()
                .any(|command| { command == "-file-exec-and-symbols \"/tmp/next\"" })
        );
    }

    #[test]
    fn shutdown_kills_launches_but_detaches_external_targets() {
        let launch = DebugSession::Launch {
            executable: PathBuf::from("/tmp/app"),
            arguments: Vec::new(),
            environment: Vec::new(),
            working_directory: PathBuf::from("/tmp"),
        };
        assert_eq!(
            shutdown_cleanup_command(Some(&launch), TargetConnection::Local, true).as_deref(),
            Some("-interpreter-exec console \"kill\"")
        );
        let attach = DebugSession::Attach {
            pid: 42,
            executable: None,
        };
        assert_eq!(
            shutdown_cleanup_command(Some(&attach), TargetConnection::Local, true).as_deref(),
            Some("-target-detach")
        );
        assert!(shutdown_cleanup_command(Some(&attach), TargetConnection::Local, false).is_none());
        let remote = DebugSession::Remote {
            endpoint: String::from("localhost:1234"),
            executable: None,
            extended: false,
            remote_executable: None,
        };
        assert_eq!(
            shutdown_cleanup_command(Some(&remote), TargetConnection::Remote, false).as_deref(),
            Some("-target-disconnect")
        );
    }
}
