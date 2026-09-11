//! Bounded immutable results, separate from the selected frame's local catalog.

use super::*;
use crate::debugger::{ReturnValue, Variable};
use std::collections::VecDeque;

pub(crate) const MAX_RETURN_VALUES: usize = 32;
const MAX_RETURN_HISTORY_BYTES: usize = 1024 * 1024;

pub(crate) struct CapturedReturnValue {
    pub(crate) id: u64,
    thread: String,
    inferior: Option<String>,
    symbol_revision: u64,
    pub(crate) value: ReturnValue,
}

impl CapturedReturnValue {
    pub(crate) fn variable(&self) -> Variable {
        Variable {
            local_index: None,
            return_value: Some(self.id),
            name: self
                .value
                .history_variable
                .clone()
                .unwrap_or_else(|| format!("return {}", self.id)),
            value: self.value.value.clone(),
            type_name: None,
            argument: false,
            varobj: None,
            num_children: 0,
            has_more: false,
            display_hint: None,
            dynamic: false,
        }
    }
}

#[derive(Default)]
pub(super) struct ReturnHistory {
    values: VecDeque<Rc<CapturedReturnValue>>,
    next_id: u64,
}

impl DebuggerModel {
    pub(crate) fn record_verified_return_value(
        &self,
        value: ReturnValue,
        thread: &str,
        inferior: &str,
        replaces: Option<&str>,
    ) {
        if let Some(reference) = replaces {
            self.stopped
                .return_value
                .borrow_mut()
                .values
                .retain(|entry| {
                    entry.thread != thread
                        || entry.inferior.as_deref() != Some(inferior)
                        || entry.value.history_variable.as_deref() != Some(reference)
                });
        }

        self.record_return_value(Some(value), Some(thread), Some(inferior));
    }

    pub(crate) fn record_return_value(
        &self,
        value: Option<ReturnValue>,
        thread: Option<&str>,
        inferior: Option<&str>,
    ) {
        let Some((value, thread)) = value.zip(thread.filter(|id| !id.is_empty() && *id != "all"))
        else {
            return;
        };

        let inferior = inferior
            .map(str::to_owned)
            .or_else(|| self.inferior_for_thread(thread));
        let mut history = self.stopped.return_value.borrow_mut();

        if let Some(reference) = &value.history_variable {
            history
                .values
                .retain(|entry| entry.value.history_variable.as_ref() != Some(reference));
        }

        history.next_id = history
            .next_id
            .checked_add(1)
            .expect("return identity exhausted");

        let id = history.next_id;

        history.values.push_front(Rc::new(CapturedReturnValue {
            id,
            thread: thread.into(),
            inferior,
            symbol_revision: self.symbols.revision(),
            value,
        }));

        history.values.truncate(MAX_RETURN_VALUES);

        let mut bytes = history
            .values
            .iter()
            .map(|entry| entry.value.value.len())
            .sum::<usize>();

        // Preserve one complete result even if it alone exceeds the history
        // budget. Individual payloads are already bounded by the MI transport.
        while bytes > MAX_RETURN_HISTORY_BYTES && history.values.len() > 1 {
            if let Some(retired) = history.values.pop_back() {
                bytes -= retired.value.value.len();
            }
        }
    }

    pub(crate) fn return_values(&self) -> Vec<Rc<CapturedReturnValue>> {
        let execution = self.execution();

        if !execution.ready || !execution.state.inferior_started() || execution.session_pending {
            return Vec::new();
        }

        let thread = self.processes.selected_thread_id.borrow();
        let inferior = self.processes.selected_inferior_id.borrow();

        self.stopped
            .return_value
            .borrow()
            .values
            .iter()
            .filter(|entry| {
                entry.symbol_revision == self.symbols.revision()
                    && thread.as_deref() == Some(entry.thread.as_str())
                    && entry
                        .inferior
                        .as_deref()
                        .is_none_or(|owner| inferior.as_deref() == Some(owner))
            })
            .cloned()
            .collect()
    }

    pub(crate) fn clear_return_value(&self) {
        let mut history = self.stopped.return_value.borrow_mut();
        history.values.clear();

        history.next_id = history
            .next_id
            .checked_add(1)
            .expect("return identity exhausted");
    }

    pub(crate) fn return_value_revision(&self) -> u64 {
        self.stopped.return_value.borrow().next_id
    }

    pub(super) fn invalidate_return_value_owner(
        &self,
        thread: Option<&str>,
        inferior: Option<&str>,
    ) {
        self.stopped
            .return_value
            .borrow_mut()
            .values
            .retain(|entry| {
                !thread.is_some_and(|thread| entry.thread == thread)
                    && !inferior.is_some_and(|inferior| entry.inferior.as_deref() == Some(inferior))
            });
    }
}

#[cfg(test)]
mod tests;
