//! Coalesce completed chains into UI updates, not one update per MI response.

use super::*;

#[derive(Default)]
pub(super) struct Progress {
    dirty: Vec<usize>,
    scheduled: bool,
    finished: bool,
}

impl Progress {
    pub(super) fn completed(&mut self, index: usize) {
        self.dirty.push(index);
    }

    pub(super) fn finish(&mut self) {
        self.finished = true;
        self.dirty.clear();
    }

    fn claim(&mut self) -> bool {
        if self.finished || self.scheduled || self.dirty.is_empty() {
            return false;
        }

        self.scheduled = true;
        true
    }

    fn take(&mut self) -> Vec<usize> {
        self.scheduled = false;
        std::mem::take(&mut self.dirty)
    }
}

pub(super) fn schedule(refresh: &Rc<RefCell<StackRefresh>>) {
    if !refresh.borrow_mut().progress.claim() {
        return;
    }

    let weak = Rc::downgrade(refresh);
    gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(16), move || {
        let Some(refresh) = weak.upgrade() else {
            return;
        };

        let (ui, generation, entries) = {
            let mut state = refresh.borrow_mut();
            if !state.requests.is_current() || state.progress.finished {
                return;
            }

            let entries = state
                .progress
                .take()
                .into_iter()
                .map(|index| state.entries[index].clone())
                .collect::<Vec<_>>();

            (state.ui.clone(), state.requests.generation(), entries)
        };

        // Never hold the refresh borrow across GTK callbacks. The model also
        // rejects replies from an older stop, frame, or stack-memory snapshot.
        if !entries.is_empty()
            && let Some(ui) = ui.upgrade()
        {
            ui.show_stack_details(generation, &entries);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coalesces_completed_rows_and_cancels_the_last_deferred_publish() {
        let mut progress = Progress::default();
        assert!(!progress.claim());
        progress.completed(4);
        assert!(progress.claim());
        progress.completed(1);
        assert!(!progress.claim());
        assert_eq!(progress.take(), [4, 1]);
        assert!(!progress.claim());
        progress.completed(7);
        assert!(progress.claim());
        progress.finish();
        assert!(progress.take().is_empty());
        assert!(!progress.claim());
    }
}
