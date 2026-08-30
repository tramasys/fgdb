use super::*;

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
                    ui.emit_inferior_action(InferiorAction::SetFollowFork(ForkFollowMode::Parent));
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
                    ui.emit_inferior_action(InferiorAction::SetFollowFork(ForkFollowMode::Child));
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
                    ui.emit_inferior_action(InferiorAction::SetDetachOnFork(button.is_active()));
                }
            });

        let weak_ui = Rc::downgrade(self);
        self.inferior_controls
            .switch_parent
            .connect_clicked(move |_| {
                let Some(ui) = weak_ui.upgrade() else {
                    return;
                };
                let Some(selected) = ui.selected_inferior_id() else {
                    return;
                };
                if let Some(parent) = ui.inferior_parents.borrow().get(&selected).cloned() {
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
                let Some(selected) = ui.selected_inferior_id() else {
                    return;
                };
                if let Some(child) = ui.first_inferior_child(&selected) {
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

    fn emit_inferior_action(&self, action: InferiorAction) {
        if self.inferior_action_pending.get() {
            return;
        }
        if let Some(handler) = self.inferior_controls.action_handler.borrow().clone() {
            handler(action);
        }
    }

    pub(crate) fn start_inferior_refresh(&self) -> u64 {
        let generation = self.inferior_refresh_generation.get().wrapping_add(1);
        self.inferior_refresh_generation.set(generation);
        generation
    }

    pub(crate) fn is_inferior_refresh_current(&self, generation: u64) -> bool {
        self.inferior_refresh_generation.get() == generation
    }

    pub(crate) fn start_fork_policy_refresh(&self) -> u64 {
        let generation = self.fork_policy_generation.get().wrapping_add(1);
        self.fork_policy_generation.set(generation);
        generation
    }

    pub(crate) fn is_fork_policy_refresh_current(&self, generation: u64) -> bool {
        self.fork_policy_generation.get() == generation
    }

    pub(crate) fn selected_inferior_id(&self) -> Option<String> {
        self.selected_inferior_id.borrow().clone()
    }

    pub(crate) fn selected_inferior_state(&self) -> Option<InferiorState> {
        let selected = self.selected_inferior_id.borrow();
        self.inferiors
            .borrow()
            .iter()
            .find(|inferior| Some(inferior.id.as_str()) == selected.as_deref())
            .map(|inferior| inferior.state)
    }

    pub(crate) fn inferior_for_thread(&self, thread_id: &str) -> Option<String> {
        self.inferiors
            .borrow()
            .iter()
            .find(|inferior| inferior.threads.iter().any(|thread| thread.id == thread_id))
            .map(|inferior| inferior.id.clone())
    }

    pub(crate) fn threads_for_selected_inferior(
        &self,
        mut threads: Vec<ThreadInfo>,
    ) -> Vec<ThreadInfo> {
        let inferiors = self.inferiors.borrow();
        if inferiors.is_empty() {
            return threads;
        }
        let selected = self.selected_inferior_id.borrow();
        threads.retain_mut(|thread| {
            let group = inferiors.iter().find(|inferior| {
                inferior
                    .threads
                    .iter()
                    .any(|candidate| candidate.id == thread.id)
            });
            if let Some(group) = group {
                thread.group_id = Some(group.id.clone());
            }
            group.is_some_and(|group| Some(group.id.as_str()) == selected.as_deref())
        });
        threads
    }

    pub(crate) fn reconcile_stop_owner_from_threads(&self, threads: &[ThreadInfo]) -> bool {
        if self.stop_owner_inferior_id.borrow().is_some() {
            return false;
        }
        let Some(thread) = threads
            .iter()
            .find(|thread| thread.current && thread.state == "stopped")
        else {
            return false;
        };
        let group = thread
            .group_id
            .clone()
            .or_else(|| self.inferior_for_thread(&thread.id));
        let Some(group) = group.filter(|group| {
            self.inferiors
                .borrow()
                .iter()
                .any(|inferior| inferior.id == *group && inferior.state == InferiorState::Stopped)
        }) else {
            return false;
        };
        self.stop_owner_thread_id.replace(Some(thread.id.clone()));
        self.stop_owner_inferior_id.replace(Some(group.clone()));
        if self.selected_inferior_id.borrow().as_deref() != Some(group.as_str()) {
            self.set_selected_inferior(&group);
            true
        } else {
            self.render_inferior_controls();
            false
        }
    }

    pub(crate) fn show_inferiors(&self, inferiors: Vec<InferiorInfo>) {
        let unresolved_stop_owner = self.stop_owner_inferior_id.borrow().is_none()
            && self.stop_owner_thread_id.borrow().is_some();
        if unresolved_stop_owner
            && let Some(owner) = self
                .stop_owner_thread_id
                .borrow()
                .as_deref()
                .and_then(|thread| {
                    inferiors
                        .iter()
                        .find(|inferior| {
                            inferior
                                .threads
                                .iter()
                                .any(|candidate| candidate.id == thread)
                        })
                        .map(|inferior| inferior.id.clone())
                })
        {
            self.stop_owner_inferior_id.replace(Some(owner));
        }
        self.update_local_inferior_relationships(&inferiors);
        let existing = self.selected_inferior_id.borrow().clone();
        let stop_owner = unresolved_stop_owner
            .then(|| self.stop_owner_inferior_id.borrow().clone())
            .flatten();
        let selected = stop_owner
            .filter(|id| inferiors.iter().any(|inferior| inferior.id == *id))
            .or_else(|| existing.filter(|id| inferiors.iter().any(|inferior| inferior.id == *id)))
            .or_else(|| {
                self.current_thread_id().and_then(|thread| {
                    inferiors
                        .iter()
                        .find(|inferior| {
                            inferior
                                .threads
                                .iter()
                                .any(|candidate| candidate.id == thread)
                        })
                        .map(|inferior| inferior.id.clone())
                })
            })
            .or_else(|| {
                inferiors
                    .iter()
                    .find(|inferior| inferior.state == InferiorState::Stopped)
                    .or_else(|| inferiors.iter().find(|inferior| inferior.state.is_live()))
                    .or_else(|| inferiors.first())
                    .map(|inferior| inferior.id.clone())
            });
        let selection_changed = self.selected_inferior_id.borrow().as_ref() != selected.as_ref();
        self.inferiors.replace(inferiors);
        self.selected_inferior_id.replace(selected);
        if selection_changed {
            self.reset_target_abi();
            self.invalidate_allocator_probe_cache();
            self.latest_modules.borrow_mut().clear();
            clear_box(&self.modules_list);
            self.modules_list
                .append(&empty_label("Modules refresh after selecting an inferior"));
        }
        self.apply_selected_inferior_state();
        self.render_inferior_controls();
    }

    pub(crate) fn set_selected_inferior(&self, id: &str) -> bool {
        if !self
            .inferiors
            .borrow()
            .iter()
            .any(|inferior| inferior.id == id)
        {
            return false;
        }
        if self.selected_inferior_id.borrow().as_deref() == Some(id) {
            return true;
        }
        self.selected_inferior_id.replace(Some(id.to_owned()));
        self.reset_target_abi();
        self.invalidate_allocator_probe_cache();
        self.latest_modules.borrow_mut().clear();
        clear_box(&self.modules_list);
        self.modules_list
            .append(&empty_label("Loading modules for the selected inferior"));
        self.apply_selected_inferior_state();
        self.render_inferior_controls();
        true
    }

    fn apply_selected_inferior_state(&self) {
        let selected = self.selected_inferior_id.borrow();
        let inferiors = self.inferiors.borrow();
        let inferior = inferiors
            .iter()
            .find(|inferior| Some(inferior.id.as_str()) == selected.as_deref());
        self.set_inferior_pid(inferior.and_then(|inferior| inferior.pid));
        self.set_inferior_started(
            inferior.is_some_and(|inferior| inferior.pid.is_some() && inferior.state.is_live()),
        );
        self.set_controls_running(
            inferior.is_some_and(|inferior| inferior.state == InferiorState::Running),
        );
        if let Some(inferior) = inferior {
            self.show_threads(&inferior.threads);
        } else {
            self.show_threads(&[]);
        }
    }

    pub(crate) fn mark_inferior_running(&self, thread_id: Option<&str>) -> bool {
        let pending_group = self.pending_execution_inferior.borrow_mut().take();
        let group = thread_id
            .filter(|thread| *thread != "all")
            .and_then(|thread| self.inferior_for_thread(thread))
            .or(pending_group);
        let all = group.is_none();
        let mut inferiors = self.inferiors.borrow_mut();
        for inferior in inferiors.iter_mut() {
            if (all || group.as_deref() == Some(inferior.id.as_str())) && inferior.state.is_live() {
                inferior.state = InferiorState::Running;
                for thread in &mut inferior.threads {
                    thread.state = String::from("running");
                }
            }
        }
        drop(inferiors);
        let selected_affected = all
            || group.as_deref() == self.selected_inferior_id.borrow().as_deref()
            || self.selected_inferior_id.borrow().is_none();
        if self.stop_owner_inferior_id.borrow().as_deref() == group.as_deref() || all {
            self.stop_owner_inferior_id.borrow_mut().take();
            self.stop_owner_thread_id.borrow_mut().take();
        }
        if selected_affected {
            self.apply_selected_inferior_state();
        }
        self.render_inferior_controls();
        selected_affected
    }

    pub(crate) fn mark_inferior_stopped(&self, thread_id: Option<&str>, all_stopped: bool) {
        self.pending_execution_inferior.borrow_mut().take();
        let group = thread_id.and_then(|thread| self.inferior_for_thread(thread));
        if let Some(thread_id) = thread_id {
            self.stop_owner_thread_id
                .replace(Some(thread_id.to_owned()));
        } else {
            self.stop_owner_thread_id.borrow_mut().take();
        }
        if let Some(group) = group.as_ref() {
            self.stop_owner_inferior_id.replace(Some(group.clone()));
            self.selected_inferior_id.replace(Some(group.clone()));
        } else {
            self.stop_owner_inferior_id.borrow_mut().take();
        }
        let mut inferiors = self.inferiors.borrow_mut();
        for inferior in inferiors.iter_mut() {
            if (all_stopped || group.as_deref() == Some(inferior.id.as_str()))
                && inferior.state.is_live()
            {
                inferior.state = InferiorState::Stopped;
                for thread in &mut inferior.threads {
                    thread.state = String::from("stopped");
                }
            }
        }
        drop(inferiors);
        if !self.inferiors.borrow().is_empty() {
            self.apply_selected_inferior_state();
        }
        self.render_inferior_controls();
    }

    pub(crate) fn record_pending_fork(&self, thread_id: Option<&str>, child_pid: Option<u32>) {
        let Some(child_pid) = child_pid else {
            return;
        };
        let parent = thread_id
            .and_then(|thread| self.inferior_for_thread(thread))
            .or_else(|| self.selected_inferior_id());
        if let Some(parent) = parent {
            self.pending_fork_parents
                .borrow_mut()
                .insert(child_pid, parent);
        }
    }

    pub(crate) fn record_inferior_started(&self, id: &str, pid: Option<u32>) {
        if let Some(parent) =
            pid.and_then(|pid| self.pending_fork_parents.borrow_mut().remove(&pid))
        {
            self.inferior_parents
                .borrow_mut()
                .insert(id.to_owned(), parent);
        }
    }

    pub(crate) fn record_inferior_exited(&self, id: &str) {
        if let Some(inferior) = self
            .inferiors
            .borrow_mut()
            .iter_mut()
            .find(|inferior| inferior.id == id)
        {
            inferior.state = InferiorState::Exited;
        }
        if self.stop_owner_inferior_id.borrow().as_deref() == Some(id) {
            self.stop_owner_inferior_id.borrow_mut().take();
            self.stop_owner_thread_id.borrow_mut().take();
        }
        if self.selected_inferior_id.borrow().as_deref() == Some(id) {
            self.apply_selected_inferior_state();
        }
        self.render_inferior_controls();
    }

    pub(crate) fn clear_inferiors(&self) {
        self.start_inferior_refresh();
        self.inferiors.borrow_mut().clear();
        self.selected_inferior_id.borrow_mut().take();
        self.stop_owner_inferior_id.borrow_mut().take();
        self.stop_owner_thread_id.borrow_mut().take();
        self.inferior_parents.borrow_mut().clear();
        self.pending_fork_parents.borrow_mut().clear();
        self.pending_execution_inferior.borrow_mut().take();
        self.inferior_action_pending.set(false);
        self.set_inferior_pid(None);
        self.render_inferior_controls();
    }

    pub(crate) fn set_fork_follow_mode(&self, mode: Option<ForkFollowMode>) {
        self.fork_follow_mode.set(mode);
        self.render_inferior_policy();
    }

    pub(crate) fn set_detach_on_fork(&self, detach: Option<bool>) {
        self.detach_on_fork.set(detach);
        self.render_inferior_policy();
    }

    pub(crate) fn set_inferior_action_pending(&self, pending: bool) {
        if self.inferior_action_pending.replace(pending) != pending {
            self.render_inferior_controls();
        }
    }

    pub(crate) fn set_pending_execution_inferior(&self, id: Option<String>) {
        self.pending_execution_inferior.replace(id);
    }

    pub(crate) fn stop_owner_summary(&self) -> Option<String> {
        let group = self.stop_owner_inferior_id.borrow().clone()?;
        let pid = self
            .inferiors
            .borrow()
            .iter()
            .find(|inferior| inferior.id == group)
            .and_then(|inferior| inferior.pid);
        Some(pid.map_or(group.clone(), |pid| format!("{group} PID {pid}")))
    }

    fn first_inferior_child(&self, parent: &str) -> Option<String> {
        let relationships = self.inferior_parents.borrow();
        self.inferiors
            .borrow()
            .iter()
            .find(|inferior| {
                relationships.get(&inferior.id).map(String::as_str) == Some(parent)
                    && inferior.state != InferiorState::Exited
            })
            .map(|inferior| inferior.id.clone())
    }

    fn update_local_inferior_relationships(&self, inferiors: &[InferiorInfo]) {
        let Some(debugger_pid) = self.debugger_pid() else {
            return;
        };
        let by_pid = inferiors
            .iter()
            .filter_map(|inferior| inferior.pid.map(|pid| (pid, inferior.id.clone())))
            .collect::<HashMap<_, _>>();
        let mut relationships = self.inferior_parents.borrow_mut();
        relationships.retain(|child, parent| {
            inferiors.iter().any(|inferior| inferior.id == *child)
                && inferiors.iter().any(|inferior| inferior.id == *parent)
        });
        for inferior in inferiors {
            let Some(pid) = inferior.pid else {
                continue;
            };
            let Some(parent) = crate::kernel::read_local_parent_pid(pid, debugger_pid)
                .and_then(|pid| by_pid.get(&pid))
            else {
                continue;
            };
            relationships.insert(inferior.id.clone(), parent.clone());
        }
    }

    fn render_inferior_policy(&self) {
        let controls = &self.inferior_controls;
        controls.selector_updating.set(true);
        let follow_parent = self.fork_follow_mode.get() == Some(ForkFollowMode::Parent);
        let follow_child = self.fork_follow_mode.get() == Some(ForkFollowMode::Child);
        let detach_on_fork = self.detach_on_fork.get().unwrap_or(true);
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
        let available = !self.inferior_action_pending.get();
        controls
            .follow_parent
            .set_sensitive(available && self.fork_follow_mode.get().is_some());
        controls
            .follow_child
            .set_sensitive(available && self.fork_follow_mode.get().is_some());
        controls
            .detach_on_fork
            .set_sensitive(available && self.detach_on_fork.get().is_some());
    }

    fn render_inferior_controls(&self) {
        let controls = &self.inferior_controls;
        let inferiors = self.inferiors.borrow();
        let selected = self.selected_inferior_id.borrow().clone();
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
        controls
            .selector
            .set_sensitive(!inferiors.is_empty() && !self.inferior_action_pending.get());
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
            let thread = self.stop_owner_thread_id.borrow();
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
            .and_then(|selected| self.inferior_parents.borrow().get(selected).cloned());
        let child = selected
            .as_deref()
            .and_then(|selected| self.first_inferior_child(selected));
        let available = !self.inferior_action_pending.get();
        controls
            .switch_parent
            .set_sensitive(available && parent.is_some());
        controls
            .switch_child
            .set_sensitive(available && child.is_some());
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
            if let Some(handler) = handler.borrow().clone() {
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
        let selected = self.selected_inferior_id.borrow().as_deref() == Some(&inferior.id);
        let stop_owner = self.stop_owner_inferior_id.borrow().as_deref() == Some(&inferior.id);
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
            .inferior_parents
            .borrow()
            .get(&inferior.id)
            .map(|parent| format!("child of {parent}"));
        if let Some(relationship) = relationship {
            set_label_text(&card.relationship, &relationship);
            card.relationship.set_visible(true);
        } else {
            card.relationship.set_visible(false);
        }
        let available = !self.inferior_action_pending.get();
        set_button_label(&card.select, if selected { "Selected" } else { "Switch" });
        card.select.set_sensitive(!selected && available);
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
        card.execution
            .set_sensitive(execution.is_some() && available);
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

fn set_label_text(label: &gtk::Label, text: &str) {
    if label.text().as_str() != text {
        label.set_text(text);
    }
}

fn set_button_label(button: &gtk::Button, text: &str) {
    if button.label().as_deref() != Some(text) {
        button.set_label(text);
    }
}

fn set_css_class(widget: &impl IsA<gtk::Widget>, class: &str, enabled: bool) {
    if enabled && !widget.has_css_class(class) {
        widget.add_css_class(class);
    } else if !enabled && widget.has_css_class(class) {
        widget.remove_css_class(class);
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

#[cfg(test)]
mod tests {
    use super::dispatch_inferior_card_action;
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
}
