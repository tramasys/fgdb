use super::*;

fn center_scroll_adjustment(scrolled: &gtk::ScrolledWindow, position: u32, item_count: u32) {
    if item_count == 0 {
        return;
    }

    let adjustment = scrolled.vadjustment();
    let lower = adjustment.lower();
    let upper = adjustment.upper();
    let page_size = adjustment.page_size();

    if !lower.is_finite()
        || !upper.is_finite()
        || !page_size.is_finite()
        || upper <= lower + page_size
    {
        return;
    }

    let row_fraction = (f64::from(position) + 0.5) / f64::from(item_count);
    let row_center = lower + (upper - lower) * row_fraction;
    let maximum = (upper - page_size).max(lower);
    adjustment.set_value((row_center - page_size / 2.0).clamp(lower, maximum));
}

fn preserve_stack_render_details(entries: &mut [StackEntry], previous: &[StackEntry]) {
    if previous.is_empty() || entries.iter().all(|entry| !entry.pointer_chain.is_empty()) {
        return;
    }

    let previous = previous
        .iter()
        .map(|entry| (entry.address, entry))
        .collect::<HashMap<_, _>>();

    for entry in entries {
        if !entry.pointer_chain.is_empty() {
            continue;
        }

        let Some(previous) = previous.get(&entry.address).copied() else {
            continue;
        };

        if previous.pointer_chain.is_empty()
            || previous.value != entry.value
            || previous.pointer_bits != entry.pointer_bits
            || previous.endian != entry.endian
            || previous.region != entry.region
        {
            continue;
        }

        entry.pointer_chain.clone_from(&previous.pointer_chain);

        if previous.memory_kind == MemoryKind::String {
            entry.memory_kind = MemoryKind::String;
        }
    }
}

fn locals_summary_text(
    locals: usize,
    arguments: usize,
    changed: usize,
    shown: usize,
    total: usize,
) -> String {
    let mut summary = format!(
        "{locals} local{}  {arguments} arg{}",
        if locals == 1 { "" } else { "s" },
        if arguments == 1 { "" } else { "s" },
    );

    if changed > 0 {
        summary.push_str(&format!("  {changed} changed"));
    }

    if shown < total {
        summary.push_str(&format!("  {shown}/{total} shown"));
    }

    summary
}

fn apply_variable_children_page_error(
    node: &VariableNode,
    parent: &Variable,
    from: usize,
    error: &str,
) {
    if from == 0 {
        node.children.splice(
            0,
            node.children.n_items(),
            &[glib::BoxedAnyObject::new(VariableNode::retry_expansion(
                parent.clone(),
                error,
            ))],
        );
    } else {
        remove_load_more_rows(&node.children);

        node.children
            .append(&glib::BoxedAnyObject::new(VariableNode::load_more_error(
                parent.clone(),
                from,
                error,
            )));
    }

    node.children_loading.set(false);
    node.children_loaded.set(true);
}

fn breakpoint_layout_matches(current: &[Breakpoint], incoming: &[Breakpoint]) -> bool {
    current.len() == incoming.len()
        && current.iter().zip(incoming).all(|(current, incoming)| {
            let mut current = current.clone();
            let mut incoming = incoming.clone();
            current.hit_count = 0;
            current.ignore_count = 0;
            incoming.hit_count = 0;
            incoming.ignore_count = 0;

            current == incoming
        })
}

fn breakpoint_status_text(breakpoint: &Breakpoint) -> String {
    let mut status = Vec::new();

    if breakpoint.hit_count > 0 {
        status.push(format!(
            "{} HIT{}",
            breakpoint.hit_count,
            if breakpoint.hit_count == 1 { "" } else { "S" }
        ));
    }

    if let Some(thread) = breakpoint.thread.as_deref() {
        status.push(format!("THREAD {thread}"));
    }

    if let Some(inferior) = breakpoint.inferior.as_deref() {
        status.push(format!("INFERIOR {inferior}"));
    }

    if breakpoint.ignore_count > 0 {
        status.push(format!(
            "STOP ON HIT {}",
            breakpoint.ignore_count.saturating_add(1)
        ));
    }

    if breakpoint.disposition.as_deref() == Some("del") {
        status.push(String::from("TEMPORARY"));
    }

    if breakpoint.pending.is_some() {
        status.push(String::from("PENDING"));
    }

    if breakpoint.location_count > 0 {
        status.push(format!(
            "{} LOCATION{}",
            breakpoint.location_count,
            if breakpoint.location_count == 1 {
                ""
            } else {
                "S"
            }
        ));
    }

    if breakpoint.is_logpoint() {
        status.push(String::from("AUTO-CONTINUE"));
    } else if !breakpoint.commands.is_empty() {
        status.push(format!(
            "{} COMMAND{}",
            breakpoint.commands.len(),
            if breakpoint.commands.len() == 1 {
                ""
            } else {
                "S"
            }
        ));
    }

    status.join("  ·  ")
}

fn bounded_stack_frames(
    frames: &[StackFrame],
    limit: usize,
    selected_level: u32,
) -> Vec<&StackFrame> {
    let mut visible = frames.iter().take(limit).collect::<Vec<_>>();

    if limit > 0
        && let Some(selected) = frames.iter().find(|frame| frame.level == selected_level)
        && !visible.iter().any(|frame| frame.level == selected_level)
    {
        visible.pop();
        visible.insert(0, selected);
    }

    visible
}

impl Ui {
    pub(crate) fn current_thread_id(&self) -> Option<String> {
        self.selected_thread_id.borrow().clone()
    }

    pub(crate) fn set_current_thread_id(&self, thread_id: Option<&str>) {
        let mut selected = self.selected_thread_id.borrow_mut();

        if selected.as_deref() != thread_id {
            *selected = thread_id.map(str::to_owned);
        }
    }

    pub fn show_frames(&self, frames: &[StackFrame]) {
        let render_started = Instant::now();
        self.latest_frames_generation.set(None);

        if self.latest_frames.borrow().as_slice() == frames {
            return;
        }

        let selected_level = self.selected_frame_level.get();

        let widget_limit = self.adaptive_render_limit(
            "call-stack pane",
            crate::performance::STACK_FRAME_WIDGET_BUDGET,
            64,
        );

        let rendered_frames = bounded_stack_frames(frames, widget_limit, selected_level);

        let can_update_in_place = {
            let latest = self.latest_frames.borrow();
            let previous_frames = bounded_stack_frames(&latest, widget_limit, selected_level);
            let buttons = self.frame_buttons.borrow();

            previous_frames.len() == rendered_frames.len()
                && buttons.len() == rendered_frames.len()
                && previous_frames
                    .iter()
                    .zip(&rendered_frames)
                    .all(|(previous, current)| previous.level == current.level)
        };

        if can_update_in_place {
            let latest = self.latest_frames.borrow();
            let previous_frames = bounded_stack_frames(&latest, widget_limit, selected_level);

            for (((level, button), previous), frame) in self
                .frame_buttons
                .borrow()
                .iter()
                .zip(previous_frames)
                .zip(rendered_frames.iter().copied())
            {
                debug_assert_eq!(*level, frame.level);

                if previous != frame {
                    update_frame_button(button, frame);
                }
            }

            drop(latest);
            self.latest_frames.replace(frames.to_vec());

            update_selected_frame_buttons(
                &self.frame_buttons.borrow(),
                self.selected_frame_level.get(),
            );

            self.update_thread_control_sensitivity();
            self.record_ui_render_duration("call-stack pane", render_started);
            return;
        }

        self.latest_frames.replace(frames.to_vec());
        clear_box(&self.call_stack_list);
        self.frame_buttons.borrow_mut().clear();

        if frames.is_empty() {
            self.call_stack_list
                .append(&empty_label("No stack frames available"));

            self.record_ui_render_duration("call-stack pane", render_started);
            return;
        }

        for frame in &rendered_frames {
            let row = gtk::Box::new(gtk::Orientation::Vertical, 0);
            let displayed_function = compact_function_name(&frame.function);

            let function =
                gtk::Label::new(Some(&format!("#{}  {displayed_function}", frame.level)));

            function.set_halign(gtk::Align::Start);
            function.set_ellipsize(pango::EllipsizeMode::End);
            function.set_tooltip_text(Some(&frame.function));
            let location_text = frame_location_text(frame);
            let location = gtk::Label::new(Some(&location_text));
            location.add_css_class("muted");
            location.set_halign(gtk::Align::Start);
            location.set_ellipsize(pango::EllipsizeMode::Middle);
            location.set_tooltip_text(Some(&location_text));
            row.append(&function);
            row.append(&location);
            let button = gtk::Button::builder().child(&row).build();
            button.add_css_class("stack-frame");

            if frame.level == self.selected_frame_level.get() {
                button.add_css_class("current-debug-item");
            }

            let level = frame.level;
            let handler = Rc::clone(&self.frame_selection_handler);

            button.connect_clicked(move |_| {
                let handler = handler.borrow().clone();

                if let Some(handler) = handler {
                    handler(level);
                }
            });

            self.frame_buttons
                .borrow_mut()
                .push((level, button.clone()));

            self.call_stack_list.append(&button);
        }

        if rendered_frames.len() < frames.len() {
            let omitted = frames.len() - rendered_frames.len();

            let notice = performance_partial_label(&format!(
                "{omitted} deeper frame{} not rendered",
                if omitted == 1 { " was" } else { "s were" }
            ));

            self.call_stack_list.append(&notice);

            self.record_performance_notice(crate::performance::PerformanceNotice::count(
                crate::performance::BudgetOutcome::Partial,
                "call-stack pane",
                rendered_frames.len(),
                frames.len(),
            ));
        }

        self.update_thread_control_sensitivity();
        self.record_ui_render_duration("call-stack pane", render_started);
    }

    pub(crate) fn select_frame_in_view(&self, level: u32) {
        self.selected_frame_level.set(level);
        update_selected_frame_buttons(&self.frame_buttons.borrow(), level);
        self.update_thread_control_sensitivity();
    }

    pub fn show_locals(&self, variables: &[Variable]) {
        self.locals_generation.set(None);

        self.locals_render_limit.set(self.adaptive_render_limit(
            "locals pane",
            crate::performance::LOCALS_ROOT_PAGE_SIZE,
            64,
        ));

        self.local_variables.borrow_mut().replace(variables);
        self.render_locals();
    }

    fn render_locals(&self) {
        let render_started = Instant::now();
        let query = self.locals_filter.text().trim().to_ascii_lowercase();
        let limit = self.locals_render_limit.get();

        let (rendered, matching_total, root_count, arguments) = {
            let variables = self.local_variables.borrow();
            let (rendered, matching_total) = variables.filtered(&query, limit);

            (
                rendered,
                matching_total,
                variables.len(),
                variables.argument_count(),
            )
        };

        let shown = rendered.len();

        let selected_name = variable_at(&self.locals_selection, self.locals_selection.selected())
            .map(|variable| (variable.name, variable.argument));

        let changed = replace_variable_roots_if_changed(&self.locals_store, &rendered);

        if changed != VariableRootChange::Unchanged {
            self.rebuild_variable_node_index();
        }

        let locals = root_count.saturating_sub(arguments);
        let changed_count = changed_variable_roots(&self.locals_store);

        self.locals_summary.set_text(&locals_summary_text(
            locals,
            arguments,
            changed_count,
            shown,
            matching_total,
        ));

        let remaining = matching_total.saturating_sub(shown);
        self.locals_more_button.set_visible(remaining > 0);

        if remaining > 0 {
            let next = remaining.min(self.adaptive_render_limit(
                "locals pane",
                crate::performance::LOCALS_ROOT_PAGE_SIZE,
                64,
            ));

            self.locals_more_button.set_label(&format!(
                "Show {next} more value{}",
                if next == 1 { "" } else { "s" }
            ));
        }

        if rendered.is_empty() {
            self.locals_empty.set_text(if root_count == 0 {
                "Values appear when the target is paused"
            } else {
                "No locals or arguments match the filter"
            });

            self.locals_empty.set_visible(true);
            self.locals_edit_button.set_sensitive(false);
        } else {
            self.locals_empty.set_visible(false);

            if changed == VariableRootChange::Rebuilt {
                self.locals_selection
                    .set_selected(gtk::INVALID_LIST_POSITION);

                let selected = selected_name
                    .as_ref()
                    .and_then(|(name, argument)| {
                        root_variable_position(&self.locals_selection, name, *argument)
                    })
                    .unwrap_or(0);

                self.locals_selection.set_selected(selected);
            }

            self.locals_edit_button.set_sensitive(
                variable_at(&self.locals_selection, self.locals_selection.selected())
                    .is_some_and(|variable| variable.is_available()),
            );
        }

        self.record_ui_render_duration("locals pane", render_started);
    }

    pub fn show_frames_for_refresh(&self, generation: u64, frames: &[StackFrame]) {
        if self.is_stop_refresh_current(generation) {
            self.show_frames(frames);
            self.latest_frames_generation.set(Some(generation));
        }
    }

    pub(crate) fn frames_for_details(&self, generation: u64) -> Option<Vec<StackFrame>> {
        (self.latest_frames_generation.get() == Some(generation))
            .then(|| self.latest_frames.borrow().clone())
    }

    pub fn show_locals_for_refresh(&self, generation: u64, variables: &[Variable]) {
        if self.is_stop_refresh_current(generation) {
            if self.locals_generation.replace(Some(generation)) != Some(generation) {
                self.locals_render_limit.set(self.adaptive_render_limit(
                    "locals pane",
                    crate::performance::LOCALS_ROOT_PAGE_SIZE,
                    64,
                ));
            }

            self.local_variables.borrow_mut().replace(variables);
            self.render_locals();
        }
    }

    pub fn show_local_root_for_refresh(&self, generation: u64, index: usize, variable: &Variable) {
        if !self.is_stop_refresh_current(generation) {
            return;
        }

        self.local_variables.borrow_mut().update(index, variable);

        if index < self.locals_store.n_items() as usize {
            let previous = variable_root_node(&self.locals_store, index);

            if replace_variable_root(&self.locals_store, index, variable, false) {
                self.reindex_variable_root(&self.locals_store, index, previous.as_ref());
            }
        }
    }

    pub fn show_variable_descendant_updates_for_refresh(
        &self,
        generation: u64,
        updates: &[VariableUpdate],
    ) {
        if !self.is_stop_refresh_current(generation) || updates.is_empty() {
            return;
        }

        let locals_updated = apply_variable_updates(&self.locals_store, updates);
        let watches_updated = apply_variable_updates(&self.expression_watches_store, updates);

        if (locals_updated > 0 || watches_updated > 0)
            && updates.iter().any(|update| {
                update.type_changed
                    || update.new_num_children.is_some()
                    || update.in_scope == Some(false)
                    || update.dynamic.is_some()
            })
        {
            self.rebuild_variable_node_index();
        }

        if locals_updated > 0 {
            refresh_changed_variable_roots(&self.locals_store);
            let roots = self.local_variables.borrow();
            let arguments = roots.argument_count();

            self.locals_summary.set_text(&locals_summary_text(
                roots.len().saturating_sub(arguments),
                arguments,
                changed_variable_roots(&self.locals_store),
                self.locals_store.n_items() as usize,
                roots.len(),
            ));
        }
    }

    pub fn show_variable_children_page(
        &self,
        parent: &Variable,
        from: usize,
        variables: &[Variable],
        has_more: bool,
    ) -> bool {
        let Some(parent_name) = parent.varobj.as_deref() else {
            return false;
        };

        let Some(node) = self.find_variable_node(parent_name) else {
            return false;
        };

        if from == 0 {
            self.variable_node_index
                .borrow_mut()
                .remove_store(&node.children);
        }

        if from != 0 {
            remove_load_more_rows(&node.children);
        }

        let new_nodes = variables
            .iter()
            .cloned()
            .map(VariableNode::new)
            .collect::<Vec<_>>();

        let mut additions = new_nodes
            .iter()
            .cloned()
            .map(glib::BoxedAnyObject::new)
            .collect::<Vec<_>>();

        if has_more {
            additions.push(glib::BoxedAnyObject::new(VariableNode::load_more(
                parent.clone(),
                from.saturating_add(variables.len()),
            )));
        }

        if from == 0 {
            node.children.splice(0, node.children.n_items(), &additions);
        } else {
            node.children.extend_from_slice(&additions);
        }

        node.children_loading.set(false);
        node.children_loaded.set(true);
        let mut index = self.variable_node_index.borrow_mut();

        for child in new_nodes {
            index.insert(child);
        }

        true
    }

    pub fn show_variable_children(&self, parent: &str, variables: &[Variable]) -> bool {
        let Some(node) = self.find_variable_node(parent) else {
            return false;
        };

        let parent = node.variable;

        self.show_variable_children_page(&parent, 0, variables, false)
    }

    pub fn has_variable_object(&self, varobj: &str) -> bool {
        self.variable_node_index.borrow().contains(varobj)
    }

    pub fn show_variable_children_error(&self, parent: &str, error: &str) {
        let Some(node) = self.find_variable_node(parent) else {
            return;
        };

        self.variable_node_index
            .borrow_mut()
            .remove_store(&node.children);

        apply_variable_children_page_error(&node, &node.variable, 0, error);
    }

    pub fn show_variable_children_page_error(&self, parent: &Variable, from: usize, error: &str) {
        let Some(parent_name) = parent.varobj.as_deref() else {
            return;
        };

        let Some(node) = self.find_variable_node(parent_name) else {
            return;
        };

        if from == 0 {
            self.variable_node_index
                .borrow_mut()
                .remove_store(&node.children);
        }

        apply_variable_children_page_error(&node, parent, from, error);
    }

    pub(crate) fn show_lazy_variable_children_error(&self, variable: &Variable, error: &str) {
        let Some((_, node)) = self.local_variable_node(variable) else {
            return;
        };

        self.variable_node_index
            .borrow_mut()
            .remove_store(&node.children);

        apply_variable_children_page_error(&node, variable, 0, error);
    }

    pub(crate) fn has_local_variable_identity(&self, variable: &Variable) -> bool {
        self.local_variable_node(variable).is_some()
    }

    pub(crate) fn claim_local_variable_object(&self, generation: u64, variable: &Variable) -> bool {
        self.is_stop_refresh_current(generation)
            && self.has_local_variable_identity(variable)
            && self.pending_local_variable_objects.borrow_mut().insert((
                generation,
                variable.name.clone(),
                variable.argument,
            ))
    }

    pub(crate) fn finish_local_variable_object(&self, generation: u64, variable: &Variable) {
        self.pending_local_variable_objects.borrow_mut().remove(&(
            generation,
            variable.name.clone(),
            variable.argument,
        ));
    }

    pub(crate) fn attach_local_variable_object(
        &self,
        generation: u64,
        original: &Variable,
        variable: &Variable,
    ) -> bool {
        if !self.is_stop_refresh_current(generation) {
            return false;
        }

        let Some((position, _)) = self.local_variable_node(original) else {
            return false;
        };

        self.local_variables
            .borrow_mut()
            .update(position as usize, variable);

        let previous = variable_root_node(
            &self.locals_store,
            usize::try_from(position).unwrap_or(usize::MAX),
        );

        let replaced = replace_variable_root(
            &self.locals_store,
            usize::try_from(position).unwrap_or(usize::MAX),
            variable,
            false,
        );

        if replaced {
            self.reindex_variable_root(
                &self.locals_store,
                usize::try_from(position).unwrap_or(usize::MAX),
                previous.as_ref(),
            );
        }

        replaced
    }

    pub fn local_variable_objects(&self) -> Vec<Variable> {
        self.local_variables.borrow().to_vec()
    }

    pub(crate) fn rendered_local_variable_count(&self) -> usize {
        self.locals_store.n_items() as usize
    }

    fn find_variable_node(&self, varobj: &str) -> Option<VariableNode> {
        self.variable_node_index.borrow().get(varobj)
    }

    pub(super) fn rebuild_variable_node_index(&self) {
        let mut index = self.variable_node_index.borrow_mut();
        index.rebuild(&self.locals_store, &self.expression_watches_store);
    }

    pub(super) fn reindex_variable_root(
        &self,
        store: &gio::ListStore,
        position: usize,
        previous: Option<&VariableNode>,
    ) {
        let mut index = self.variable_node_index.borrow_mut();

        if let Some(previous) = previous {
            index.remove_node(previous);
        }

        let Ok(position) = u32::try_from(position) else {
            return;
        };

        let Some(item) = store.item(position).and_downcast::<glib::BoxedAnyObject>() else {
            return;
        };

        let node = item.borrow::<VariableNode>().clone();
        index.insert(node.clone());
        index.index_store(&node.children);
    }

    fn local_variable_node(&self, variable: &Variable) -> Option<(u32, VariableNode)> {
        (0..self.locals_store.n_items()).find_map(|position| {
            let item = self
                .locals_store
                .item(position)
                .and_downcast::<glib::BoxedAnyObject>()?;

            let node = item.borrow::<VariableNode>().clone();

            (!node.placeholder
                && node.variable.name == variable.name
                && node.variable.argument == variable.argument
                && node.variable.varobj == variable.varobj)
                .then_some((position, node))
        })
    }

    pub(crate) fn connect_local_paging(self: &Rc<Self>) {
        let weak_ui = Rc::downgrade(self);

        self.locals_more_button.connect_clicked(move |_| {
            if let Some(ui) = weak_ui.upgrade() {
                let page = ui.adaptive_render_limit(
                    "locals pane",
                    crate::performance::LOCALS_ROOT_PAGE_SIZE,
                    64,
                );

                ui.locals_render_limit
                    .set(ui.locals_render_limit.get().saturating_add(page));

                ui.render_locals();
            }
        });

        let weak_ui = Rc::downgrade(self);

        self.locals_filter.connect_changed(move |_| {
            if let Some(ui) = weak_ui.upgrade() {
                ui.locals_render_limit.set(ui.adaptive_render_limit(
                    "locals pane",
                    crate::performance::LOCALS_ROOT_PAGE_SIZE,
                    64,
                ));

                ui.render_locals();
            }
        });
    }

    pub(super) fn connect_local_activation(&self) {
        let window = self.window.clone();
        let selection = self.locals_selection.clone();
        let handler = Rc::clone(&self.variable_assignment_handler);
        let float_handler = Rc::clone(&self.float_assignment_handler);
        let editor_handler = Rc::clone(&self.variable_editor_handler);
        let string_handler = Rc::clone(&self.string_assignment_handler);
        let children_handler = Rc::clone(&self.variable_children_handler);
        let target_pointer_bits = Rc::clone(&self.target_pointer_bits);
        let target_architecture = Rc::clone(&self.target_architecture);
        let current_source_is_rust = Rc::clone(&self.current_source_is_rust);
        let debugger_ready = Rc::clone(&self.debugger_ready);
        let debugger_state = Rc::clone(&self.debugger_state);
        let command_pending = Rc::clone(&self.command_pending);
        let session_pending = Rc::clone(&self.session_pending);

        self.locals_view.connect_activate(move |_, position| {
            if !debugger_ready.get()
                || !debugger_state.get().inferior_started()
                || debugger_state.get().inferior_running()
                || command_pending.get()
                || session_pending.get()
            {
                return;
            }

            let Some((row, node)) = variable_node_at(&selection, position) else {
                return;
            };

            if node.load_more.is_some() {
                request_next_variable_page_if_needed(&node, &children_handler);
            } else if !node.placeholder {
                if row.is_expandable() {
                    let expanded = !row.is_expanded();
                    node.expanded.set(expanded);
                    row.set_expanded(expanded);

                    if expanded {
                        request_variable_children_if_needed(&node, &children_handler);
                    }
                } else {
                    let variable = node.variable;

                    if !variable.is_available()
                        && variable.value.trim().starts_with("<not available")
                    {
                        let handler = children_handler.borrow().clone();

                        if let Some(handler) = handler {
                            handler(variable, 0);
                        }

                        return;
                    }

                    if !variable.is_available() {
                        return;
                    }

                    let editor_handler = editor_handler.borrow().clone();

                    if let Some(editor_handler) = editor_handler {
                        editor_handler(variable);
                    } else {
                        open_variable_editor(
                            &window,
                            variable,
                            target_pointer_bits.get(),
                            target_architecture.get(),
                            current_source_is_rust.get(),
                            None,
                            ValueEditorHandlers {
                                assignment: Rc::clone(&handler),
                                float: Rc::clone(&float_handler),
                                string: Rc::clone(&string_handler),
                            },
                        );
                    }
                }
            }
        });

        let window = self.window.clone();
        let selection = self.locals_selection.clone();
        let handler = Rc::clone(&self.variable_assignment_handler);
        let float_handler = Rc::clone(&self.float_assignment_handler);
        let editor_handler = Rc::clone(&self.variable_editor_handler);
        let string_handler = Rc::clone(&self.string_assignment_handler);
        let target_pointer_bits = Rc::clone(&self.target_pointer_bits);
        let target_architecture = Rc::clone(&self.target_architecture);
        let current_source_is_rust = Rc::clone(&self.current_source_is_rust);

        self.locals_edit_button.connect_clicked(move |_| {
            if let Some(variable) = variable_at(&selection, selection.selected())
                && variable.is_available()
            {
                let editor_handler = editor_handler.borrow().clone();

                if let Some(editor_handler) = editor_handler {
                    editor_handler(variable);
                } else {
                    open_variable_editor(
                        &window,
                        variable,
                        target_pointer_bits.get(),
                        target_architecture.get(),
                        current_source_is_rust.get(),
                        None,
                        ValueEditorHandlers {
                            assignment: Rc::clone(&handler),
                            float: Rc::clone(&float_handler),
                            string: Rc::clone(&string_handler),
                        },
                    );
                }
            }
        });

        let edit_button = self.locals_edit_button.clone();
        let ready = Rc::clone(&self.debugger_ready);
        let debugger_state = Rc::clone(&self.debugger_state);
        let pending = Rc::clone(&self.command_pending);

        self.locals_selection
            .connect_selected_notify(move |selection| {
                edit_button.set_sensitive(
                    ready.get()
                        && debugger_state.get().inferior_started()
                        && !debugger_state.get().inferior_running()
                        && !pending.get()
                        && variable_at(selection, selection.selected())
                            .is_some_and(|variable| variable.is_available()),
                );
            });
    }

    pub(super) fn connect_register_activation(&self) {
        for group in &self.register_groups {
            let parent = self.window.clone();
            let store = group.store.clone();
            let handler = Rc::clone(&self.variable_assignment_handler);
            let float_handler = Rc::clone(&self.float_assignment_handler);
            let string_handler = Rc::clone(&self.string_assignment_handler);
            let vector_handler = Rc::clone(&self.vector_assignment_handler);
            let target_pointer_bits = Rc::clone(&self.target_pointer_bits);
            let target_architecture = Rc::clone(&self.target_architecture);
            let current_source_is_rust = Rc::clone(&self.current_source_is_rust);
            let debugger_ready = Rc::clone(&self.debugger_ready);
            let debugger_state = Rc::clone(&self.debugger_state);
            let command_pending = Rc::clone(&self.command_pending);
            let session_pending = Rc::clone(&self.session_pending);

            group.view.connect_activate(move |_, position| {
                if !debugger_ready.get()
                    || !debugger_state.get().inferior_started()
                    || debugger_state.get().inferior_running()
                    || command_pending.get()
                    || session_pending.get()
                {
                    return;
                }

                let Some(item) = store
                    .item(position)
                    .and_then(|item| item.downcast::<glib::BoxedAnyObject>().ok())
                else {
                    return;
                };

                let register = item.borrow::<RegisterRowData>().register.clone();

                if matches!(register.name.as_str(), "eflags" | "rflags") {
                    open_flag_editor(&parent, register, Rc::clone(&handler));
                } else if vector_register_bytes(&register.name).is_some() {
                    open_vector_editor(&parent, register, Rc::clone(&vector_handler));
                } else {
                    open_variable_editor(
                        &parent,
                        Variable {
                            name: format!("${}", register.name),
                            value: register.value,
                            type_name: None,
                            argument: false,
                            varobj: None,
                            num_children: 0,
                            has_more: false,
                            display_hint: None,
                            dynamic: false,
                        },
                        target_pointer_bits.get(),
                        target_architecture.get(),
                        current_source_is_rust.get(),
                        None,
                        ValueEditorHandlers {
                            assignment: Rc::clone(&handler),
                            float: Rc::clone(&float_handler),
                            string: Rc::clone(&string_handler),
                        },
                    );
                }
            });
        }
    }

    pub fn show_threads(&self, threads: &[ThreadInfo]) {
        let render_started = Instant::now();
        let explicit_current = threads.iter().find(|thread| thread.current);

        let retained_current = if explicit_current.is_none() {
            self.current_thread_id()
                .filter(|selected| threads.iter().any(|thread| thread.id == selected.as_str()))
        } else {
            None
        };

        let normalized_threads = retained_current.as_ref().map(|selected| {
            let mut normalized = threads.to_vec();

            if let Some(thread) = normalized
                .iter_mut()
                .find(|thread| thread.id == selected.as_str())
            {
                thread.current = true;
            }

            normalized
        });

        let threads = normalized_threads.as_deref().unwrap_or(threads);

        self.set_current_thread_id(
            explicit_current
                .map(|thread| thread.id.as_str())
                .or(retained_current.as_deref()),
        );

        let executable_name = self
            .current_session
            .borrow()
            .as_ref()
            .and_then(DebugSession::executable)
            .and_then(std::path::Path::file_name)
            .and_then(std::ffi::OsStr::to_str)
            .map(str::to_owned);

        let stop_reason = self.thread_stop_reason.borrow().clone();
        let (query, state_filter, sort) = self.current_thread_filter_state();

        let thread_limit =
            self.adaptive_render_limit("thread pane", crate::performance::THREAD_WIDGET_BUDGET, 32);

        let (rendered_threads, visible_thread_count) =
            Self::filtered_sorted_thread_page(threads, &query, state_filter, sort, thread_limit);

        let rendered_thread_count = rendered_threads.len();
        let omitted_thread_count = visible_thread_count.saturating_sub(rendered_thread_count);

        if self.latest_threads.borrow().as_ref().is_some_and(|state| {
            state.source_threads == threads
                && state.rendered_threads == rendered_threads
                && state.stop_reason == stop_reason
                && state.executable_name == executable_name
                && state.query == query
                && state.state_filter == state_filter
                && state.sort == sort
        }) {
            return;
        }

        self.kernel_view
            .set_tls_thread(threads, executable_name.as_deref());

        let can_update_in_place = self
            .latest_threads
            .borrow()
            .as_ref()
            .is_some_and(|previous| {
                previous.rendered_threads.len() == rendered_threads.len()
                    && self.thread_buttons.borrow().len() == rendered_threads.len()
                    && previous
                        .rendered_threads
                        .iter()
                        .zip(&rendered_threads)
                        .all(|(previous, current)| previous.id == current.id)
            });

        if can_update_in_place {
            let latest = self.latest_threads.borrow();

            let Some(previous) = latest.as_ref() else {
                return;
            };

            for (((_, button), old_thread), thread) in self
                .thread_buttons
                .borrow()
                .iter()
                .zip(previous.rendered_threads.iter())
                .zip(&rendered_threads)
            {
                let reason = thread
                    .current
                    .then(|| stop_reason.as_deref().unwrap_or("STOPPED"));

                let old_reason = old_thread
                    .current
                    .then(|| previous.stop_reason.as_deref().unwrap_or("STOPPED"));

                if old_thread != thread || old_reason != reason {
                    update_thread_button(button, thread, reason);
                }
            }

            drop(latest);

            self.latest_threads.replace(Some(ThreadRenderState {
                source_threads: threads.to_vec(),
                rendered_threads,
                stop_reason,
                executable_name,
                query,
                state_filter,
                sort,
            }));

            self.sync_thread_controls(threads, visible_thread_count);
            sync_thread_partial_notice(&self.threads_list, omitted_thread_count);

            if omitted_thread_count > 0 {
                self.record_performance_notice(crate::performance::PerformanceNotice::count(
                    crate::performance::BudgetOutcome::Partial,
                    "thread pane",
                    rendered_thread_count,
                    visible_thread_count,
                ));
            }

            self.record_ui_render_duration("thread pane", render_started);
            return;
        }

        self.latest_threads.replace(Some(ThreadRenderState {
            source_threads: threads.to_vec(),
            rendered_threads: rendered_threads.clone(),
            stop_reason: stop_reason.clone(),
            executable_name,
            query,
            state_filter,
            sort,
        }));

        self.sync_thread_controls(threads, visible_thread_count);
        clear_box(&self.threads_list);
        self.thread_buttons.borrow_mut().clear();

        if rendered_threads.is_empty() {
            self.threads_list
                .append(&empty_label(if threads.is_empty() {
                    "No threads available"
                } else {
                    "No threads match the current filter"
                }));

            self.record_ui_render_duration("thread pane", render_started);
            return;
        }

        for thread in &rendered_threads {
            let reason = thread
                .current
                .then(|| stop_reason.as_deref().unwrap_or("STOPPED"));

            let row = thread_button_content(thread, reason);
            let button = gtk::Button::builder().child(&row).build();
            button.add_css_class("stack-frame");

            if thread.current {
                button.add_css_class("current-debug-item");
            }

            let id = thread.id.clone();
            let handler = Rc::clone(&self.thread_selection_handler);

            button.connect_clicked(move |_| {
                let handler = handler.borrow().clone();

                if let Some(handler) = handler {
                    handler(id.clone());
                }
            });

            self.thread_buttons
                .borrow_mut()
                .push((thread.id.clone(), button.clone()));

            self.threads_list.append(&button);
        }

        sync_thread_partial_notice(&self.threads_list, omitted_thread_count);

        if omitted_thread_count > 0 {
            self.record_performance_notice(crate::performance::PerformanceNotice::count(
                crate::performance::BudgetOutcome::Partial,
                "thread pane",
                rendered_thread_count,
                visible_thread_count,
            ));
        }

        self.update_thread_control_sensitivity();
        self.record_ui_render_duration("thread pane", render_started);
    }

    pub fn show_modules(&self, modules: &[SharedLibrary]) -> bool {
        if self.latest_modules.borrow().as_slice() == modules {
            return false;
        }

        let render_started = Instant::now();
        self.latest_modules.replace(modules.to_vec());
        self.reset_debug_data_module_paging();

        if modules.is_empty() {
            self.module_debug_metadata.borrow_mut().clear();
        }

        self.render_debug_data_overview();
        self.render_debug_data_modules();
        clear_box(&self.modules_list);

        if modules.is_empty() {
            self.modules_list
                .append(&empty_label("No shared libraries loaded"));

            self.record_ui_render_duration("module pane", render_started);
            return true;
        }

        let module_limit =
            self.adaptive_render_limit("module pane", crate::performance::MODULE_WIDGET_BUDGET, 32);

        for module in modules.iter().take(module_limit) {
            let row = gtk::Box::new(gtk::Orientation::Vertical, 0);
            row.add_css_class("module-row");
            let heading = gtk::Box::new(gtk::Orientation::Horizontal, 4);

            let name = Path::new(&module.target_name)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(&module.target_name);

            let name = gtk::Label::new(Some(name));
            name.add_css_class("module-name");
            name.set_halign(gtk::Align::Start);
            name.set_hexpand(true);
            name.set_ellipsize(pango::EllipsizeMode::End);

            let symbol_state = gtk::Label::new(Some(if module.symbols_loaded {
                "SYMBOLS"
            } else {
                "NO SYMBOLS"
            }));

            symbol_state.add_css_class("module-symbol-state");

            symbol_state.add_css_class(if module.symbols_loaded {
                "module-symbols-loaded"
            } else {
                "module-symbols-missing"
            });

            heading.append(&name);
            heading.append(&symbol_state);

            let range = match (&module.from, &module.to) {
                (Some(from), Some(to)) => format!("{from}-{to}"),
                _ => String::from("address range unavailable"),
            };

            let range = gtk::Label::new(Some(&range));
            range.add_css_class("module-range");
            range.set_halign(gtk::Align::Start);
            enable_stable_text_selection(&range);
            let path = module.host_name.as_deref().unwrap_or(&module.target_name);
            let path_label = gtk::Label::new(Some(path));
            path_label.add_css_class("module-path");
            path_label.set_halign(gtk::Align::Start);
            path_label.set_ellipsize(pango::EllipsizeMode::Middle);
            enable_stable_text_selection(&path_label);

            path_label.set_tooltip_text(Some(&format!(
                "Target: {}\nHost: {}",
                module.target_name, path
            )));

            row.append(&heading);
            row.append(&range);
            row.append(&path_label);
            self.modules_list.append(&row);
        }

        if modules.len() > module_limit {
            let shown = module_limit;
            let omitted = modules.len() - shown;

            let notice = performance_partial_label(&format!(
                "{omitted} additional module{} available in Debug Data",
                if omitted == 1 { " is" } else { "s are" }
            ));

            self.modules_list.append(&notice);

            self.record_performance_notice(crate::performance::PerformanceNotice::count(
                crate::performance::BudgetOutcome::Partial,
                "module pane",
                shown,
                modules.len(),
            ));
        }

        self.record_ui_render_duration("module pane", render_started);

        true
    }

    pub fn start_thread_refresh(&self) -> u64 {
        let generation = self.thread_refresh_generation.get().wrapping_add(1);
        self.thread_refresh_generation.set(generation);

        generation
    }

    pub fn show_threads_for_refresh(&self, generation: u64, threads: &[ThreadInfo]) {
        if self.is_thread_refresh_current(generation) {
            self.show_threads(threads);
        }
    }

    pub fn is_thread_refresh_current(&self, generation: u64) -> bool {
        self.thread_refresh_generation.get() == generation
    }

    pub fn show_instructions(
        &self,
        instructions: Vec<Instruction>,
        pc: &str,
        focus: &str,
        architecture: Option<&str>,
        mixed: bool,
    ) {
        if let Some(description) = architecture {
            let detected = TargetArchitecture::from_gdb_description(description);

            if detected != TargetArchitecture::Unknown {
                self.set_target_architecture(detected);
            }

            if let Some(bits) =
                TargetArchitecture::explicit_pointer_bits_from_gdb_description(description)
            {
                self.set_target_pointer_bits(bits);
            }

            if let Some(endian) = TargetEndian::from_architecture_description(description) {
                self.set_target_endian(Some(endian));
            }
        }

        self.disassembly_controls.source_column.set_visible(mixed);

        let syntax_applicable = matches!(
            self.target_architecture(),
            TargetArchitecture::X86 | TargetArchitecture::X86_64
        );

        self.disassembly_controls
            .syntax_applicable
            .set(syntax_applicable);

        self.disassembly_controls
            .syntax_intel
            .set_sensitive(syntax_applicable);

        self.disassembly_controls
            .syntax_att
            .set_sensitive(syntax_applicable);

        let title = architecture.map_or_else(
            || String::from("INSTRUCTIONS"),
            |architecture| format!("INSTRUCTIONS · {architecture}"),
        );

        self.instructions_title.set_text(&title);
        self.instructions_title.set_tooltip_text(Some(&title));

        self.misc_view.cfg.show(
            &instructions,
            pc,
            self.target_architecture(),
            self.target_pointer_bits(),
        );

        if instructions.is_empty() {
            self.instructions_empty.set_visible(true);
            self.instructions_store.remove_all();

            self.instructions_selection
                .set_selected(gtk::INVALID_LIST_POSITION);

            self.disassembly_controls.range.set_text("");

            self.disassembly_controls
                .previous_function
                .set_sensitive(false);

            self.disassembly_controls.next_function.set_sensitive(false);
            self.disassembly_controls.follow.set_sensitive(false);
            self.disassembly_controls.open_memory.set_sensitive(false);
            self.current_instruction.replace(None);
            self.current_instruction_memory_expression.replace(None);

            self.instruction_flow
                .set_text("Flow information appears at a branch or call");

            self.instruction_flow.set_visible(true);
            self.instruction_arguments.set_visible(false);
            self.instruction_memory.set_visible(false);
            self.update_control_sensitivity();
            return;
        }

        self.instructions_empty.set_visible(false);

        let current = instructions
            .iter()
            .position(|instruction| addresses_equal(&instruction.address, pc));

        let selected = instructions
            .iter()
            .position(|instruction| addresses_equal(&instruction.address, focus))
            .or(current)
            .unwrap_or(0);

        let selected_address = instructions[selected].address.clone();

        self.current_instruction
            .replace(current.and_then(|position| instructions.get(position).cloned()));

        if let Some(position) = current {
            self.call_abi_instruction
                .replace(Some(CallAbiInstructionContext {
                    current: instructions[position].clone(),
                    previous: position
                        .checked_sub(1)
                        .and_then(|previous| instructions.get(previous).cloned()),
                    target_resolution: None,
                    pending_target: None,
                }));

            self.call_abi_instruction_generation
                .set(Some(self.current_stop_refresh_generation()));

            self.refresh_call_abi_transfer();
        }

        let rows = instructions
            .into_iter()
            .map(|instruction| InstructionRowData {
                current: addresses_equal(&instruction.address, pc),
                pointer_bits: self.target_pointer_bits(),
                source_text: self.disassembly_source_text(&instruction),
                instruction,
            })
            .collect::<Vec<_>>();

        replace_boxed_store_if_changed(&self.instructions_store, rows);
        let selected = u32::try_from(selected).unwrap_or(0);
        let selection_changed = self.instructions_selection.selected() != selected;

        if selection_changed {
            self.instructions_selection.set_selected(selected);
        }

        self.center_instruction_row(selected, self.instructions_store.n_items());

        if let (Some(first), Some(last)) = (
            self.instructions_store
                .item(0)
                .and_downcast::<glib::BoxedAnyObject>(),
            self.instructions_store
                .item(self.instructions_store.n_items().saturating_sub(1))
                .and_downcast::<glib::BoxedAnyObject>(),
        ) {
            let first = first.borrow::<InstructionRowData>();
            let last = last.borrow::<InstructionRowData>();

            let function = if first.instruction.function == "??" {
                "unknown function"
            } else {
                first.instruction.function.as_str()
            };

            let range = format!(
                "{function} · {}-{} · {} instructions",
                full_address(&first.instruction.address, self.target_pointer_bits()),
                full_address(&last.instruction.address, self.target_pointer_bits()),
                self.instructions_store.n_items()
            );

            self.disassembly_controls.range.set_text(&range);

            self.disassembly_controls
                .range
                .set_tooltip_text(Some(&range));

            self.disassembly_controls
                .location
                .set_text(&selected_address);
        }

        self.disassembly_controls
            .previous_function
            .set_sensitive(true);

        self.disassembly_controls.next_function.set_sensitive(true);
        self.update_disassembly_selection();
        self.update_instruction_insight();
        self.update_control_sensitivity();
    }

    fn center_instruction_row(&self, position: u32, item_count: u32) {
        if item_count == 0 || position >= item_count {
            return;
        }

        self.instructions_view
            .scroll_to(position, None, gtk::ListScrollFlags::FOCUS, None);

        let generation = self
            .disassembly_controls
            .scroll_generation
            .get()
            .wrapping_add(1);

        self.disassembly_controls.scroll_generation.set(generation);
        center_scroll_adjustment(&self.disassembly_controls.scrolled, position, item_count);
        let scrolled = self.disassembly_controls.scrolled.clone();
        let scroll_generation = Rc::clone(&self.disassembly_controls.scroll_generation);

        glib::timeout_add_local_once(Duration::from_millis(16), move || {
            if scroll_generation.get() == generation {
                center_scroll_adjustment(&scrolled, position, item_count);
            }
        });
    }

    fn disassembly_source_text(&self, instruction: &Instruction) -> Option<Rc<str>> {
        const MAX_SOURCE_BYTES: usize = 2 * 1024 * 1024;
        let source = instruction.source.as_ref()?;
        let path = self.resolve_source_path(source.source_path())?;

        let lines =
            if let Some(lines) = self.disassembly_source_cache.borrow_mut().get_cloned(&path) {
                lines
            } else {
                let contents = crate::bounded::read_string(&path, MAX_SOURCE_BYTES).ok()?;
                let lines = Rc::new(contents.lines().map(Rc::<str>::from).collect::<Vec<_>>());
                let mut cache = self.disassembly_source_cache.borrow_mut();
                let evicted = cache.insert(path, Rc::clone(&lines));
                drop(cache);

                if evicted {
                    self.record_performance_notice(crate::performance::PerformanceNotice {
                        outcome: crate::performance::BudgetOutcome::Evicted,
                        operation: String::from("disassembly source cache"),
                        detail: format!(
                            "least-recently used file was removed at the {}-file budget",
                            crate::performance::DISASSEMBLY_SOURCE_CACHE_BUDGET
                        ),
                    });
                }

                lines
            };

        let index = usize::try_from(source.line).ok()?.checked_sub(1)?;

        lines.get(index).cloned()
    }

    fn update_instruction_insight(&self) {
        let Some(instruction) = self.current_instruction.borrow().clone() else {
            self.instruction_flow
                .set_text("Flow information appears at the current branch or call");

            self.instruction_flow.set_tooltip_text(None);
            self.instruction_flow.remove_css_class("branch-taken");
            self.instruction_flow.remove_css_class("branch-not-taken");
            self.instruction_arguments.set_visible(false);
            self.instruction_memory.set_visible(false);
            self.current_instruction_memory_expression.replace(None);
            return;
        };

        let registers = self.latest_registers.borrow();
        let architecture = self.target_architecture();
        let branch_taken = conditional_branch_taken(&instruction, &registers, architecture);
        let flow = instruction_flow_description(&instruction, &registers, architecture);
        self.instruction_flow.set_text(&flow);
        self.instruction_flow.set_tooltip_text(Some(&flow));
        self.instruction_flow.set_visible(true);
        self.instruction_flow.remove_css_class("branch-taken");
        self.instruction_flow.remove_css_class("branch-not-taken");

        if let Some(taken) = branch_taken {
            self.instruction_flow.add_css_class(if taken {
                "branch-taken"
            } else {
                "branch-not-taken"
            });
        }

        let arguments = instruction_arguments_description(&instruction, &registers, architecture);

        self.instruction_arguments
            .set_visible(!arguments.is_empty());

        self.instruction_arguments.set_text(&arguments);

        self.instruction_arguments
            .set_tooltip_text((!arguments.is_empty()).then_some(arguments.as_str()));

        let expression = instruction_memory_expression(&instruction, &registers, architecture);
        drop(registers);
        let mut current = self.current_instruction_memory_expression.borrow_mut();

        if current.as_ref() == expression.as_ref() {
            return;
        }

        current.clone_from(&expression);
        drop(current);

        let Some(expression) = expression else {
            self.instruction_memory.set_visible(false);
            return;
        };

        self.instruction_memory
            .set_text(&format!("MEMORY  {expression} · reading…"));

        self.instruction_memory.set_visible(true);
        let handler = self.instruction_memory_handler.borrow().clone();

        if let Some(handler) = handler {
            handler(expression);
        }
    }

    pub fn show_instruction_memory(&self, expression: &str, result: Result<&MemoryBlock, &str>) {
        if self
            .current_instruction_memory_expression
            .borrow()
            .as_deref()
            != Some(expression)
        {
            return;
        }

        let text = match result {
            Ok(memory) => {
                let width = usize::try_from(self.target_pointer_bits() / 4)
                    .unwrap_or(16)
                    .clamp(8, 16);

                format!(
                    "MEMORY  {expression} = 0x{:0width$x}  {}",
                    memory.begin,
                    compact_memory_preview(&memory.bytes)
                )
            }
            Err(error) => format!("MEMORY  {expression} · {error}"),
        };

        self.instruction_memory.set_text(&text);
        self.instruction_memory.set_tooltip_text(Some(&text));
        self.instruction_memory.set_visible(true);
    }

    pub(super) fn connect_instruction_activation(&self) {
        let store = self.instructions_store.clone();
        let handler = Rc::clone(&self.instruction_handler);

        self.instructions_view.connect_activate(move |_, position| {
            let Some(item) = store
                .item(position)
                .and_then(|item| item.downcast::<glib::BoxedAnyObject>().ok())
            else {
                return;
            };

            let address = item
                .borrow::<InstructionRowData>()
                .instruction
                .address
                .clone();

            let handler = handler.borrow().clone();

            if let Some(handler) = handler {
                handler(address);
            }
        });

        let ui = self.clone();

        self.instructions_selection
            .connect_selected_notify(move |_| ui.update_disassembly_selection());
    }

    pub(super) fn connect_disassembly_controls(&self) {
        let handler = Rc::clone(&self.disassembly_handler);
        let location = self.disassembly_controls.location.clone();

        self.disassembly_controls.go.connect_clicked(move |_| {
            let expression = location.text().trim().to_owned();
            let handler = handler.borrow().clone();

            if !expression.is_empty()
                && let Some(handler) = handler
            {
                handler(DisassemblyRequest::Navigate(expression));
            }
        });

        let go = self.disassembly_controls.go.clone();

        self.disassembly_controls
            .location
            .connect_activate(move |_| {
                if go.is_sensitive() {
                    go.emit_clicked();
                }
            });

        for (button, request) in [
            (&self.disassembly_controls.back, DisassemblyRequest::Back),
            (
                &self.disassembly_controls.forward,
                DisassemblyRequest::Forward,
            ),
            (
                &self.disassembly_controls.previous_function,
                DisassemblyRequest::PreviousFunction,
            ),
            (
                &self.disassembly_controls.next_function,
                DisassemblyRequest::NextFunction,
            ),
            (
                &self.disassembly_controls.current_pc,
                DisassemblyRequest::Navigate(String::from("$pc")),
            ),
        ] {
            let handler = Rc::clone(&self.disassembly_handler);

            button.connect_clicked(move |_| {
                let handler = handler.borrow().clone();

                if let Some(handler) = handler {
                    handler(request.clone());
                }
            });
        }

        let handler = Rc::clone(&self.disassembly_handler);

        self.disassembly_controls
            .mixed
            .connect_toggled(move |button| {
                let handler = handler.borrow().clone();

                if let Some(handler) = handler {
                    handler(DisassemblyRequest::Mixed(button.is_active()));
                }
            });

        for (button, syntax) in [
            (
                &self.disassembly_controls.syntax_intel,
                DisassemblySyntax::Intel,
            ),
            (
                &self.disassembly_controls.syntax_att,
                DisassemblySyntax::Att,
            ),
        ] {
            let handler = Rc::clone(&self.disassembly_handler);
            let setting_syntax = Rc::clone(&self.disassembly_controls.setting_syntax);

            button.connect_toggled(move |button| {
                if setting_syntax.get() || !button.is_active() {
                    return;
                }

                let handler = handler.borrow().clone();

                if let Some(handler) = handler {
                    handler(DisassemblyRequest::Syntax(syntax));
                }
            });
        }

        let ui = self.clone();

        self.disassembly_controls.follow.connect_clicked(move |_| {
            let Some(instruction) = ui.selected_instruction() else {
                return;
            };

            let Some(target) = instruction_flow_target(&instruction, ui.target_architecture())
            else {
                return;
            };

            let handler = ui.disassembly_handler.borrow().clone();

            if let Some(handler) = handler {
                handler(DisassemblyRequest::Navigate(target));
            }
        });

        let ui = self.clone();

        self.disassembly_controls
            .open_memory
            .connect_clicked(move |_| {
                if ui.disassembly_commands_available() {
                    ui.open_selected_instruction_memory();
                }
            });

        self.disassembly_controls.back.set_sensitive(false);
        self.disassembly_controls.forward.set_sensitive(false);

        self.disassembly_controls
            .previous_function
            .set_sensitive(false);

        self.disassembly_controls.next_function.set_sensitive(false);
        self.disassembly_controls.follow.set_sensitive(false);
        self.disassembly_controls.open_memory.set_sensitive(false);
    }

    fn selected_instruction(&self) -> Option<Instruction> {
        self.selected_instruction_row().map(|row| row.instruction)
    }

    fn selected_instruction_row(&self) -> Option<InstructionRowData> {
        let position = self.instructions_selection.selected();

        self.instructions_store
            .item(position)
            .and_then(|item| item.downcast::<glib::BoxedAnyObject>().ok())
            .map(|item| item.borrow::<InstructionRowData>().clone())
    }

    fn update_disassembly_selection(&self) {
        clear_label_selections(&self.instructions_view);
        let row = self.selected_instruction_row();

        if let Some(row) = row.as_ref() {
            self.disassembly_controls
                .location
                .set_text(&row.instruction.address);
        }

        let instruction = row.as_ref().map(|row| &row.instruction);
        let registers = self.latest_registers.borrow();
        let architecture = self.target_architecture();

        self.disassembly_controls.follow.set_sensitive(
            instruction
                .as_ref()
                .and_then(|instruction| instruction_flow_target(instruction, architecture))
                .is_some(),
        );

        self.disassembly_controls.open_memory.set_sensitive(
            instruction
                .as_ref()
                .and_then(|instruction| {
                    instruction_memory_expression(instruction, &registers, architecture)
                })
                .is_some(),
        );
    }

    fn open_selected_instruction_memory(&self) {
        let Some(instruction) = self.selected_instruction() else {
            return;
        };

        let Some(expression) = instruction_memory_expression(
            &instruction,
            &self.latest_registers.borrow(),
            self.target_architecture(),
        ) else {
            return;
        };

        if add_memory_watch(
            &self.memory_watch_container,
            &self.memory_watches,
            &self.memory_watch_handler,
            expression.clone(),
            128,
            MemoryWatchFormat::Bytes,
        ) {
            self.inspector_notebook.set_current_page(Some(4));

            self.set_status(
                "Memory",
                &format!("Opened effective address {expression}"),
                Some("status-ready"),
            );
        } else {
            self.set_status(
                "Memory watch limit",
                "Remove a memory watch before adding another (limit 256)",
                Some("status-error"),
            );
        }
    }

    pub(crate) fn set_disassembly_loading(&self, loading: bool) {
        self.disassembly_controls.loading.set(loading);
    }

    pub(crate) fn set_disassembly_history(&self, can_back: bool, can_forward: bool) {
        self.disassembly_controls.back.set_sensitive(can_back);
        self.disassembly_controls.forward.set_sensitive(can_forward);
    }

    pub(crate) fn set_disassembly_syntax(&self, syntax: DisassemblySyntax) {
        self.disassembly_controls.setting_syntax.set(true);

        self.disassembly_controls
            .syntax_intel
            .set_active(syntax == DisassemblySyntax::Intel);

        self.disassembly_controls
            .syntax_att
            .set_active(syntax == DisassemblySyntax::Att);

        self.disassembly_controls.setting_syntax.set(false);
    }

    pub(crate) fn show_disassembly_error(&self, message: &str) {
        self.set_disassembly_loading(false);

        self.disassembly_controls
            .location
            .add_css_class("input-error");

        self.set_status("Disassembly failed", message, Some("status-error"));
    }

    pub(crate) fn clear_disassembly_error(&self) {
        self.disassembly_controls
            .location
            .remove_css_class("input-error");
    }

    pub fn show_signal(&self, name: Option<&str>, meaning: Option<&str>) {
        let text = match (name, meaning) {
            (Some(name), Some(meaning)) => format!("{name} · {meaning}"),
            (Some(name), None) => name.to_owned(),
            (None, _) => String::from("No signal at the current stop"),
        };

        self.signal_detail.set_text(&text);

        if name.is_some() {
            self.signal_detail.add_css_class("signal-active");
        } else {
            self.signal_detail.remove_css_class("signal-active");
        }
    }

    pub fn show_registers(&self, registers: &[Register]) -> bool {
        self.latest_registers_generation.set(None);

        if registers.is_empty() {
            for group in &self.register_groups {
                if group.store.n_items() != 0 {
                    group.store.remove_all();
                }

                if group.panel.is_visible() {
                    group.panel.set_visible(false);
                }
            }

            if !self.registers_empty.is_visible() {
                self.registers_empty.set_visible(true);
            }
        } else {
            if self.registers_empty.is_visible() {
                self.registers_empty.set_visible(false);
            }

            let architecture = self.target_architecture();
            let endian = self.target_endian();
            let pointer_bits = self.target_pointer_bits();
            let previous = self.previous_registers.borrow();

            let ring = registers
                .iter()
                .find(|register| register.name == "cs")
                .and_then(|register| hex_value(&register.value))
                .map(|value| value & 0x3);

            for group in self.register_groups.iter() {
                let grouped = registers.iter().filter(|register| {
                    register_in_group(group.kind, &register.name, architecture)
                        && (group.kind != RegisterGroupKind::Other
                            || !self.register_groups.iter().any(|candidate| {
                                candidate.kind != RegisterGroupKind::Other
                                    && register_in_group(
                                        candidate.kind,
                                        &register.name,
                                        architecture,
                                    )
                            }))
                });

                populate_register_group(
                    group,
                    grouped,
                    &previous,
                    ring,
                    architecture,
                    endian,
                    pointer_bits,
                );
            }
        }

        let values_changed = {
            let latest = self.latest_registers.borrow();

            !same_register_values(&latest, registers)
        };

        if values_changed {
            self.latest_registers.replace(registers.to_vec());
        }

        self.update_instruction_insight();

        values_changed
    }

    pub fn start_stop_refresh(&self) -> u64 {
        self.active_stop_context.borrow_mut().take();

        self.memory_watch_container
            .refresh_batch
            .borrow_mut()
            .clear();

        update_memory_container_state(&self.memory_watch_container, false);
        self.pending_local_variable_objects.borrow_mut().clear();
        clear_variable_change_markers(&self.locals_store);
        clear_variable_change_markers(&self.expression_watches_store);
        let roots = self.local_variables.borrow();
        let arguments = roots.argument_count();

        self.locals_summary.set_text(&locals_summary_text(
            roots.len().saturating_sub(arguments),
            arguments,
            0,
            self.locals_store.n_items() as usize,
            roots.len(),
        ));

        let latest = self.latest_registers.borrow();
        let mut previous = self.previous_registers.borrow_mut();
        previous.clear();
        previous.reserve(latest.len());

        previous.extend(
            latest
                .iter()
                .map(|register| (register.name.clone(), register.value.clone())),
        );

        drop(previous);
        drop(latest);
        let generation = self.stop_refresh_generation.get().wrapping_add(1);
        self.stop_refresh_generation.set(generation);
        self.call_abi_instruction.replace(None);
        self.call_abi_instruction_generation.set(None);
        self.misc_view.show_call_abi_pending();

        generation
    }

    pub(crate) fn begin_stop_refresh(
        &self,
        transport_epoch: u64,
    ) -> Option<crate::debugger::StopContext> {
        let generation = self.start_stop_refresh();
        let thread_id = self.current_thread_id()?;

        let frame_level = match self.selected_frame_level.get() {
            u32::MAX => 0,
            level => level,
        };

        if self.selected_frame_level.get() != frame_level {
            self.select_frame_in_view(frame_level);
        }

        let context = crate::debugger::StopContext::new(
            transport_epoch,
            generation,
            self.selected_inferior_id(),
            thread_id,
            frame_level,
        )?;

        self.active_stop_context.replace(Some(context.clone()));

        Some(context)
    }

    pub(crate) fn stop_context(&self, generation: u64) -> Option<crate::debugger::StopContext> {
        self.active_stop_context
            .borrow()
            .as_ref()
            .filter(|context| context.generation() == generation)
            .cloned()
    }

    pub fn is_stop_refresh_current(&self, generation: u64) -> bool {
        if self.stop_refresh_generation.get() != generation {
            return false;
        }

        self.active_stop_context
            .borrow()
            .as_ref()
            .is_some_and(|context| {
                self.current_thread_id().as_deref() == Some(context.thread_id())
                    && self.selected_frame_level.get() == context.frame_level()
                    && context.inferior_id().is_none_or(|inferior| {
                        self.selected_inferior_id().as_deref() == Some(inferior)
                    })
            })
    }

    pub fn current_stop_refresh_generation(&self) -> u64 {
        self.stop_refresh_generation.get()
    }

    pub fn cached_register_names(&self) -> Option<Rc<Vec<String>>> {
        self.cached_register_names.borrow().clone()
    }

    pub fn cache_register_names(&self, names: Rc<Vec<String>>) {
        if !names.is_empty() {
            self.cached_register_names.replace(Some(names));
        }
    }

    pub fn show_registers_for_refresh(&self, generation: u64, registers: &[Register]) {
        if self.is_stop_refresh_current(generation) {
            let first_for_generation = self.latest_registers_generation.get() != Some(generation);
            let values_changed = self.show_registers(registers);
            self.latest_registers_generation.set(Some(generation));

            if first_for_generation || values_changed {
                self.refresh_call_abi_transfer();
            }
        }
    }

    pub(crate) fn registers_for_details(&self, generation: u64) -> Option<Vec<Register>> {
        (self.latest_registers_generation.get() == Some(generation))
            .then(|| self.latest_registers.borrow().clone())
            .filter(|registers| !registers.is_empty())
    }

    pub(crate) fn claim_register_details(&self, generation: u64) -> bool {
        self.is_stop_refresh_current(generation)
            && self.register_details_generation.replace(Some(generation)) != Some(generation)
    }

    pub fn show_stack(&self, entries: &[StackEntry]) {
        self.latest_stack_generation.set(None);

        if self.latest_stack.borrow().as_slice() != entries {
            self.latest_stack.replace(entries.to_vec());
        }

        self.render_stack(Cow::Borrowed(entries));
    }

    fn render_stack(&self, entries: Cow<'_, [StackEntry]>) {
        if self.displayed_stack.borrow().as_slice() != entries.as_ref() {
            replace_boxed_store_if_changed(&self.stack_store, entries.iter().cloned());
            self.displayed_stack.replace(entries.into_owned());
        }

        if self.displayed_stack.borrow().is_empty() {
            self.stack_empty
                .set_text("Stack values appear when the target is paused");

            self.stack_empty.set_visible(true);
            return;
        }

        self.stack_empty.set_visible(false);
    }

    pub fn show_stack_for_refresh(&self, generation: u64, entries: &[StackEntry]) {
        if self.is_stop_refresh_current(generation) {
            if self.latest_stack.borrow().as_slice() != entries {
                self.latest_stack.replace(entries.to_vec());
            }

            let mut rendered = entries.to_vec();
            preserve_stack_render_details(&mut rendered, &self.displayed_stack.borrow());
            self.render_stack(Cow::Owned(rendered));
            self.latest_stack_generation.set(Some(generation));
        }
    }

    pub(crate) fn stack_for_details(&self, generation: u64) -> Option<Vec<StackEntry>> {
        (self.latest_stack_generation.get() == Some(generation))
            .then(|| self.latest_stack.borrow().clone())
            .filter(|entries| !entries.is_empty())
    }

    pub(crate) fn claim_stack_details(&self, generation: u64) -> bool {
        self.is_stop_refresh_current(generation)
            && self.stack_details_generation.replace(Some(generation)) != Some(generation)
    }

    pub(crate) fn claim_stack_memory_refresh(&self, generation: u64) -> bool {
        self.is_stop_refresh_current(generation)
            && self
                .stack_memory_refresh_generation
                .replace(Some(generation))
                != Some(generation)
    }

    pub fn show_stack_unavailable_for_refresh(&self, generation: u64, reason: &str) {
        if !self.is_stop_refresh_current(generation) {
            return;
        }

        self.latest_stack.replace(Vec::new());
        self.displayed_stack.replace(Vec::new());
        self.latest_stack_generation.set(Some(generation));
        self.stack_store.remove_all();
        self.stack_empty.set_text(reason);
        self.stack_empty.set_visible(true);
    }

    pub fn show_memory_regions_for_refresh(&self, generation: u64, regions: &[MemoryRegion]) {
        if !self.is_stop_refresh_current(generation) {
            return;
        }

        if self.memory_regions.borrow().as_slice() != regions {
            replace_boxed_store_if_changed(&self.memory_region_store, regions.iter().cloned());
            self.memory_regions.replace(regions.to_vec());
        }

        self.memory_regions_empty.set_visible(regions.is_empty());
        self.memory_regions_generation.set(Some(generation));
    }

    pub(crate) fn memory_regions_for_details(&self, generation: u64) -> Option<Vec<MemoryRegion>> {
        (self.memory_regions_generation.get() == Some(generation))
            .then(|| self.memory_regions.borrow().clone())
    }

    pub(crate) fn claim_memory_watches_refresh(&self, generation: u64) -> bool {
        self.is_stop_refresh_current(generation)
            && self
                .memory_watches_refresh_generation
                .replace(Some(generation))
                != Some(generation)
    }

    pub(crate) fn claim_tls_runtime_refresh(&self, generation: u64) -> bool {
        self.is_stop_refresh_current(generation)
            && self
                .tls_runtime_refresh_generation
                .replace(Some(generation))
                != Some(generation)
    }

    pub(super) fn connect_memory_controls(&self) {
        let container = self.memory_watch_container.clone();
        let watches = Rc::clone(&self.memory_watches);
        let handler = Rc::clone(&self.memory_watch_handler);
        let expression = self.memory_address_entry.clone();
        let size = self.memory_size.clone();
        let format = self.memory_format.clone();
        let status_label = self.status_label.clone();
        let status_detail = self.status_detail.clone();

        self.memory_add_button.connect_clicked(move |_| {
            let expression_text = expression.text().trim().to_owned();

            if expression_text.is_empty() {
                return;
            }

            let byte_count = usize::try_from(size.value_as_int()).unwrap_or(128);

            let format = match format.selected() {
                1 => MemoryWatchFormat::U16,
                2 => MemoryWatchFormat::U32,
                3 => MemoryWatchFormat::U64,
                4 => MemoryWatchFormat::F32,
                5 => MemoryWatchFormat::F64,
                6 => MemoryWatchFormat::Pointers,
                _ => MemoryWatchFormat::Bytes,
            };

            let added = add_memory_watch(
                &container,
                &watches,
                &handler,
                expression_text,
                byte_count,
                format,
            );

            if added {
                expression.set_text("");
            } else {
                set_status_widgets(
                    &status_label,
                    &status_detail,
                    "Memory watch limit",
                    "Remove a memory watch before adding another (limit 256)",
                    Some("status-error"),
                );
            }

            expression.grab_focus();
        });

        let button = self.memory_add_button.clone();

        self.memory_address_entry.connect_activate(move |_| {
            if button.is_sensitive() {
                button.emit_clicked();
            }
        });

        let button = self.memory_add_button.clone();
        let ready = Rc::clone(&self.debugger_ready);
        let debugger_state = Rc::clone(&self.debugger_state);
        let pending = Rc::clone(&self.command_pending);

        self.memory_address_entry.connect_changed(move |entry| {
            button.set_sensitive(
                ready.get()
                    && debugger_state.get().inferior_started()
                    && !debugger_state.get().inferior_running()
                    && !pending.get()
                    && !entry.text().trim().is_empty(),
            );
        });

        let ui = self.clone();

        self.memory_watch_container
            .refresh_all
            .connect_clicked(move |_| ui.refresh_memory_watches());

        let container = self.memory_watch_container.clone();
        let watches = Rc::clone(&self.memory_watches);

        self.memory_watch_container
            .clear_all
            .connect_clicked(move |_| clear_memory_watches(&container, &watches));

        self.memory_regions_view.set_single_click_activate(false);
        let container = self.memory_watch_container.clone();
        let watches = Rc::clone(&self.memory_watches);
        let handler = Rc::clone(&self.memory_watch_handler);
        let size = self.memory_size.clone();

        self.memory_regions_view
            .connect_activate(move |view, position| {
                let Some(region) = view
                    .model()
                    .and_then(|model| model.item(position))
                    .and_downcast::<glib::BoxedAnyObject>()
                else {
                    return;
                };

                let region = region.borrow::<MemoryRegion>();
                let requested = usize::try_from(size.value_as_int()).unwrap_or(128);

                let region_size =
                    usize::try_from(region.end.saturating_sub(region.start)).unwrap_or(usize::MAX);

                let byte_count = requested.min(region_size).max(1);

                let _ = add_memory_watch(
                    &container,
                    &watches,
                    &handler,
                    format!("0x{:x}", region.start),
                    byte_count,
                    MemoryWatchFormat::Bytes,
                );
            });
    }

    pub(super) fn connect_watchpoint_controls(&self) {
        let expression = self.watchpoint_expression.clone();
        let access = self.watchpoint_access.clone();
        let mask = self.watchpoint_mask.clone();
        let handler = Rc::clone(&self.watchpoint_insert_handler);

        self.watchpoint_add_button.connect_clicked(move |_| {
            let expression = expression.text().trim().to_owned();

            if expression.is_empty() {
                return;
            }

            let request = match access.selected() {
                1 => WatchpointRequest::Standard {
                    expression,
                    access: WatchpointAccess::Read,
                },
                2 => WatchpointRequest::Standard {
                    expression,
                    access: WatchpointAccess::Access,
                },
                3 => WatchpointRequest::Masked {
                    expression,
                    mask: mask.text().trim().to_owned(),
                },
                _ => WatchpointRequest::Standard {
                    expression,
                    access: WatchpointAccess::Write,
                },
            };

            let handler = handler.borrow().clone();

            if let Some(handler) = handler {
                handler(request);
            }
        });

        let mask = self.watchpoint_mask.clone();

        self.watchpoint_access
            .connect_selected_notify(move |access| {
                mask.set_visible(access.selected() == 3);
            });

        let button = self.watchpoint_add_button.clone();

        self.watchpoint_expression
            .connect_activate(move |_| button.emit_clicked());

        let button = self.watchpoint_add_button.clone();

        self.watchpoint_mask
            .connect_activate(move |_| button.emit_clicked());
    }

    pub(super) fn connect_breakpoint_bulk_controls(&self) {
        let rows = Rc::clone(&self.stop_point_filter_rows);
        let metadata = Rc::clone(&self.stop_point_metadata);
        let search = self.stop_point_filter.search.clone();
        let kind = self.stop_point_filter.kind.clone();
        let rows_for_search = Rc::clone(&rows);
        let metadata_for_search = Rc::clone(&metadata);
        let controls_for_search = self.stop_point_filter.clone();

        search.connect_search_changed(move |search| {
            let _ = search;

            apply_stop_point_filter(
                &rows_for_search.borrow(),
                &metadata_for_search.borrow(),
                &controls_for_search,
            );
        });

        let rows_for_kind = rows;
        let metadata_for_kind = metadata;
        let controls_for_kind = self.stop_point_filter.clone();

        kind.connect_selected_notify(move |kind| {
            let _ = kind;

            apply_stop_point_filter(
                &rows_for_kind.borrow(),
                &metadata_for_kind.borrow(),
                &controls_for_kind,
            );
        });

        let parent = self.window.clone();
        let handler = Rc::clone(&self.breakpoint_editor_handler);
        let capabilities = Rc::clone(&self.gdb_capabilities);

        self.add_breakpoint_button.connect_clicked(move |_| {
            let pending_supported = capabilities.borrow().supports("pending-breakpoints");
            open_breakpoint_editor(&parent, None, pending_supported, Rc::clone(&handler));
        });

        let breakpoints = Rc::clone(&self.breakpoints);
        let handler = Rc::clone(&self.breakpoint_bulk_delete_handler);

        self.delete_all_breakpoints_button
            .connect_clicked(move |_| {
                let numbers = breakpoint_command_numbers(&breakpoints.borrow(), false);
                let handler = handler.borrow().clone();

                if !numbers.is_empty()
                    && let Some(handler) = handler
                {
                    handler(numbers);
                }
            });

        let breakpoints = Rc::clone(&self.breakpoints);
        let handler = Rc::clone(&self.breakpoint_bulk_delete_handler);

        self.delete_all_watchpoints_button
            .connect_clicked(move |_| {
                let numbers = breakpoint_command_numbers(&breakpoints.borrow(), true);
                let handler = handler.borrow().clone();

                if !numbers.is_empty()
                    && let Some(handler) = handler
                {
                    handler(numbers);
                }
            });

        let breakpoints = Rc::clone(&self.breakpoints);
        let handler = Rc::clone(&self.breakpoint_bulk_delete_handler);

        self.delete_all_catchpoints_button
            .connect_clicked(move |_| {
                let numbers = event_catchpoint_command_numbers(&breakpoints.borrow());
                let handler = handler.borrow().clone();

                if !numbers.is_empty()
                    && let Some(handler) = handler
                {
                    handler(numbers);
                }
            });

        let breakpoints = Rc::clone(&self.breakpoints);
        let handler = Rc::clone(&self.breakpoint_bulk_delete_handler);
        let ready = Rc::clone(&self.debugger_ready);
        let debugger_state = Rc::clone(&self.debugger_state);
        let command_pending = Rc::clone(&self.command_pending);
        let session_pending = Rc::clone(&self.session_pending);
        let until_active = Rc::clone(&self.native_until_active);

        self.delete_all_signal_catchpoints_button
            .connect_clicked(move |_| {
                if !ready.get()
                    || debugger_state.get().inferior_running()
                    || command_pending.get()
                    || session_pending.get()
                    || until_active.get()
                {
                    return;
                }

                let numbers = signal_catchpoint_command_numbers(&breakpoints.borrow());
                let handler = handler.borrow().clone();

                if !numbers.is_empty()
                    && let Some(handler) = handler
                {
                    handler(numbers);
                }
            });
    }

    pub(super) fn connect_event_catchpoint_controls(&self) {
        for (button, event) in &self.event_catchpoint_buttons {
            let event = *event;
            let breakpoints = Rc::clone(&self.breakpoints);
            let handler = Rc::clone(&self.event_catchpoint_handler);

            button.connect_clicked(move |_| {
                let existing = event_catchpoint_command_number(&breakpoints.borrow(), event);
                let handler = handler.borrow().clone();

                if let Some(handler) = handler {
                    handler(event, existing);
                }
            });
        }
    }

    pub(super) fn connect_filtered_catchpoint_controls(&self) {
        let filter = self.filtered_catchpoint.filter.clone();
        let kind = self.filtered_catchpoint.kind.clone();
        let handler = Rc::clone(&self.filtered_catchpoint_handler);

        self.filtered_catchpoint.add.connect_clicked(move |_| {
            let filter_text = filter.text().trim().to_owned();

            if filter_text.is_empty() {
                return;
            }

            let kind = match kind.selected() {
                1 => FilteredCatchpointKind::LibraryLoad,
                2 => FilteredCatchpointKind::LibraryUnload,
                _ => FilteredCatchpointKind::Syscall,
            };

            if let Some(handler) = handler.borrow().clone() {
                handler(FilteredCatchpointRequest {
                    kind,
                    filter: filter_text,
                });
            }
        });

        let button = self.filtered_catchpoint.add.clone();

        self.filtered_catchpoint
            .filter
            .connect_activate(move |_| button.emit_clicked());

        let filter = self.filtered_catchpoint.filter.clone();

        self.filtered_catchpoint
            .kind
            .connect_selected_notify(move |kind| {
                filter.set_placeholder_text(Some(if kind.selected() == 0 {
                    "syscall names or numbers"
                } else {
                    "shared-library regular expression"
                }));
            });
    }

    pub fn refresh_memory_watches(&self) {
        if !self.stopped_inspection_available() {
            update_memory_container_state(&self.memory_watch_container, false);
            return;
        }

        let requests = {
            let watches = self.memory_watches.borrow();

            if watches.is_empty() {
                return;
            }

            self.memory_watch_container
                .refresh_batch
                .borrow_mut()
                .begin(watches.iter().map(|watch| watch.id));

            update_memory_container_state(&self.memory_watch_container, true);

            watches
                .iter()
                .map(|watch| {
                    set_memory_watch_reading(watch);

                    (
                        watch.id,
                        memory_watch_request_expression(watch),
                        watch.byte_count,
                    )
                })
                .collect::<Vec<_>>()
        };

        let handler = self.memory_watch_handler.borrow().clone();

        if let Some(handler) = handler {
            for (id, expression, byte_count) in requests {
                handler(id, expression, byte_count);
            }
        } else {
            self.memory_watch_container
                .refresh_batch
                .borrow_mut()
                .clear();

            update_memory_container_state(&self.memory_watch_container, false);
        }
    }

    pub fn show_memory_watch(&self, id: u64, result: Result<MemoryBlock, &str>) {
        let reading = self
            .memory_watch_container
            .refresh_batch
            .borrow_mut()
            .finish(id);

        let watch = self
            .memory_watches
            .borrow()
            .iter()
            .find(|watch| watch.id == id)
            .cloned();

        let Some(watch) = watch else {
            update_memory_container_state(&self.memory_watch_container, reading);
            return;
        };

        match result {
            Ok(memory) => {
                let endian = self.target_endian().or_else(|| {
                    (watch.format == MemoryWatchFormat::Bytes).then_some(TargetEndian::Little)
                });

                let Some(endian) = endian else {
                    show_memory_watch_error(
                        &watch,
                        "Target byte order is unavailable. Use Hex bytes display",
                    );

                    update_memory_container_state(&self.memory_watch_container, reading);
                    return;
                };

                show_memory_watch_data(
                    &watch,
                    memory,
                    &self.memory_regions.borrow(),
                    self.target_pointer_bits.get(),
                    endian,
                );
            }
            Err(error) => show_memory_watch_error(&watch, error),
        }

        update_memory_container_state(&self.memory_watch_container, reading);
    }

    pub fn show_breakpoints(&self, breakpoints: Vec<Breakpoint>) {
        if self.breakpoints.borrow().as_slice() == breakpoints {
            return;
        }

        let render_started = Instant::now();
        let status_only = breakpoint_layout_matches(&self.breakpoints.borrow(), &breakpoints);
        self.breakpoints.replace(breakpoints);

        let active_numbers = self
            .breakpoints
            .borrow()
            .iter()
            .filter(|breakpoint| !breakpoint.is_location())
            .map(|breakpoint| breakpoint.command_number().to_owned())
            .collect::<HashSet<_>>();

        self.stop_point_metadata
            .borrow_mut()
            .retain(|number, _| active_numbers.contains(number));

        if status_only {
            let breakpoints = self.breakpoints.borrow();
            let rows = self.stop_point_filter_rows.borrow();

            let rendered_numbers = rows
                .iter()
                .map(|row| row.number.as_str())
                .collect::<HashSet<_>>();

            let by_number = breakpoints
                .iter()
                .filter(|breakpoint| !breakpoint.is_location())
                .filter(|breakpoint| rendered_numbers.contains(breakpoint.command_number()))
                .map(|breakpoint| (breakpoint.command_number(), breakpoint))
                .collect::<HashMap<_, _>>();

            if rows
                .iter()
                .all(|row| by_number.contains_key(row.number.as_str()))
            {
                for row in rows.iter() {
                    let breakpoint = by_number[row.number.as_str()];
                    let status = breakpoint_status_text(breakpoint);
                    set_label_text(&row.status, &status);
                    row.status.set_visible(!status.is_empty());
                }

                drop(rows);
                drop(breakpoints);
                self.update_control_sensitivity();
                self.record_ui_render_duration("stop-point pane", render_started);
                return;
            }
        }

        clear_box(&self.breakpoints_list);
        self.stop_point_filter_rows.borrow_mut().clear();
        let breakpoints = self.breakpoints.borrow();

        let pending_supported = self
            .gdb_capabilities
            .borrow()
            .supports("pending-breakpoints");

        let total_stop_points = breakpoints.len();
        let mut rendered_stop_points = 0_usize;

        // Stop-point filtering currently operates on rendered rows, so this
        // hard cap must not adapt downward and hide entries that were
        // previously searchable. Unlike locals and threads, the pane needs a
        // true paged model before runtime shedding is safe.
        let stop_point_limit = crate::performance::STOP_POINT_WIDGET_BUDGET;

        if breakpoints.is_empty() {
            self.breakpoints_list.append(&empty_label(
                "No breakpoints, catchpoints, or watchpoints set",
            ));
        } else {
            let rendered_parent_numbers = breakpoints
                .iter()
                .filter(|breakpoint| !breakpoint.is_location())
                .take(stop_point_limit)
                .map(|breakpoint| breakpoint.number.as_str())
                .collect::<HashSet<_>>();

            let mut locations_by_parent: HashMap<&str, Vec<&Breakpoint>> = HashMap::new();
            let mut retained_location_count = 0_usize;

            for location in breakpoints
                .iter()
                .filter(|breakpoint| breakpoint.is_location())
            {
                if retained_location_count >= stop_point_limit {
                    break;
                }

                if let Some(parent) = location
                    .parent_number
                    .as_deref()
                    .filter(|parent| rendered_parent_numbers.contains(parent))
                {
                    locations_by_parent
                        .entry(parent)
                        .or_default()
                        .push(location);

                    retained_location_count += 1;
                }
            }

            for breakpoint in breakpoints
                .iter()
                .filter(|breakpoint| !breakpoint.is_location())
            {
                if rendered_stop_points >= stop_point_limit {
                    break;
                }

                rendered_stop_points += 1;

                let name = if breakpoint.is_watchpoint() {
                    breakpoint
                        .original_location
                        .as_deref()
                        .or(breakpoint.function.as_deref())
                        .or(breakpoint.address.as_deref())
                        .unwrap_or("unresolved expression")
                } else if breakpoint.is_catchpoint() {
                    breakpoint
                        .original_location
                        .as_deref()
                        .or(breakpoint.catch_type.as_deref())
                        .unwrap_or("event")
                } else {
                    breakpoint
                        .function
                        .as_deref()
                        .or(breakpoint.original_location.as_deref())
                        .or(breakpoint.address.as_deref())
                        .unwrap_or("unresolved")
                };

                let location = match (breakpoint.source_path(), breakpoint.line) {
                    (Some(file), Some(line)) => format!("{file}:{line}"),
                    _ if breakpoint.location_count > 0 => format!(
                        "{} resolved location{}",
                        breakpoint.location_count,
                        if breakpoint.location_count == 1 {
                            ""
                        } else {
                            "s"
                        }
                    ),
                    _ if let Some(pending) = breakpoint.pending.as_deref() => {
                        format!("pending · {pending}")
                    }
                    _ if breakpoint.is_watchpoint() => breakpoint.kind.clone(),
                    _ if breakpoint.is_catchpoint() => {
                        breakpoint.catch_type.as_deref().map_or_else(
                            || String::from("event catchpoint"),
                            |kind| format!("{kind} catchpoint"),
                        )
                    }
                    _ => breakpoint
                        .address
                        .clone()
                        .unwrap_or_else(|| String::from("pending")),
                };

                let row = gtk::Box::new(gtk::Orientation::Vertical, 0);
                row.add_css_class("stack-row");
                row.add_css_class("breakpoint-row");

                if breakpoint.is_watchpoint() {
                    row.add_css_class("watchpoint-row");
                }

                if !breakpoint.enabled {
                    row.add_css_class("breakpoint-row-disabled");
                }

                if breakpoint.pending.is_some() {
                    row.add_css_class("breakpoint-row-pending");
                }

                let heading_row = gtk::Box::new(gtk::Orientation::Horizontal, 3);

                let kind = if breakpoint.is_logpoint() {
                    String::from("LOGPOINT")
                } else if breakpoint.is_hardware_breakpoint() {
                    String::from("HARDWARE BREAKPOINT")
                } else if breakpoint.is_watchpoint() || breakpoint.is_catchpoint() {
                    breakpoint.kind.to_ascii_uppercase()
                } else {
                    String::from("BREAKPOINT")
                };

                let badge = gtk::Button::with_label(&format!("#{}", breakpoint.number));
                badge.add_css_class("breakpoint-badge");
                badge.set_focus_on_click(false);

                badge.add_css_class(if breakpoint.enabled {
                    "breakpoint-badge-enabled"
                } else {
                    "breakpoint-badge-disabled"
                });

                badge.set_tooltip_text(Some(if breakpoint.enabled {
                    "Disable this stop point"
                } else {
                    "Enable this stop point"
                }));

                let heading_text = format!("{kind}  {}", compact_function_name(name));
                let heading = gtk::Label::new(Some(&heading_text));
                heading.set_halign(gtk::Align::Start);
                heading.set_ellipsize(pango::EllipsizeMode::End);
                heading.set_hexpand(true);
                heading.set_tooltip_text(Some(&format!("{kind}  {name}")));

                let condition_button = gtk::Button::with_label(
                    if breakpoint.is_watchpoint() || breakpoint.is_catchpoint() {
                        if breakpoint.condition.is_some() {
                            "Edit condition"
                        } else {
                            "Condition"
                        }
                    } else {
                        "Edit"
                    },
                );

                condition_button.add_css_class("inline-action");

                condition_button.set_tooltip_text(Some(
                    if breakpoint.is_watchpoint() || breakpoint.is_catchpoint() {
                        "Add, edit, or clear a GDB condition"
                    } else {
                        "Edit location, behavior, restrictions, commands, or logpoint settings"
                    },
                ));

                let organize_button = gtk::Button::with_label("Organize");
                organize_button.add_css_class("inline-action");

                organize_button.set_tooltip_text(Some(
                    "Assign this stop point to a group and add searchable tags",
                ));

                let delete_button = gtk::Button::with_label("Delete");
                delete_button.add_css_class("inline-action");
                delete_button.add_css_class("danger-action");
                delete_button.set_tooltip_text(Some("Delete this breakpoint"));
                heading_row.append(&badge);
                heading_row.append(&heading);
                heading_row.append(&condition_button);
                heading_row.append(&organize_button);
                heading_row.append(&delete_button);
                let location_text = location;
                let location = gtk::Label::new(Some(&location_text));
                location.add_css_class("muted");
                location.set_halign(gtk::Align::Start);
                location.set_ellipsize(pango::EllipsizeMode::Middle);
                enable_stable_text_selection(&location);
                location.set_tooltip_text(Some(&location_text));
                row.append(&heading_row);
                row.append(&location);
                let organization = gtk::Label::new(None);
                organization.add_css_class("breakpoint-metadata");
                organization.set_halign(gtk::Align::Start);

                let current_metadata = self
                    .stop_point_metadata
                    .borrow()
                    .get(breakpoint.command_number())
                    .cloned()
                    .unwrap_or_default();

                let organization_text = stop_point_metadata_text(&current_metadata);
                organization.set_text(&organization_text);
                organization.set_visible(!organization_text.is_empty());
                row.append(&organization);
                let status_text = breakpoint_status_text(breakpoint);
                let status = gtk::Label::new(Some(&status_text));
                status.add_css_class("breakpoint-metadata");
                status.set_halign(gtk::Align::Start);
                status.set_visible(!status_text.is_empty());
                enable_stable_text_selection(&status);
                row.append(&status);

                if let Some(condition) = breakpoint.condition.as_deref() {
                    let condition = gtk::Label::new(Some(&format!("WHEN  {condition}")));
                    condition.add_css_class("breakpoint-condition");
                    condition.set_halign(gtk::Align::Start);
                    condition.set_ellipsize(pango::EllipsizeMode::End);
                    condition.set_tooltip_text(Some(condition.text().as_str()));
                    row.append(&condition);
                }

                if !breakpoint.commands.is_empty() {
                    let command_text = breakpoint
                        .commands
                        .iter()
                        .map(|command| command.trim())
                        .collect::<Vec<_>>()
                        .join("  ·  ");

                    let commands = gtk::Label::new(Some(&format!("DO  {command_text}")));
                    commands.add_css_class("breakpoint-commands");
                    commands.set_halign(gtk::Align::Start);
                    commands.set_ellipsize(pango::EllipsizeMode::End);
                    commands.set_tooltip_text(Some(&command_text));
                    enable_stable_text_selection(&commands);
                    row.append(&commands);
                }

                let parent = self.window.clone();
                let breakpoint_for_condition = breakpoint.clone();
                let condition_handler = Rc::clone(&self.breakpoint_condition_handler);
                let editor_handler = Rc::clone(&self.breakpoint_editor_handler);

                condition_button.connect_clicked(move |_| {
                    if breakpoint_for_condition.is_watchpoint()
                        || breakpoint_for_condition.is_catchpoint()
                    {
                        open_breakpoint_condition_editor(
                            &parent,
                            breakpoint_for_condition.clone(),
                            Rc::clone(&condition_handler),
                        );
                    } else {
                        open_breakpoint_editor(
                            &parent,
                            Some(breakpoint_for_condition.clone()),
                            pending_supported,
                            Rc::clone(&editor_handler),
                        );
                    }
                });

                let parent = self.window.clone();
                let number = breakpoint.command_number().to_owned();
                let metadata = Rc::clone(&self.stop_point_metadata);
                let filter_rows = Rc::clone(&self.stop_point_filter_rows);
                let filter_controls = self.stop_point_filter.clone();

                organize_button.connect_clicked(move |_| {
                    let current = metadata.borrow().get(&number).cloned().unwrap_or_default();
                    let metadata_for_apply = Rc::clone(&metadata);
                    let rows_for_apply = Rc::clone(&filter_rows);
                    let controls_for_apply = filter_controls.clone();
                    let organization = organization.clone();
                    let number_for_apply = number.clone();

                    open_stop_point_metadata_editor(
                        &parent,
                        &number,
                        &current,
                        Rc::new(move |updated| {
                            let text = stop_point_metadata_text(&updated);
                            organization.set_text(&text);
                            organization.set_visible(!text.is_empty());

                            if updated == StopPointMetadata::default() {
                                metadata_for_apply.borrow_mut().remove(&number_for_apply);
                            } else {
                                metadata_for_apply
                                    .borrow_mut()
                                    .insert(number_for_apply.clone(), updated);
                            }

                            apply_stop_point_filter(
                                &rows_for_apply.borrow(),
                                &metadata_for_apply.borrow(),
                                &controls_for_apply,
                            );
                        }),
                    );
                });

                let number = breakpoint.command_number().to_owned();
                let enable = !breakpoint.enabled;
                let enabled_handler = Rc::clone(&self.breakpoint_enabled_handler);

                badge.connect_clicked(move |_| {
                    let handler = enabled_handler.borrow().clone();

                    if let Some(handler) = handler {
                        handler(number.clone(), enable);
                    }
                });

                let number = breakpoint.command_number().to_owned();
                let delete_handler = Rc::clone(&self.breakpoint_delete_handler);

                delete_button.connect_clicked(move |_| {
                    let handler = delete_handler.borrow().clone();

                    if let Some(handler) = handler {
                        handler(number.clone());
                    }
                });

                self.breakpoints_list.append(&row);
                let mut filter_widgets = vec![row.clone().upcast::<gtk::Widget>()];

                for location in locations_by_parent
                    .get(breakpoint.number.as_str())
                    .into_iter()
                    .flatten()
                {
                    if rendered_stop_points >= stop_point_limit {
                        break;
                    }

                    rendered_stop_points += 1;
                    let location_row = gtk::Box::new(gtk::Orientation::Horizontal, 4);
                    location_row.add_css_class("breakpoint-location-row");

                    if !location.enabled {
                        location_row.add_css_class("breakpoint-row-disabled");
                    }

                    let badge = gtk::Button::with_label(&format!("#{}", location.number));
                    badge.add_css_class("breakpoint-badge");
                    badge.add_css_class("breakpoint-location-badge");
                    badge.set_focus_on_click(false);

                    badge.add_css_class(if location.enabled {
                        "breakpoint-badge-enabled"
                    } else {
                        "breakpoint-badge-disabled"
                    });

                    badge.set_tooltip_text(Some(if location.enabled {
                        "Disable only this resolved location"
                    } else {
                        "Enable only this resolved location"
                    }));

                    let details = gtk::Box::new(gtk::Orientation::Vertical, 0);
                    details.set_hexpand(true);
                    let function_text = location.function.as_deref().unwrap_or("resolved location");
                    let function = gtk::Label::new(Some(&compact_function_name(function_text)));
                    function.set_halign(gtk::Align::Start);
                    function.set_ellipsize(pango::EllipsizeMode::End);
                    function.set_tooltip_text(Some(function_text));

                    let source = match (location.source_path(), location.line) {
                        (Some(path), Some(line)) => format!(
                            "{path}:{line}  ·  {}",
                            location.address.as_deref().unwrap_or("resolved")
                        ),
                        _ => location
                            .address
                            .clone()
                            .unwrap_or_else(|| String::from("resolved")),
                    };

                    let source_label = gtk::Label::new(Some(&source));
                    source_label.add_css_class("muted");
                    source_label.set_halign(gtk::Align::Start);
                    source_label.set_ellipsize(pango::EllipsizeMode::Middle);
                    source_label.set_tooltip_text(Some(&source));
                    enable_stable_text_selection(&source_label);
                    details.append(&function);
                    details.append(&source_label);
                    location_row.append(&badge);
                    location_row.append(&details);
                    let number = location.number.clone();
                    let enable = !location.enabled;
                    let enabled_handler = Rc::clone(&self.breakpoint_enabled_handler);

                    badge.connect_clicked(move |_| {
                        let handler = enabled_handler.borrow().clone();

                        if let Some(handler) = handler {
                            handler(number.clone(), enable);
                        }
                    });

                    self.breakpoints_list.append(&location_row);
                    filter_widgets.push(location_row.upcast::<gtk::Widget>());
                }

                self.stop_point_filter_rows
                    .borrow_mut()
                    .push(StopPointFilterRow {
                        widgets: filter_widgets,
                        number: breakpoint.command_number().to_owned(),
                        searchable: stop_point_search_text(breakpoint),
                        status,
                        hardware: breakpoint.is_hardware_breakpoint(),
                        watchpoint: breakpoint.is_watchpoint(),
                        catchpoint: breakpoint.is_catchpoint(),
                        enabled: breakpoint.enabled,
                    });
            }
        }

        if rendered_stop_points < total_stop_points {
            let omitted = total_stop_points - rendered_stop_points;

            let notice = performance_partial_label(&format!(
                "{omitted} additional stop point{} not rendered. Use the GDB console for the complete set",
                if omitted == 1 { " was" } else { "s were" }
            ));

            self.breakpoints_list.append(&notice);

            self.record_performance_notice(crate::performance::PerformanceNotice::count(
                crate::performance::BudgetOutcome::Partial,
                "stop-point pane",
                rendered_stop_points,
                total_stop_points,
            ));
        }

        self.breakpoints_list.append(&self.stop_point_filter.empty);

        apply_stop_point_filter(
            &self.stop_point_filter_rows.borrow(),
            &self.stop_point_metadata.borrow(),
            &self.stop_point_filter,
        );

        for (button, signal, description) in &self.signal_buttons {
            if let Some(number) = signal_catchpoint_command_number(&breakpoints, signal) {
                button.add_css_class("signal-caught");

                button.set_tooltip_text(Some(&format!(
                    "{description}\nCatchpoint #{number} is active. Click to remove it"
                )));
            } else {
                button.remove_css_class("signal-caught");

                button.set_tooltip_text(Some(&format!(
                    "{description}\nClick to add a GDB signal catchpoint"
                )));
            }
        }

        for (button, event) in &self.event_catchpoint_buttons {
            if let Some(number) = event_catchpoint_command_number(&breakpoints, *event) {
                button.add_css_class("signal-caught");

                button.set_tooltip_text(Some(&format!(
                    "{} catchpoint #{number} is active. Click to remove it",
                    event.label()
                )));
            } else {
                button.remove_css_class("signal-caught");

                let description = EventCatchpoint::ALL
                    .iter()
                    .find(|(candidate, _, _)| candidate == event)
                    .map(|(_, _, description)| *description)
                    .unwrap_or("Click to add this catchpoint");

                button.set_tooltip_text(Some(description));
            }
        }

        for document in self.source_documents.borrow().iter() {
            document.breakpoint_renderer.queue_draw();
        }

        drop(breakpoints);
        self.update_control_sensitivity();
        self.record_ui_render_duration("stop-point pane", render_started);
    }

    pub fn start_breakpoint_refresh(&self) -> u64 {
        let generation = self.breakpoint_refresh_generation.get().wrapping_add(1);
        self.breakpoint_refresh_generation.set(generation);

        generation
    }

    pub fn begin_breakpoint_refresh(&self) -> Option<u64> {
        self.breakpoint_refresh_gate
            .begin()
            .then(|| self.start_breakpoint_refresh())
    }

    pub fn finish_breakpoint_refresh(&self) -> bool {
        self.breakpoint_refresh_gate.finish()
    }

    pub fn begin_module_refresh(&self) -> bool {
        self.module_refresh_gate.begin()
    }

    pub fn finish_module_refresh(&self) -> bool {
        self.module_refresh_gate.finish()
    }

    pub fn mark_modules_dirty(&self) {
        self.modules_dirty.set(true);
    }

    pub fn take_modules_dirty(&self) -> bool {
        self.modules_dirty.replace(false)
    }

    pub fn show_breakpoints_for_refresh(&self, generation: u64, breakpoints: Vec<Breakpoint>) {
        if self.is_breakpoint_refresh_current(generation) {
            self.show_breakpoints(breakpoints);
        }
    }

    pub(crate) fn is_breakpoint_refresh_current(&self, generation: u64) -> bool {
        self.breakpoint_refresh_generation.get() == generation
    }

    pub fn set_breakpoint_enabled_pending(&self, number: &str, enabled: bool) -> bool {
        let mut breakpoints = self.breakpoints.borrow().clone();
        let changed = set_breakpoint_enabled(&mut breakpoints, number, enabled);

        if changed {
            self.start_breakpoint_refresh();
            self.breakpoint_refresh_gate.invalidate();
            self.show_breakpoints(breakpoints);
        }

        changed
    }

    pub fn breakpoint_number_at_address(&self, address: &str) -> Option<String> {
        breakpoint_command_number_at_address(&self.breakpoints.borrow(), address)
    }
}

pub(super) fn frame_location_text(frame: &StackFrame) -> String {
    frame.line.map_or_else(
        || frame.address.clone(),
        |line| {
            format!(
                "{}:{line}",
                frame.source_path().unwrap_or(frame.address.as_str())
            )
        },
    )
}

fn update_frame_button(button: &gtk::Button, frame: &StackFrame) {
    let Some(row) = button.child().and_downcast::<gtk::Box>() else {
        return;
    };

    let Some(function) = row.first_child().and_downcast::<gtk::Label>() else {
        return;
    };

    let Some(location) = row.last_child().and_downcast::<gtk::Label>() else {
        return;
    };

    function.set_text(&format!(
        "#{}  {}",
        frame.level,
        compact_function_name(&frame.function)
    ));

    function.set_tooltip_text(Some(&frame.function));
    let location_text = frame_location_text(frame);
    location.set_text(&location_text);
    location.set_tooltip_text(Some(&location_text));
}

fn thread_button_content(thread: &ThreadInfo, stop_reason: Option<&str>) -> gtk::Box {
    let marker = if thread.current { "*" } else { " " };
    let tid = thread_os_id(&thread.target_id).unwrap_or_else(|| String::from("?"));
    let name = thread.name.as_deref().unwrap_or("<unnamed>");
    let detail = thread_detail(thread, stop_reason);
    let row = gtk::Box::new(gtk::Orientation::Vertical, 0);

    let heading = gtk::Label::new(Some(&format!(
        "[{marker}Thread Id:{}, tid:{tid}]",
        thread.id
    )));

    heading.add_css_class("thread-heading");
    heading.set_halign(gtk::Align::Start);
    heading.set_ellipsize(pango::EllipsizeMode::End);
    let name = gtk::Label::new(Some(&format!("Name: \"{name}\"")));
    name.add_css_class("thread-name");
    name.set_halign(gtk::Align::Start);
    name.set_ellipsize(pango::EllipsizeMode::End);
    let detail_widget = thread_detail_widget(thread, stop_reason);
    let full_symbol = thread.frame.as_ref().map(|frame| frame.function.as_str());

    detail_widget.set_tooltip_text(Some(&match full_symbol {
        Some(symbol) => format!(
            "{detail}\nFull symbol: {symbol}\nGDB target: {}",
            thread.target_id
        ),
        None => format!("{detail}\nGDB target: {}", thread.target_id),
    }));

    row.append(&heading);
    row.append(&name);
    row.append(&detail_widget);

    row
}

fn update_thread_button(button: &gtk::Button, thread: &ThreadInfo, stop_reason: Option<&str>) {
    let Some(row) = button.child().and_downcast::<gtk::Box>() else {
        return;
    };

    let Some(heading) = row.first_child().and_downcast::<gtk::Label>() else {
        return;
    };

    let Some(name) = heading.next_sibling().and_downcast::<gtk::Label>() else {
        return;
    };

    let Some(detail_widget) = row.last_child().and_downcast::<gtk::Box>() else {
        return;
    };

    let marker = if thread.current { "*" } else { " " };
    let tid = thread_os_id(&thread.target_id).unwrap_or_else(|| String::from("?"));
    let thread_name = thread.name.as_deref().unwrap_or("<unnamed>");

    set_label_text(
        &heading,
        &format!("[{marker}Thread Id:{}, tid:{tid}]", thread.id),
    );

    set_label_text(&name, &format!("Name: \"{thread_name}\""));

    if !update_thread_detail_widget(&detail_widget, thread, stop_reason) {
        return;
    }

    let detail = thread_detail(thread, stop_reason);
    let full_symbol = thread.frame.as_ref().map(|frame| frame.function.as_str());

    detail_widget.set_tooltip_text(Some(&match full_symbol {
        Some(symbol) => format!(
            "{detail}\nFull symbol: {symbol}\nGDB target: {}",
            thread.target_id
        ),
        None => format!("{detail}\nGDB target: {}", thread.target_id),
    }));

    set_css_class(button, "current-debug-item", thread.current);
}

fn sync_thread_partial_notice(container: &gtk::Box, omitted: usize) {
    let existing = container
        .last_child()
        .filter(|child| child.has_css_class("performance-partial"));

    if omitted == 0 {
        if let Some(existing) = existing {
            container.remove(&existing);
        }

        return;
    }

    let text = format!(
        "{omitted} matching thread{} not rendered. Narrow the filter to inspect them",
        if omitted == 1 { " was" } else { "s were" }
    );

    if let Some(label) = existing.and_downcast::<gtk::Label>() {
        label.set_text(&text);
    } else {
        let label = performance_partial_label(&text);
        container.append(&label);
    }
}

fn performance_partial_label(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.add_css_class("performance-partial");
    label.set_halign(gtk::Align::Fill);
    label.set_xalign(0.0);
    label.set_wrap(true);

    label
}

#[cfg(test)]
mod render_tests {
    use super::*;

    fn breakpoint(number: &str) -> Breakpoint {
        Breakpoint {
            number: number.to_owned(),
            kind: String::from("breakpoint"),
            enabled: true,
            condition: None,
            address: Some(String::from("0x1000")),
            function: Some(String::from("main")),
            file: Some(String::from("main.c")),
            fullname: Some(String::from("/tmp/main.c")),
            line: Some(12),
            original_location: Some(String::from("main")),
            catch_type: None,
            disposition: Some(String::from("keep")),
            hit_count: 0,
            ignore_count: 0,
            thread: None,
            inferior: None,
            pending: None,
            commands: Vec::new(),
            parent_number: None,
            location_count: 0,
        }
    }

    fn stack_entry(value: &str, chain: &[&str], region: Option<&str>) -> StackEntry {
        StackEntry {
            address: 0x1000,
            offset: 0,
            index: 0,
            pointer_bits: 64,
            endian: TargetEndian::Little,
            value: value.to_owned(),
            pointer_chain: chain.iter().map(|value| (*value).to_owned()).collect(),
            address_registers: vec![String::from("rsp")],
            value_registers: Vec::new(),
            return_frame: None,
            memory_kind: MemoryKind::Heap,
            region: region.map(str::to_owned),
        }
    }

    fn frame(level: u32) -> StackFrame {
        StackFrame {
            level,
            address: format!("0x{level:x}"),
            function: format!("frame_{level}"),
            architecture: None,
            file: None,
            fullname: None,
            line: None,
        }
    }

    #[test]
    fn bounded_stack_frames_keep_a_deep_selected_frame() {
        let frames = (0..20).map(frame).collect::<Vec<_>>();
        let visible = bounded_stack_frames(&frames, 5, 19);
        assert_eq!(visible.len(), 5);
        assert_eq!(visible[0].level, 19);
        assert_eq!(visible[1].level, 0);
    }

    #[test]
    fn local_summary_reports_the_visible_page_without_losing_totals() {
        assert_eq!(
            locals_summary_text(900, 100, 0, 512, 1_000),
            "900 locals  100 args  512/1000 shown"
        );
    }

    #[test]
    fn preserves_only_stable_stack_pointer_details_between_refresh_phases() {
        let mut previous = stack_entry(
            "0x2000",
            &["0x2000", "0x3000", "0x6f6c6c6568"],
            Some("heap"),
        );

        previous.memory_kind = MemoryKind::String;
        let mut stable = vec![stack_entry("0x2000", &[], Some("heap"))];
        preserve_stack_render_details(&mut stable, std::slice::from_ref(&previous));
        assert_eq!(stable[0].pointer_chain, previous.pointer_chain);
        assert_eq!(stable[0].memory_kind, MemoryKind::String);
        let mut changed_value = vec![stack_entry("0x2008", &[], Some("heap"))];
        preserve_stack_render_details(&mut changed_value, std::slice::from_ref(&previous));
        assert!(changed_value[0].pointer_chain.is_empty());
        let mut changed_region = vec![stack_entry("0x2000", &[], Some("unmapped"))];
        preserve_stack_render_details(&mut changed_region, &[previous]);
        assert!(changed_region[0].pointer_chain.is_empty());
    }

    #[test]
    fn breakpoint_counter_changes_do_not_require_row_reconstruction() {
        let current = breakpoint("1");
        let mut updated = current.clone();
        updated.hit_count = 2;
        updated.ignore_count = 3;

        assert!(breakpoint_layout_matches(
            std::slice::from_ref(&current),
            std::slice::from_ref(&updated)
        ));

        assert_eq!(breakpoint_status_text(&updated), "2 HITS  ·  STOP ON HIT 4");
        updated.enabled = false;
        assert!(!breakpoint_layout_matches(&[current], &[updated]));
    }

    #[test]
    fn page_errors_preserve_loaded_children_and_offer_a_retry() {
        let parent = Variable {
            name: String::from("items"),
            value: String::from("{...}"),
            type_name: Some(String::from("Item [256]")),
            argument: false,
            varobj: Some(String::from("var1")),
            num_children: 256,
            has_more: true,
            display_hint: Some(String::from("array")),
            dynamic: true,
        };

        let node = VariableNode::new(parent.clone());

        node.children
            .append(&glib::BoxedAnyObject::new(VariableNode::new(Variable {
                name: String::from("[0]"),
                value: String::from("1"),
                type_name: Some(String::from("int")),
                argument: false,
                varobj: Some(String::from("var1.0")),
                num_children: 0,
                has_more: false,
                display_hint: None,
                dynamic: false,
            })));

        node.children
            .append(&glib::BoxedAnyObject::new(VariableNode::load_more(
                parent.clone(),
                128,
            )));

        apply_variable_children_page_error(&node, &parent, 128, "temporary failure");
        assert_eq!(node.children.n_items(), 2);

        let first = node
            .children
            .item(0)
            .and_downcast::<glib::BoxedAnyObject>()
            .unwrap();

        assert_eq!(first.borrow::<VariableNode>().variable.name, "[0]");

        let retry = node
            .children
            .item(1)
            .and_downcast::<glib::BoxedAnyObject>()
            .unwrap();

        let retry = retry.borrow::<VariableNode>();
        assert_eq!(retry.variable.name, "Retry loading more…");
        assert_eq!(retry.load_more.as_ref().map(|(_, from)| *from), Some(128));
    }

    #[test]
    fn initial_expansion_errors_can_be_retried() {
        let parent = Variable {
            name: String::from("head"),
            value: String::from("0x20"),
            type_name: Some(String::from("Node *")),
            argument: false,
            varobj: Some(String::from("var1")),
            num_children: 1,
            has_more: false,
            display_hint: None,
            dynamic: false,
        };

        let node = VariableNode::new(parent.clone());
        apply_variable_children_page_error(&node, &parent, 0, "temporary failure");
        assert_eq!(node.children.n_items(), 1);

        let retry = node
            .children
            .item(0)
            .and_downcast::<glib::BoxedAnyObject>()
            .unwrap();

        let retry = retry.borrow::<VariableNode>();
        assert_eq!(retry.variable.name, "Retry expansion…");
        assert_eq!(retry.load_more.as_ref().map(|(_, from)| *from), Some(0));
    }
}
