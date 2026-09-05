use super::*;
use std::sync::mpsc::{self, Receiver, TryRecvError};

struct PendingIndex {
    revision: u64,
    breakpoints: Arc<Vec<Breakpoint>>,
    sources: Option<Arc<source::SourceIndex>>,
    receiver: Option<Receiver<source::SourceBreakpointIndex>>,
    retry_after: Instant,
    reported_failure: bool,
}

#[derive(Default)]
pub(super) struct SourceBreakpointRefresh {
    revision: Arc<AtomicU64>,
    pending: Option<PendingIndex>,
    published_sources: Option<Arc<source::SourceIndex>>,
    polling: bool,
}

impl Drop for SourceBreakpointRefresh {
    fn drop(&mut self) {
        self.revision.fetch_add(1, Ordering::Relaxed);
    }
}

fn same_source_index(
    left: Option<&Arc<source::SourceIndex>>,
    right: Option<&Arc<source::SourceIndex>>,
) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => Arc::ptr_eq(left, right),
        (None, None) => true,
        _ => false,
    }
}

fn same_locations(left: &[Breakpoint], right: &[Breakpoint]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(old, new)| old.line == new.line && old.source_path() == new.source_path())
}

impl Ui {
    pub(super) fn latest_source_breakpoints(&self) -> Vec<Breakpoint> {
        self.source_breakpoint_refresh
            .borrow()
            .pending
            .as_ref()
            .map_or_else(
                || self.breakpoints.borrow().clone(),
                |pending| (*pending.breakpoints).clone(),
            )
    }

    pub(super) fn refresh_source_breakpoint_index(&self) {
        self.prepare_source_breakpoints(self.latest_source_breakpoints(), true);
    }

    pub(super) fn prepare_source_breakpoints(&self, breakpoints: Vec<Breakpoint>, force: bool) {
        let sources = self.source_index_snapshot();
        let mut state = self.source_breakpoint_refresh.borrow_mut();

        if !force
            && let Some(pending) = state.pending.as_mut()
            && same_locations(&pending.breakpoints, &breakpoints)
            && same_source_index(pending.sources.as_ref(), sources.as_ref())
        {
            // The worker's positions remain valid when only metadata changes.
            // Keep its work and publish the newest values with the result.
            pending.breakpoints = Arc::new(breakpoints);
            return;
        }

        let revision = state
            .revision
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1);

        // Hit counts, enable/disable changes and unrelated breakpoint metadata
        // do not require path resolution or rebuilding the line lookup.
        let has_source_locations = breakpoints
            .iter()
            .any(|bp| bp.line.is_some() && bp.source_path().is_some());

        if !has_source_locations
            || (!force
                && state.pending.is_none()
                && same_locations(&self.breakpoints.borrow(), &breakpoints)
                && same_source_index(state.published_sources.as_ref(), sources.as_ref()))
        {
            state.pending = None;
            state.published_sources = sources;
            drop(state);
            if !has_source_locations {
                self.source_breakpoint_index.replace(Default::default());
            }

            self.render_breakpoints(breakpoints, false);
            return;
        }

        state.pending = Some(PendingIndex {
            revision,
            breakpoints: Arc::new(breakpoints),
            sources,
            receiver: None,
            retry_after: Instant::now(),
            reported_failure: false,
        });
        if state.polling {
            return;
        }

        state.polling = true;
        drop(state);
        let weak_ui = self.self_weak.borrow().clone();

        glib::timeout_add_local(Duration::from_millis(16), move || {
            let Some(ui) = weak_ui.upgrade() else {
                return glib::ControlFlow::Break;
            };

            if ui.poll_source_breakpoint_index() {
                glib::ControlFlow::Continue
            } else {
                ui.source_breakpoint_refresh.borrow_mut().polling = false;
                glib::ControlFlow::Break
            }
        });
    }

    fn poll_source_breakpoint_index(&self) -> bool {
        let mut state = self.source_breakpoint_refresh.borrow_mut();
        let revision = Arc::clone(&state.revision);
        let Some(pending) = state.pending.as_mut() else {
            return false;
        };

        if let Some(receiver) = &pending.receiver {
            match receiver.try_recv() {
                Ok(index) => {
                    let pending = state
                        .pending
                        .take()
                        .expect("pending source breakpoint index");
                    state.published_sources = pending.sources;
                    drop(state);
                    let breakpoints = Arc::try_unwrap(pending.breakpoints)
                        .unwrap_or_else(|shared| (*shared).clone());
                    self.source_breakpoint_index.replace(index);
                    self.render_breakpoints(breakpoints, false);
                    // Source roots can change without changing any breakpoint.
                    for document in self.source_documents.borrow().iter() {
                        document.breakpoint_renderer.queue_draw();
                    }

                    // Rendering may re-enter a callback that requests a newer index.
                    return self.source_breakpoint_refresh.borrow().pending.is_some();
                }
                Err(TryRecvError::Empty) => return true,
                Err(TryRecvError::Disconnected) => {
                    pending.receiver = None;
                    pending.retry_after = Instant::now() + Duration::from_millis(500);
                    if !pending.reported_failure {
                        pending.reported_failure = true;
                        drop(state);
                        self.source_breakpoint_index_failure(
                            "The source identity worker did not finish",
                        );
                    }

                    return true;
                }
            }
        }

        if Instant::now() < pending.retry_after {
            return true;
        }

        let (sender, receiver) = mpsc::channel();
        let breakpoints = Arc::clone(&pending.breakpoints);
        let sources = pending.sources.clone();
        let expected = pending.revision;
        let queued_revision = Arc::clone(&revision);
        match crate::background::submit_cancellable_with_priority(
            crate::background::Priority::Interactive,
            move || queued_revision.load(Ordering::Relaxed) == expected,
            move || {
                if let Some(index) = source::SourceBreakpointIndex::build_while(
                    &breakpoints,
                    sources.as_deref(),
                    || revision.load(Ordering::Relaxed) == expected,
                ) {
                    let _ = sender.send(index);
                }
            },
        ) {
            Ok(()) => pending.receiver = Some(receiver),
            Err(error) => {
                pending.retry_after = Instant::now() + Duration::from_millis(500);
                if !pending.reported_failure {
                    pending.reported_failure = true;
                    drop(state);
                    self.source_breakpoint_index_failure(&error.to_string());
                }
            }
        }
        true
    }

    fn source_breakpoint_index_failure(&self, detail: &str) {
        self.record_performance_notice(crate::performance::PerformanceNotice {
            outcome: crate::performance::BudgetOutcome::Rejected,
            operation: String::from("source breakpoint index"),
            detail: format!(
                "{detail}. Keeping the previous snapshot and retrying the latest update"
            ),
        });
    }
}
