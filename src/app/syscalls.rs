use std::{
    cell::RefCell,
    rc::{Rc, Weak},
    time::Duration,
};

use gtk::glib;

use crate::{
    debugger::MiClient,
    model::{DebuggerModel, TargetConnection},
    syscalls::{Collector, Phase, Target},
    ui::{SyscallAction, Ui},
};

struct Run {
    collector: Collector,
    target: Target,
    epoch: u64,
    revision: u64,
    stopping: bool,
    status: String,
}

pub(super) struct SyscallController {
    ui: Weak<Ui>,
    model: Rc<DebuggerModel>,
    client: Rc<MiClient>,
    run: RefCell<Option<Run>>,
    poll: RefCell<Option<glib::SourceId>>,
}

impl SyscallController {
    pub(super) fn new(ui: Weak<Ui>, model: Rc<DebuggerModel>, client: Rc<MiClient>) -> Rc<Self> {
        Rc::new(Self {
            ui,
            model,
            client,
            run: RefCell::new(None),
            poll: RefCell::new(None),
        })
    }

    fn target(&self) -> Option<Target> {
        (self.model.target_connection() == TargetConnection::Local).then_some(())?;

        Some(Target {
            pid: self.model.inferior_pid()?,
            debugger_pid: self.model.debugger_pid()?,
        })
    }

    pub(super) fn handle(self: &Rc<Self>, action: SyscallAction) {
        let Some(ui) = self.ui.upgrade() else {
            return;
        };

        match action {
            SyscallAction::Start => self.start(&ui),
            SyscallAction::Stop => {
                self.stop();
                ui.set_syscall_state("Stopping collection", true, true);
            }
            SyscallAction::Reset => {
                let mut run = self.run.borrow_mut();

                if let Some(run) = run.as_mut() {
                    if run.stopping {
                        return;
                    }

                    run.revision = run.collector.reset();
                }

                drop(run);
                ui.clear_syscall_counts();
            }
        }
    }

    fn start(self: &Rc<Self>, ui: &Ui) {
        if self.run.borrow().is_some() {
            return;
        }

        let Some(target) = self.target() else {
            ui.set_syscall_state("Unavailable. Start or attach a local process first. Remote and core-file targets cannot be counted", false, false);
            return;
        };

        let collector = match Collector::start(target) {
            Ok(collector) => collector,
            Err(error) => {
                ui.set_syscall_state(&error, false, false);
                return;
            }
        };

        let status = format!("Starting collection for PID {}", target.pid);

        self.run.replace(Some(Run {
            collector,
            target,
            epoch: self.client.transport_epoch(),
            revision: 0,
            stopping: false,
            status: status.clone(),
        }));

        ui.clear_syscall_counts();
        ui.set_syscall_state(&status, true, false);
        let controller = Rc::downgrade(self);

        let source = glib::timeout_add_local(Duration::from_millis(100), move || {
            let Some(controller) = controller.upgrade() else {
                return glib::ControlFlow::Break;
            };

            if controller.poll_once() {
                glib::ControlFlow::Continue
            } else {
                controller.poll.borrow_mut().take();
                glib::ControlFlow::Break
            }
        });

        self.poll.replace(Some(source));
    }

    pub(super) fn stop(&self) {
        if let Some(run) = self.run.borrow_mut().as_mut() {
            run.stopping = true;
            run.collector.stop();
        }
    }

    fn poll_once(&self) -> bool {
        let Some(ui) = self.ui.upgrade() else {
            self.run.borrow_mut().take();
            return false;
        };

        let (counts, status, finished, stopping) = {
            let mut current = self.run.borrow_mut();

            let Some(run) = current.as_mut() else {
                return false;
            };

            if run.epoch != self.client.transport_epoch() || self.target() != Some(run.target) {
                run.stopping = true;
                run.collector.stop();
            }

            // Completion is checked before consuming the final publication.
            let finished = run.collector.is_finished();
            let mut counts = None;

            if let Some(snapshot) = run.collector.take_snapshot() {
                if snapshot.revision == run.revision {
                    counts = snapshot.counts;
                }

                run.status = match snapshot.phase {
                    #[cfg(syscall_bpf)]
                    Phase::Collecting => format!(
                        "Collecting PID {}   All threads   Child processes excluded",
                        run.target.pid
                    ),
                    #[cfg(syscall_bpf)]
                    Phase::Stopped(message) => {
                        format!("{message}. Counts retained for PID {}", run.target.pid)
                    }
                    Phase::Unavailable(message) => format!("Unavailable: {message}"),
                };
            }

            (counts, run.status.clone(), finished, run.stopping)
        };

        if finished {
            self.run.borrow_mut().take();
        }

        if let Some(counts) = counts {
            ui.render_syscall_counts(&counts);
        }

        ui.set_syscall_state(&status, !finished, stopping && !finished);
        !finished
    }
}

impl Drop for SyscallController {
    fn drop(&mut self) {
        if let Some(source) = self.poll.get_mut().take() {
            source.remove();
        }
    }
}
