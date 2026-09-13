use super::*;

impl Ui {
    pub fn expression_watch_expressions(&self) -> Vec<String> {
        self.model.watch_expressions().clone()
    }

    pub fn expression_watches_match(&self, expected: &[String]) -> bool {
        self.model.watch_expressions().as_slice() == expected
    }

    pub fn expression_watch_variable_objects(&self) -> Vec<Variable> {
        self.model.watch_variables()
    }

    pub fn show_expression_watches_for_refresh(&self, generation: u64, variables: &[Variable]) {
        if !self.model.publish_watches(generation, variables) {
            return;
        }

        let selected = root_variable_at(
            &self.watches_tree.selection,
            self.watches_tree.selection.selected(),
        )
        .map(|variable| variable.name);
        let changed = self.watches_tree.replace_roots(variables, true);

        self.expression_watches_empty
            .set_visible(variables.is_empty());

        if changed == VariableRootChange::Unchanged {
            self.update_control_sensitivity();
            return;
        }

        if changed == VariableRootChange::Rebuilt && !variables.is_empty() {
            self.watches_tree
                .selection
                .set_selected(gtk::INVALID_LIST_POSITION);

            let selected = selected
                .as_deref()
                .and_then(|name| {
                    root_variable_position(&self.watches_tree.selection, name, false, None)
                })
                .unwrap_or(0);

            self.watches_tree.selection.set_selected(selected);
        }

        self.update_control_sensitivity();
    }

    pub fn show_expression_watch_root_for_refresh(
        &self,
        generation: u64,
        index: usize,
        variable: &Variable,
    ) {
        if !self.model.update_watch_root(generation, index, variable) {
            return;
        }

        self.watches_tree.replace_root(index, variable, false);
    }

    pub fn show_expression_watches_unavailable(&self, value: &str) {
        let variables = self
            .model
            .watch_expressions()
            .iter()
            .map(|expression| Variable {
                local_index: None,
                return_value: None,
                name: expression.clone(),
                value: value.to_owned(),
                type_name: None,
                argument: false,
                varobj: None,
                num_children: 0,
                has_more: false,
                display_hint: None,
                dynamic: false,
            })
            .collect::<Vec<_>>();

        self.show_expression_watches_for_refresh(
            self.model.current_stop_refresh_generation(),
            &variables,
        );
    }

    pub(super) fn connect_expression_watch_controls(&self) {
        let add_button = self.expression_watch_add_button.clone();

        self.expression_watch_entry.connect_activate(move |_| {
            if add_button.is_sensitive() {
                add_button.emit_clicked();
            }
        });

        let button = self.expression_watch_add_button.clone();
        let model = Rc::clone(&self.model);

        self.expression_watch_entry.connect_changed(move |entry| {
            let expression = entry.text();

            button.set_sensitive(
                model.execution().ready
                    && !model.execution().state.inferior_running()
                    && !model.execution().command_pending
                    && model.can_add_watch(&expression),
            );
        });

        let entry = self.expression_watch_entry.clone();
        let model = Rc::clone(&self.model);
        let refresh = Rc::clone(&self.expression_watch_refresh_handler);

        self.expression_watch_add_button.connect_clicked(move |_| {
            let expression = entry.text().trim().to_owned();

            if !model.add_watch(&expression) {
                return;
            }

            entry.set_text("");
            let refresh = refresh.borrow().clone();

            if let Some(refresh) = refresh {
                refresh();
            }
        });

        let remove_button = self.expression_watch_remove_button.clone();
        let model = Rc::clone(&self.model);

        self.watches_tree
            .selection
            .connect_selected_notify(move |selection| {
                remove_button.set_sensitive(
                    model.execution().ready
                        && !model.execution().state.inferior_running()
                        && !model.execution().command_pending
                        && root_variable_at(selection, selection.selected()).is_some(),
                );
            });

        let selection = self.watches_tree.selection.clone();
        let model = Rc::clone(&self.model);
        let refresh = Rc::clone(&self.expression_watch_refresh_handler);

        self.expression_watch_remove_button
            .connect_clicked(move |_| {
                let Some(variable) = root_variable_at(&selection, selection.selected()) else {
                    return;
                };

                model.remove_watch(&variable.name);

                let refresh = refresh.borrow().clone();

                if let Some(refresh) = refresh {
                    refresh();
                }
            });

        let selection = self.watches_tree.selection.clone();
        let handler = Rc::clone(&self.variable_assignment_handler);
        let float_handler = Rc::clone(&self.float_assignment_handler);
        let editor_handler = Rc::clone(&self.variable_editor_handler);
        let string_handler = Rc::clone(&self.string_assignment_handler);
        let children_handler = Rc::clone(&self.variable_children_handler);
        let current_source_language = Rc::clone(&self.current_source_language);
        let model = Rc::clone(&self.model);

        let panels = Rc::clone(&self.panels);

        self.expression_watches_view
            .connect_activate(move |view, position| {
                if !model.execution().ready
                    || !model.execution().state.inferior_started()
                    || model.execution().state.inferior_running()
                    || model.execution().command_pending
                    || model.execution().session_pending
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
                        let expanded = !node.expanded.get();
                        node.expanded.set(expanded);
                        row.set_expanded(expanded);

                        if expanded {
                            defer_variable_children_if_expanded(
                                &row,
                                &selection,
                                &children_handler,
                            );
                        }
                    } else {
                        let variable = node.variable;
                        let editor_handler = editor_handler.borrow().clone();

                        if let Some(editor_handler) = editor_handler {
                            editor_handler(variable, PanelId::Watches);
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

                            panels.track_dialog(PanelId::Watches, &editor);
                        }
                    }
                }
            });
    }
}
