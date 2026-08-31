use super::*;

use std::cell::Cell;

pub(super) struct SessionController {
    ui: Weak<Ui>,
    client: Rc<MiClient>,
    configured_environment: RefCell<HashSet<String>>,
    busy: Cell<bool>,
}

enum SequenceCompletion {
    Configure(DebugSession),
    Restart,
    Kill,
    Detach,
}

struct CommandSequence {
    controller: Rc<SessionController>,
    commands: RefCell<VecDeque<String>>,
    completion: RefCell<Option<SequenceCompletion>>,
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
        let mut commands =
            cleanup_commands(ui.current_session().as_ref(), ui.inferior_has_started());
        commands.extend(session_commands(
            &session,
            &self.configured_environment.borrow(),
        ));
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
        self.run_sequence(
            session_commands(&session, &HashSet::new()),
            SequenceCompletion::Configure(session),
        );
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
        self.run_sequence(
            session_commands(&session, &HashSet::new()),
            SequenceCompletion::Configure(session),
        );
    }

    pub fn action(self: &Rc<Self>, action: SessionAction) {
        let Some(ui) = self.ui.upgrade() else {
            return;
        };
        if self.busy.replace(true) {
            return;
        }
        let (commands, completion, title, detail) = match action {
            SessionAction::Restart => (
                vec![String::from("-exec-run")],
                SequenceCompletion::Restart,
                "Restarting",
                "Restarting the configured inferior…",
            ),
            SessionAction::Kill => (
                vec![console_command("kill")],
                SequenceCompletion::Kill,
                "Killing inferior",
                "Terminating the inferior without closing GDB…",
            ),
            SessionAction::Detach => (
                vec![
                    if matches!(ui.current_session(), Some(DebugSession::Remote { .. })) {
                        String::from("-target-disconnect")
                    } else {
                        String::from("-target-detach")
                    },
                ],
                SequenceCompletion::Detach,
                "Detaching",
                "Releasing and resuming the inferior…",
            ),
        };
        ui.set_session_pending(true);
        ui.set_status(title, detail, None);
        self.run_sequence(commands, completion);
    }

    fn run_sequence(self: &Rc<Self>, commands: Vec<String>, completion: SequenceCompletion) {
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
            SequenceCompletion::Restart => {
                ui.set_status(
                    "Restarting",
                    "GDB accepted the restart. Waiting for target state…",
                    Some("status-running"),
                );
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
    if let Err(error) = sequence
        .controller
        .client
        .request(&command, move |_, record| {
            if record.is_success() {
                run_next(sequence_for_response);
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

fn cleanup_commands(session: Option<&DebugSession>, inferior_started: bool) -> Vec<String> {
    match session {
        Some(DebugSession::Launch { .. }) if inferior_started => vec![console_command("kill")],
        Some(DebugSession::Attach { .. }) if inferior_started => {
            vec![String::from("-target-detach")]
        }
        Some(DebugSession::CoreDump { .. }) => vec![console_command("core-file")],
        Some(DebugSession::Remote { .. }) if inferior_started => {
            vec![String::from("-target-disconnect")]
        }
        Some(DebugSession::Launch { .. }) | Some(DebugSession::Attach { .. }) | None => Vec::new(),
        Some(DebugSession::Remote { .. }) => Vec::new(),
    }
}

pub(super) fn shutdown_cleanup_command(
    session: Option<&DebugSession>,
    inferior_started: bool,
) -> Option<String> {
    match session {
        Some(DebugSession::Launch { .. }) if inferior_started => console_command("kill").into(),
        Some(DebugSession::Attach { .. }) if inferior_started => {
            String::from("-target-detach").into()
        }
        Some(DebugSession::Remote { .. }) => String::from("-target-disconnect").into(),
        Some(DebugSession::Launch { .. })
        | Some(DebugSession::Attach { .. })
        | Some(DebugSession::CoreDump { .. })
        | None => None,
    }
}

fn session_commands(session: &DebugSession, old_environment: &HashSet<String>) -> Vec<String> {
    let mut commands = Vec::new();
    for name in old_environment {
        commands.push(console_command(&format!("unset environment {name}")));
    }
    match session {
        DebugSession::Launch {
            executable,
            arguments,
            environment,
            working_directory,
        } => {
            commands.push(format!(
                "-environment-cd {}",
                crate::debugger::quote(&working_directory.to_string_lossy())
            ));
            commands.push(file_command(Some(executable)));
            let mut argument_command = String::from("-exec-arguments");
            for argument in arguments {
                argument_command.push(' ');
                argument_command.push_str(&crate::debugger::quote(argument));
            }
            commands.push(argument_command);
            for (name, value) in environment {
                commands.push(console_command(&format!("set environment {name}={value}")));
            }
        }
        DebugSession::Attach { pid, executable } => {
            commands.push(file_command(executable.as_deref()));
            commands.push(format!("-target-attach {pid}"));
        }
        DebugSession::CoreDump {
            executable,
            core_dump,
        } => {
            commands.push(file_command(Some(executable)));
            commands.push(format!(
                "-target-select core {}",
                crate::debugger::quote(&core_dump.to_string_lossy())
            ));
        }
        DebugSession::Remote {
            endpoint,
            executable,
            extended,
            remote_executable,
        } => {
            commands.push(file_command(executable.as_deref()));
            if let Some(remote_executable) = remote_executable {
                commands.push(console_command(&format!(
                    "set remote exec-file {remote_executable}"
                )));
            }
            commands.push(format!(
                "-target-select {} {endpoint}",
                if *extended {
                    "extended-remote"
                } else {
                    "remote"
                }
            ));
        }
    }
    commands
}

fn file_command(path: Option<&std::path::Path>) -> String {
    path.map_or_else(
        || String::from("-file-exec-and-symbols"),
        |path| {
            format!(
                "-file-exec-and-symbols {}",
                crate::debugger::quote(&path.to_string_lossy())
            )
        },
    )
}

fn console_command(command: &str) -> String {
    format!(
        "-interpreter-exec console {}",
        crate::debugger::quote(command)
    )
}

#[cfg(test)]
mod tests {
    use super::{cleanup_commands, session_commands, shutdown_cleanup_command};
    use crate::config::DebugSession;
    use std::{collections::HashSet, path::PathBuf};

    #[test]
    fn launch_configuration_preserves_argument_and_environment_boundaries() {
        let session = DebugSession::Launch {
            executable: PathBuf::from("/tmp/app with spaces"),
            arguments: vec![String::from("first value"), String::from("--mode=test")],
            environment: vec![(String::from("MODE"), String::from("debug build"))],
            working_directory: PathBuf::from("/tmp/project"),
        };
        let commands = session_commands(&session, &HashSet::from([String::from("OLD")]));
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
            session_commands(&attach, &HashSet::new()),
            ["-file-exec-and-symbols", "-target-attach 42"]
        );
        let core = DebugSession::CoreDump {
            executable: PathBuf::from("/tmp/app"),
            core_dump: PathBuf::from("/tmp/core file"),
        };
        assert_eq!(
            session_commands(&core, &HashSet::new())[1],
            "-target-select core \"/tmp/core file\""
        );
        let remote = DebugSession::Remote {
            endpoint: String::from("localhost:1234"),
            executable: None,
            extended: true,
            remote_executable: Some(String::from("/srv/app")),
        };
        let commands = session_commands(&remote, &HashSet::new());
        assert!(
            commands
                .iter()
                .any(|command| command.contains("set remote exec-file /srv/app"))
        );
        assert_eq!(
            commands.last().unwrap(),
            "-target-select extended-remote localhost:1234"
        );
    }

    #[test]
    fn switching_away_from_live_targets_uses_safe_cleanup() {
        let attach = DebugSession::Attach {
            pid: 42,
            executable: None,
        };
        assert_eq!(cleanup_commands(Some(&attach), true), ["-target-detach"]);
        let remote = DebugSession::Remote {
            endpoint: String::from("host:1"),
            executable: None,
            extended: false,
            remote_executable: None,
        };
        assert_eq!(
            cleanup_commands(Some(&remote), true),
            ["-target-disconnect"]
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
            shutdown_cleanup_command(Some(&launch), true).as_deref(),
            Some("-interpreter-exec console \"kill\"")
        );
        let attach = DebugSession::Attach {
            pid: 42,
            executable: None,
        };
        assert_eq!(
            shutdown_cleanup_command(Some(&attach), true).as_deref(),
            Some("-target-detach")
        );
        assert!(shutdown_cleanup_command(Some(&attach), false).is_none());
        let remote = DebugSession::Remote {
            endpoint: String::from("localhost:1234"),
            executable: None,
            extended: false,
            remote_executable: None,
        };
        assert_eq!(
            shutdown_cleanup_command(Some(&remote), false).as_deref(),
            Some("-target-disconnect")
        );
    }
}
