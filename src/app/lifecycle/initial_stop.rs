//! Discover stops that occurred before the MI channel was attached.

use super::*;
use crate::model::DebuggerModel;

#[cfg(test)]
mod tests;

struct ProbeContext {
    epoch: u64,
    generation: u64,
    inferior: Option<String>,
    thread: Option<String>,
}

impl ProbeContext {
    fn new(model: &DebuggerModel, epoch: u64) -> Self {
        Self {
            epoch,
            generation: model.current_stop_refresh_generation(),
            inferior: model.selected_inferior_id(),
            thread: model.current_thread_id(),
        }
    }

    fn is_current(&self, model: &DebuggerModel, epoch: u64) -> bool {
        self.epoch == epoch
            && self.generation == model.current_stop_refresh_generation()
            && self.inferior == model.selected_inferior_id()
            && self.thread == model.current_thread_id()
            && model.debugger_synchronization_available()
    }
}

pub(super) fn refresh(ui: &Weak<Ui>, client: &MiClient) {
    let Some(current_ui) = ui.upgrade() else {
        return;
    };

    let context = ProbeContext::new(&current_ui.model, client.transport_epoch());

    if !context.is_current(&current_ui.model, client.transport_epoch()) {
        return;
    }

    let command = match context.inferior.as_deref() {
        Some(id) => {
            let Some(id) = crate::debugger::thread_group_argument(id) else {
                return;
            };

            format!("-thread-info --thread-group {id}")
        }
        None => String::from("-thread-info"),
    };

    let model = Rc::downgrade(&current_ui.model);
    let backend = client.weak();
    let weak_ui = ui.clone();

    let is_current: Rc<dyn Fn() -> bool> = Rc::new(move || {
        let (Some(model), Some(client)) = (model.upgrade(), backend.upgrade()) else {
            return false;
        };

        client.is_ready() && context.is_current(&model, client.transport_epoch())
    });

    let guard = Rc::clone(&is_current);

    let _ = client.request_when(
        "-data-evaluate-expression \"$_hit_bpnum\"",
        move || guard(),
        move |client, record| {
            if !is_current() {
                return;
            }

            let hit_breakpoint = record.is_done()
                && crate::debugger::evaluated_value(&record)
                    .as_deref()
                    .and_then(parse_gdb_integer)
                    .is_some_and(|number| number != 0);

            let guard = Rc::clone(&is_current);

            let _ = client.request_when(
                &command,
                move || guard(),
                move |client, record| {
                    if !record.is_done() || !is_current() {
                        return;
                    }

                    let threads = crate::debugger::threads(&record);
                    let Some(reason) = verified_stop_reason(&threads, hit_breakpoint) else {
                        return;
                    };

                    if let Some(ui) = weak_ui.upgrade() {
                        ui.model.publish_threads(&threads);
                        ui.set_thread_stop_reason(Some(reason));
                        ui.set_inferior_started(true);
                    }

                    refresh_stopped_state(&weak_ui, client);
                },
            );
        },
    );
}

fn verified_stop_reason(threads: &[ThreadInfo], hit_breakpoint: bool) -> Option<&'static str> {
    // $_hit_bpnum survives process exit and is only a diagnostic hint. A
    // current stopped thread, not a historical breakpoint, proves a live stop.
    threads
        .iter()
        .find(|thread| thread.current)
        .filter(|thread| thread.state == "stopped")
        .map(|_| {
            if hit_breakpoint {
                "breakpoint-hit"
            } else {
                "stopped"
            }
        })
}
