//! Workspace mutations use the session controller and the normal MI transport.

use super::*;
use crate::investigation::{CaptureResult, Investigation, RestoreResult, SavedBreakpoint};
use gtk::glib;

struct Controller {
    ui: Weak<Ui>,
    client: Rc<MiClient>,
    pending: RefCell<Option<PendingRestore>>,
    generation: Cell<u64>,
    session: Weak<SessionController>,
}

struct PendingRestore {
    workspace: Investigation,
    complete: RestoreResult,
}

struct Restore {
    ui: Weak<Ui>,
    client: Rc<MiClient>,
    epoch: u64,
    session: Weak<SessionController>,
    session_generation: u64,
    operation: CommandLease,
    workspace: Investigation,
    breakpoints: RefCell<VecDeque<SavedBreakpoint>>,
    failures: RefCell<Vec<String>>,
    complete: RefCell<Option<RestoreResult>>,
}

struct CaptureCompletion {
    callback: RefCell<Option<CaptureResult>>,
    ui: Weak<Ui>,
    client: Weak<MiClient>,
    epoch: u64,
    session: Weak<SessionController>,
    session_generation: u64,
    operation: CommandLease,
}

// The token owns only this operation's interlock. Old callbacks cannot unlock a newer operation.
struct CommandLease {
    ui: Weak<Ui>,
    id: Cell<Option<crate::model::CommandOperationId>>,
}

impl CommandLease {
    fn new(ui: &Rc<Ui>) -> Option<Self> {
        Some(Self {
            ui: Rc::downgrade(ui),
            id: Cell::new(Some(ui.begin_command_operation()?)),
        })
    }

    fn release(&self) {
        if let Some(id) = self.id.take()
            && let Some(ui) = self.ui.upgrade()
        {
            ui.finish_command_operation(id);
        }
    }

    fn current(&self) -> bool {
        self.id.get().is_some_and(|id| {
            self.ui
                .upgrade()
                .is_some_and(|ui| ui.model.command_operation_is_current(id))
        })
    }
}

impl Drop for CommandLease {
    fn drop(&mut self) {
        if let Some(id) = self.id.take() {
            let weak = self.ui.clone();
            glib::idle_add_local_once(move || {
                if let Some(ui) = weak.upgrade() {
                    ui.finish_command_operation(id);
                }
            });
        }
    }
}

impl CaptureCompletion {
    fn finish(&self, result: Result<Vec<Breakpoint>, String>) {
        let current = self.operation.current()
            && self
                .client
                .upgrade()
                .is_some_and(|client| client.is_ready() && client.transport_epoch() == self.epoch)
            && self
                .session
                .upgrade()
                .is_some_and(|session| session.generation() == self.session_generation)
            && self
                .ui
                .upgrade()
                .is_some_and(|ui| !ui.model.inferior_is_running());

        self.operation.release();

        let callback = self.callback.borrow_mut().take();

        if let Some(callback) = callback {
            callback(if current {
                result
            } else {
                Err(String::from("Debugger changed while saving the workspace"))
            });
        }
    }
}

impl Drop for CaptureCompletion {
    fn drop(&mut self) {
        if let Some(callback) = self.callback.get_mut().take() {
            glib::idle_add_local_once(move || {
                callback(Err(String::from(
                    "Debugger transport closed while saving the workspace",
                )))
            });
        }
    }
}

pub(super) fn connect(ui: &Rc<Ui>, client: &Rc<MiClient>, session: &Rc<SessionController>) {
    let controller = Rc::new(Controller {
        ui: Rc::downgrade(ui),
        client: Rc::clone(client),
        pending: RefCell::new(None),
        generation: Cell::new(0),
        session: Rc::downgrade(session),
    });

    let weak = Rc::downgrade(&controller);

    session.set_configuration_handler(move |result| {
        let Some(controller) = weak.upgrade() else {
            return;
        };

        let pending = controller.pending.borrow_mut().take();
        let Some(pending) = pending else { return };

        match result {
            Ok(session) if session == pending.workspace.session => controller.apply(pending),
            Ok(_) => (pending.complete)(Err(String::from(
                "A different session was configured. Workspace restoration was cancelled",
            ))),
            Err(error) => (pending.complete)(Err(error)),
        }
    });

    let capture_ui = Rc::downgrade(ui);
    let capture_client = Rc::clone(client);
    let capture_session = Rc::clone(session);
    ui.connect_investigation_actions(
        move |complete| capture(&capture_ui, &capture_client, &capture_session, complete),
        move |workspace, complete| {
            let Some(ui) = controller.ui.upgrade() else {
                return;
            };

            if !available(&ui, &controller.client) || controller.pending.borrow().is_some() {
                complete(Err(String::from(
                    "The debugger is busy. Pause it and retry",
                )));
                return;
            }

            let session = workspace.session.clone();
            controller.pending.replace(Some(PendingRestore {
                workspace,
                complete,
            }));

            let generation = controller.generation.get().wrapping_add(1);
            controller.generation.set(generation);
            ui.configure_workspace_session(session);
            let weak = Rc::downgrade(&controller);

            // Backend startup failures may happen before session configuration can notify us.
            glib::timeout_add_local_once(std::time::Duration::from_secs(120), move || {
                if let Some(controller) = weak.upgrade()
                    && controller.generation.get() == generation
                {
                    let pending = controller.pending.borrow_mut().take();

                    if let Some(pending) = pending {
                        (pending.complete)(Err(String::from(
                            "Workspace session setup did not complete. \
                             Check the application log before retrying",
                        )));
                    }
                }
            });
        },
    );
}

fn available(ui: &Ui, client: &MiClient) -> bool {
    client.is_ready() && ui.model.debugger_synchronization_available()
}

fn capture(
    ui: &Weak<Ui>,
    client: &Rc<MiClient>,
    session: &Rc<SessionController>,
    complete: CaptureResult,
) {
    let Some(current) = ui.upgrade() else { return };

    if !available(&current, client) {
        complete(Err(String::from(
            "Pause the debugger and wait for pending operations before saving",
        )));

        return;
    }

    let Some(operation) = CommandLease::new(&current) else {
        complete(Err(String::from("Another debugger operation is active")));
        return;
    };

    let callback = Rc::new(CaptureCompletion {
        callback: RefCell::new(Some(complete)),
        ui: ui.clone(),
        client: Rc::downgrade(client),
        epoch: client.transport_epoch(),
        session: Rc::downgrade(session),
        session_generation: session.generation(),
        operation,
    });

    let result_callback = Rc::clone(&callback);

    if let Err(error) = client.request("-break-list", move |_, record| {
        let result = if record.is_done() {
            Ok(crate::debugger::breakpoints(&record))
        } else {
            Err(record
                .error_message()
                .unwrap_or("Could not read breakpoints")
                .to_owned())
        };

        result_callback.finish(result);
    }) {
        callback.finish(Err(error.to_string()));
    }
}

impl Controller {
    fn apply(&self, mut pending: PendingRestore) {
        let Some(ui) = self.ui.upgrade() else { return };
        let Some(session) = self.session.upgrade() else {
            return;
        };
        let Some(operation) = CommandLease::new(&ui) else {
            (pending.complete)(Err(String::from("Another debugger operation is active")));
            return;
        };

        let breakpoints = std::mem::take(&mut pending.workspace.breakpoints).into();

        let restore = Rc::new(Restore {
            ui: self.ui.clone(),
            client: Rc::clone(&self.client),
            epoch: self.client.transport_epoch(),
            session: self.session.clone(),
            session_generation: session.generation(),
            operation,
            breakpoints: RefCell::new(breakpoints),
            workspace: pending.workspace,
            failures: RefCell::new(Vec::new()),
            complete: RefCell::new(Some(pending.complete)),
        });

        let response = Rc::clone(&restore);
        if let Err(error) = self.client.request("-break-delete", move |_, record| {
            if !response.current() {
                response.finish(Some("Session changed during workspace restoration"));
                return;
            }

            if !record.is_done() {
                response.finish(Some(
                    record
                        .error_message()
                        .unwrap_or("Could not replace the existing breakpoints"),
                ));

                return;
            }

            response.next();
        }) {
            restore.finish(Some(&error.to_string()));
        }
    }
}

impl Restore {
    fn current(&self) -> bool {
        self.operation.current()
            && self.client.transport_epoch() == self.epoch
            && self.client.is_ready()
            && self
                .session
                .upgrade()
                .is_some_and(|session| session.generation() == self.session_generation)
            && self.ui.upgrade().is_some_and(|ui| {
                ui.model.current_session().as_ref() == Some(&self.workspace.session)
                    && !ui.model.execution().state.inferior_running()
            })
    }

    fn next(self: &Rc<Self>) {
        if !self.current() {
            self.finish(Some("Session changed during workspace restoration"));
            return;
        }

        let next = self.breakpoints.borrow_mut().pop_front();
        let Some(breakpoint) = next else {
            self.finish(None);
            return;
        };

        let spec = BreakpointSpec {
            location: breakpoint.location.clone(),
            regex: false,
            hardware: breakpoint.hardware,
            // Publish the enabled state only after all commands were accepted.
            enabled: false,
            temporary: breakpoint.temporary,
            allow_pending: true,
            condition: (!breakpoint.condition.is_empty()).then_some(breakpoint.condition),
            stop_after: breakpoint.ignore_count.saturating_add(1),
            thread: None,
            inferior: None,
            commands: Vec::new(),
            logpoint: false,
        };

        let command = breakpoint_insert_command(&spec);
        let response = Rc::clone(self);

        if let Err(error) = self.client.request(&command, move |_, record| {
            if !response.current() {
                response.finish(Some("Session changed during workspace restoration"));
                return;
            }

            if !record.is_done() {
                response.failures.borrow_mut().push(format!(
                    "{}: {}",
                    breakpoint.location,
                    record.error_message().unwrap_or("breakpoint rejected")
                ));

                response.next();
                return;
            }

            let number = crate::debugger::inserted_breakpoints(&record)
                .first()
                .map(|breakpoint| breakpoint.number.clone());

            let Some(number) = number else {
                response.failures.borrow_mut().push(format!(
                    "{}: GDB did not report the breakpoint identity. The breakpoint remains disabled",
                    breakpoint.location
                ));

                response.next();
                return;
            };

            let mut commands = VecDeque::new();

            if !breakpoint.commands.is_empty() {
                commands.push_back(breakpoint_commands_command(&number, &breakpoint.commands));
            }

            if breakpoint.enabled {
                commands.push_back(format!("-break-enable {number}"));
            }

            response.configure_breakpoint(breakpoint.location, commands);
        }) {
            self.finish(Some(&error.to_string()));
        }
    }

    fn configure_breakpoint(self: &Rc<Self>, location: String, mut commands: VecDeque<String>) {
        if !self.current() {
            self.finish(Some("Session changed during workspace restoration"));
            return;
        }

        let Some(command) = commands.pop_front() else {
            self.next();
            return;
        };

        let response = Rc::clone(self);

        if let Err(error) = self.client.request(&command, move |_, record| {
            if !response.current() {
                response.finish(Some("Session changed during workspace restoration"));
            } else if !record.is_done() {
                response.failures.borrow_mut().push(format!(
                    "{location}: breakpoint remains disabled: {}",
                    record.error_message().unwrap_or("configuration rejected"),
                ));

                response.next();
            } else {
                response.configure_breakpoint(location, commands);
            }
        }) {
            self.finish(Some(&error.to_string()));
        }
    }

    fn finish(&self, error: Option<&str>) {
        let complete = self.complete.borrow_mut().take();
        let Some(complete) = complete else { return };
        self.operation.release();

        if let Some(ui) = self.ui.upgrade() {
            if self.client.transport_epoch() == self.epoch
                && ui.model.current_session().as_ref() == Some(&self.workspace.session)
            {
                refresh_breakpoints(&self.ui, &self.client);
                refresh_inferiors(&self.ui, &self.client);
            }

            if error.is_none() {
                ui.restore_investigation_views(&self.workspace, self.failures.take(), complete);
                return;
            }
        }

        complete(match error {
            Some(error) => Err(error.to_owned()),
            None => Ok(self.failures.take()),
        });
    }
}

impl Drop for Restore {
    fn drop(&mut self) {
        if let Some(complete) = self.complete.get_mut().take() {
            glib::idle_add_local_once(move || {
                complete(Err(String::from(
                    "Debugger transport closed during workspace restoration",
                )))
            });
        }
    }
}
