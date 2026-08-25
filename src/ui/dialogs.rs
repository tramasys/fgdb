use super::*;

pub(super) fn variable_at(selection: &gtk::SingleSelection, position: u32) -> Option<Variable> {
    variable_row_at(selection, position).map(|(_, variable)| variable)
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

pub(super) fn find_variable_node(store: &gio::ListStore, varobj: &str) -> Option<VariableNode> {
    for position in 0..store.n_items() {
        let item = store
            .item(position)?
            .downcast::<glib::BoxedAnyObject>()
            .ok()?;
        let node = item.borrow::<VariableNode>().clone();
        if node.variable.varobj.as_deref() == Some(varobj) {
            return Some(node);
        }
        if let Some(node) = find_variable_node(&node.children, varobj) {
            return Some(node);
        }
    }
    None
}

pub(super) fn collect_variable_object_roots(
    store: &gio::ListStore,
    owner: Option<&str>,
    names: &mut Vec<String>,
) {
    for position in 0..store.n_items() {
        let Some(item) = store
            .item(position)
            .and_then(|item| item.downcast::<glib::BoxedAnyObject>().ok())
        else {
            continue;
        };
        let node = item.borrow::<VariableNode>();
        let mut child_owner = owner.map(str::to_owned);
        if let Some(name) = &node.variable.varobj {
            let belongs_to_owner = owner.is_some_and(|owner| {
                name == owner
                    || name
                        .strip_prefix(owner)
                        .is_some_and(|suffix| suffix.starts_with('.'))
            });
            if !belongs_to_owner {
                names.push(name.clone());
                child_owner = Some(name.clone());
            }
        }
        collect_variable_object_roots(&node.children, child_owner.as_deref(), names);
    }
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
    let entry = gtk::Entry::new();
    let (editable_value, _) = variable_value_parts(&variable.value);
    entry.set_text(editable_value);
    entry.set_activates_default(true);
    entry.set_hexpand(true);
    entry.set_tooltip_text(Some(
        "Enter a GDB expression for the new value, then press Enter",
    ));
    content.append(&entry);

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
    let variable_for_submit = variable;
    let entry_for_submit = entry.clone();
    let editor_for_submit = editor.clone();
    let submit = Rc::new(move || {
        let value = entry_for_submit.text().trim().to_owned();
        if !value.is_empty()
            && value != original_value
            && let Some(handler) = handler.borrow().as_ref()
        {
            handler(variable_for_submit.clone(), value);
        }
        editor_for_submit.close();
    });
    let submit_for_button = Rc::clone(&submit);
    apply.connect_clicked(move |_| submit_for_button());
    entry.connect_activate(move |_| submit());
    let editor_for_cancel = editor.clone();
    cancel.connect_clicked(move |_| editor_for_cancel.close());

    editor.present();
    entry.grab_focus();
    entry.select_region(0, -1);
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
    interpretation.set_selected(3);
    interpretation.set_hexpand(true);
    interpretation_row.append(&interpretation_label);
    interpretation_row.append(&interpretation);
    content.append(&interpretation_row);

    let hint = gtk::Label::new(Some(
        "Each view addresses the same register bits. Apply edits before changing the interpretation; switching views resets unapplied lane edits.",
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
    );
    let scroll = gtk::ScrolledWindow::builder()
        .child(&grid)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build();
    content.append(&scroll);

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
    interpretation.connect_selected_notify(move |dropdown| {
        populate_vector_lane_grid(
            &grid_for_format,
            &entries_for_format,
            &originals_for_format,
            &register_value,
            register_bytes,
            VectorLaneFormat::from_index(dropdown.selected()),
        );
    });

    let editor_for_apply = editor.clone();
    let register_name = register.name;
    apply.connect_clicked(move |_| {
        let format = VectorLaneFormat::from_index(interpretation.selected());
        let changes = entries
            .borrow()
            .iter()
            .zip(original_values.borrow().iter())
            .enumerate()
            .filter_map(|(index, (entry, original))| {
                let value = entry.text().trim().to_owned();
                (!value.is_empty() && value != *original).then_some((index, value))
            })
            .collect::<Vec<_>>();
        if !changes.is_empty()
            && let Some(handler) = handler.borrow().as_ref()
        {
            handler(register_name.clone(), format.field(register_bytes), changes);
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
    let columns = if lane_count <= 8 { 2 } else { 4 };
    for (index, value) in values.into_iter().enumerate() {
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
        varobj: None,
        num_children: 0,
        has_more: false,
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
        if value != original
            && let Some(handler) = handler.borrow().as_ref()
        {
            handler(variable.clone(), format!("0x{value:x}"));
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
        if condition != original_condition
            && let Some(handler) = handler.borrow().as_ref()
        {
            handler(number.clone(), condition);
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
