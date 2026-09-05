use super::*;
#[cfg(test)]
use crate::model::processes::{
    inferior_context_running, preferred_inferior_id, running_event_affects_all,
};

const EXECUTION_CONTEXT_VISUAL_DELAY: Duration = Duration::from_millis(300);

impl Ui {
    pub(crate) fn set_inferior_action_handler(&self, handler: impl Fn(InferiorAction) + 'static) {
        self.inferior_controls
            .action_handler
            .replace(Some(Rc::new(handler)));
    }

    pub(crate) fn connect_inferior_controls(self: &Rc<Self>) {
        let weak_ui = Rc::downgrade(self);

        self.inferior_controls
            .selector
            .connect_selected_notify(move |selector| {
                let Some(ui) = weak_ui.upgrade() else {
                    return;
                };

                if ui.inferior_controls.selector_updating.get() {
                    return;
                }

                let Some(id) = ui
                    .inferior_controls
                    .selector_ids
                    .borrow()
                    .get(selector.selected() as usize)
                    .cloned()
                else {
                    return;
                };

                ui.emit_inferior_action(InferiorAction::Select(id));
            });

        let weak_ui = Rc::downgrade(self);

        self.inferior_controls
            .follow_parent
            .connect_toggled(move |button| {
                if !button.is_active() {
                    return;
                }

                let Some(ui) = weak_ui.upgrade() else {
                    return;
                };

                if !ui.inferior_controls.selector_updating.get() {
                    let dispatched = ui.emit_inferior_action(InferiorAction::SetFollowFork(
                        ForkFollowMode::Parent,
                    ));

                    if !dispatched {
                        ui.render_inferior_policy();
                    }
                }
            });

        let weak_ui = Rc::downgrade(self);

        self.inferior_controls
            .follow_child
            .connect_toggled(move |button| {
                if !button.is_active() {
                    return;
                }

                let Some(ui) = weak_ui.upgrade() else {
                    return;
                };

                if !ui.inferior_controls.selector_updating.get() {
                    let dispatched = ui
                        .emit_inferior_action(InferiorAction::SetFollowFork(ForkFollowMode::Child));

                    if !dispatched {
                        ui.render_inferior_policy();
                    }
                }
            });

        let weak_ui = Rc::downgrade(self);

        self.inferior_controls
            .detach_on_fork
            .connect_toggled(move |button| {
                let Some(ui) = weak_ui.upgrade() else {
                    return;
                };

                if !ui.inferior_controls.selector_updating.get() {
                    let dispatched = ui
                        .emit_inferior_action(InferiorAction::SetDetachOnFork(button.is_active()));

                    if !dispatched {
                        ui.render_inferior_policy();
                    }
                }
            });

        let weak_ui = Rc::downgrade(self);

        self.inferior_controls
            .switch_parent
            .connect_clicked(move |_| {
                let Some(ui) = weak_ui.upgrade() else {
                    return;
                };

                let Some(selected) = ui.model.selected_inferior_id() else {
                    return;
                };

                if let Some(parent) = ui.model.inferior_parent(&selected) {
                    ui.emit_inferior_action(InferiorAction::Select(parent));
                }
            });

        let weak_ui = Rc::downgrade(self);

        self.inferior_controls
            .switch_child
            .connect_clicked(move |_| {
                let Some(ui) = weak_ui.upgrade() else {
                    return;
                };

                let Some(selected) = ui.model.selected_inferior_id() else {
                    return;
                };

                if let Some(child) = ui.model.first_inferior_child(&selected) {
                    ui.emit_inferior_action(InferiorAction::Select(child));
                }
            });

        let weak_ui = Rc::downgrade(self);

        self.inferior_controls.refresh.connect_clicked(move |_| {
            if let Some(ui) = weak_ui.upgrade() {
                ui.emit_inferior_action(InferiorAction::Refresh);
            }
        });
    }

    fn emit_inferior_action(&self, action: InferiorAction) -> bool {
        if !self.model.inferior_action_is_current(&action) {
            return false;
        }

        let handler = self.inferior_controls.action_handler.borrow().clone();

        if let Some(handler) = handler {
            handler(action);

            true
        } else {
            false
        }
    }

    pub(crate) fn apply_gdb_selection(&self, thread_id: Option<&str>, group_id: Option<&str>) {
        let previous = self.model.selected_inferior_id();
        self.model.apply_gdb_selection(thread_id, group_id);

        if previous != self.model.selected_inferior_id() {
            self.invalidate_inferior_selection();
        }

        self.render_selected_inferior_state();
        self.render_inferior_controls();
    }

    pub(crate) fn reconcile_stop_owner_from_threads(&self, threads: &[ThreadInfo]) -> bool {
        let changed = self.model.reconcile_stop_owner_from_threads(threads);

        if changed {
            self.invalidate_inferior_selection();
            self.render_selected_inferior_state();
        }

        self.render_inferior_controls();
        changed
    }

    pub(crate) fn show_inferiors(self: &Rc<Self>, inferiors: Vec<InferiorInfo>) {
        if self.model.show_inferiors(inferiors) {
            self.invalidate_inferior_selection();
        }

        self.render_selected_inferior_state();
        self.render_inferior_controls();
        self.start_local_inferior_relationship_refresh();
    }

    pub(crate) fn set_selected_inferior(&self, id: &str) -> bool {
        let previous = self.model.selected_inferior_id();

        if !self.model.set_selected_inferior(id) {
            return false;
        }

        if previous.as_deref() != Some(id) {
            self.invalidate_inferior_selection();
            self.render_selected_inferior_state();
            self.render_inferior_controls();
        }

        true
    }

    pub(crate) fn mark_inferior_running(&self, thread_id: Option<&str>) -> (bool, bool) {
        let affected = self.model.mark_inferior_running(thread_id);
        self.update_thread_control_sensitivity();

        affected
    }

    /// Defer the running presentation long enough to avoid flashing it during
    /// a normal step. The process and thread models have already changed, so
    /// command validation remains immediate and only painting is delayed.
    pub(crate) fn schedule_running_context_render(self: &Rc<Self>) {
        if self.execution_context_visual_pending.replace(true) {
            return;
        }

        let generation = self
            .execution_context_visual_generation
            .get()
            .wrapping_add(1);

        self.execution_context_visual_generation.set(generation);
        let weak_ui = Rc::downgrade(self);

        gtk::glib::timeout_add_local_once(EXECUTION_CONTEXT_VISUAL_DELAY, move || {
            let Some(ui) = weak_ui.upgrade() else {
                return;
            };

            if ui.execution_context_visual_generation.get() != generation {
                return;
            }

            ui.execution_context_visual_pending.set(false);
            ui.render_selected_inferior_state();
            ui.render_inferior_controls();
        });
    }

    pub(super) fn cancel_running_context_render(&self) {
        self.execution_context_visual_generation.set(
            self.execution_context_visual_generation
                .get()
                .wrapping_add(1),
        );

        self.execution_context_visual_pending.set(false);
    }

    pub(crate) fn mark_inferior_stopped(&self, thread_id: Option<&str>, all_stopped: bool) -> bool {
        self.cancel_running_context_render();
        let affected = self.model.mark_inferior_stopped(thread_id, all_stopped);

        if !self.model.inferiors().is_empty() {
            self.render_selected_inferior_state();
        }

        self.render_inferior_controls();
        affected
    }

    pub(crate) fn record_inferior_exited(&self, id: &str) {
        self.cancel_running_context_render();
        let selected_exited = self.model.selected_inferior_id().as_deref() == Some(id);
        self.model.record_inferior_exited(id);

        if selected_exited {
            self.render_selected_inferior_state();
        }

        self.render_inferior_controls();
    }

    pub(crate) fn clear_inferiors(&self) {
        self.cancel_running_context_render();
        self.model.clear_inferiors();
        self.render_inferior_controls();
    }

    pub(crate) fn set_fork_follow_mode(&self, mode: Option<ForkFollowMode>) {
        self.model.set_fork_follow_mode(mode);
        self.render_inferior_policy();
    }

    pub(crate) fn set_detach_on_fork(&self, detach: Option<bool>) {
        self.model.set_detach_on_fork(detach);
        self.render_inferior_policy();
    }

    pub(crate) fn set_fork_policy(&self, mode: Option<ForkFollowMode>, detach: Option<bool>) {
        self.model.set_fork_policy(mode, detach);
        self.render_inferior_policy();
    }

    pub(crate) fn set_inferior_action_pending(&self, pending: Option<InferiorActionPending>) {
        if self.model.set_inferior_action_pending(pending) {
            self.render_inferior_controls();
            self.update_control_sensitivity();
            self.update_thread_control_sensitivity();
        }
    }

    pub(crate) fn finish_inferior_execution_action(&self) {
        if self.model.finish_inferior_execution_action() {
            self.update_control_sensitivity();
            self.update_thread_control_sensitivity();
            self.render_inferior_controls();
        }
    }

    pub(crate) fn begin_inferior_execution_action(&self, id: String) -> u64 {
        let generation = self.model.begin_inferior_execution_action(id);
        self.update_control_sensitivity();
        self.update_thread_control_sensitivity();
        self.render_inferior_controls();

        generation
    }

    pub(crate) fn clear_inferior_action_pending(&self) {
        if self.model.clear_inferior_action_pending() {
            self.update_control_sensitivity();
            self.update_thread_control_sensitivity();
            self.render_inferior_controls();
        }
    }

    pub(crate) fn stop_owner_summary(&self) -> Option<String> {
        let group = self.model.stop_owner_inferior_id()?;

        let pid = self
            .model
            .inferiors()
            .iter()
            .find(|inferior| inferior.id == group)
            .and_then(|inferior| inferior.pid);

        Some(match pid {
            Some(pid) => format!("{group} PID {pid}"),
            None => group,
        })
    }

    fn start_local_inferior_relationship_refresh(self: &Rc<Self>) {
        let Some(debugger_pid) = self.model.debugger_pid() else {
            return;
        };

        let inferiors = self.model.inferiors().clone();

        let by_pid = inferiors
            .iter()
            .filter_map(|inferior| inferior.pid.map(|pid| (pid, inferior.id.clone())))
            .collect::<HashMap<_, _>>();

        self.model.prune_inferior_relationships();

        let generation = self.model.inferior_refresh_generation();
        let (sender, receiver) = std::sync::mpsc::channel();

        if crate::background::submit_with_priority(
            crate::background::Priority::Interactive,
            move || {
                let relationships = inferiors
                    .iter()
                    .filter_map(|inferior| {
                        let pid = inferior.pid?;
                        let parent = crate::kernel::read_local_parent_pid(pid, debugger_pid)?;

                        Some((inferior.id.clone(), by_pid.get(&parent)?.clone()))
                    })
                    .collect::<HashMap<_, _>>();

                let _ = sender.send(relationships);
            },
        )
        .is_err()
        {
            return;
        }

        let weak_ui = Rc::downgrade(self);

        gtk::glib::timeout_add_local(Duration::from_millis(25), move || {
            let Some(ui) = weak_ui.upgrade() else {
                return glib::ControlFlow::Break;
            };

            match receiver.try_recv() {
                Ok(discovered) => {
                    if ui
                        .model
                        .merge_inferior_relationships(generation, discovered)
                    {
                        ui.render_inferior_controls();
                    }

                    glib::ControlFlow::Break
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => glib::ControlFlow::Break,
            }
        });
    }

    fn render_inferior_policy(&self) {
        let controls = &self.inferior_controls;
        controls.selector_updating.set(true);
        let follow_parent = self.model.fork_follow_mode() == Some(ForkFollowMode::Parent);
        let follow_child = self.model.fork_follow_mode() == Some(ForkFollowMode::Child);
        let detach_on_fork = self.model.detach_on_fork().unwrap_or(true);

        if controls.follow_parent.is_active() != follow_parent {
            controls.follow_parent.set_active(follow_parent);
        }

        if controls.follow_child.is_active() != follow_child {
            controls.follow_child.set_active(follow_child);
        }

        if controls.detach_on_fork.is_active() != detach_on_fork {
            controls.detach_on_fork.set_active(detach_on_fork);
        }

        controls.selector_updating.set(false);
        let debugger_state = self.model.execution().state;

        let busy = debugger_state.inferior_running()
            || debugger_state.stopped_context_is_stale()
            || self.model.execution().command_pending
            || debugger_state.transition_pending()
            || self.model.execution().session_pending
            || self.model.execution().native_until_active
            || debugger_state.resynchronizing()
            || self.model.execution().inferior_action_pending.is_some();

        let available = self.model.execution().ready && !busy;
        let visual_transition = self.execution_visual_transition_pending();

        set_transient_execution_sensitive(
            &controls.follow_parent,
            available && self.model.fork_follow_mode().is_some(),
            visual_transition,
        );

        set_transient_execution_sensitive(
            &controls.follow_child,
            available && self.model.fork_follow_mode().is_some(),
            visual_transition,
        );

        set_transient_execution_sensitive(
            &controls.detach_on_fork,
            available && self.model.detach_on_fork().is_some(),
            visual_transition,
        );
    }

    pub(super) fn render_inferior_controls(&self) {
        if self.execution_context_visual_pending.get() {
            return;
        }

        let controls = &self.inferior_controls;
        let inferiors = self.model.inferiors();
        let selected = self.model.selected_inferior_id();

        let labels = inferiors
            .iter()
            .map(inferior_selector_label)
            .collect::<Vec<_>>();

        let selector_changed = controls.selector_model.n_items() as usize != labels.len()
            || labels.iter().enumerate().any(|(index, label)| {
                controls.selector_model.string(index as u32).as_deref() != Some(label.as_str())
            });

        controls.selector_updating.set(true);

        if selector_changed {
            let label_refs = labels.iter().map(String::as_str).collect::<Vec<_>>();

            controls
                .selector_model
                .splice(0, controls.selector_model.n_items(), &label_refs);

            controls.selector_ids.replace(
                inferiors
                    .iter()
                    .map(|inferior| inferior.id.clone())
                    .collect(),
            );
        }

        let selected_index = selected
            .as_ref()
            .and_then(|selected| {
                inferiors
                    .iter()
                    .position(|inferior| inferior.id == *selected)
            })
            .and_then(|index| u32::try_from(index).ok())
            .unwrap_or(gtk::INVALID_LIST_POSITION);

        if controls.selector.selected() != selected_index {
            controls.selector.set_selected(selected_index);
        }

        controls.selector_updating.set(false);
        let debugger_state = self.model.execution().state;

        let busy = self.model.execution().command_pending
            || debugger_state.transition_pending()
            || self.model.execution().session_pending
            || self.model.execution().native_until_active
            || debugger_state.resynchronizing()
            || self.model.execution().inferior_action_pending.is_some();

        let process_controls_available = self.model.execution().ready && !busy;

        let visual_busy =
            busy || debugger_state.inferior_running() || debugger_state.stopped_context_is_stale();

        set_transient_execution_sensitive(
            &controls.selector,
            !inferiors.is_empty() && process_controls_available,
            visual_busy,
        );

        set_transient_execution_sensitive(
            &controls.refresh,
            process_controls_available,
            visual_busy,
        );

        let selected_inferior = inferiors
            .iter()
            .find(|inferior| Some(inferior.id.as_str()) == selected.as_deref());

        set_label_text(
            &controls.selected_state,
            selected_inferior.map_or("idle", |inferior| inferior.state.label()),
        );

        for class in [
            "inferior-running",
            "inferior-stopped",
            "inferior-exited",
            "inferior-unknown",
        ] {
            set_css_class(
                &controls.selected_state,
                class,
                selected_inferior
                    .is_some_and(|inferior| class == inferior_state_css(inferior.state)),
            );
        }

        if let Some(owner) = self.stop_owner_summary() {
            let thread = self.model.stop_owner_thread_id();

            let owner = thread.as_ref().map_or_else(
                || format!("STOP OWNER  {owner}"),
                |thread| format!("STOP OWNER  {owner}  thread {thread}"),
            );

            set_label_text(&controls.stop_owner, &owner);
            controls.stop_owner.set_visible(true);
        } else {
            controls.stop_owner.set_visible(false);
            set_label_text(&controls.stop_owner, "");
        }

        let parent = selected
            .as_ref()
            .and_then(|selected| self.model.inferior_parent(selected));

        let child = selected
            .as_deref()
            .and_then(|selected| self.model.first_inferior_child(selected));

        let available = process_controls_available
            && !debugger_state.stopped_context_is_stale()
            && self.model.execution().inferior_action_pending.is_none();

        set_execution_sensitive(&controls.switch_parent, available && parent.is_some(), busy);
        set_execution_sensitive(&controls.switch_child, available && child.is_some(), busy);

        if inferiors.is_empty() {
            clear_box(&controls.list);
            controls.cards.borrow_mut().clear();

            controls
                .list
                .append(&empty_label("No GDB inferiors are available"));
        } else {
            let rebuild = {
                let cards = controls.cards.borrow();

                cards.len() != inferiors.len()
                    || cards
                        .iter()
                        .zip(inferiors.iter())
                        .any(|((id, _), inferior)| id != &inferior.id)
            };

            if rebuild {
                clear_box(&controls.list);

                let cards = inferiors
                    .iter()
                    .map(|inferior| {
                        let card = self.create_inferior_card(&inferior.id);
                        controls.list.append(&card.root);

                        (inferior.id.clone(), card)
                    })
                    .collect();

                controls.cards.replace(cards);
            }

            for inferior in inferiors.iter() {
                if let Some((_, card)) = controls
                    .cards
                    .borrow()
                    .iter()
                    .find(|(id, _)| id == &inferior.id)
                {
                    self.update_inferior_card(card, inferior);
                }
            }
        }

        drop(inferiors);
        self.render_inferior_policy();
    }

    fn create_inferior_card(&self, inferior_id: &str) -> InferiorCardControls {
        let card = gtk::Box::new(gtk::Orientation::Vertical, 5);
        card.add_css_class("inferior-card");
        let heading = gtk::Box::new(gtk::Orientation::Horizontal, 5);
        let id = gtk::Label::new(Some(inferior_id));
        id.add_css_class("inferior-id");
        id.set_halign(gtk::Align::Start);
        let name = gtk::Label::new(None);
        name.add_css_class("inferior-name");
        name.set_halign(gtk::Align::Start);
        name.set_hexpand(true);
        name.set_ellipsize(pango::EllipsizeMode::Middle);
        let state = gtk::Label::new(None);
        state.add_css_class("inferior-card-state");
        heading.append(&id);
        heading.append(&name);
        heading.append(&state);
        card.append(&heading);
        let facts = gtk::Label::new(None);
        facts.add_css_class("inferior-facts");
        facts.set_halign(gtk::Align::Start);
        facts.set_ellipsize(pango::EllipsizeMode::End);
        card.append(&facts);
        let relationship = gtk::Label::new(None);
        relationship.add_css_class("inferior-relationship");
        relationship.set_halign(gtk::Align::Start);
        relationship.set_visible(false);
        card.append(&relationship);
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        actions.set_homogeneous(true);
        let select = gtk::Button::with_label("Switch");
        select.add_css_class("inferior-inline-action");
        let id = inferior_id.to_owned();
        let handler = Rc::clone(&self.inferior_controls.action_handler);

        select.connect_clicked(move |_| {
            let handler = handler.borrow().clone();

            if let Some(handler) = handler {
                handler(InferiorAction::Select(id.clone()));
            }
        });

        actions.append(&select);
        let execution_button = gtk::Button::with_label("Unavailable");
        execution_button.add_css_class("inferior-inline-action");
        let execution_action = Rc::new(RefCell::new(None::<InferiorAction>));
        let handler = Rc::clone(&self.inferior_controls.action_handler);
        let action = Rc::clone(&execution_action);

        execution_button.connect_clicked(move |_| {
            dispatch_inferior_card_action(&action, &handler);
        });

        actions.append(&execution_button);
        card.append(&actions);

        InferiorCardControls {
            root: card,
            name,
            state,
            facts,
            relationship,
            select,
            execution: execution_button,
            execution_action,
        }
    }

    fn update_inferior_card(&self, card: &InferiorCardControls, inferior: &InferiorInfo) {
        let selected = self.model.is_selected_inferior(&inferior.id);
        let stop_owner = self.model.is_stop_owner_inferior(&inferior.id);
        set_css_class(&card.root, "inferior-card-selected", selected);
        set_css_class(&card.root, "inferior-card-stop-owner", stop_owner);
        set_label_text(&card.name, &inferior_display_name(inferior));
        set_label_text(&card.state, inferior.state.label());

        for class in [
            "inferior-running",
            "inferior-stopped",
            "inferior-exited",
            "inferior-unknown",
        ] {
            set_css_class(
                &card.state,
                class,
                class == inferior_state_css(inferior.state),
            );
        }

        set_label_text(&card.facts, &inferior_facts(inferior, stop_owner));

        let relationship = self
            .model
            .inferior_parent(&inferior.id)
            .map(|parent| format!("child of {parent}"));

        if let Some(relationship) = relationship {
            set_label_text(&card.relationship, &relationship);
            card.relationship.set_visible(true);
        } else {
            card.relationship.set_visible(false);
        }

        let debugger_state = self.model.execution().state;

        let busy = self.model.execution().command_pending
            || debugger_state.transition_pending()
            || self.model.execution().session_pending
            || self.model.execution().native_until_active
            || debugger_state.resynchronizing()
            || self.model.execution().inferior_action_pending.is_some();

        let available = self.model.execution().ready && !busy;

        let visual_busy =
            busy || debugger_state.inferior_running() || debugger_state.stopped_context_is_stale();

        set_button_label(&card.select, if selected { "Selected" } else { "Switch" });
        set_transient_execution_sensitive(&card.select, !selected && available, visual_busy);

        let execution = match inferior.state {
            InferiorState::Running => {
                Some(("Freeze", InferiorAction::Interrupt(inferior.id.clone())))
            }
            InferiorState::Stopped => Some(("Resume", InferiorAction::Resume(inferior.id.clone()))),
            InferiorState::Exited | InferiorState::NotStarted | InferiorState::Unknown => None,
        };

        set_button_label(
            &card.execution,
            execution
                .as_ref()
                .map_or("Unavailable", |(label, _)| *label),
        );

        set_transient_execution_sensitive(
            &card.execution,
            execution.is_some() && available,
            visual_busy,
        );

        card.execution_action
            .replace(execution.map(|(_, action)| action));
    }
}

fn inferior_selector_label(inferior: &InferiorInfo) -> String {
    inferior.pid.map_or_else(
        || inferior.id.clone(),
        |pid| format!("{}  PID {pid}", inferior.id),
    )
}

fn set_button_label(button: &gtk::Button, text: &str) {
    if button.label().as_deref() != Some(text) {
        button.set_label(text);
    }
}

fn dispatch_inferior_card_action(
    action: &RefCell<Option<InferiorAction>>,
    handler: &RefCell<Option<InferiorActionHandler>>,
) {
    let action = action.borrow().clone();
    let handler = handler.borrow().clone();

    if let (Some(action), Some(handler)) = (action, handler) {
        handler(action);
    }
}

fn inferior_display_name(inferior: &InferiorInfo) -> String {
    inferior
        .executable
        .as_deref()
        .and_then(|path| Path::new(path).file_name())
        .and_then(|name| name.to_str())
        .unwrap_or("no executable")
        .to_owned()
}

fn inferior_facts(inferior: &InferiorInfo, stop_owner: bool) -> String {
    let pid = inferior.pid.map_or_else(
        || String::from("PID unavailable"),
        |pid| format!("PID {pid}"),
    );

    let threads = match inferior.threads.len() {
        1 => String::from("1 thread"),
        count => format!("{count} threads"),
    };

    let owner = if stop_owner { "  STOP OWNER" } else { "" };

    format!("{pid}  {threads}{owner}")
}

fn inferior_state_css(state: InferiorState) -> &'static str {
    match state {
        InferiorState::Running => "inferior-running",
        InferiorState::Stopped => "inferior-stopped",
        InferiorState::Exited => "inferior-exited",
        InferiorState::NotStarted | InferiorState::Unknown => "inferior-unknown",
    }
}

impl Ui {
    fn invalidate_inferior_selection(&self) {
        self.reset_target_abi();
        self.invalidate_allocator_probe_cache();
        self.latest_modules.borrow_mut().clear();
        clear_box(&self.modules_list);
        self.modules_list
            .append(&empty_label("Modules refresh after selecting an inferior"));
    }

    fn render_selected_inferior_state(&self) {
        self.update_run_control_label();
        self.update_control_sensitivity();
        self.update_thread_control_sensitivity();
        self.render_threads();
    }
}

#[cfg(test)]
mod tests {
    use super::{
        dispatch_inferior_card_action, inferior_context_running, preferred_inferior_id,
        running_event_affects_all,
    };
    use crate::debugger::{InferiorInfo, InferiorState, ThreadInfo};
    use crate::ui::{InferiorAction, InferiorActionHandler};
    use std::{cell::RefCell, rc::Rc};

    #[test]
    fn card_action_can_be_replaced_during_synchronous_dispatch() {
        let action = Rc::new(RefCell::new(Some(InferiorAction::Resume(String::from(
            "i1",
        )))));

        let action_for_handler = Rc::clone(&action);

        let handler = RefCell::new(Some(Rc::new(move |_| {
            action_for_handler.replace(None);
        }) as InferiorActionHandler));

        dispatch_inferior_card_action(&action, &handler);
        assert!(action.borrow().is_none());
    }

    #[test]
    fn a_stopped_current_thread_remains_inspectable_in_non_stop_mode() {
        let thread = |id: &str, state: &str, current: bool| ThreadInfo {
            id: id.to_owned(),
            group_id: Some(String::from("i1")),
            target_id: id.to_owned(),
            name: None,
            state: state.to_owned(),
            core: None,
            frame: None,
            pc_symbol: None,
            current,
        };

        let inferior = InferiorInfo {
            id: String::from("i1"),
            pid: Some(42),
            executable: None,
            exit_code: None,
            state: InferiorState::Running,
            threads: vec![thread("1", "stopped", true), thread("2", "running", false)],
        };

        assert!(!inferior_context_running(&inferior, Some("1")));
        assert!(inferior_context_running(&inferior, Some("2")));
    }

    #[test]
    fn an_unknown_exact_thread_does_not_mark_every_inferior_running() {
        assert!(!running_event_affects_all(None, Some("99")));
        assert!(!running_event_affects_all(Some("i2"), None));
        assert!(running_event_affects_all(None, None));
    }

    #[test]
    fn rerun_selection_prefers_a_live_inferior_over_the_exited_previous_run() {
        let inferior = |id: &str, state| InferiorInfo {
            id: id.to_owned(),
            pid: None,
            executable: None,
            exit_code: None,
            state,
            threads: Vec::new(),
        };

        let inferiors = [
            inferior("i1", InferiorState::Exited),
            inferior("i2", InferiorState::Running),
        ];

        assert_eq!(
            preferred_inferior_id(&inferiors, None, Some("i1"), None).as_deref(),
            Some("i2")
        );

        assert_eq!(
            preferred_inferior_id(&inferiors[..1], None, Some("i1"), None),
            None
        );
    }
}
