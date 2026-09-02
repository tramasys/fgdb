use super::*;

impl Ui {
    pub fn present_variable_editor(&self, variable: Variable, metadata: Option<ValueTypeMetadata>) {
        open_variable_editor(
            &self.window,
            variable,
            self.target_pointer_bits.get(),
            self.target_architecture(),
            self.current_source_is_rust.get(),
            metadata.as_ref(),
            ValueEditorHandlers {
                assignment: Rc::clone(&self.variable_assignment_handler),
                float: Rc::clone(&self.float_assignment_handler),
                string: Rc::clone(&self.string_assignment_handler),
            },
        );
    }
}

pub(super) fn variable_at(selection: &gtk::SingleSelection, position: u32) -> Option<Variable> {
    variable_row_at(selection, position).map(|(_, variable)| variable)
}

pub(super) fn root_variable_at(
    selection: &gtk::SingleSelection,
    position: u32,
) -> Option<Variable> {
    let (mut row, _) = variable_node_at(selection, position)?;

    while let Some(parent) = row.parent() {
        row = parent;
    }

    let item = row.item()?.downcast::<glib::BoxedAnyObject>().ok()?;
    let node = item.borrow::<VariableNode>();

    (!node.placeholder).then(|| node.variable.clone())
}

pub(super) fn variable_row_at(
    selection: &gtk::SingleSelection,
    position: u32,
) -> Option<(gtk::TreeListRow, Variable)> {
    variable_node_at(selection, position)
        .and_then(|(row, node)| (!node.placeholder).then_some((row, node.variable)))
}

pub(super) fn variable_node_at(
    selection: &gtk::SingleSelection,
    position: u32,
) -> Option<(gtk::TreeListRow, VariableNode)> {
    selection
        .item(position)
        .and_then(|item| item.downcast::<gtk::TreeListRow>().ok())
        .and_then(|row| {
            let item = row
                .item()
                .and_then(|item| item.downcast::<glib::BoxedAnyObject>().ok())?;

            let node = item.borrow::<VariableNode>();

            Some((row, node.clone()))
        })
}

pub(super) fn index_variable_nodes(
    store: &gio::ListStore,
    index: &mut HashMap<String, VariableNode>,
) {
    let mut pending = vec![store.clone()];

    while let Some(store) = pending.pop() {
        for position in 0..store.n_items() {
            let Some(item) = store.item(position).and_downcast::<glib::BoxedAnyObject>() else {
                continue;
            };

            let node = item.borrow::<VariableNode>().clone();

            if let Some(varobj) = node.variable.varobj.as_ref() {
                index.insert(varobj.clone(), node.clone());
            }

            if node.children.n_items() > 0 {
                pending.push(node.children);
            }
        }
    }
}

pub(super) fn remove_indexed_variable_nodes(
    store: &gio::ListStore,
    index: &mut HashMap<String, VariableNode>,
) {
    let mut pending = vec![store.clone()];

    while let Some(store) = pending.pop() {
        for position in 0..store.n_items() {
            let Some(item) = store.item(position).and_downcast::<glib::BoxedAnyObject>() else {
                continue;
            };

            let node = item.borrow::<VariableNode>().clone();

            if let Some(varobj) = node.variable.varobj.as_ref() {
                index.remove(varobj);
            }

            if node.children.n_items() > 0 {
                pending.push(node.children);
            }
        }
    }
}

pub(super) fn root_variables(store: &gio::ListStore) -> Vec<Variable> {
    (0..store.n_items())
        .filter_map(|position| {
            store
                .item(position)
                .and_then(|item| item.downcast::<glib::BoxedAnyObject>().ok())
                .and_then(|item| {
                    let node = item.borrow::<VariableNode>();

                    (!node.placeholder).then(|| node.variable.clone())
                })
        })
        .collect()
}

pub(super) fn variable_root_node(store: &gio::ListStore, position: usize) -> Option<VariableNode> {
    store
        .item(u32::try_from(position).ok()?)
        .and_downcast::<glib::BoxedAnyObject>()
        .map(|item| item.borrow::<VariableNode>().clone())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum VariableRootChange {
    Unchanged,
    Updated,
    Rebuilt,
}

pub(super) fn replace_variable_roots_if_changed(
    store: &gio::ListStore,
    variables: &[Variable],
) -> VariableRootChange {
    replace_variable_roots(store, variables, true)
}

pub(super) fn replace_variable_roots(
    store: &gio::ListStore,
    variables: &[Variable],
    mark_changed: bool,
) -> VariableRootChange {
    let same_roots = usize::try_from(store.n_items()).ok() == Some(variables.len())
        && variables.iter().enumerate().all(|(index, variable)| {
            store
                .item(u32::try_from(index).unwrap_or(u32::MAX))
                .and_downcast::<glib::BoxedAnyObject>()
                .is_some_and(|item| {
                    let node = item.borrow::<VariableNode>();

                    !node.placeholder
                        && node.variable.name == variable.name
                        && node.variable.argument == variable.argument
                })
        });

    if !same_roots {
        replace_boxed_store(store, variables.iter().cloned().map(VariableNode::new));
        return VariableRootChange::Rebuilt;
    }

    let mut changed = false;

    for (index, variable) in variables.iter().enumerate() {
        let position = u32::try_from(index).unwrap_or(u32::MAX);

        let Some(item) = store.item(position).and_downcast::<glib::BoxedAnyObject>() else {
            continue;
        };

        let node = item.borrow::<VariableNode>().clone();

        let value_changed = if mark_changed {
            node.variable.value != variable.value
        } else {
            node.changed
        };

        if node.variable == *variable && node.changed == value_changed {
            continue;
        }

        store.splice(
            position,
            1,
            &[glib::BoxedAnyObject::new(
                node.updated(variable.clone(), mark_changed),
            )],
        );

        changed = true;
    }

    if changed {
        VariableRootChange::Updated
    } else {
        VariableRootChange::Unchanged
    }
}

pub(super) fn replace_variable_root(
    store: &gio::ListStore,
    index: usize,
    variable: &Variable,
    mark_changed: bool,
) -> bool {
    let Ok(position) = u32::try_from(index) else {
        return false;
    };

    let Some(item) = store.item(position).and_downcast::<glib::BoxedAnyObject>() else {
        return false;
    };

    let node = item.borrow::<VariableNode>().clone();

    if node.placeholder
        || node.variable.name != variable.name
        || node.variable.argument != variable.argument
    {
        return false;
    }

    let target_changed = if mark_changed {
        node.variable.value != variable.value
    } else {
        node.changed
    };

    if node.variable == *variable && node.changed == target_changed {
        return true;
    }

    store.splice(
        position,
        1,
        &[glib::BoxedAnyObject::new(
            node.updated(variable.clone(), mark_changed),
        )],
    );

    true
}

pub(super) fn changed_variable_roots(store: &gio::ListStore) -> usize {
    (0..store.n_items())
        .filter(|position| {
            store
                .item(*position)
                .and_downcast::<glib::BoxedAnyObject>()
                .is_some_and(|item| item.borrow::<VariableNode>().has_changes())
        })
        .count()
}

pub(super) fn apply_variable_updates(store: &gio::ListStore, updates: &[VariableUpdate]) -> usize {
    let updates = updates
        .iter()
        .map(|update| (update.varobj.as_str(), update))
        .collect::<HashMap<_, _>>();

    apply_variable_updates_to_store(store, &updates)
}

pub(super) fn clear_variable_change_markers(store: &gio::ListStore) {
    let mut pending = vec![store.clone()];

    while let Some(store) = pending.pop() {
        for position in 0..store.n_items() {
            let Some(item) = store.item(position).and_downcast::<glib::BoxedAnyObject>() else {
                continue;
            };

            let node = item.borrow::<VariableNode>().clone();

            if node.children.n_items() > 0 {
                pending.push(node.children.clone());
            }

            if node.changed {
                store.splice(
                    position,
                    1,
                    &[glib::BoxedAnyObject::new(node.without_change_marker())],
                );
            }
        }
    }
}

pub(super) fn refresh_changed_variable_roots(store: &gio::ListStore) {
    for position in 0..store.n_items() {
        let Some(item) = store.item(position).and_downcast::<glib::BoxedAnyObject>() else {
            continue;
        };

        let node = item.borrow::<VariableNode>().clone();

        if !node.changed && node.has_changes() {
            store.splice(position, 1, &[glib::BoxedAnyObject::new(node.rebound())]);
        }
    }
}

fn apply_variable_updates_to_store(
    store: &gio::ListStore,
    updates: &HashMap<&str, &VariableUpdate>,
) -> usize {
    let mut applied = 0;
    let mut pending = vec![store.clone()];

    while let Some(store) = pending.pop() {
        for position in 0..store.n_items() {
            let Some(item) = store.item(position).and_downcast::<glib::BoxedAnyObject>() else {
                continue;
            };

            let node = item.borrow::<VariableNode>().clone();

            let update = node
                .variable
                .varobj
                .as_deref()
                .and_then(|varobj| updates.get(varobj).copied());

            let children = if let Some(update) = update {
                let mut variable = node.variable.clone();

                if let Some(value) = update.value.as_ref() {
                    variable.value.clone_from(value);
                }

                if let Some(type_name) = update.new_type.as_ref() {
                    variable.type_name = Some(type_name.clone());
                }

                if let Some(num_children) = update.new_num_children {
                    variable.num_children = num_children;
                }

                if let Some(has_more) = update.has_more {
                    variable.has_more = has_more;
                }

                if let Some(display_hint) = update.display_hint.as_ref() {
                    variable.display_hint = Some(display_hint.clone());
                }

                if let Some(dynamic) = update.dynamic {
                    variable.dynamic = dynamic;
                }

                if update.in_scope == Some(false) {
                    variable.value = String::from("<out of scope>");
                    variable.num_children = 0;
                    variable.has_more = false;
                } else if update.type_changed {
                    variable.value = update
                        .value
                        .clone()
                        .unwrap_or_else(|| String::from("<type changed>"));
                }

                let updated = node.updated(variable, true);
                let children = updated.children.clone();
                store.splice(position, 1, &[glib::BoxedAnyObject::new(updated)]);
                applied += 1;

                children
            } else {
                node.children
            };

            if children.n_items() > 0 {
                pending.push(children);
            }
        }
    }

    applied
}

pub(super) fn root_variable_position(
    selection: &gtk::SingleSelection,
    name: &str,
    argument: bool,
) -> Option<u32> {
    let model = selection.model()?;

    (0..model.n_items()).find(|position| {
        variable_node_at(selection, *position).is_some_and(|(row, node)| {
            row.depth() == 0 && node.variable.name == name && node.variable.argument == argument
        })
    })
}

pub(super) fn remove_load_more_rows(store: &gio::ListStore) {
    for position in (0..store.n_items()).rev() {
        let is_load_more = store
            .item(position)
            .and_then(|item| item.downcast::<glib::BoxedAnyObject>().ok())
            .is_some_and(|item| item.borrow::<VariableNode>().load_more.is_some());

        if is_load_more {
            store.remove(position);
        }
    }
}

pub(super) fn open_variable_editor(
    parent: &gtk::ApplicationWindow,
    variable: Variable,
    target_pointer_bits: u32,
    target_architecture: TargetArchitecture,
    rust_source: bool,
    metadata: Option<&ValueTypeMetadata>,
    handlers: ValueEditorHandlers,
) {
    if let Some(string) = string_edit(&variable) {
        open_string_editor(
            parent,
            variable,
            string,
            Rc::clone(&handlers.assignment),
            Rc::clone(&handlers.string),
        );

        return;
    }

    if is_rust_string(&variable) {
        open_unavailable_rust_string_editor(parent, &variable);
        return;
    }

    if let Some(metadata) = metadata.filter(|metadata| {
        metadata.kind == ValueTypeKind::Enum && !metadata.enum_variants.is_empty()
    }) {
        open_enum_editor(parent, variable, metadata, Rc::clone(&handlers.assignment));
        return;
    }

    if let Some(float) = variable_float_edit(&variable, metadata) {
        open_float_editor(
            parent,
            variable,
            float,
            Rc::clone(&handlers.assignment),
            Rc::clone(&handlers.float),
        );

        return;
    }

    if let Some(value) = variable_boolean_value(&variable, metadata) {
        open_boolean_editor(parent, variable, value, Rc::clone(&handlers.assignment));
        return;
    }

    let editor = gtk::Window::builder()
        .title(format!("Edit {}", variable.name))
        .transient_for(parent)
        .modal(true)
        .default_width(620)
        .build();

    editor.add_css_class("value-editor");
    let content = gtk::Box::new(gtk::Orientation::Vertical, 6);
    content.set_spacing(6);
    content.set_margin_top(10);
    content.set_margin_bottom(10);
    content.set_margin_start(10);
    content.set_margin_end(10);
    let expression = gtk::Label::new(Some(&variable.name));
    expression.add_css_class("local-name");
    expression.set_halign(gtk::Align::Start);
    content.append(&expression);

    let type_name = gtk::Label::new(Some(
        variable.type_name.as_deref().unwrap_or("<unknown type>"),
    ));

    type_name.add_css_class("local-type");
    type_name.set_halign(gtk::Align::Start);
    content.append(&type_name);

    let character_format =
        variable_character_format(&variable, target_pointer_bits, rust_source, metadata);

    let integer_format = character_format
        .or_else(|| variable_integer_format(&variable, target_pointer_bits, metadata))
        .or_else(|| {
            variable
                .type_name
                .is_none()
                .then(|| {
                    register_integer_format(
                        &variable.name,
                        target_pointer_bits,
                        target_architecture,
                    )
                })
                .flatten()
        });

    let address = variable_is_address(&variable, target_architecture);
    let entry = gtk::Entry::new();
    let (editable_value, _) = variable_value_parts(&variable.value);
    entry.set_activates_default(true);
    entry.set_hexpand(true);
    let validation = gtk::Label::new(None);
    validation.add_css_class("value-editor-validation");
    validation.set_halign(gtk::Align::Start);
    validation.set_visible(false);

    let notation = integer_format.map(|format| {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 5);

        let label = gtk::Label::new(Some(if character_format.is_some() {
            "Display"
        } else {
            "Base"
        }));

        label.add_css_class("muted");
        let mut labels = Vec::new();

        if character_format.is_some() {
            labels.push("Character");
        }

        labels.extend(IntegerRadix::ALL.map(IntegerRadix::label));
        let dropdown = gtk::DropDown::from_strings(&labels);
        dropdown.add_css_class("value-editor-select");
        let detected = IntegerRadix::detect(editable_value);

        let selected = if character_format.is_some() {
            0
        } else {
            detected.index()
        };

        dropdown.set_selected(selected);
        dropdown.set_hexpand(true);
        row.append(&label);
        row.append(&dropdown);
        content.append(&row);

        let raw = parse_integer_input(editable_value, format, detected)
            .or_else(|error| {
                if character_format.is_some() {
                    parse_character_input(editable_value, format)
                } else {
                    Err(error)
                }
            })
            .ok();

        if let Some(raw) = raw {
            entry.set_text(&format_scalar_value(
                raw,
                format,
                selected,
                character_format.is_some(),
            ));
        } else {
            entry.set_text(editable_value);
        }

        (format, dropdown, raw, Rc::new(Cell::new(selected)))
    });

    if notation.is_none() {
        entry.set_text(editable_value);
    }

    if address {
        let hint = gtk::Label::new(Some("ADDRESS · hexadecimal or a GDB address expression"));
        hint.add_css_class("muted");
        hint.set_halign(gtk::Align::Start);
        content.append(&hint);
    }

    entry.set_tooltip_text(Some(if notation.is_some() {
        "Choose a representation, enter a value, then press Enter"
    } else if address {
        "Enter a hexadecimal address or a GDB expression such as &symbol"
    } else {
        "Enter a GDB expression for the new value, then press Enter"
    }));

    content.append(&entry);
    content.append(&validation);
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    actions.set_halign(gtk::Align::End);
    let cancel = gtk::Button::with_label("Cancel");
    let apply = gtk::Button::with_label("Set value");
    apply.add_css_class("primary-control");
    actions.append(&cancel);
    actions.append(&apply);
    content.append(&actions);
    editor.set_child(Some(&content));
    connect_escape_to_close(&editor);
    let original_value = editable_value.to_owned();
    let handler = handlers.assignment;
    let variable_for_submit = variable.clone();
    let entry_for_submit = entry.clone();
    let editor_for_submit = editor.clone();

    let notation_for_submit = notation.as_ref().map(|(format, dropdown, raw, _)| {
        (*format, dropdown.clone(), *raw, character_format.is_some())
    });

    let submit = Rc::new(move || {
        let value = if let Some((format, dropdown, original_raw, character)) =
            notation_for_submit.as_ref()
        {
            let selected = dropdown.selected();

            let Ok(raw) =
                parse_scalar_value(&entry_for_submit.text(), *format, selected, *character)
            else {
                return;
            };

            if Some(raw) == *original_raw {
                editor_for_submit.close();
                return;
            }

            let radix = scalar_radix(selected, *character).unwrap_or(IntegerRadix::Hexadecimal);

            canonical_gdb_integer(raw, *format, radix)
        } else {
            let value = entry_for_submit.text().trim().to_owned();

            if value.is_empty() || value == original_value {
                editor_for_submit.close();
                return;
            }

            value
        };

        let handler = handler.borrow().clone();

        if let Some(handler) = handler {
            handler(variable_for_submit.clone(), value);
        }

        editor_for_submit.close();
    });

    let submit_for_button = Rc::clone(&submit);
    apply.connect_clicked(move |_| submit_for_button());
    entry.connect_activate(move |_| submit());
    let editor_for_cancel = editor.clone();
    cancel.connect_clicked(move |_| editor_for_cancel.close());

    if let Some((format, dropdown, _, active)) = notation {
        update_scalar_validation(
            &entry,
            &validation,
            &apply,
            format,
            dropdown.selected(),
            character_format.is_some(),
        );

        let validation_for_entry = validation.clone();
        let apply_for_entry = apply.clone();
        let dropdown_for_entry = dropdown.clone();

        entry.connect_changed(move |entry| {
            update_scalar_validation(
                entry,
                &validation_for_entry,
                &apply_for_entry,
                format,
                dropdown_for_entry.selected(),
                character_format.is_some(),
            );
        });

        let entry_for_notation = entry.clone();
        let validation_for_notation = validation;
        let apply_for_notation = apply;

        dropdown.connect_selected_notify(move |dropdown| {
            let selected = dropdown.selected();
            let previous = active.replace(selected);

            if let Ok(raw) = parse_scalar_value(
                &entry_for_notation.text(),
                format,
                previous,
                character_format.is_some(),
            ) {
                entry_for_notation.set_text(&format_scalar_value(
                    raw,
                    format,
                    selected,
                    character_format.is_some(),
                ));
            }

            update_scalar_validation(
                &entry_for_notation,
                &validation_for_notation,
                &apply_for_notation,
                format,
                selected,
                character_format.is_some(),
            );
        });
    }

    editor.present();
    entry.grab_focus();
    entry.select_region(0, -1);
}

fn open_float_editor(
    parent: &gtk::ApplicationWindow,
    variable: Variable,
    float: value::FloatEdit,
    handler: Rc<RefCell<Option<VariableAssignmentHandler>>>,
    raw_handler: Rc<RefCell<Option<FloatAssignmentHandler>>>,
) {
    let editor = gtk::Window::builder()
        .title(format!("Edit {}", variable.name))
        .transient_for(parent)
        .modal(true)
        .default_width(620)
        .build();

    editor.add_css_class("value-editor");
    let content = gtk::Box::new(gtk::Orientation::Vertical, 6);
    content.set_margin_top(10);
    content.set_margin_bottom(10);
    content.set_margin_start(10);
    content.set_margin_end(10);
    let expression = gtk::Label::new(Some(&variable.name));
    expression.add_css_class("local-name");
    expression.set_halign(gtk::Align::Start);
    content.append(&expression);
    let type_name = gtk::Label::new(variable.type_name.as_deref());
    type_name.add_css_class("local-type");
    type_name.set_halign(gtk::Align::Start);
    content.append(&type_name);
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 5);
    let label = gtk::Label::new(Some("Format"));
    label.add_css_class("muted");

    let representation =
        gtk::DropDown::from_strings(&FloatRepresentation::ALL.map(FloatRepresentation::label));

    representation.add_css_class("value-editor-select");
    representation.set_hexpand(true);
    row.append(&label);
    row.append(&representation);
    content.append(&row);
    let entry = gtk::Entry::new();
    entry.set_hexpand(true);
    entry.set_activates_default(true);

    entry.set_text(&format_float_value(
        &float.raw_bytes,
        float.bits,
        FloatRepresentation::Decimal,
    ));

    content.append(&entry);

    let detail = gtk::Label::new(Some(&format!(
        "{}-bit floating point · accepts inf, -inf, and nan · raw mode preserves the exact bit pattern",
        float.bits
    )));

    detail.add_css_class("muted");
    detail.set_halign(gtk::Align::Start);
    detail.set_wrap(true);
    content.append(&detail);
    let validation = gtk::Label::new(None);
    validation.add_css_class("value-editor-validation");
    validation.set_halign(gtk::Align::Start);
    validation.set_visible(false);
    content.append(&validation);
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    actions.set_halign(gtk::Align::End);
    let cancel = gtk::Button::with_label("Cancel");
    let apply = gtk::Button::with_label("Set value");
    apply.add_css_class("primary-control");
    actions.append(&cancel);
    actions.append(&apply);
    content.append(&actions);
    editor.set_child(Some(&content));
    connect_escape_to_close(&editor);

    update_float_validation(
        &entry,
        &validation,
        &apply,
        float.bits,
        FloatRepresentation::Decimal,
    );

    let active = Rc::new(Cell::new(0_u32));
    let entry_for_representation = entry.clone();
    let validation_for_representation = validation.clone();
    let apply_for_representation = apply.clone();
    let active_for_representation = Rc::clone(&active);
    let original_raw_for_representation = float.raw_bytes.clone();

    representation.connect_selected_notify(move |representation| {
        let selected = representation.selected();
        let previous = active_for_representation.replace(selected);
        let previous_representation = FloatRepresentation::from_index(previous);

        let raw = if entry_for_representation.text()
            == format_float_value(
                &original_raw_for_representation,
                float.bits,
                previous_representation,
            ) {
            Ok(original_raw_for_representation.clone())
        } else {
            parse_float_value(
                &entry_for_representation.text(),
                float.bits,
                previous_representation,
            )
        };

        if let Ok(raw) = raw {
            entry_for_representation.set_text(&format_float_value(
                &raw,
                float.bits,
                FloatRepresentation::from_index(selected),
            ));
        }

        update_float_validation(
            &entry_for_representation,
            &validation_for_representation,
            &apply_for_representation,
            float.bits,
            FloatRepresentation::from_index(selected),
        );
    });

    let representation_for_entry = representation.clone();
    let validation_for_entry = validation;
    let apply_for_entry = apply.clone();

    entry.connect_changed(move |entry| {
        update_float_validation(
            entry,
            &validation_for_entry,
            &apply_for_entry,
            float.bits,
            FloatRepresentation::from_index(representation_for_entry.selected()),
        );
    });

    let editor_for_apply = editor.clone();
    let entry_for_apply = entry.clone();
    let representation_for_apply = representation;
    let original_raw = float.raw_bytes;

    apply.connect_clicked(move |_| {
        let representation = FloatRepresentation::from_index(representation_for_apply.selected());

        let Ok(raw) = parse_float_value(&entry_for_apply.text(), float.bits, representation) else {
            return;
        };

        if raw != original_raw {
            if representation == FloatRepresentation::RawBits {
                let handler = raw_handler.borrow().clone();

                if let Some(handler) = handler {
                    handler(variable.clone(), raw);
                }
            } else {
                let handler = handler.borrow().clone();

                if let Some(handler) = handler {
                    handler(variable.clone(), canonical_gdb_float(&raw, float.bits));
                }
            }
        }

        editor_for_apply.close();
    });

    let apply_for_activate = apply;

    entry.connect_activate(move |_| {
        if apply_for_activate.is_sensitive() {
            apply_for_activate.emit_clicked();
        }
    });

    let editor_for_cancel = editor.clone();
    cancel.connect_clicked(move |_| editor_for_cancel.close());
    editor.present();
    entry.grab_focus();
    entry.select_region(0, -1);
}

fn update_float_validation(
    entry: &gtk::Entry,
    validation: &gtk::Label,
    apply: &gtk::Button,
    bits: u32,
    representation: FloatRepresentation,
) {
    match parse_float_value(&entry.text(), bits, representation) {
        Ok(_) => {
            validation.set_visible(false);
            apply.set_sensitive(true);
        }
        Err(error) => {
            validation.set_text(error);
            validation.set_visible(true);
            apply.set_sensitive(false);
        }
    }
}

fn open_enum_editor(
    parent: &gtk::ApplicationWindow,
    variable: Variable,
    metadata: &ValueTypeMetadata,
    handler: Rc<RefCell<Option<VariableAssignmentHandler>>>,
) {
    let editor = gtk::Window::builder()
        .title(format!("Edit {}", variable.name))
        .transient_for(parent)
        .modal(true)
        .default_width(620)
        .build();

    editor.add_css_class("value-editor");
    let content = gtk::Box::new(gtk::Orientation::Vertical, 6);
    content.set_margin_top(10);
    content.set_margin_bottom(10);
    content.set_margin_start(10);
    content.set_margin_end(10);
    let expression = gtk::Label::new(Some(&variable.name));
    expression.add_css_class("local-name");
    expression.set_halign(gtk::Align::Start);
    content.append(&expression);
    let type_name = gtk::Label::new(variable.type_name.as_deref());
    type_name.add_css_class("local-type");
    type_name.set_halign(gtk::Align::Start);
    content.append(&type_name);
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 5);
    let label = gtk::Label::new(Some("Variant"));
    label.add_css_class("muted");

    let mut labels = metadata
        .enum_variants
        .iter()
        .map(|variant| variant.name.as_str())
        .collect::<Vec<_>>();

    labels.push("Custom expression…");
    let variants = gtk::DropDown::from_strings(&labels);
    variants.add_css_class("value-editor-select");
    variants.set_hexpand(true);
    row.append(&label);
    row.append(&variants);
    content.append(&row);
    let original = variable.value.trim().to_owned();

    let selected = metadata
        .enum_variants
        .iter()
        .position(|variant| enum_value_matches(&original, &variant.name))
        .and_then(|position| u32::try_from(position).ok())
        .unwrap_or(metadata.enum_variants.len() as u32);

    variants.set_selected(selected);
    let custom = gtk::Entry::new();
    custom.set_hexpand(true);
    custom.set_activates_default(true);
    custom.set_text(&original);
    custom.set_visible(selected as usize == metadata.enum_variants.len());
    content.append(&custom);
    let detail = gtk::Label::new(None);
    detail.add_css_class("muted");
    detail.set_halign(gtk::Align::Start);
    detail.set_wrap(true);
    update_enum_detail(&detail, metadata, selected);
    content.append(&detail);
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    actions.set_halign(gtk::Align::End);
    let cancel = gtk::Button::with_label("Cancel");
    let apply = gtk::Button::with_label("Set value");
    apply.add_css_class("primary-control");
    actions.append(&cancel);
    actions.append(&apply);
    content.append(&actions);
    editor.set_child(Some(&content));
    connect_escape_to_close(&editor);
    let metadata = metadata.clone();
    let metadata_for_selection = metadata.clone();
    let custom_for_selection = custom.clone();
    let detail_for_selection = detail;

    variants.connect_selected_notify(move |variants| {
        let selected = variants.selected();

        custom_for_selection
            .set_visible(selected as usize == metadata_for_selection.enum_variants.len());

        update_enum_detail(&detail_for_selection, &metadata_for_selection, selected);

        if selected as usize == metadata_for_selection.enum_variants.len() {
            custom_for_selection.grab_focus();
            custom_for_selection.select_region(0, -1);
        }
    });

    let editor_for_apply = editor.clone();
    let variants_for_apply = variants;
    let custom_for_apply = custom.clone();
    let enum_variants = metadata.enum_variants;

    apply.connect_clicked(move |_| {
        let value = enum_variants
            .get(variants_for_apply.selected() as usize)
            .map_or_else(
                || custom_for_apply.text().trim().to_owned(),
                |variant| variant.name.clone(),
            );

        if !value.is_empty() && !enum_value_matches(&original, &value) {
            let handler = handler.borrow().clone();

            if let Some(handler) = handler {
                handler(variable.clone(), value);
            }
        }

        editor_for_apply.close();
    });

    let apply_for_entry = apply;
    custom.connect_activate(move |_| apply_for_entry.emit_clicked());
    let editor_for_cancel = editor.clone();
    cancel.connect_clicked(move |_| editor_for_cancel.close());
    editor.present();
}

fn enum_value_matches(current: &str, variant: &str) -> bool {
    current == variant
        || current
            .rsplit("::")
            .next()
            .zip(variant.rsplit("::").next())
            .is_some_and(|(current, variant)| current == variant)
}

fn update_enum_detail(detail: &gtk::Label, metadata: &ValueTypeMetadata, selected: u32) {
    if let Some(variant) = metadata.enum_variants.get(selected as usize) {
        detail.set_text(&format!(
            "Discriminant {} · {}-bit enum",
            variant.value,
            metadata.bits.unwrap_or_default()
        ));
    } else {
        detail.set_text("Raw GDB expression · useful for values absent from the debug information");
    }
}

fn open_boolean_editor(
    parent: &gtk::ApplicationWindow,
    variable: Variable,
    original: bool,
    handler: Rc<RefCell<Option<VariableAssignmentHandler>>>,
) {
    let editor = gtk::Window::builder()
        .title(format!("Edit {}", variable.name))
        .transient_for(parent)
        .modal(true)
        .default_width(620)
        .build();

    editor.add_css_class("value-editor");
    let content = gtk::Box::new(gtk::Orientation::Vertical, 6);
    content.set_margin_top(10);
    content.set_margin_bottom(10);
    content.set_margin_start(10);
    content.set_margin_end(10);
    let expression = gtk::Label::new(Some(&variable.name));
    expression.add_css_class("local-name");
    expression.set_halign(gtk::Align::Start);
    content.append(&expression);
    let type_name = gtk::Label::new(variable.type_name.as_deref());
    type_name.add_css_class("local-type");
    type_name.set_halign(gtk::Align::Start);
    content.append(&type_name);
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 5);
    let label = gtk::Label::new(Some("Value"));
    label.add_css_class("muted");
    let value = gtk::DropDown::from_strings(&["false", "true"]);
    value.add_css_class("value-editor-select");
    value.set_selected(u32::from(original));
    value.set_hexpand(true);
    row.append(&label);
    row.append(&value);
    content.append(&row);

    let detail = gtk::Label::new(Some(
        "Boolean value · fgdb sends the language-neutral value 0 or 1 to GDB",
    ));

    detail.add_css_class("muted");
    detail.set_halign(gtk::Align::Start);
    content.append(&detail);
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    actions.set_halign(gtk::Align::End);
    let cancel = gtk::Button::with_label("Cancel");
    let apply = gtk::Button::with_label("Set value");
    apply.add_css_class("primary-control");
    actions.append(&cancel);
    actions.append(&apply);
    content.append(&actions);
    editor.set_child(Some(&content));
    connect_escape_to_close(&editor);
    let editor_for_apply = editor.clone();

    apply.connect_clicked(move |_| {
        let selected = value.selected() == 1;

        if selected != original {
            let handler = handler.borrow().clone();

            if let Some(handler) = handler {
                handler(
                    variable.clone(),
                    if selected { "1" } else { "0" }.to_owned(),
                );
            }
        }

        editor_for_apply.close();
    });

    let editor_for_cancel = editor.clone();
    cancel.connect_clicked(move |_| editor_for_cancel.close());
    editor.present();
}

fn scalar_radix(selected: u32, character: bool) -> Option<IntegerRadix> {
    if character && selected == 0 {
        None
    } else {
        Some(IntegerRadix::from_index(
            selected.saturating_sub(u32::from(character)),
        ))
    }
}

fn parse_scalar_value(
    value: &str,
    format: value::IntegerFormat,
    selected: u32,
    character: bool,
) -> Result<u128, &'static str> {
    scalar_radix(selected, character).map_or_else(
        || parse_character_input(value, format),
        |radix| parse_integer_input(value, format, radix),
    )
}

fn format_scalar_value(
    raw: u128,
    format: value::IntegerFormat,
    selected: u32,
    character: bool,
) -> String {
    scalar_radix(selected, character).map_or_else(
        || format_character_value(raw, format),
        |radix| format_integer_value(raw, format, radix),
    )
}

fn update_scalar_validation(
    entry: &gtk::Entry,
    validation: &gtk::Label,
    apply: &gtk::Button,
    format: value::IntegerFormat,
    selected: u32,
    character: bool,
) {
    match parse_scalar_value(&entry.text(), format, selected, character) {
        Ok(_) => {
            validation.set_visible(false);
            apply.set_sensitive(true);
        }
        Err(error) => {
            validation.set_text(error);
            validation.set_visible(true);
            apply.set_sensitive(false);
        }
    }
}

fn open_string_editor(
    parent: &gtk::ApplicationWindow,
    variable: Variable,
    string: value::StringEdit,
    assignment_handler: Rc<RefCell<Option<VariableAssignmentHandler>>>,
    string_handler: Rc<RefCell<Option<StringAssignmentHandler>>>,
) {
    let editor = gtk::Window::builder()
        .title(format!("Edit {}", variable.name))
        .transient_for(parent)
        .modal(true)
        .default_width(620)
        .build();

    editor.add_css_class("value-editor");
    let content = gtk::Box::new(gtk::Orientation::Vertical, 6);
    content.set_margin_top(10);
    content.set_margin_bottom(10);
    content.set_margin_start(10);
    content.set_margin_end(10);
    let expression = gtk::Label::new(Some(&variable.name));
    expression.add_css_class("local-name");
    expression.set_halign(gtk::Align::Start);
    content.append(&expression);
    let type_name = gtk::Label::new(variable.type_name.as_deref());
    type_name.add_css_class("local-type");
    type_name.set_halign(gtk::Align::Start);
    content.append(&type_name);

    let mode = matches!(string.storage, StringStorage::Buffer { pointer: true, .. }).then(|| {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 5);
        let label = gtk::Label::new(Some("Edit"));
        label.add_css_class("muted");
        let dropdown = gtk::DropDown::from_strings(&["String contents", "Pointer address"]);
        dropdown.add_css_class("value-editor-select");
        dropdown.set_hexpand(true);
        row.append(&label);
        row.append(&dropdown);
        content.append(&row);

        dropdown
    });

    let entry = gtk::Entry::new();
    let original_text = format_string_bytes(&string.bytes);
    let (original_address, _) = variable_value_parts(&variable.value);
    let original_address = original_address.to_owned();
    entry.set_text(&original_text);
    entry.set_activates_default(true);
    entry.set_hexpand(true);
    content.append(&entry);
    let detail = gtk::Label::new(None);
    detail.add_css_class("muted");
    detail.set_halign(gtk::Align::Start);
    detail.set_wrap(true);
    content.append(&detail);
    let validation = gtk::Label::new(None);
    validation.add_css_class("value-editor-validation");
    validation.set_halign(gtk::Align::Start);
    validation.set_visible(false);
    content.append(&validation);
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    actions.set_halign(gtk::Align::End);
    let cancel = gtk::Button::with_label("Cancel");
    let apply = gtk::Button::with_label("Set value");
    apply.add_css_class("primary-control");
    actions.append(&cancel);
    actions.append(&apply);
    content.append(&actions);
    editor.set_child(Some(&content));
    connect_escape_to_close(&editor);
    update_string_editor(&entry, &detail, &validation, &apply, &string, false);
    let detail_for_entry = detail.clone();
    let validation_for_entry = validation.clone();
    let apply_for_entry = apply.clone();
    let string_for_entry = string.clone();
    let mode_for_entry = mode.clone();

    entry.connect_changed(move |entry| {
        let address_mode = mode_for_entry
            .as_ref()
            .is_some_and(|mode| mode.selected() == 1);

        update_string_editor(
            entry,
            &detail_for_entry,
            &validation_for_entry,
            &apply_for_entry,
            &string_for_entry,
            address_mode,
        );
    });

    if let Some(mode) = &mode {
        let entry_for_mode = entry.clone();
        let detail_for_mode = detail;
        let validation_for_mode = validation;
        let apply_for_mode = apply.clone();
        let string_for_mode = string.clone();
        let original_text_for_mode = original_text;
        let original_address_for_mode = original_address.clone();

        mode.connect_selected_notify(move |mode| {
            let address_mode = mode.selected() == 1;

            entry_for_mode.set_text(if address_mode {
                &original_address_for_mode
            } else {
                &original_text_for_mode
            });

            update_string_editor(
                &entry_for_mode,
                &detail_for_mode,
                &validation_for_mode,
                &apply_for_mode,
                &string_for_mode,
                address_mode,
            );
        });
    }

    let editor_for_apply = editor.clone();
    let variable_for_apply = variable;
    let entry_for_apply = entry.clone();
    let mode_for_apply = mode;

    apply.connect_clicked(move |_| {
        let address_mode = mode_for_apply
            .as_ref()
            .is_some_and(|mode| mode.selected() == 1);

        if address_mode {
            let address = entry_for_apply.text().trim().to_owned();

            if !address.is_empty() && address != original_address {
                let handler = assignment_handler.borrow().clone();

                if let Some(handler) = handler {
                    handler(variable_for_apply.clone(), address);
                }
            }
        } else {
            let Ok(bytes) = parse_string_input(&entry_for_apply.text()) else {
                return;
            };

            if matches!(
                string.storage,
                StringStorage::Buffer { capacity, .. } if bytes.len() > capacity
            ) || matches!(
                string.storage,
                StringStorage::RustString { length } if bytes.len() != length
            ) {
                return;
            }

            if bytes != string.bytes {
                let handler = string_handler.borrow().clone();

                if let Some(handler) = handler {
                    handler(variable_for_apply.clone(), bytes, string.assignment_kind());
                }
            }
        }

        editor_for_apply.close();
    });

    let apply_for_entry = apply;

    entry.connect_activate(move |_| {
        if apply_for_entry.is_sensitive() {
            apply_for_entry.emit_clicked();
        }
    });

    let editor_for_cancel = editor.clone();
    cancel.connect_clicked(move |_| editor_for_cancel.close());
    editor.present();
    entry.grab_focus();
    entry.select_region(0, -1);
}

fn update_string_editor(
    entry: &gtk::Entry,
    detail: &gtk::Label,
    validation: &gtk::Label,
    apply: &gtk::Button,
    string: &value::StringEdit,
    address_mode: bool,
) {
    if address_mode {
        detail.set_text("Pointer address · hexadecimal or a GDB address expression");
        let valid = !entry.text().trim().is_empty();
        validation.set_visible(false);
        apply.set_sensitive(valid);
        return;
    }

    entry.set_tooltip_text(Some(
        "Edit text directly. Use C escapes such as \\n, \\t, \\0, or \\x41 for individual bytes",
    ));

    match (parse_string_input(&entry.text()), string.storage) {
        (Ok(bytes), StringStorage::Buffer { capacity, pointer }) if bytes.len() <= capacity => {
            detail.set_text(&format!(
                "String contents · {} / {} bytes · terminating NUL is written automatically{}",
                bytes.len(),
                capacity,
                if pointer {
                    " · growth is limited to the currently known buffer"
                } else {
                    ""
                }
            ));

            validation.set_visible(false);
            apply.set_sensitive(true);
        }
        (Ok(bytes), StringStorage::Buffer { capacity, .. }) => {
            detail.set_text(&format!(
                "String contents · {} / {} bytes",
                bytes.len(),
                capacity
            ));

            validation.set_text("The text does not fit the known destination buffer");
            validation.set_visible(true);
            apply.set_sensitive(false);
        }
        (Ok(bytes), StringStorage::CppString) => {
            detail.set_text(&format!(
                "std::string contents · {} bytes · applying calls assign() in the inferior and may allocate",
                bytes.len()
            ));

            validation.set_visible(false);
            apply.set_sensitive(true);
        }

        (Ok(bytes), StringStorage::RustString { length })
            if bytes.len() == length && std::str::from_utf8(&bytes).is_ok() =>
        {
            detail.set_text(&format!(
                "Rust String contents · {length} UTF-8 bytes · edited in place without changing its allocation"
            ));

            validation.set_visible(false);
            apply.set_sensitive(true);
        }
        (Ok(bytes), StringStorage::RustString { length }) => {
            detail.set_text(&format!(
                "Rust String contents · {} / {length} bytes · in-place edits must keep the same UTF-8 byte length",
                bytes.len()
            ));

            validation.set_text(if std::str::from_utf8(&bytes).is_err() {
                "Rust String contents must remain valid UTF-8"
            } else {
                "This GDB integration can safely edit Rust String only without resizing it"
            });

            validation.set_visible(true);
            apply.set_sensitive(false);
        }
        (Err(error), StringStorage::Buffer { capacity, .. }) => {
            detail.set_text(&format!("String contents · up to {} bytes", capacity));
            validation.set_text(error);
            validation.set_visible(true);
            apply.set_sensitive(false);
        }
        (Err(error), StringStorage::CppString) => {
            detail.set_text(
                "std::string contents · applying calls assign() in the inferior and may allocate",
            );

            validation.set_text(error);
            validation.set_visible(true);
            apply.set_sensitive(false);
        }
        (Err(error), StringStorage::RustString { length }) => {
            detail.set_text(&format!(
                "Rust String contents · exactly {length} UTF-8 bytes"
            ));

            validation.set_text(error);
            validation.set_visible(true);
            apply.set_sensitive(false);
        }
    }
}

fn open_unavailable_rust_string_editor(parent: &gtk::ApplicationWindow, variable: &Variable) {
    let editor = gtk::Window::builder()
        .title(format!("Edit {}", variable.name))
        .transient_for(parent)
        .modal(true)
        .default_width(620)
        .build();

    editor.add_css_class("value-editor");
    let content = gtk::Box::new(gtk::Orientation::Vertical, 7);
    content.set_margin_top(10);
    content.set_margin_bottom(10);
    content.set_margin_start(10);
    content.set_margin_end(10);
    let expression = gtk::Label::new(Some(&variable.name));
    expression.add_css_class("local-name");
    expression.set_halign(gtk::Align::Start);
    content.append(&expression);
    let type_name = gtk::Label::new(variable.type_name.as_deref());
    type_name.add_css_class("local-type");
    type_name.set_halign(gtk::Align::Start);
    content.append(&type_name);

    let explanation = gtk::Label::new(Some(
        "Rust String editing needs GDB's Rust pretty-printer to locate the backing buffer safely. Start fgdb with rust-gdb, or configure the matching Rust pretty-printer in GDB.",
    ));

    explanation.set_halign(gtk::Align::Start);
    explanation.set_wrap(true);
    content.append(&explanation);
    let close = gtk::Button::with_label("Close");
    close.set_halign(gtk::Align::End);
    content.append(&close);
    editor.set_child(Some(&content));
    connect_escape_to_close(&editor);
    let editor_for_close = editor.clone();
    close.connect_clicked(move |_| editor_for_close.close());
    editor.present();
}

pub(super) fn open_vector_editor(
    parent: &gtk::ApplicationWindow,
    register: Register,
    handler: Rc<RefCell<Option<VectorAssignmentHandler>>>,
) {
    let Some(register_bytes) = vector_register_bytes(&register.name) else {
        return;
    };

    let editor = gtk::Window::builder()
        .title(format!("Edit ${}", register.name))
        .transient_for(parent)
        .modal(true)
        .default_width(700)
        .default_height(470)
        .build();

    editor.add_css_class("value-editor");
    editor.add_css_class("vector-editor");
    let content = gtk::Box::new(gtk::Orientation::Vertical, 7);
    content.set_margin_top(10);
    content.set_margin_bottom(10);
    content.set_margin_start(10);
    content.set_margin_end(10);

    let heading = gtk::Label::new(Some(&format!(
        "${} · {} bits · edit interpreted lanes",
        register.name,
        register_bytes * 8
    )));

    heading.add_css_class("local-name");
    heading.set_halign(gtk::Align::Start);
    content.append(&heading);
    let interpretation_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let interpretation_label = gtk::Label::new(Some("Interpret as"));
    interpretation_label.add_css_class("muted");

    let interpretations = gtk::StringList::new(
        VectorLaneFormat::ALL
            .map(VectorLaneFormat::label)
            .as_slice(),
    );

    let interpretation = gtk::DropDown::new(Some(interpretations), gtk::Expression::NONE);
    interpretation.add_css_class("value-editor-select");
    interpretation.set_selected(3);
    interpretation.set_hexpand(true);
    interpretation_row.append(&interpretation_label);
    interpretation_row.append(&interpretation);
    content.append(&interpretation_row);
    let radix_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let radix_label = gtk::Label::new(Some("Integer base"));
    radix_label.add_css_class("muted");
    let radix = gtk::DropDown::from_strings(&IntegerRadix::ALL.map(IntegerRadix::label));
    radix.add_css_class("value-editor-select");
    radix.set_selected(IntegerRadix::Hexadecimal.index());
    radix.set_hexpand(true);
    radix_row.append(&radix_label);
    radix_row.append(&radix);
    content.append(&radix_row);

    let hint = gtk::Label::new(Some(
        "Each view addresses the same register bits. Apply edits before changing the interpretation. Switching views resets unapplied lane edits.",
    ));

    hint.add_css_class("muted");
    hint.set_halign(gtk::Align::Start);
    hint.set_wrap(true);
    content.append(&hint);

    let grid = gtk::Grid::builder()
        .column_spacing(8)
        .row_spacing(4)
        .hexpand(true)
        .build();

    let entries = Rc::new(RefCell::new(Vec::<gtk::Entry>::new()));
    let original_values = Rc::new(RefCell::new(Vec::<String>::new()));

    populate_vector_lane_grid(
        &grid,
        &entries,
        &original_values,
        &register.value,
        register_bytes,
        VectorLaneFormat::Int64,
        IntegerRadix::Hexadecimal,
    );

    let scroll = gtk::ScrolledWindow::builder()
        .child(&grid)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build();

    content.append(&scroll);
    let lane_validation = gtk::Label::new(None);
    lane_validation.add_css_class("value-editor-validation");
    lane_validation.set_halign(gtk::Align::Start);
    lane_validation.set_visible(false);
    content.append(&lane_validation);
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    actions.set_halign(gtk::Align::End);
    let cancel = gtk::Button::with_label("Cancel");
    let apply = gtk::Button::with_label("Apply lanes");
    apply.add_css_class("primary-control");
    actions.append(&cancel);
    actions.append(&apply);
    content.append(&actions);
    editor.set_child(Some(&content));
    connect_escape_to_close(&editor);
    let grid_for_format = grid.clone();
    let entries_for_format = Rc::clone(&entries);
    let originals_for_format = Rc::clone(&original_values);
    let register_value = register.value.clone();
    let radix_for_format = radix.clone();

    interpretation.connect_selected_notify(move |dropdown| {
        let format = VectorLaneFormat::from_index(dropdown.selected());
        radix_for_format.set_sensitive(!format.is_float());

        populate_vector_lane_grid(
            &grid_for_format,
            &entries_for_format,
            &originals_for_format,
            &register_value,
            register_bytes,
            format,
            IntegerRadix::from_index(radix_for_format.selected()),
        );
    });

    let grid_for_radix = grid;
    let entries_for_radix = Rc::clone(&entries);
    let originals_for_radix = Rc::clone(&original_values);
    let register_value_for_radix = register.value.clone();
    let interpretation_for_radix = interpretation.clone();

    radix.connect_selected_notify(move |radix| {
        let format = VectorLaneFormat::from_index(interpretation_for_radix.selected());

        if !format.is_float() {
            populate_vector_lane_grid(
                &grid_for_radix,
                &entries_for_radix,
                &originals_for_radix,
                &register_value_for_radix,
                register_bytes,
                format,
                IntegerRadix::from_index(radix.selected()),
            );
        }
    });

    let editor_for_apply = editor.clone();
    let register_name = register.name;
    let validation_for_apply = lane_validation;

    apply.connect_clicked(move |_| {
        let format = VectorLaneFormat::from_index(interpretation.selected());
        let selected_radix = IntegerRadix::from_index(radix.selected());

        let changes = entries
            .borrow()
            .iter()
            .zip(original_values.borrow().iter())
            .enumerate()
            .map(|(index, (entry, original))| {
                let value = entry.text().trim().to_owned();

                if value.is_empty() || value == *original {
                    return Ok(None);
                }

                if format.is_float() {
                    value
                        .parse::<f64>()
                        .map_err(|_| ())
                        .map(|_| Some((index, value)))
                } else {
                    let integer_format = value::IntegerFormat::signed(
                        u32::try_from(format.lane_bytes() * 8).unwrap_or(64),
                    );

                    parse_integer_input(&value, integer_format, selected_radix)
                        .map(|raw| {
                            Some((
                                index,
                                canonical_gdb_integer(raw, integer_format, selected_radix),
                            ))
                        })
                        .map_err(|_| ())
                }
            })
            .collect::<Result<Vec<_>, _>>();

        let Ok(changes) = changes else {
            validation_for_apply
                .set_text("At least one lane is invalid for the selected interpretation and base");

            validation_for_apply.set_visible(true);
            return;
        };

        validation_for_apply.set_visible(false);
        let changes = changes.into_iter().flatten().collect::<Vec<_>>();

        if !changes.is_empty() {
            let handler = handler.borrow().clone();

            if let Some(handler) = handler {
                handler(register_name.clone(), format.field(register_bytes), changes);
            }
        }

        editor_for_apply.close();
    });

    let editor_for_cancel = editor.clone();
    cancel.connect_clicked(move |_| editor_for_cancel.close());
    editor.present();
}

pub(super) fn populate_vector_lane_grid(
    grid: &gtk::Grid,
    entries: &Rc<RefCell<Vec<gtk::Entry>>>,
    original_values: &Rc<RefCell<Vec<String>>>,
    register_value: &str,
    register_bytes: usize,
    format: VectorLaneFormat,
    radix: IntegerRadix,
) {
    while let Some(child) = grid.first_child() {
        grid.remove(&child);
    }

    entries.borrow_mut().clear();
    original_values.borrow_mut().clear();
    let lane_count = register_bytes / format.lane_bytes();
    let field = format.field(register_bytes);

    let values = vector_field_values(register_value, &field, lane_count, format)
        .unwrap_or_else(|| vec![String::from("0"); lane_count]);

    let integer_format =
        value::IntegerFormat::signed(u32::try_from(format.lane_bytes() * 8).unwrap_or(64));

    let values = values.into_iter().map(|value| {
        if format.is_float() {
            value
        } else {
            parse_integer_input(&value, integer_format, IntegerRadix::detect(&value))
                .map(|raw| format_integer_value(raw, integer_format, radix))
                .unwrap_or(value)
        }
    });

    let columns = if lane_count <= 8 { 2 } else { 4 };

    for (index, value) in values.enumerate() {
        let group = index % columns;
        let row = index / columns;
        let label = gtk::Label::new(Some(&format!("[{index}]")));
        label.add_css_class("vector-lane-index");
        label.set_halign(gtk::Align::End);
        let entry = gtk::Entry::new();
        entry.set_text(&value);
        entry.set_hexpand(true);
        entry.set_tooltip_text(Some(&format!("${field}[{index}]")));
        grid.attach(&label, (group * 2) as i32, row as i32, 1, 1);
        grid.attach(&entry, (group * 2 + 1) as i32, row as i32, 1, 1);
        original_values.borrow_mut().push(value);
        entries.borrow_mut().push(entry);
    }
}

pub(super) fn open_flag_editor(
    parent: &gtk::ApplicationWindow,
    register: Register,
    handler: Rc<RefCell<Option<VariableAssignmentHandler>>>,
) {
    let Some(original) = hex_value(&register.value) else {
        return;
    };

    let editor = gtk::Window::builder()
        .title(format!("Edit ${}", register.name))
        .transient_for(parent)
        .modal(true)
        .default_width(540)
        .build();

    editor.add_css_class("value-editor");
    let content = gtk::Box::new(gtk::Orientation::Vertical, 6);
    content.set_margin_top(10);
    content.set_margin_bottom(10);
    content.set_margin_start(10);
    content.set_margin_end(10);

    let heading = gtk::Label::new(Some(&format!(
        "${} = 0x{original:016x} · toggle individual flags",
        register.name
    )));

    heading.set_halign(gtk::Align::Start);
    heading.add_css_class("local-name");
    content.append(&heading);

    let flags = gtk::Grid::builder()
        .column_spacing(12)
        .row_spacing(3)
        .build();

    let mut toggles = Vec::new();

    for (index, (bit, name)) in FLAGS.iter().enumerate() {
        let toggle = gtk::CheckButton::with_label(&name.to_uppercase());
        toggle.set_active(original & (1_u64 << bit) != 0);
        toggle.set_tooltip_text(Some(&format!("Bit {bit}")));
        flags.attach(&toggle, (index % 2) as i32, (index / 2) as i32, 1, 1);
        toggles.push((toggle, *bit));
    }

    content.append(&flags);
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    actions.set_halign(gtk::Align::End);
    let cancel = gtk::Button::with_label("Cancel");
    let apply = gtk::Button::with_label("Apply flags");
    apply.add_css_class("primary-control");
    actions.append(&cancel);
    actions.append(&apply);
    content.append(&actions);
    editor.set_child(Some(&content));
    connect_escape_to_close(&editor);
    let editor_for_apply = editor.clone();

    let variable = Variable {
        name: format!("${}", register.name),
        value: register.value,
        type_name: Some(String::from("flags register")),
        argument: false,
        varobj: None,
        num_children: 0,
        has_more: false,
        display_hint: None,
        dynamic: false,
    };

    apply.connect_clicked(move |_| {
        let mut value = original;

        for (toggle, bit) in &toggles {
            if toggle.is_active() {
                value |= 1_u64 << bit;
            } else {
                value &= !(1_u64 << bit);
            }
        }

        if value != original {
            let handler = handler.borrow().clone();

            if let Some(handler) = handler {
                handler(variable.clone(), format!("0x{value:x}"));
            }
        }

        editor_for_apply.close();
    });

    let editor_for_cancel = editor.clone();
    cancel.connect_clicked(move |_| editor_for_cancel.close());
    editor.present();
}

pub(super) fn open_breakpoint_condition_editor(
    parent: &gtk::ApplicationWindow,
    breakpoint: Breakpoint,
    handler: Rc<RefCell<Option<BreakpointConditionHandler>>>,
) {
    let editor = gtk::Window::builder()
        .title(format!("Breakpoint #{} condition", breakpoint.number))
        .transient_for(parent)
        .modal(true)
        .default_width(620)
        .build();

    editor.add_css_class("value-editor");
    let content = gtk::Box::new(gtk::Orientation::Vertical, 6);
    content.set_margin_top(10);
    content.set_margin_bottom(10);
    content.set_margin_start(10);
    content.set_margin_end(10);

    let breakpoint_name = breakpoint
        .function
        .as_deref()
        .or(breakpoint.original_location.as_deref())
        .or(breakpoint.address.as_deref())
        .unwrap_or("unresolved");

    let expression = gtk::Label::new(Some(&format!("#{}  {breakpoint_name}", breakpoint.number)));
    expression.add_css_class("local-name");
    expression.set_halign(gtk::Align::Start);
    expression.set_ellipsize(pango::EllipsizeMode::End);
    content.append(&expression);

    let hint = gtk::Label::new(Some(
        "Stop only when this GDB expression is true. Leave it empty to clear.",
    ));

    hint.add_css_class("muted");
    hint.set_halign(gtk::Align::Start);
    hint.set_wrap(true);
    content.append(&hint);
    let entry = gtk::Entry::new();
    entry.set_text(breakpoint.condition.as_deref().unwrap_or(""));
    entry.set_hexpand(true);
    entry.set_tooltip_text(Some("Examples: count == 4, ptr != 0, $rax == 0x10"));
    content.append(&entry);
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    actions.set_halign(gtk::Align::End);
    let cancel = gtk::Button::with_label("Cancel");
    let clear = gtk::Button::with_label("Clear");
    clear.set_sensitive(breakpoint.condition.is_some());
    let apply = gtk::Button::with_label("Set condition");
    apply.add_css_class("primary-control");
    actions.append(&cancel);
    actions.append(&clear);
    actions.append(&apply);
    content.append(&actions);
    editor.set_child(Some(&content));
    connect_escape_to_close(&editor);
    let number = breakpoint.command_number().to_owned();
    let original_condition = breakpoint.condition;
    let editor_for_submit = editor.clone();

    let submit = Rc::new(move |condition: Option<String>| {
        if condition != original_condition {
            let handler = handler.borrow().clone();

            if let Some(handler) = handler {
                handler(number.clone(), condition);
            }
        }

        editor_for_submit.close();
    });

    let entry_for_apply = entry.clone();
    let submit_for_apply = Rc::clone(&submit);

    apply.connect_clicked(move |_| {
        let condition = entry_for_apply.text().trim().to_owned();
        submit_for_apply((!condition.is_empty()).then_some(condition));
    });

    let entry_for_activate = entry.clone();
    let submit_for_activate = Rc::clone(&submit);

    entry.connect_activate(move |_| {
        let condition = entry_for_activate.text().trim().to_owned();
        submit_for_activate((!condition.is_empty()).then_some(condition));
    });

    clear.connect_clicked(move |_| submit(None));
    let editor_for_cancel = editor.clone();
    cancel.connect_clicked(move |_| editor_for_cancel.close());
    editor.present();
    entry.grab_focus();
    entry.select_region(0, -1);
}

pub(super) fn open_breakpoint_editor(
    parent: &gtk::ApplicationWindow,
    breakpoint: Option<Breakpoint>,
    pending_supported: bool,
    handler: Rc<RefCell<Option<BreakpointEditorHandler>>>,
) {
    let original = breakpoint.clone();

    let spec = breakpoint.as_ref().map_or_else(
        || BreakpointSpec {
            location: String::new(),
            regex: false,
            hardware: false,
            enabled: true,
            temporary: false,
            allow_pending: false,
            condition: None,
            stop_after: 1,
            thread: None,
            inferior: None,
            commands: Vec::new(),
            logpoint: false,
        },
        BreakpointSpec::from_breakpoint,
    );

    let title = breakpoint.as_ref().map_or_else(
        || String::from("Add breakpoint"),
        |breakpoint| format!("Edit breakpoint #{}", breakpoint.command_number()),
    );

    let editor = gtk::Window::builder()
        .title(title)
        .transient_for(parent)
        .modal(true)
        .default_width(700)
        .default_height(570)
        .build();

    editor.add_css_class("value-editor");
    editor.add_css_class("breakpoint-editor");
    let content = gtk::Box::new(gtk::Orientation::Vertical, 7);
    content.set_margin_top(10);
    content.set_margin_bottom(10);
    content.set_margin_start(10);
    content.set_margin_end(10);

    let grid = gtk::Grid::builder()
        .column_spacing(10)
        .row_spacing(6)
        .build();

    let field_label = |text: &str| {
        let label = gtk::Label::new(Some(text));
        label.add_css_class("muted");
        label.set_halign(gtk::Align::End);

        label
    };

    let location = gtk::Entry::builder()
        .placeholder_text("function, *0xaddress, or file:line")
        .hexpand(true)
        .build();

    location.set_text(&spec.location);
    grid.attach(&field_label("Location"), 0, 0, 1, 1);
    grid.attach(&location, 1, 0, 3, 1);
    let regex = gtk::CheckButton::with_label("Function regex");
    regex.set_active(spec.regex);
    regex.set_sensitive(breakpoint.is_none());
    let hardware = gtk::CheckButton::with_label("Hardware");
    hardware.set_active(spec.hardware);

    hardware.set_tooltip_text(Some(
        "Use a hardware instruction breakpoint. Availability and slot limits depend on the target",
    ));

    let enabled = gtk::CheckButton::with_label("Enabled");
    enabled.set_active(spec.enabled);
    let temporary = gtk::CheckButton::with_label("Temporary");
    temporary.set_active(spec.temporary);
    let pending = gtk::CheckButton::with_label("Allow pending");
    pending.set_active(spec.allow_pending);

    if !pending_supported {
        pending.set_sensitive(false);

        pending.set_tooltip_text(Some(
            "This GDB did not report support for pending breakpoints",
        ));
    }

    let options = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    options.append(&regex);
    options.append(&hardware);
    options.append(&enabled);
    options.append(&temporary);
    options.append(&pending);
    grid.attach(&field_label("Behavior"), 0, 1, 1, 1);
    grid.attach(&options, 1, 1, 3, 1);

    let condition = gtk::Entry::builder()
        .placeholder_text("optional GDB expression")
        .hexpand(true)
        .build();

    condition.set_text(spec.condition.as_deref().unwrap_or(""));
    grid.attach(&field_label("Condition"), 0, 2, 1, 1);
    grid.attach(&condition, 1, 2, 3, 1);
    let stop_after = gtk::SpinButton::with_range(1.0, f64::from(u32::MAX), 1.0);
    stop_after.set_value(spec.stop_after.max(1) as f64);
    stop_after.set_width_chars(9);
    grid.attach(&field_label("Stop on hit"), 0, 3, 1, 1);
    grid.attach(&stop_after, 1, 3, 1, 1);

    let thread = gtk::Entry::builder()
        .placeholder_text("all threads")
        .hexpand(true)
        .build();

    thread.set_text(spec.thread.as_deref().unwrap_or(""));

    let inferior = gtk::Entry::builder()
        .placeholder_text("all inferiors")
        .hexpand(true)
        .build();

    inferior.set_text(spec.inferior.as_deref().unwrap_or(""));
    grid.attach(&field_label("Thread"), 0, 4, 1, 1);
    grid.attach(&thread, 1, 4, 1, 1);
    grid.attach(&field_label("Inferior"), 2, 4, 1, 1);
    grid.attach(&inferior, 3, 4, 1, 1);
    content.append(&grid);
    let command_header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let command_title = gtk::Label::new(Some("COMMANDS"));
    command_title.add_css_class("section-title");
    command_title.set_halign(gtk::Align::Start);
    command_title.set_hexpand(true);
    let logpoint = gtk::CheckButton::with_label("Logpoint / auto-continue");
    logpoint.set_active(spec.logpoint);
    command_header.append(&command_title);
    command_header.append(&logpoint);
    content.append(&command_header);

    let command_hint = gtk::Label::new(Some(
        "One GDB command per line. Logpoints add ‘silent’ and ‘continue’ automatically.",
    ));

    command_hint.add_css_class("muted");
    command_hint.set_halign(gtk::Align::Start);
    command_hint.set_wrap(true);
    content.append(&command_hint);
    let commands = gtk::TextView::new();
    commands.set_monospace(true);
    commands.set_wrap_mode(gtk::WrapMode::None);
    commands.buffer().set_text(&spec.commands.join("\n"));

    let commands_scrolled = gtk::ScrolledWindow::builder()
        .child(&commands)
        .min_content_height(130)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .build();

    content.append(&commands_scrolled);
    let validation = gtk::Label::new(None);
    validation.add_css_class("value-editor-validation");
    validation.set_halign(gtk::Align::Start);
    validation.set_wrap(true);
    content.append(&validation);
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    actions.set_halign(gtk::Align::End);
    let cancel = gtk::Button::with_label("Cancel");

    let apply = gtk::Button::with_label(if breakpoint.is_some() {
        "Apply"
    } else {
        "Add breakpoint"
    });

    apply.add_css_class("primary-control");
    actions.append(&cancel);
    actions.append(&apply);
    content.append(&actions);
    editor.set_child(Some(&content));
    connect_escape_to_close(&editor);

    let update_regex_controls = {
        let regex = regex.clone();
        let pending = pending.clone();
        let thread = thread.clone();
        let inferior = inferior.clone();
        let hardware = hardware.clone();

        move || {
            let restricted = regex.is_active();
            pending.set_sensitive(pending_supported && !restricted);
            thread.set_sensitive(!restricted);
            inferior.set_sensitive(!restricted);
            hardware.set_sensitive(!restricted);

            if restricted {
                hardware.set_active(false);
            }
        }
    };

    update_regex_controls();
    regex.connect_toggled(move |_| update_regex_controls());
    let commands_for_logpoint = commands.clone();

    logpoint.connect_toggled(move |toggle| {
        if !toggle.is_active() {
            return;
        }

        let buffer = commands_for_logpoint.buffer();

        if buffer
            .text(&buffer.start_iter(), &buffer.end_iter(), false)
            .trim()
            .is_empty()
        {
            buffer.set_text("printf \"breakpoint hit\\n\"");
        }
    });

    let editor_for_apply = editor.clone();
    let location_for_apply = location.clone();
    let apply_for_location = apply.clone();
    location.connect_activate(move |_| apply_for_location.emit_clicked());
    let apply_for_condition = apply.clone();
    condition.connect_activate(move |_| apply_for_condition.emit_clicked());

    apply.connect_clicked(move |_| {
        let location_text = location_for_apply.text().trim().to_owned();

        if location_text.is_empty() {
            validation.set_text("Enter a function, address, source line, or function regex.");
            location_for_apply.grab_focus();
            return;
        }

        let regex_active = regex.is_active();
        let buffer = commands.buffer();

        let command_lines = buffer
            .text(&buffer.start_iter(), &buffer.end_iter(), false)
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect();

        let optional = |text: glib::GString| {
            let text = text.trim().to_owned();

            (!text.is_empty()).then_some(text)
        };

        let request = BreakpointEditRequest {
            original: original.clone(),
            spec: BreakpointSpec {
                location: location_text,
                regex: regex_active,
                hardware: !regex_active && hardware.is_active(),
                enabled: enabled.is_active(),
                temporary: temporary.is_active(),
                allow_pending: !regex_active && pending.is_active(),
                condition: optional(condition.text()),
                stop_after: u64::try_from(stop_after.value_as_int()).unwrap_or(1).max(1),
                thread: (!regex_active).then(|| optional(thread.text())).flatten(),
                inferior: (!regex_active).then(|| optional(inferior.text())).flatten(),
                commands: command_lines,
                logpoint: logpoint.is_active(),
            },
        };

        let handler = handler.borrow().clone();

        if let Some(handler) = handler {
            handler(request);
        }

        editor_for_apply.close();
    });

    let editor_for_cancel = editor.clone();
    cancel.connect_clicked(move |_| editor_for_cancel.close());
    editor.present();
    location.grab_focus();

    if breakpoint.is_none() {
        location.select_region(0, -1);
    }
}

pub(super) fn open_stop_point_metadata_editor(
    parent: &gtk::ApplicationWindow,
    number: &str,
    metadata: &StopPointMetadata,
    on_apply: Rc<dyn Fn(StopPointMetadata)>,
) {
    let editor = gtk::Window::builder()
        .title(format!("Organize stop point #{number}"))
        .transient_for(parent)
        .modal(true)
        .default_width(430)
        .resizable(false)
        .build();

    editor.add_css_class("value-editor");
    let content = gtk::Box::new(gtk::Orientation::Vertical, 7);
    content.set_margin_top(10);
    content.set_margin_bottom(10);
    content.set_margin_start(10);
    content.set_margin_end(10);
    let title = gtk::Label::new(Some("GROUPS / TAGS"));
    title.add_css_class("section-title");
    title.set_halign(gtk::Align::Start);
    content.append(&title);

    let hint = gtk::Label::new(Some(
        "Groups and tags are kept for this debugger session and are included in stop-point search.",
    ));

    hint.add_css_class("muted");
    hint.set_halign(gtk::Align::Start);
    hint.set_wrap(true);
    content.append(&hint);

    let group = gtk::Entry::builder()
        .placeholder_text("optional group, e.g. networking")
        .build();

    group.set_text(metadata.group.as_deref().unwrap_or_default());

    let tags = gtk::Entry::builder()
        .placeholder_text("comma-separated tags, e.g. startup, flaky")
        .build();

    tags.set_text(&metadata.tags.join(", "));
    content.append(&group);
    content.append(&tags);
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    actions.set_halign(gtk::Align::End);
    let cancel = gtk::Button::with_label("Cancel");
    let apply = gtk::Button::with_label("Apply");
    apply.add_css_class("primary-control");
    actions.append(&cancel);
    actions.append(&apply);
    content.append(&actions);
    editor.set_child(Some(&content));
    connect_escape_to_close(&editor);
    let editor_for_apply = editor.clone();
    let group_for_apply = group.clone();
    let tags_for_apply = tags.clone();
    let apply_metadata = Rc::clone(&on_apply);

    apply.connect_clicked(move |_| {
        apply_metadata(normalized_stop_point_metadata(
            group_for_apply.text().as_str(),
            tags_for_apply.text().as_str(),
        ));

        editor_for_apply.close();
    });

    let apply_for_group = apply.clone();
    group.connect_activate(move |_| apply_for_group.emit_clicked());
    tags.connect_activate(move |_| apply.emit_clicked());
    let editor_for_cancel = editor.clone();
    cancel.connect_clicked(move |_| editor_for_cancel.close());
    editor.present();
    group.grab_focus();
    group.select_region(0, -1);
}

pub(super) fn connect_escape_to_close(window: &gtk::Window) {
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak_window = window.downgrade();

    keys.connect_key_pressed(move |_, key, _, _| {
        if key != gtk::gdk::Key::Escape {
            return gtk::glib::Propagation::Proceed;
        }

        if let Some(window) = weak_window.upgrade() {
            window.close();
        }

        gtk::glib::Propagation::Stop
    });

    window.add_controller(keys);
}

#[cfg(test)]
mod variable_tree_tests {
    use super::*;

    fn variable(name: &str, value: &str, varobj: Option<&str>, children: usize) -> Variable {
        Variable {
            name: name.to_owned(),
            value: value.to_owned(),
            type_name: Some(String::from("demo::Value")),
            argument: false,
            varobj: varobj.map(str::to_owned),
            num_children: children,
            has_more: false,
            display_hint: None,
            dynamic: false,
        }
    }

    #[test]
    fn incremental_child_updates_preserve_expansion_and_clear_per_stop_markers() {
        let root = VariableNode::new(variable("root", "{...}", Some("var1"), 1));

        root.children
            .append(&glib::BoxedAnyObject::new(VariableNode::new(variable(
                "field",
                "1",
                Some("var1.field"),
                0,
            ))));

        root.children_loaded.set(true);
        root.expanded.set(true);
        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        store.append(&glib::BoxedAnyObject::new(root));

        let applied = apply_variable_updates(
            &store,
            &[VariableUpdate {
                varobj: String::from("var1.field"),
                value: Some(String::from("2")),
                in_scope: Some(true),
                type_changed: false,
                new_type: None,
                new_num_children: None,
                has_more: None,
                display_hint: None,
                dynamic: None,
            }],
        );

        assert_eq!(applied, 1);

        let root = store
            .item(0)
            .and_downcast::<glib::BoxedAnyObject>()
            .unwrap();

        let root = root.borrow::<VariableNode>();
        assert!(root.expanded.get());
        assert!(root.has_changes());

        let child = root
            .children
            .item(0)
            .and_downcast::<glib::BoxedAnyObject>()
            .unwrap();

        assert_eq!(child.borrow::<VariableNode>().variable.value, "2");
        assert!(child.borrow::<VariableNode>().changed);
        drop(root);
        clear_variable_change_markers(&store);

        let root = store
            .item(0)
            .and_downcast::<glib::BoxedAnyObject>()
            .unwrap();

        let root = root.borrow::<VariableNode>();
        assert!(root.expanded.get());
        assert!(!root.has_changes());

        assert_eq!(
            root.children
                .item(0)
                .and_downcast::<glib::BoxedAnyObject>()
                .unwrap()
                .borrow::<VariableNode>()
                .variable
                .value,
            "2"
        );
    }

    #[test]
    fn argument_scope_is_part_of_a_root_identity() {
        let store = gio::ListStore::new::<glib::BoxedAnyObject>();

        store.append(&glib::BoxedAnyObject::new(VariableNode::new(variable(
            "value", "1", None, 0,
        ))));

        let mut argument = variable("value", "1", None, 0);
        argument.argument = true;

        assert_eq!(
            replace_variable_roots_if_changed(&store, &[argument]),
            VariableRootChange::Rebuilt
        );
    }

    #[test]
    fn variable_node_index_includes_loaded_descendants() {
        let root = VariableNode::new(variable("root", "{...}", Some("var1"), 1));

        root.children
            .append(&glib::BoxedAnyObject::new(VariableNode::new(variable(
                "field",
                "1",
                Some("var1.field"),
                0,
            ))));

        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        store.append(&glib::BoxedAnyObject::new(root));
        let mut index = HashMap::new();
        index_variable_nodes(&store, &mut index);
        assert_eq!(index.len(), 2);
        assert_eq!(index["var1.field"].variable.name, "field");
    }
}
