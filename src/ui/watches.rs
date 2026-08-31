use super::*;

impl Ui {
    pub fn expression_watch_expressions(&self) -> Vec<String> {
        self.expression_watches.borrow().clone()
    }

    pub fn expression_watches_match(&self, expected: &[String]) -> bool {
        self.expression_watches.borrow().as_slice() == expected
    }

    pub fn expression_watch_variable_objects(&self) -> Vec<Variable> {
        root_variables(&self.expression_watches_store)
    }

    pub fn show_expression_watches_for_refresh(&self, generation: u64, variables: &[Variable]) {
        if !self.is_stop_refresh_current(generation) {
            return;
        }
        let selected = root_variable_at(
            &self.expression_watches_selection,
            self.expression_watches_selection.selected(),
        )
        .map(|variable| variable.name);
        let changed = replace_variable_roots_if_changed(&self.expression_watches_store, variables);
        self.expression_watches_empty
            .set_visible(variables.is_empty());
        if changed == VariableRootChange::Unchanged {
            self.update_control_sensitivity();
            return;
        }
        if changed == VariableRootChange::Rebuilt && !variables.is_empty() {
            self.expression_watches_selection
                .set_selected(gtk::INVALID_LIST_POSITION);
            let selected = selected
                .as_deref()
                .and_then(|name| {
                    root_variable_position(&self.expression_watches_selection, name, false)
                })
                .unwrap_or(0);
            self.expression_watches_selection.set_selected(selected);
        }
        self.update_control_sensitivity();
    }

    pub fn show_expression_watch_root_for_refresh(
        &self,
        generation: u64,
        index: usize,
        variable: &Variable,
    ) {
        if !self.is_stop_refresh_current(generation) {
            return;
        }
        replace_variable_root(&self.expression_watches_store, index, variable, false);
    }

    pub fn show_expression_watches_unavailable(&self, value: &str) {
        let variables = self
            .expression_watches
            .borrow()
            .iter()
            .map(|expression| Variable {
                name: expression.clone(),
                value: value.to_owned(),
                type_name: None,
                argument: false,
                varobj: None,
                num_children: 0,
                has_more: false,
            })
            .collect::<Vec<_>>();
        self.show_expression_watches_for_refresh(
            self.current_stop_refresh_generation(),
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
        let expressions = Rc::clone(&self.expression_watches);
        let ready = Rc::clone(&self.debugger_ready);
        let running = Rc::clone(&self.inferior_running);
        let pending = Rc::clone(&self.command_pending);
        self.expression_watch_entry.connect_changed(move |entry| {
            let expression = entry.text();
            button.set_sensitive(
                ready.get()
                    && !running.get()
                    && !pending.get()
                    && !expression.trim().is_empty()
                    && expressions.borrow().len() < MAX_EXPRESSION_WATCHES
                    && !expressions
                        .borrow()
                        .iter()
                        .any(|existing| existing == expression.trim()),
            );
        });

        let entry = self.expression_watch_entry.clone();
        let expressions = Rc::clone(&self.expression_watches);
        let refresh = Rc::clone(&self.expression_watch_refresh_handler);
        self.expression_watch_add_button.connect_clicked(move |_| {
            let expression = entry.text().trim().to_owned();
            if expression.is_empty()
                || expressions.borrow().len() >= MAX_EXPRESSION_WATCHES
                || expressions
                    .borrow()
                    .iter()
                    .any(|existing| existing == &expression)
            {
                return;
            }
            expressions.borrow_mut().push(expression);
            entry.set_text("");
            if let Some(refresh) = refresh.borrow().as_ref() {
                refresh();
            }
        });

        let remove_button = self.expression_watch_remove_button.clone();
        let ready = Rc::clone(&self.debugger_ready);
        let running = Rc::clone(&self.inferior_running);
        let pending = Rc::clone(&self.command_pending);
        self.expression_watches_selection
            .connect_selected_notify(move |selection| {
                remove_button.set_sensitive(
                    ready.get()
                        && !running.get()
                        && !pending.get()
                        && root_variable_at(selection, selection.selected()).is_some(),
                );
            });

        let selection = self.expression_watches_selection.clone();
        let expressions = Rc::clone(&self.expression_watches);
        let refresh = Rc::clone(&self.expression_watch_refresh_handler);
        self.expression_watch_remove_button
            .connect_clicked(move |_| {
                let Some(variable) = root_variable_at(&selection, selection.selected()) else {
                    return;
                };
                expressions
                    .borrow_mut()
                    .retain(|expression| expression != &variable.name);
                if let Some(refresh) = refresh.borrow().as_ref() {
                    refresh();
                }
            });

        let window = self.window.clone();
        let selection = self.expression_watches_selection.clone();
        let handler = Rc::clone(&self.variable_assignment_handler);
        let float_handler = Rc::clone(&self.float_assignment_handler);
        let editor_handler = Rc::clone(&self.variable_editor_handler);
        let string_handler = Rc::clone(&self.string_assignment_handler);
        let children_handler = Rc::clone(&self.variable_children_handler);
        let target_pointer_bits = Rc::clone(&self.target_pointer_bits);
        let target_architecture = Rc::clone(&self.target_architecture);
        let current_source_is_rust = Rc::clone(&self.current_source_is_rust);
        let debugger_ready = Rc::clone(&self.debugger_ready);
        let inferior_started = Rc::clone(&self.inferior_started);
        let inferior_running = Rc::clone(&self.inferior_running);
        let command_pending = Rc::clone(&self.command_pending);
        let session_pending = Rc::clone(&self.session_pending);
        self.expression_watches_view
            .connect_activate(move |_, position| {
                if !debugger_ready.get()
                    || !inferior_started.get()
                    || inferior_running.get()
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
                        row.set_expanded(!row.is_expanded());
                    } else {
                        let variable = node.variable;
                        if let Some(editor_handler) = editor_handler.borrow().as_ref() {
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
    }
}
