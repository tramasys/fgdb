use super::*;

pub(super) struct Validation {
    identity: ProcessIdentity,
    error: RefCell<Option<String>>,
}

impl Validation {
    pub(super) fn new(identity: ProcessIdentity) -> Self {
        Self {
            identity,
            error: RefCell::new(None),
        }
    }

    pub(super) fn is_current(&self) -> bool {
        match crate::local_process::validate_identity(self.identity) {
            Ok(()) => true,
            Err(error) => {
                self.error.replace(Some(error));
                false
            }
        }
    }

    pub(super) fn take_error(&self) -> Option<String> {
        self.error.borrow_mut().take()
    }
}

pub(super) fn release_unverified_attach(sequence: Rc<CommandSequence>, pid: u32, reason: String) {
    // GDB accepts a numeric PID, not a pidfd. A target can exit between the
    // final dispatch check and ptrace. Release an unverified attachment and
    // block inspection if cleanup leaves the backend state uncertain.
    let guard = Rc::clone(&sequence);
    let response = Rc::clone(&sequence);
    let message = reason.clone();

    let result = sequence.controller.client.request_for_session(
        "-target-detach",
        sequence.generation,
        move || guard.current(),
        move |_, record| {
            if !response.current() {
                return;
            }

            let message = if record.is_success() {
                if let Some(ui) = response.controller.ui.upgrade() {
                    ui.apply_debugger_state_delta(DebuggerStateDelta::clear_inferior());
                }

                message
            } else {
                if let Some(ui) = response.controller.ui.upgrade() {
                    ui.set_debug_state_stale(true);
                }

                format!("{message}\nCould not confirm detach: {}. Refresh debugger state or restart GDB before continuing", record.error_message().unwrap_or("GDB did not confirm cleanup"))
            };

            response.controller.fail_attach(pid, &message);
        },
    );

    if let Err(error) = result {
        if let Some(ui) = sequence.controller.ui.upgrade() {
            ui.set_debug_state_stale(true);
        }

        sequence.controller.fail_attach(pid, &format!("{reason}\nCould not queue detach: {error}. Refresh debugger state or restart GDB before continuing"));
    }
}
