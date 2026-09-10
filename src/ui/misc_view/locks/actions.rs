//! Revalidate lock navigation against the selected inferior and stopped state.

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LockSymbolRequest {
    pub generation: u64,
    pub stamp: u64,
    pub address: u64,
}

pub(super) fn thread_for_tid(threads: &[ThreadInfo], inferior: &str, tid: u32) -> Option<String> {
    let mut candidates = threads.iter().filter(|thread| {
        thread.group_id.as_deref() == Some(inferior)
            && thread.state == "stopped"
            && thread_os_id(&thread.target_id).and_then(|id| id.parse::<u32>().ok()) == Some(tid)
    });

    let candidate = candidates.next()?;

    candidates.next().is_none().then(|| candidate.id.clone())
}

impl Ui {
    pub(crate) fn connect_lock_actions(
        self: &Rc<Self>,
        symbol: impl Fn(LockSymbolRequest) + 'static,
    ) {
        let weak = Rc::downgrade(self);

        self.misc_view
            .locks
            .handler
            .replace(Some(Rc::new(move |action| {
                let Some(ui) = weak.upgrade() else {
                    return;
                };

                if action != LockAction::Selection {
                    ui.perform_lock_action(action);
                }

                if let Some(request) = ui.take_lock_symbol_request() {
                    symbol(request);
                }

                ui.update_lock_controls();
            })));
    }

    pub(in crate::ui) fn bind_lock_context(&self) {
        let context = self
            .model
            .selected_inferior_id()
            .zip(self.model.inferior_pid())
            .map(|(inferior, pid)| LockContext {
                generation: self.misc_refresh_generation.get(),
                inferior,
                pid,
            });

        let view = &self.misc_view.locks;

        let changed_process = view
            .context
            .borrow()
            .as_ref()
            .zip(context.as_ref())
            .is_some_and(|(old, new)| old.inferior != new.inferior || old.pid != new.pid);

        if changed_process {
            view.clear();
        }

        view.context.replace(context);
    }

    fn lock_navigation_current(&self) -> bool {
        self.misc_view
            .locks
            .navigation_current(&self.model, self.misc_refresh_generation.get())
    }

    fn lock_snapshot_current(&self) -> bool {
        self.misc_view
            .locks
            .snapshot_current(&self.model, self.misc_refresh_generation.get())
    }

    fn lock_thread(&self, tid: u32) -> Option<String> {
        let inferior = self.model.selected_inferior_id()?;

        thread_for_tid(&self.model.threads(), &inferior, tid)
    }

    pub(in crate::ui) fn update_lock_controls(&self) {
        let view = &self.misc_view.locks;

        if view.fresh.get() && view.process_running(&self.model) {
            self.invalidate_misc_refresh();
        }

        let current = view.update_controls(&self.model, self.misc_refresh_generation.get());

        if let Some(request) = view.symbol_pending.get()
            && !self.lock_symbol_request_is_current(request)
        {
            view.cancel_symbol(request);
        }

        if current {
            view.schedule_symbol();
        }
    }

    fn perform_lock_action(&self, action: LockAction) {
        if !self.lock_navigation_current() {
            return;
        }

        let view = &self.misc_view.locks;

        if matches!(action, LockAction::Back | LockAction::Follow) {
            view.navigate(action);
            return;
        }

        let Some(wait) = view.selected_wait() else {
            return;
        };

        match action {
            LockAction::Waiter | LockAction::Owner => {
                let tid = if action == LockAction::Waiter {
                    Some(wait.tid)
                } else {
                    wait.observation.ownership.owner()
                };

                if let Some(thread) = tid.and_then(|tid| self.lock_thread(tid))
                    && self.emit_thread_action(ThreadAction::SelectFrame { thread, frame: 0 })
                {
                    self.panels.reveal(PanelId::CallStack);
                }
            }
            LockAction::Memory => {
                if let Some(address) = wait.address {
                    if add_memory_watch(
                        &self.memory_watch_container,
                        &self.memory_watches,
                        &self.memory_watch_handler,
                        format!("0x{address:x}"),
                        32,
                        MemoryWatchFormat::Bytes,
                    ) {
                        self.memory_search.show_inspector();
                        self.panels.reveal(PanelId::Memory);
                    } else {
                        self.set_status(
                            "Memory watch limit",
                            "Remove a memory watch before adding another (limit 256)",
                            Some("status-error"),
                        );
                    }
                }
            }
            LockAction::Copy => {
                if let Some(address) = wait.address {
                    view.root
                        .display()
                        .clipboard()
                        .set_text(&format!("0x{address:x}"));
                }
            }
            LockAction::Selection | LockAction::Back | LockAction::Follow => {}
        }
    }

    fn take_lock_symbol_request(&self) -> Option<LockSymbolRequest> {
        self.lock_navigation_current()
            .then(|| {
                self.misc_view
                    .locks
                    .begin_symbol(self.model.current_stop_refresh_generation())
            })
            .flatten()
    }

    pub(crate) fn lock_symbol_request_is_current(&self, request: LockSymbolRequest) -> bool {
        self.lock_snapshot_current()
            && self.model.is_stop_refresh_current(request.generation)
            && self.misc_view.locks.symbol_is_current(request)
    }

    pub(crate) fn show_lock_symbol(&self, request: LockSymbolRequest, symbol: String) {
        if !self.lock_symbol_request_is_current(request) {
            self.misc_view.locks.cancel_symbol(request);
            self.update_lock_controls();
            return;
        }

        self.misc_view.locks.complete_symbol(request, symbol);
    }
}

impl LocksView {
    pub(super) fn navigate(&self, action: LockAction) {
        if !self.fresh.get() {
            return;
        }

        if action == LockAction::Back {
            let previous = self.history.borrow_mut().pop();

            if let Some(state) = previous {
                self.restore_selection(Some(state));
            }

            return;
        }

        let next = self
            .selected_wait()
            .and_then(|wait| {
                wait.observation
                    .ownership
                    .owner()
                    .filter(|owner| *owner != wait.tid)
            })
            .and_then(|owner| {
                self.snapshot
                    .borrow()
                    .as_ref()?
                    .waits
                    .iter()
                    .find(|wait| wait.tid == owner)
                    .map(WaitKey::from)
            });

        if let Some(next) = next {
            self.remember_selection();
            self.select(next);
        }
    }

    pub(super) fn begin_symbol(&self, generation: u64) -> Option<LockSymbolRequest> {
        if !self.fresh.get() || self.symbol_pending.get().is_some() {
            return None;
        }

        self.context.borrow().as_ref()?;
        let address = self.selected_wait()?.address?;

        if self.symbols.borrow().contains_key(&address) {
            return None;
        }

        let request = LockSymbolRequest {
            generation,
            stamp: self.stamp.get(),
            address,
        };

        self.symbol_pending.set(Some(request));
        self.symbol.set_text("Symbol  Resolving…");

        Some(request)
    }

    pub(super) fn symbol_is_current(&self, request: LockSymbolRequest) -> bool {
        self.fresh.get()
            && self.stamp.get() == request.stamp
            && self.symbol_pending.get() == Some(request)
    }

    pub(super) fn cancel_symbol(&self, request: LockSymbolRequest) {
        if self.symbol_pending.get() == Some(request) {
            self.symbol_pending.set(None);
            self.stamp.set(self.stamp.get().wrapping_add(1));
            self.render_details();
        }
    }

    pub(super) fn schedule_symbol(self: &Rc<Self>) {
        let needed = self.fresh.get()
            && self.symbol_pending.get().is_none()
            && self
                .selected_key()
                .and_then(|key| key.address)
                .is_some_and(|address| !self.symbols.borrow().contains_key(&address));

        if !needed || self.symbol_scheduled.replace(true) {
            return;
        }

        let weak = Rc::downgrade(self);

        // State setters can run inside model reconciliation. Resolve only
        // after that transition finishes, with the then-current stop context.
        glib::idle_add_local_once(move || {
            if let Some(view) = weak.upgrade() {
                view.symbol_scheduled.set(false);
                view.emit(LockAction::Selection);
            }
        });
    }

    pub(super) fn complete_symbol(&self, request: LockSymbolRequest, symbol: String) {
        if !self.symbol_is_current(request) {
            return;
        }

        self.symbol_pending.set(None);
        self.symbols.borrow_mut().insert(request.address, symbol);
        self.render_details();
        self.emit(LockAction::Selection);
    }

    pub(in crate::ui) fn invalidate(&self) {
        if self.fresh.replace(false) {
            self.note.set_text(&format!(
                "{LOCKS_NOTE}\nPrevious stop snapshot. Navigation is disabled until refreshed"
            ));
        }

        self.symbol_pending.set(None);
        self.stamp.set(self.stamp.get().wrapping_add(1));
        self.disable_actions();
    }
}
