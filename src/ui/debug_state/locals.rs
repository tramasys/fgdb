use super::*;

pub(super) fn locals_summary_text(
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

impl Ui {
    pub fn show_locals(&self, variables: &[Variable]) {
        self.model.publish_locals(None, variables);
        self.locals_view.set_tooltip_text(None);
        self.locals_summary.set_tooltip_text(None);

        self.locals_render_limit.set(self.adaptive_render_limit(
            "locals pane",
            crate::performance::LOCALS_ROOT_PAGE_SIZE,
            64,
        ));

        self.render_locals();
    }

    pub(super) fn render_locals(&self) {
        let render_started = Instant::now();
        let query = self.locals_filter.text().trim().to_ascii_lowercase();
        let limit = self.locals_render_limit.get();

        let (rendered, matching_total, root_count, arguments) = {
            let variables = self.model.locals();
            let (rendered, matching_total) = variables.filtered(&query, limit);

            (
                rendered,
                matching_total,
                variables.len(),
                variables.argument_count(),
            )
        };

        let shown = rendered.len();

        let selected_name = variable_at(
            &self.locals_tree.selection,
            self.locals_tree.selection.selected(),
        )
        .map(|variable| (variable.name, variable.argument, variable.local_index));

        let changed = self.locals_tree.replace_roots(&rendered, true);

        let locals = root_count.saturating_sub(arguments);
        let changed_count = changed_variable_roots(&self.locals_tree.store);

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
        } else {
            self.locals_empty.set_visible(false);

            if changed == VariableRootChange::Rebuilt {
                self.locals_tree
                    .selection
                    .set_selected(gtk::INVALID_LIST_POSITION);

                let selected = selected_name
                    .as_ref()
                    .and_then(|(name, argument, index)| {
                        root_variable_position(&self.locals_tree.selection, name, *argument, *index)
                    })
                    .unwrap_or(0);

                self.locals_tree.selection.set_selected(selected);
            }
        }

        self.update_control_sensitivity();
        self.record_ui_render_duration("locals pane", render_started);
    }

    pub fn show_locals_for_refresh(&self, generation: u64, variables: &[Variable]) {
        if let Some(new_snapshot) = self.model.publish_locals(Some(generation), variables) {
            if new_snapshot {
                self.locals_render_limit.set(self.adaptive_render_limit(
                    "locals pane",
                    crate::performance::LOCALS_ROOT_PAGE_SIZE,
                    64,
                ));
            }

            self.render_locals();
            self.locals_view.set_tooltip_text(None);
            self.locals_summary.set_tooltip_text(None);
        }
    }

    pub(crate) fn show_locals_refresh_error(&self, generation: u64, error: &str) {
        if !self.model.is_stop_refresh_current(generation) {
            return;
        }

        self.application_log
            .record(LogLevel::Error, "Locals refresh failed", error);

        self.locals_view.set_tooltip_text(Some(error));
        self.locals_summary.set_text("Locals refresh failed");
        self.locals_summary.set_tooltip_text(Some(error));

        if self.locals_tree.store.n_items() == 0 {
            self.locals_empty.set_text(error);
            self.locals_empty.set_visible(true);
        }

        self.update_control_sensitivity();
    }

    pub(in crate::ui) fn locals_inspection_available(&self) -> bool {
        self.model.locals_inspection_available()
    }

    pub fn show_local_root_for_refresh(&self, generation: u64, index: usize, variable: &Variable) {
        // Automatic root creation is staged by the refresh owner until the
        // complete snapshot arrives. Do not patch a previous stop's rows.
        if !self.model.update_local_root(generation, index, variable) {
            return;
        }

        let position = (0..self.locals_tree.store.n_items() as usize).find(|position| {
            variable_root_node(&self.locals_tree.store, *position)
                .is_some_and(|node| node.variable.local_index == Some(index))
        });

        if let Some(position) = position {
            let mut variable = variable.clone();
            variable.local_index = Some(index);
            self.locals_tree.replace_root(position, &variable, false);
        }
    }

    pub fn show_variable_descendant_updates_for_refresh(
        &self,
        generation: u64,
        updates: &[VariableUpdate],
    ) {
        if !self.model.is_stop_refresh_current(generation) || updates.is_empty() {
            return;
        }

        let locals_updated = self.locals_tree.apply_updates(updates);
        self.watches_tree.apply_updates(updates);

        if locals_updated > 0 {
            let roots = self.model.locals();
            let arguments = roots.argument_count();

            self.locals_summary.set_text(&locals_summary_text(
                roots.len().saturating_sub(arguments),
                arguments,
                changed_variable_roots(&self.locals_tree.store),
                self.locals_tree.store.n_items() as usize,
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

        if !node.accepts_child_page(parent, from) {
            return false;
        }

        self.variable_tree_for(parent)
            .replace_children(&node, parent, from, variables, has_more);

        true
    }

    pub(crate) fn variable_children_target_is_current(
        &self,
        parent: &Variable,
        from: usize,
    ) -> bool {
        self.variable_action_is_current(parent)
            && parent
                .varobj
                .as_deref()
                .and_then(|varobj| self.find_variable_node(varobj))
                .is_some_and(|node| node.accepts_child_page(parent, from))
    }

    pub(crate) fn begin_variable_children_loading(&self, parent: &Variable, from: usize) -> bool {
        let Some(node) = parent
            .varobj
            .as_deref()
            .and_then(|varobj| self.find_variable_node(varobj))
        else {
            return false;
        };

        if !node.accepts_child_page(parent, from) {
            return false;
        }

        node.children_loading.set(true);

        // Lazy varobj attachment replaces the root before GTK binds its row.
        // Claim loading now so that rebinding cannot enqueue a duplicate read.
        if from == 0 && node.children.n_items() == 0 {
            node.children
                .append(&SnapshotRow::new(VariableNode::placeholder(
                    "loading…",
                    "waiting for GDB",
                )));
        }

        true
    }

    pub fn has_variable_object(&self, varobj: &str) -> bool {
        self.variable_trees()
            .iter()
            .any(|tree| tree.contains(varobj))
    }

    pub fn show_variable_children_page_error(&self, parent: &Variable, from: usize, error: &str) {
        let Some(parent_name) = parent.varobj.as_deref() else {
            return;
        };

        let Some(node) = self.find_variable_node(parent_name) else {
            return;
        };

        if !node.accepts_child_page(parent, from) {
            return;
        }

        self.variable_tree_for(parent)
            .children_error(&node, parent, from, error);
        self.application_log
            .record(LogLevel::Error, &format!("Expand {}", parent.name), error);
    }

    pub(crate) fn show_lazy_variable_children_error(&self, variable: &Variable, error: &str) {
        let Some(node) = self
            .local_variable_node(variable)
            .map(|(_, node)| node)
            .or_else(|| self.return_variable_node(variable))
        else {
            return;
        };

        self.variable_tree_for(variable)
            .children_error(&node, variable, 0, error);
        self.application_log
            .record(LogLevel::Error, &format!("Expand {}", variable.name), error);
    }

    pub(crate) fn has_local_variable_identity(&self, variable: &Variable) -> bool {
        self.model.has_local_variable_identity(variable)
            && self.local_variable_node(variable).is_some()
    }

    /// The model authorizes the stop. Displayed variables must additionally
    /// belong to the published snapshot, not the previous stop kept on screen.
    pub(crate) fn variable_action_is_current(&self, variable: &Variable) -> bool {
        let generation = self.model.current_stop_refresh_generation();
        if !self.model.can_edit_variable(generation) {
            return false;
        }

        if variable.return_value.is_some() {
            return self.return_value_action_is_current(variable);
        }

        if variable.local_index.is_some() {
            return self.locals_inspection_available()
                && self.local_variable_node(variable).is_some();
        }

        let Some(varobj) = variable.varobj.as_deref() else {
            return true;
        };

        let Some(node) = self.find_variable_node(varobj) else {
            return false;
        };

        (!node.local || self.locals_inspection_available())
            && node.variable.has_same_children(variable)
    }

    pub(crate) fn cancel_variable_children_request(&self, variable: &Variable) {
        let node = variable
            .varobj
            .as_deref()
            .and_then(|varobj| self.find_variable_node(varobj))
            .or_else(|| self.local_variable_node(variable).map(|(_, node)| node))
            .or_else(|| self.return_variable_node(variable));

        if let Some(node) = node
            && node.variable.has_same_children(variable)
        {
            let loading = node.children_loading.replace(false);

            if loading && !node.children_loaded.get() {
                self.variable_tree_for(variable).children_error(
                    &node,
                    variable,
                    0,
                    "Expansion cancelled. Retry while the target is paused",
                );
            }

            if let Some(item) = node
                .children
                .n_items()
                .checked_sub(1)
                .and_then(|index| node.children.item(index))
                .and_downcast::<SnapshotRow>()
            {
                item.borrow::<VariableNode>().children_loading.set(false);
            }
        }
    }

    pub(crate) fn claim_local_variable_object(&self, generation: u64, variable: &Variable) -> bool {
        self.local_variable_node(variable).is_some()
            && self.model.claim_local_variable_object(generation, variable)
    }

    pub(crate) fn finish_local_variable_object(&self, generation: u64, variable: &Variable) {
        self.model
            .finish_local_variable_object(generation, variable);
    }

    pub(crate) fn attach_local_variable_object(
        &self,
        generation: u64,
        original: &Variable,
        variable: &Variable,
    ) -> bool {
        if !self.model.is_stop_refresh_current(generation) {
            return false;
        }

        let Some((position, _)) = self.local_variable_node(original) else {
            return false;
        };

        let Some(index) = original.local_index else {
            return false;
        };

        if !self.model.has_local_variable_identity(original)
            || !self.model.update_local_root(generation, index, variable)
        {
            return false;
        }

        self.locals_tree
            .replace_root(position as usize, variable, false)
            .is_some()
    }

    pub fn local_variable_objects(&self) -> Vec<Variable> {
        self.model.locals().to_vec()
    }

    pub(crate) fn local_variable_refresh_indices(&self, variables: &[Variable]) -> HashSet<usize> {
        let query = self.locals_filter.text().trim().to_ascii_lowercase();
        let limit = self.adaptive_render_limit(
            "locals pane",
            crate::performance::LOCALS_ROOT_PAGE_SIZE,
            64,
        );

        local_refresh_indices(variables, &query, limit)
    }

    pub(in crate::ui) fn find_variable_node(&self, varobj: &str) -> Option<VariableNode> {
        self.variable_trees()
            .iter()
            .find_map(|tree| tree.get(varobj))
    }

    fn variable_trees(&self) -> [&VariableTree; 3] {
        [
            &self.locals_tree,
            &self.watches_tree,
            &self.return_value.tree,
        ]
    }

    fn variable_tree_for(&self, variable: &Variable) -> &VariableTree {
        if variable.local_index.is_some() {
            &self.locals_tree
        } else if variable.return_value.is_some() {
            &self.return_value.tree
        } else {
            self.variable_trees()
                .into_iter()
                .find(|tree| {
                    variable
                        .varobj
                        .as_deref()
                        .is_some_and(|varobj| tree.contains(varobj))
                })
                .unwrap_or(&self.watches_tree)
        }
    }

    pub(super) fn local_variable_node(&self, variable: &Variable) -> Option<(u32, VariableNode)> {
        (0..self.locals_tree.store.n_items()).find_map(|position| {
            let item = self
                .locals_tree
                .store
                .item(position)
                .and_downcast::<SnapshotRow>()?;

            let node = item.borrow::<VariableNode>();

            (!node.placeholder && node.variable.has_same_children(variable))
                .then(|| (position, node.clone()))
        })
    }

    pub(crate) fn connect_local_paging(self: &Rc<Self>) {
        let weak_ui = Rc::downgrade(self);

        self.locals_tree
            .selection
            .connect_selected_notify(move |_| {
                if let Some(ui) = weak_ui.upgrade() {
                    ui.update_control_sensitivity();
                }
            });

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

    pub(in crate::ui) fn connect_local_activation(&self) {
        // Keep GTK sensitivity stable across steps. Reject new interactions
        // with the retained snapshot before they reach row/button controllers.
        // Let releases finish existing gestures, and retain scrolling and Tab
        // navigation. Action handlers also guard non-pointer activation.
        for widget in [
            self.locals_view.upcast_ref::<gtk::Widget>(),
            self.locals_edit_button.upcast_ref::<gtk::Widget>(),
        ] {
            let events = gtk::EventControllerLegacy::new();
            events.set_propagation_phase(gtk::PropagationPhase::Capture);
            let model = Rc::clone(&self.model);

            events.connect_event(move |_, event| {
                let starts_interaction = match event.event_type() {
                    gtk::gdk::EventType::ButtonPress
                    | gtk::gdk::EventType::TouchBegin
                    | gtk::gdk::EventType::PadButtonPress => true,
                    gtk::gdk::EventType::KeyPress => event
                        .downcast_ref::<gtk::gdk::KeyEvent>()
                        .is_some_and(|event| {
                            !matches!(
                                event.keyval(),
                                gtk::gdk::Key::Tab
                                    | gtk::gdk::Key::ISO_Left_Tab
                                    | gtk::gdk::Key::Escape
                            )
                        }),
                    _ => false,
                };

                if starts_interaction && !model.locals_inspection_available() {
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            });

            widget.add_controller(events);
        }

        let selection = self.locals_tree.selection.clone();
        let handler = Rc::clone(&self.variable_assignment_handler);
        let float_handler = Rc::clone(&self.float_assignment_handler);
        let editor_handler = Rc::clone(&self.variable_editor_handler);
        let string_handler = Rc::clone(&self.string_assignment_handler);
        let children_handler = Rc::clone(&self.variable_children_handler);
        let current_source_language = Rc::clone(&self.current_source_language);
        let model = Rc::clone(&self.model);

        let panels = Rc::clone(&self.panels);

        self.locals_view.connect_activate(move |view, position| {
            if !model.locals_inspection_available() {
                return;
            }

            let Some((row, node)) = variable_node_at(&selection, position) else {
                return;
            };

            if node.load_more.is_some() {
                defer_next_variable_page(&row, &selection, &children_handler);
            } else if !node.placeholder {
                if row.is_expandable() {
                    defer_variable_toggle(&row, &selection, &children_handler);
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
                        editor_handler(variable, PanelId::Context);
                    } else if let Some(window) = workspace::host_window(view) {
                        let editor = open_variable_editor(
                            &window,
                            variable,
                            model.target_pointer_width(),
                            model.target_architecture(),
                            current_source_language.get(),
                            None,
                            ValueEditorHandlers {
                                model: Rc::clone(&model),
                                assignment: Rc::clone(&handler),
                                float: Rc::clone(&float_handler),
                                string: Rc::clone(&string_handler),
                            },
                        );

                        panels.track_dialog(PanelId::Context, &editor);
                    }
                }
            }
        });

        let selection = self.locals_tree.selection.clone();
        let handler = Rc::clone(&self.variable_assignment_handler);
        let float_handler = Rc::clone(&self.float_assignment_handler);
        let editor_handler = Rc::clone(&self.variable_editor_handler);
        let string_handler = Rc::clone(&self.string_assignment_handler);
        let current_source_language = Rc::clone(&self.current_source_language);
        let model = Rc::clone(&self.model);

        let panels = Rc::clone(&self.panels);

        self.locals_edit_button.connect_clicked(move |button| {
            if !model.locals_inspection_available() {
                return;
            }

            if let Some(variable) = variable_at(&selection, selection.selected())
                && variable.is_available()
            {
                let editor_handler = editor_handler.borrow().clone();

                if let Some(editor_handler) = editor_handler {
                    editor_handler(variable, PanelId::Context);
                } else if let Some(window) = workspace::host_window(button) {
                    let editor = open_variable_editor(
                        &window,
                        variable,
                        model.target_pointer_width(),
                        model.target_architecture(),
                        current_source_language.get(),
                        None,
                        ValueEditorHandlers {
                            model: Rc::clone(&model),
                            assignment: Rc::clone(&handler),
                            float: Rc::clone(&float_handler),
                            string: Rc::clone(&string_handler),
                        },
                    );

                    panels.track_dialog(PanelId::Context, &editor);
                }
            }
        });
    }
}
