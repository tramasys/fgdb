use super::*;

use std::{cell::Cell, time::Duration};

use gtk::glib;
use nix::{
    sys::signal::{Signal, kill},
    unistd::Pid,
};
use vte4::prelude::*;

const GRACEFUL_SHUTDOWN_TIMEOUT: Duration = Duration::from_millis(2500);
const TERMINATE_SHUTDOWN_TIMEOUT: Duration = Duration::from_millis(1000);
const GDB_CONNECTION_TIMEOUT: Duration = Duration::from_secs(10);
const RESTART_QUIT_TIMEOUT: Duration = Duration::from_millis(1500);
const RESTART_TERMINATE_TIMEOUT: Duration = Duration::from_millis(1000);
const RESTART_KILL_TIMEOUT: Duration = Duration::from_millis(1500);

pub(super) struct BackendController {
    ui: Weak<Ui>,
    client: Rc<MiClient>,
    session: Rc<SessionController>,
    configuration: LaunchConfig,
    initial_configuration_pending: Cell<bool>,
    restart_requested: Cell<bool>,
    waiting_for_old_exit: Cell<bool>,
    pending_restore: RefCell<Option<DebugSession>>,
    closing: Cell<bool>,
    close_allowed: Cell<bool>,
    graceful_timeout: RefCell<Option<glib::SourceId>>,
    terminate_timeout: RefCell<Option<glib::SourceId>>,
    connection_timeout: RefCell<Option<glib::SourceId>>,
    restart_terminate_timeout: RefCell<Option<glib::SourceId>>,
    restart_kill_timeout: RefCell<Option<glib::SourceId>>,
    restart_failure_timeout: RefCell<Option<glib::SourceId>>,
}

impl BackendController {
    pub fn new(
        ui: Weak<Ui>,
        client: Rc<MiClient>,
        session: Rc<SessionController>,
        configuration: LaunchConfig,
    ) -> Rc<Self> {
        let initial_configuration_pending = configuration.needs_deferred_session_configuration();
        Rc::new(Self {
            ui,
            client,
            session,
            configuration,
            initial_configuration_pending: Cell::new(initial_configuration_pending),
            restart_requested: Cell::new(false),
            waiting_for_old_exit: Cell::new(false),
            pending_restore: RefCell::new(None),
            closing: Cell::new(false),
            close_allowed: Cell::new(false),
            graceful_timeout: RefCell::new(None),
            terminate_timeout: RefCell::new(None),
            connection_timeout: RefCell::new(None),
            restart_terminate_timeout: RefCell::new(None),
            restart_kill_timeout: RefCell::new(None),
            restart_failure_timeout: RefCell::new(None),
        })
    }

    pub fn install(self: &Rc<Self>) {
        let Some(ui) = self.ui.upgrade() else {
            return;
        };
        let weak = Rc::downgrade(self);
        ui.connect_gdb_recovery_handler(move || {
            if let Some(controller) = weak.upgrade() {
                controller.restart();
            }
        });
        let weak = Rc::downgrade(self);
        ui.window.connect_close_request(move |_| {
            weak.upgrade()
                .map_or(glib::Propagation::Proceed, |controller| {
                    controller.request_close()
                })
        });
        let weak = Rc::downgrade(self);
        ui.terminal.connect_child_exited(move |_, status| {
            if let Some(controller) = weak.upgrade() {
                controller.handle_session_event(SessionEvent::Exited(status));
            }
        });
        self.launch();
    }

    fn launch(self: &Rc<Self>) {
        let Some(ui) = self.ui.upgrade() else {
            return;
        };
        let weak = Rc::downgrade(self);
        launch_gdb(
            &ui.terminal,
            &self.configuration,
            &self.client,
            move |event| {
                if let Some(controller) = weak.upgrade() {
                    controller.handle_session_event(event);
                }
            },
        );
    }

    fn restart(self: &Rc<Self>) {
        let Some(ui) = self.ui.upgrade() else {
            return;
        };
        if self.closing.get() || self.restart_requested.get() {
            return;
        }
        self.restart_requested.set(true);
        self.cancel_connection_timeout();
        self.pending_restore.replace(ui.current_session());
        ui.clear_gdb_capabilities();
        ui.set_controls_ready(false);
        if ui.debugger_pid().is_some() {
            ui.set_status(
                "Restarting GDB",
                "Stopping the unresponsive debugger before opening a fresh backend…",
                None,
            );
            self.waiting_for_old_exit.set(true);
            ui.terminal.feed_child(b"quit\ny\n");
            self.start_restart_terminate_timeout();
            return;
        }
        drop(ui);
        self.begin_relaunch();
    }

    fn begin_relaunch(self: &Rc<Self>) {
        let Some(ui) = self.ui.upgrade() else {
            return;
        };
        ui.set_status(
            "Restarting GDB",
            "Allocating a fresh MI channel and restoring the configured session…",
            None,
        );
        if let Err(error) = self.client.reconnect() {
            self.restart_requested.set(false);
            ui.set_gdb_recovery_available(true);
            ui.set_status(
                "GDB restart failed",
                &format!("Could not allocate a new MI channel: {error}"),
                Some("status-error"),
            );
            return;
        }
        self.launch();
    }

    pub fn on_ready(&self) {
        self.cancel_connection_timeout();
        let restart_completed = self.restart_requested.replace(false);
        let restore = self.pending_restore.borrow_mut().take();
        if let Some(session) = restore {
            self.session.restore(session);
            return;
        }
        if restart_completed {
            return;
        }
        if self.initial_configuration_pending.replace(false)
            && let Some(session) = self.configuration.initial_session()
        {
            self.session.configure_initial(session);
        }
    }

    fn handle_session_event(self: &Rc<Self>, event: SessionEvent) {
        if matches!(event, SessionEvent::Exited(_)) && self.closing.get() {
            self.finish_close();
            return;
        }
        if matches!(event, SessionEvent::Exited(_)) && self.waiting_for_old_exit.replace(false) {
            self.cancel_restart_timeouts();
            handle_session_event(&self.ui, event);
            self.begin_relaunch();
            return;
        }
        if matches!(event, SessionEvent::Spawned(_)) {
            self.start_connection_timeout();
        } else if matches!(event, SessionEvent::Failed(_) | SessionEvent::Exited(_)) {
            self.cancel_connection_timeout();
        }
        let failed_or_exited = matches!(event, SessionEvent::Failed(_) | SessionEvent::Exited(_));
        handle_session_event(&self.ui, event);
        if failed_or_exited {
            self.restart_requested.set(false);
            if let Some(ui) = self.ui.upgrade() {
                ui.clear_gdb_capabilities();
                ui.set_gdb_recovery_available(true);
            }
        }
    }

    fn request_close(self: &Rc<Self>) -> glib::Propagation {
        if self.close_allowed.get() {
            return glib::Propagation::Proceed;
        }
        if self.closing.replace(true) {
            return glib::Propagation::Stop;
        }
        let Some(ui) = self.ui.upgrade() else {
            return glib::Propagation::Proceed;
        };
        let cleanup = shutdown_cleanup_command(
            ui.current_session().as_ref(),
            ui.target_connection(),
            ui.inferior_has_started(),
        );
        ui.save_layout();
        ui.set_gdb_recovery_available(false);
        ui.set_controls_ready(false);
        ui.set_status(
            "Closing safely",
            "Releasing the current target and stopping GDB…",
            None,
        );
        if ui.debugger_pid().is_none() {
            self.close_allowed.set(true);
            return glib::Propagation::Proceed;
        }
        self.start_graceful_timeout();
        if !self.client.is_ready() {
            ui.terminal.feed_child(b"quit\ny\n");
            return glib::Propagation::Stop;
        }
        drop(ui);
        if let Some(command) = cleanup {
            let weak = Rc::downgrade(self);
            if self
                .client
                .request(&command, move |_, _| {
                    if let Some(controller) = weak.upgrade() {
                        controller.request_gdb_exit();
                    }
                })
                .is_err()
            {
                self.request_gdb_exit();
            }
        } else {
            self.request_gdb_exit();
        }
        glib::Propagation::Stop
    }

    fn request_gdb_exit(self: &Rc<Self>) {
        if self
            .client
            .request("-gdb-exit", |_, _| {
                // VTE's child-exited signal performs the final close. Waiting
                // for the process avoids leaving a debugger behind the window.
            })
            .is_err()
            && let Some(ui) = self.ui.upgrade()
        {
            ui.terminal.feed_child(b"quit\ny\n");
        }
    }

    fn start_graceful_timeout(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        let timeout = glib::timeout_add_local_once(GRACEFUL_SHUTDOWN_TIMEOUT, move || {
            let Some(controller) = weak.upgrade() else {
                return;
            };
            controller.graceful_timeout.borrow_mut().take();
            controller.signal_debugger(Signal::SIGTERM);
            controller.start_terminate_timeout();
        });
        self.graceful_timeout.replace(Some(timeout));
    }

    fn start_connection_timeout(self: &Rc<Self>) {
        self.cancel_connection_timeout();
        let weak = Rc::downgrade(self);
        let timeout = glib::timeout_add_local_once(GDB_CONNECTION_TIMEOUT, move || {
            let Some(controller) = weak.upgrade() else {
                return;
            };
            controller.connection_timeout.borrow_mut().take();
            if controller.client.is_ready() || controller.closing.get() {
                return;
            }
            // Allow the recovery action to terminate this newly spawned but
            // unresponsive debugger and retry. Keep pending_restore intact so
            // either a late Ready or the next backend can restore the session.
            controller.restart_requested.set(false);
            if let Some(ui) = controller.ui.upgrade() {
                ui.set_controls_ready(false);
                ui.set_gdb_recovery_available(true);
                ui.set_status(
                    "GDB connection timed out",
                    "The debugger did not open its MI interface. Restart GDB from the Session menu.",
                    Some("status-error"),
                );
            }
        });
        self.connection_timeout.replace(Some(timeout));
    }

    fn cancel_connection_timeout(&self) {
        if let Some(timeout) = self.connection_timeout.borrow_mut().take() {
            timeout.remove();
        }
    }

    fn start_restart_terminate_timeout(self: &Rc<Self>) {
        self.cancel_restart_timeouts();
        let weak = Rc::downgrade(self);
        let timeout = glib::timeout_add_local_once(RESTART_QUIT_TIMEOUT, move || {
            let Some(controller) = weak.upgrade() else {
                return;
            };
            controller.restart_terminate_timeout.borrow_mut().take();
            controller.signal_debugger(Signal::SIGTERM);
            controller.start_restart_kill_timeout();
        });
        self.restart_terminate_timeout.replace(Some(timeout));
    }

    fn start_restart_kill_timeout(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        let timeout = glib::timeout_add_local_once(RESTART_TERMINATE_TIMEOUT, move || {
            let Some(controller) = weak.upgrade() else {
                return;
            };
            controller.restart_kill_timeout.borrow_mut().take();
            controller.signal_debugger(Signal::SIGKILL);
            controller.start_restart_failure_timeout();
        });
        self.restart_kill_timeout.replace(Some(timeout));
    }

    fn start_restart_failure_timeout(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        let timeout = glib::timeout_add_local_once(RESTART_KILL_TIMEOUT, move || {
            let Some(controller) = weak.upgrade() else {
                return;
            };
            controller.restart_failure_timeout.borrow_mut().take();
            if !controller.waiting_for_old_exit.replace(false) {
                return;
            }
            controller.restart_requested.set(false);
            if let Some(ui) = controller.ui.upgrade() {
                ui.set_controls_ready(false);
                ui.set_gdb_recovery_available(true);
                ui.set_status(
                    "GDB restart blocked",
                    "The old debugger did not exit after quit, SIGTERM, and SIGKILL. fgdb remains responsive, but the debugger process must exit before a fresh backend can be launched.",
                    Some("status-error"),
                );
            }
        });
        self.restart_failure_timeout.replace(Some(timeout));
    }

    fn cancel_restart_timeouts(&self) {
        if let Some(timeout) = self.restart_terminate_timeout.borrow_mut().take() {
            timeout.remove();
        }
        if let Some(timeout) = self.restart_kill_timeout.borrow_mut().take() {
            timeout.remove();
        }
        if let Some(timeout) = self.restart_failure_timeout.borrow_mut().take() {
            timeout.remove();
        }
    }

    fn start_terminate_timeout(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        let timeout = glib::timeout_add_local_once(TERMINATE_SHUTDOWN_TIMEOUT, move || {
            let Some(controller) = weak.upgrade() else {
                return;
            };
            controller.terminate_timeout.borrow_mut().take();
            controller.signal_debugger(Signal::SIGKILL);
            controller.finish_close();
        });
        self.terminate_timeout.replace(Some(timeout));
    }

    fn signal_debugger(&self, signal: Signal) {
        let Some(pid) = self
            .ui
            .upgrade()
            .and_then(|ui| ui.debugger_pid())
            .and_then(|pid| i32::try_from(pid).ok())
        else {
            return;
        };
        let _ = kill(Pid::from_raw(pid), signal);
    }

    fn finish_close(&self) {
        self.cancel_connection_timeout();
        self.cancel_restart_timeouts();
        if let Some(timeout) = self.graceful_timeout.borrow_mut().take() {
            timeout.remove();
        }
        if let Some(timeout) = self.terminate_timeout.borrow_mut().take() {
            timeout.remove();
        }
        self.close_allowed.set(true);
        if let Some(ui) = self.ui.upgrade() {
            ui.window.close();
        }
    }
}
