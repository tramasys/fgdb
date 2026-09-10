//! Process-snapshot validity and action availability, independent of frame refreshes.

use super::*;
use crate::model::DebuggerModel;

pub(in crate::ui) fn process_running(model: &DebuggerModel) -> bool {
    let inferior = model.selected_inferior_id();

    model.inferior_is_running()
        || model.threads().iter().any(|thread| {
            thread.state == "running"
                && thread
                    .group_id
                    .as_deref()
                    .is_none_or(|group| Some(group) == inferior.as_deref())
        })
}

impl LocksView {
    pub(super) fn context_matches(
        &self,
        generation: u64,
        inferior: Option<&str>,
        pid: Option<u32>,
    ) -> bool {
        self.fresh.get()
            && self.context.borrow().as_ref().is_some_and(|context| {
                context.generation == generation
                    && inferior == Some(context.inferior.as_str())
                    && pid == Some(context.pid)
            })
    }

    pub(super) fn process_running(&self, model: &DebuggerModel) -> bool {
        let context = self.context.borrow();

        let Some(context) = context.as_ref() else {
            return false;
        };

        model.selected_inferior_id().as_deref() == Some(context.inferior.as_str())
            && model.inferior_pid() == Some(context.pid)
            && process_running(model)
    }

    pub(super) fn snapshot_current(&self, model: &DebuggerModel, generation: u64) -> bool {
        self.context_matches(
            generation,
            model.selected_inferior_id().as_deref(),
            model.inferior_pid(),
        ) && !self.process_running(model)
    }

    pub(super) fn navigation_current(&self, model: &DebuggerModel, generation: u64) -> bool {
        let execution = model.execution();

        self.snapshot_current(model, generation)
            && model.stopped_inspection_available()
            && model.is_stop_refresh_current(model.current_stop_refresh_generation())
            && execution.thread_action_pending.is_none()
            && execution.inferior_action_pending.is_none()
    }

    pub(super) fn update_controls(&self, model: &DebuggerModel, generation: u64) -> bool {
        let current = self.navigation_current(model, generation);
        let wait = self.selected_wait();
        let inferior = model.selected_inferior_id();
        let threads = model.threads();

        for (action, button) in &self.actions {
            let enabled = current
                && match action {
                    LockAction::Waiter | LockAction::Owner => wait
                        .as_ref()
                        .and_then(|wait| {
                            let tid = if *action == LockAction::Waiter {
                                Some(wait.tid)
                            } else {
                                wait.observation.ownership.owner()
                            }?;

                            actions::thread_for_tid(&threads, inferior.as_deref()?, tid)
                        })
                        .is_some_and(|thread| {
                            model.thread_action_can_dispatch(&ThreadAction::SelectFrame {
                                thread,
                                frame: 0,
                            })
                        }),
                    LockAction::Memory | LockAction::Copy => {
                        wait.as_ref().is_some_and(|wait| wait.address.is_some())
                    }
                    LockAction::Follow => wait
                        .as_ref()
                        .and_then(|wait| {
                            wait.observation
                                .ownership
                                .owner()
                                .filter(|owner| *owner != wait.tid)
                        })
                        .is_some_and(|owner| {
                            self.snapshot.borrow().as_ref().is_some_and(|snapshot| {
                                snapshot.waits.iter().any(|wait| wait.tid == owner)
                            })
                        }),
                    LockAction::Back => !self.history.borrow().is_empty(),
                    LockAction::Selection => false,
                };

            if button.is_sensitive() != enabled {
                button.set_sensitive(enabled);
            }
        }

        current
    }
}
