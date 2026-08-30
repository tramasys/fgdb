use super::*;

pub(super) fn enable_stable_text_selection(label: &gtk::Label) {
    label.set_selectable(true);
    // Gtk list factories recycle labels, and GtkLabel otherwise preserves its
    // character selection across a text replacement onto an unrelated row.
    label.connect_label_notify(clear_label_selection);
}

pub(super) fn clear_label_selections(root: &impl IsA<gtk::Widget>) {
    clear_widget_label_selections(root.as_ref());
}

fn clear_widget_label_selections(widget: &gtk::Widget) {
    if let Some(label) = widget.downcast_ref::<gtk::Label>() {
        clear_label_selection(label);
    }
    let mut child = widget.first_child();
    while let Some(current) = child {
        child = current.next_sibling();
        clear_widget_label_selections(&current);
    }
}

pub(super) fn clear_label_selection(label: &gtk::Label) {
    if label
        .selection_bounds()
        .is_some_and(|(start, end)| start != end)
    {
        label.select_region(0, 0);
    }
}

pub(super) fn build_locals_view(
    children_handler: &Rc<RefCell<Option<VariableChildrenHandler>>>,
    target_pointer_bits: &Rc<Cell<u32>>,
) -> (gtk::ColumnView, gio::ListStore, gtk::SingleSelection) {
    let store = gio::ListStore::new::<glib::BoxedAnyObject>();
    let tree = gtk::TreeListModel::new(store.clone(), false, false, |item| {
        let item = item.downcast_ref::<glib::BoxedAnyObject>()?;
        let node = item.borrow::<VariableNode>();
        node.variable
            .can_expand()
            .then(|| node.children.clone().upcast())
    });
    let selection = gtk::SingleSelection::new(Some(tree));
    selection.set_autoselect(true);
    selection.set_can_unselect(false);
    let view = gtk::ColumnView::new(Some(selection.clone()));
    view.add_css_class("debug-table");
    view.add_css_class("locals-table");
    view.set_vexpand(true);
    view.set_single_click_activate(false);
    view.set_reorderable(true);

    view.append_column(&local_name_column(&selection, children_handler));
    view.append_column(&local_text_column(
        "TYPE",
        155,
        false,
        LocalColumn::Type,
        Rc::clone(target_pointer_bits),
    ));
    view.append_column(&local_text_column(
        "VALUE",
        190,
        false,
        LocalColumn::Value,
        Rc::clone(target_pointer_bits),
    ));
    view.append_column(&local_text_column(
        "DETAILS",
        300,
        true,
        LocalColumn::Details,
        Rc::clone(target_pointer_bits),
    ));
    (view, store, selection)
}

pub(super) fn insight_label(placeholder: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(placeholder));
    label.add_css_class("instruction-insight-line");
    label.set_halign(gtk::Align::Fill);
    label.set_xalign(0.0);
    label.set_hexpand(true);
    label.set_wrap(true);
    label.set_wrap_mode(pango::WrapMode::WordChar);
    label.set_lines(2);
    label.set_ellipsize(pango::EllipsizeMode::End);
    enable_stable_text_selection(&label);
    label.set_visible(!placeholder.is_empty());
    label
}

pub(super) fn build_memory_region_view(
    target_pointer_bits: &Rc<Cell<u32>>,
    search: &gtk::SearchEntry,
) -> (gtk::ColumnView, gio::ListStore) {
    let store = gio::ListStore::new::<glib::BoxedAnyObject>();
    let query = Rc::new(RefCell::new(String::new()));
    let query_for_filter = Rc::clone(&query);
    let filter = gtk::CustomFilter::new(move |object| {
        let Some(data) = object.downcast_ref::<glib::BoxedAnyObject>() else {
            return false;
        };
        memory_region_matches_filter(&data.borrow::<MemoryRegion>(), &query_for_filter.borrow())
    });
    let filtered = gtk::FilterListModel::new(Some(store.clone()), Some(filter.clone()));
    let selection = gtk::SingleSelection::new(Some(filtered));
    selection.set_autoselect(false);
    selection.set_can_unselect(true);
    let view = gtk::ColumnView::new(Some(selection));
    view.add_css_class("debug-table");
    view.add_css_class("memory-map-table");
    view.set_vexpand(true);
    view.set_reorderable(true);
    for (title, width, expand, column) in [
        ("START", 175, false, MemoryColumn::Start),
        ("END", 175, false, MemoryColumn::End),
        ("SIZE", 90, false, MemoryColumn::Size),
        ("PERM", 65, false, MemoryColumn::Permissions),
        ("REGS", 110, false, MemoryColumn::Registers),
        ("PATH", 280, true, MemoryColumn::Path),
    ] {
        view.append_column(&memory_region_column(
            title,
            width,
            expand,
            column,
            Rc::clone(target_pointer_bits),
        ));
    }
    search.connect_search_changed(move |search| {
        query.replace(search.text().trim().to_ascii_lowercase());
        filter.changed(gtk::FilterChange::Different);
    });
    (view, store)
}

pub(super) fn memory_region_matches_filter(region: &MemoryRegion, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    let search_text = format!(
        "{:x} 0x{:x} {:x} 0x{:x} {} {} {}",
        region.start,
        region.start,
        region.end,
        region.end,
        region.permissions,
        region.referenced_by.join(" "),
        region.path.as_deref().unwrap_or("anonymous"),
    )
    .to_ascii_lowercase();
    query
        .split_whitespace()
        .all(|term| search_text.contains(term))
}

pub(super) fn memory_region_column(
    title: &str,
    width: i32,
    expand: bool,
    column: MemoryColumn,
    target_pointer_bits: Rc<Cell<u32>>,
) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(move |_, object| {
        let Some(item) = object.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let label = gtk::Label::new(None);
        label.add_css_class("debug-table-cell");
        label.set_halign(gtk::Align::Start);
        label.set_ellipsize(pango::EllipsizeMode::Middle);
        item.set_child(Some(&label));
    });
    factory.connect_bind(move |_, object| {
        let Some(item) = object.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let (Some(label), Some(data)) = (
            item.child().and_downcast::<gtk::Label>(),
            item.item().and_downcast::<glib::BoxedAnyObject>(),
        ) else {
            return;
        };
        clear_label_selection(&label);
        let region = data.borrow::<MemoryRegion>();
        reset_semantic_css(&label);
        label.add_css_class(memory_kind_css(region.kind));
        let address_width = usize::try_from(target_pointer_bits.get() / 4)
            .unwrap_or(16)
            .clamp(8, 16);
        let text = match column {
            MemoryColumn::Start => format!("0x{:0address_width$x}", region.start),
            MemoryColumn::End => format!("0x{:0address_width$x}", region.end),
            MemoryColumn::Size => format_memory_size(region.end.saturating_sub(region.start)),
            MemoryColumn::Permissions => region.permissions.clone(),
            MemoryColumn::Registers => region.referenced_by.join(" "),
            MemoryColumn::Path => region
                .path
                .clone()
                .unwrap_or_else(|| String::from("anonymous")),
        };
        label.set_text(&text);
        label.set_tooltip_text(Some(&format!(
            "0x{:0address_width$x}-0x{:0address_width$x} · {}",
            region.start,
            region.end,
            region.description()
        )));
    });
    let column = gtk::ColumnViewColumn::new(Some(title), Some(factory));
    column.set_fixed_width(width);
    column.set_resizable(true);
    column.set_expand(expand);
    column
}

pub(super) fn format_memory_size(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

pub(super) fn local_name_column(
    selection: &gtk::SingleSelection,
    children_handler: &Rc<RefCell<Option<VariableChildrenHandler>>>,
) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    let selection = selection.clone();
    let children_handler_for_setup = Rc::clone(children_handler);
    factory.connect_setup(move |_, object| {
        let Some(item) = object.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let label = gtk::Label::new(None);
        label.add_css_class("debug-table-cell");
        label.add_css_class("local-name");
        label.set_halign(gtk::Align::Start);
        label.set_ellipsize(pango::EllipsizeMode::End);
        label.set_hexpand(true);
        let expander = gtk::TreeExpander::new();
        expander.set_hexpand(true);
        expander.set_child(Some(&label));

        let click = gtk::GestureClick::new();
        click.set_button(gtk::gdk::BUTTON_PRIMARY);
        click.set_propagation_phase(gtk::PropagationPhase::Capture);
        let expander_for_click = expander.clone();
        let selection = selection.clone();
        let children_handler = Rc::clone(&children_handler_for_setup);
        click.connect_pressed(move |gesture, presses, _, _| {
            if presses != 1 {
                return;
            }
            let Some(row) = expander_for_click.list_row() else {
                return;
            };
            let node = row
                .item()
                .and_then(|item| item.downcast::<glib::BoxedAnyObject>().ok())
                .map(|item| item.borrow::<VariableNode>().clone());
            let Some(node) = node else {
                return;
            };
            if !row.is_expandable() && node.load_more.is_none() {
                return;
            }
            selection.set_selected(row.position());
            if row.is_expandable() {
                row.set_expanded(!row.is_expanded());
            } else {
                request_next_variable_page_if_needed(&node, &children_handler);
            }
            gesture.set_state(gtk::EventSequenceState::Claimed);
        });
        expander.add_controller(click);
        item.set_child(Some(&expander));
    });
    let children_handler = Rc::clone(children_handler);
    factory.connect_bind(move |_, object| {
        let Some(item) = object.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let (Some(expander), Some(row)) = (
            item.child().and_downcast::<gtk::TreeExpander>(),
            item.item().and_downcast::<gtk::TreeListRow>(),
        ) else {
            return;
        };
        let Some(data) = row.item().and_downcast::<glib::BoxedAnyObject>() else {
            return;
        };
        let node = data.borrow::<VariableNode>();
        let Some(label) = expander.child().and_downcast::<gtk::Label>() else {
            return;
        };
        expander.set_list_row(Some(&row));
        label.set_text(&node.variable.name);
        let expandable = node.variable.can_expand();
        if expandable && !node.expansion_observer_attached.replace(true) {
            let node = node.clone();
            let children_handler = Rc::clone(&children_handler);
            row.connect_expanded_notify(move |row| {
                if row.is_expanded() {
                    request_variable_children_if_needed(&node, &children_handler);
                }
            });
        }
        let load_more = node.load_more.is_some();
        if expandable || load_more {
            label.add_css_class("local-expandable");
            expander.set_cursor_from_name(Some("pointer"));
        } else {
            label.remove_css_class("local-expandable");
            expander.set_cursor(None);
        }
        let tooltip = if node.placeholder {
            format!("{}\n{}", node.variable.name, node.variable.value)
        } else {
            variable_tooltip(&node.variable)
        };
        label.set_tooltip_text(Some(&tooltip));
        label.remove_css_class("local-load-more");
        if load_more {
            label.remove_css_class("muted");
            label.remove_css_class("local-name");
            label.add_css_class("local-load-more");
        } else if node.placeholder {
            label.remove_css_class("local-name");
            label.add_css_class("muted");
        } else {
            label.remove_css_class("muted");
            label.add_css_class("local-name");
        }
    });
    let column = gtk::ColumnViewColumn::new(Some("NAME / EXPRESSION"), Some(factory));
    column.set_fixed_width(175);
    column.set_resizable(true);
    column
}

pub(super) fn request_variable_children_if_needed(
    node: &VariableNode,
    children_handler: &Rc<RefCell<Option<VariableChildrenHandler>>>,
) {
    if node.children_loaded.get() || node.children_loading.replace(true) {
        return;
    }
    node.children
        .append(&glib::BoxedAnyObject::new(VariableNode::placeholder(
            "loading…",
            "waiting for GDB",
        )));
    if let Some(handler) = children_handler.borrow().as_ref() {
        handler(node.variable.clone(), 0);
    } else {
        node.children.remove_all();
        node.children_loading.set(false);
    }
}

pub(super) fn request_next_variable_page_if_needed(
    node: &VariableNode,
    children_handler: &Rc<RefCell<Option<VariableChildrenHandler>>>,
) {
    let Some((parent, from)) = node.load_more.as_ref() else {
        return;
    };
    if node.children_loading.replace(true) {
        return;
    }
    if let Some(handler) = children_handler.borrow().as_ref() {
        handler(parent.clone(), *from);
    } else {
        node.children_loading.set(false);
    }
}

pub(super) fn local_text_column(
    title: &str,
    width: i32,
    expand: bool,
    column: LocalColumn,
    target_pointer_bits: Rc<Cell<u32>>,
) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(move |_, object| {
        let Some(item) = object.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let label = gtk::Label::new(None);
        label.add_css_class("debug-table-cell");
        label.add_css_class(match column {
            LocalColumn::Type => "local-type",
            LocalColumn::Value => "local-value",
            LocalColumn::Details => "local-details",
        });
        label.set_halign(gtk::Align::Start);
        label.set_ellipsize(pango::EllipsizeMode::End);
        enable_stable_text_selection(&label);
        item.set_child(Some(&label));
    });
    factory.connect_bind(move |_, object| {
        let Some(item) = object.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let (Some(label), Some(row)) = (
            item.child().and_downcast::<gtk::Label>(),
            item.item().and_downcast::<gtk::TreeListRow>(),
        ) else {
            return;
        };
        let Some(data) = row.item().and_downcast::<glib::BoxedAnyObject>() else {
            return;
        };
        let node = data.borrow::<VariableNode>();
        let variable = &node.variable;
        clear_label_selection(&label);
        let (value, details) = variable_value_parts(&variable.value);
        label.remove_css_class("local-details-error");
        match column {
            LocalColumn::Type => {
                label.set_text(variable.type_name.as_deref().unwrap_or("<unknown>"));
            }
            LocalColumn::Value => label.set_text(value),
            LocalColumn::Details => {
                let decoded = variable_details(variable, value, details, target_pointer_bits.get());
                label.set_text(&decoded);
                if decoded.contains("<error:") {
                    label.add_css_class("local-details-error");
                }
            }
        }
        if node.placeholder {
            label.add_css_class("muted");
        } else {
            label.remove_css_class("muted");
        }
        let tooltip = if node.placeholder {
            format!("{}\n{}", variable.name, variable.value)
        } else {
            variable_tooltip(variable)
        };
        label.set_tooltip_text(Some(&tooltip));
    });
    let column_view = gtk::ColumnViewColumn::new(Some(title), Some(factory));
    column_view.set_fixed_width(width);
    column_view.set_resizable(true);
    column_view.set_expand(expand);
    column_view
}

pub(super) fn variable_value_parts(value: &str) -> (&str, &str) {
    let value = value.trim();
    let Some(separator) = value.find(char::is_whitespace) else {
        return (value, "");
    };
    let (raw, remainder) = value.split_at(separator);
    let details = remainder.trim_start();
    let raw_is_address = raw.strip_prefix("0x").is_some_and(|digits| {
        !digits.is_empty() && digits.chars().all(|digit| digit.is_ascii_hexdigit())
    });
    let raw_is_number = raw
        .strip_prefix('-')
        .unwrap_or(raw)
        .chars()
        .all(|digit| digit.is_ascii_digit());
    let details_describe_value = details.starts_with(['"', '\'', '<']) || details.starts_with("->");
    if (raw_is_address || raw_is_number) && details_describe_value {
        (raw, details)
    } else {
        (value, "")
    }
}

pub(super) fn variable_details(
    variable: &Variable,
    value: &str,
    details: &str,
    target_pointer_bits: u32,
) -> String {
    let Some(decimal) = integer_decimal_value(variable, value, target_pointer_bits) else {
        return details.to_owned();
    };
    if details.is_empty() {
        decimal
    } else {
        format!("{decimal}  ·  {details}")
    }
}

pub(super) fn variable_tooltip(variable: &Variable) -> String {
    let interaction = if variable.can_expand() {
        "Click the name or press Enter to expand. Use Edit to change the value"
    } else {
        "Double-click or press Enter to edit"
    };
    format!(
        "{}  {}\n{}\n{} child{}\n{interaction}",
        variable.type_name.as_deref().unwrap_or("<unknown type>"),
        variable.name,
        variable.value,
        variable.num_children,
        if variable.num_children == 1 {
            ""
        } else {
            "ren"
        }
    )
}

pub(super) fn build_instruction_view() -> (
    gtk::ColumnView,
    gio::ListStore,
    gtk::SingleSelection,
    gtk::ColumnViewColumn,
) {
    let store = gio::ListStore::new::<glib::BoxedAnyObject>();
    let selection = gtk::SingleSelection::new(Some(store.clone()));
    selection.set_autoselect(false);
    selection.set_can_unselect(true);
    let view = gtk::ColumnView::new(Some(selection.clone()));
    view.add_css_class("instruction-table");
    view.add_css_class("debug-table");
    view.set_vexpand(true);
    view.set_single_click_activate(false);
    view.set_reorderable(true);
    view.set_tooltip_text(Some(
        "Double-click an instruction to toggle an address breakpoint",
    ));

    for column in [
        instruction_column(
            "ADDRESS",
            170,
            false,
            "instruction-address",
            &selection,
            |row| {
                let marker = if row.current { "›" } else { " " };
                Cow::Owned(format!(
                    "{marker} {}",
                    full_address(&row.instruction.address, row.pointer_bits)
                ))
            },
        ),
        instruction_column(
            "OPCODE",
            72,
            false,
            "instruction-mnemonic",
            &selection,
            |row| Cow::Borrowed(split_instruction(&row.instruction.text).0),
        ),
        instruction_column(
            "OPERANDS",
            180,
            true,
            "instruction-operands",
            &selection,
            |row| Cow::Borrowed(split_instruction(&row.instruction.text).1),
        ),
        instruction_column(
            "BYTES",
            130,
            false,
            "instruction-opcodes",
            &selection,
            |row| {
                row.instruction
                    .opcodes
                    .as_deref()
                    .map_or(Cow::Borrowed("unavailable"), Cow::Borrowed)
            },
        ),
        instruction_column(
            "SYMBOL",
            140,
            false,
            "instruction-symbol",
            &selection,
            |row| Cow::Owned(instruction_symbol(&row.instruction)),
        ),
    ] {
        view.append_column(&column);
    }
    let source_column = instruction_column(
        "SOURCE",
        280,
        true,
        "instruction-source",
        &selection,
        |row| {
            let Some(source) = row.instruction.source.as_ref() else {
                return Cow::Borrowed("");
            };
            let file = Path::new(source.source_path())
                .file_name()
                .unwrap_or_default()
                .to_string_lossy();
            Cow::Owned(row.source_text.as_ref().map_or_else(
                || format!("{file}:{}", source.line),
                |text| format!("{file}:{}  {}", source.line, text.trim()),
            ))
        },
    );
    source_column.set_visible(false);
    view.append_column(&source_column);
    (view, store, selection, source_column)
}

pub(super) fn build_register_view() -> (gtk::Box, Vec<RegisterGroupView>) {
    let content = gtk::Box::new(gtk::Orientation::Vertical, 4);
    content.add_css_class("register-groups");
    content.set_hexpand(true);
    let mut groups = Vec::new();
    for (title, kind) in [
        ("GENERAL PURPOSE", RegisterGroupKind::General),
        ("THREAD BASES", RegisterGroupKind::Bases),
        ("FLAGS", RegisterGroupKind::Flags),
        ("SEGMENTS", RegisterGroupKind::Segments),
        ("SIMD / VECTOR", RegisterGroupKind::Vector),
        ("FLOATING POINT", RegisterGroupKind::FloatingPoint),
        ("OTHER", RegisterGroupKind::Other),
    ] {
        let (view, store) = build_register_group_table();
        let expanded = matches!(
            kind,
            RegisterGroupKind::General
                | RegisterGroupKind::Bases
                | RegisterGroupKind::Flags
                | RegisterGroupKind::Segments
        );
        let panel = build_disclosure(title, &view, expanded, "register-disclosure");
        panel.add_css_class("register-group-panel");
        panel.set_visible(false);
        content.append(&panel);
        groups.push(RegisterGroupView {
            kind,
            store,
            view,
            panel,
        });
    }
    (content, groups)
}

pub(super) fn build_register_group_table() -> (gtk::ColumnView, gio::ListStore) {
    let store = gio::ListStore::new::<glib::BoxedAnyObject>();
    let selection = gtk::SingleSelection::new(Some(store.clone()));
    selection.set_autoselect(false);
    selection.set_can_unselect(true);
    let view = gtk::ColumnView::new(Some(selection.clone()));
    view.add_css_class("debug-table");
    view.add_css_class("register-table");
    view.set_hexpand(true);
    view.set_reorderable(true);
    view.set_single_click_activate(false);

    for (title, width, expand, column) in [
        ("REGISTER", 90, false, RegisterColumn::Name),
        ("VALUE", 185, false, RegisterColumn::Value),
        ("POINTER CHAIN / FLAGS", 330, true, RegisterColumn::Details),
    ] {
        view.append_column(&register_column(title, width, expand, column));
    }
    (view, store)
}

pub(super) fn register_column(
    title: &str,
    width: i32,
    expand: bool,
    column: RegisterColumn,
) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(move |_, object| {
        let Some(item) = object.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let label = gtk::Label::new(None);
        label.add_css_class("debug-table-cell");
        label.add_css_class(register_column_css(column));
        label.set_halign(gtk::Align::Start);
        label.set_ellipsize(pango::EllipsizeMode::End);
        if !matches!(column, RegisterColumn::Name) {
            enable_stable_text_selection(&label);
        }
        item.set_child(Some(&label));
    });
    factory.connect_bind(move |_, object| {
        let Some(item) = object.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let (Some(label), Some(data)) = (
            item.child().and_downcast::<gtk::Label>(),
            item.item().and_downcast::<glib::BoxedAnyObject>(),
        ) else {
            return;
        };
        clear_label_selection(&label);
        let data = data.borrow::<RegisterRowData>();
        reset_semantic_css(&label);
        if data.changed && matches!(column, RegisterColumn::Name) {
            label.add_css_class("modified-register");
        }
        match column {
            RegisterColumn::Name => {
                label.set_text(&format!("${}:", data.register.name));
                label.set_tooltip_text(Some(&format!(
                    "{}\nDouble-click or press Enter to edit",
                    data.register.name
                )));
            }
            RegisterColumn::Value => {
                let semantic_class = register_value_css(
                    &data.register,
                    data.architecture,
                    data.endian,
                    data.pointer_bits,
                );
                label.add_css_class(semantic_class);
                let text = register_primary_value(&data.register, data.architecture);
                label.set_text(&text);
                label.set_tooltip_text(Some(&format!(
                    "{}\nDouble-click or press Enter to edit",
                    register_text(
                        &data.register,
                        data.architecture,
                        data.endian,
                        data.pointer_bits,
                    )
                )));
            }
            RegisterColumn::Details => {
                let semantic_class = register_value_css(
                    &data.register,
                    data.architecture,
                    data.endian,
                    data.pointer_bits,
                );
                label.add_css_class(semantic_class);
                if is_flags_register(&data.register.name) {
                    label.set_markup(&flags_details_markup(
                        &data.register.name,
                        &data.register.value,
                        data.ring,
                    ));
                } else {
                    label.set_text(&register_details(
                        &data.register,
                        data.architecture,
                        data.endian,
                        data.pointer_bits,
                    ));
                }
                label.set_tooltip_text(Some(&format!(
                    "{}\nDouble-click or press Enter to edit",
                    register_text(
                        &data.register,
                        data.architecture,
                        data.endian,
                        data.pointer_bits,
                    )
                )));
            }
        }
    });
    let column_view = gtk::ColumnViewColumn::new(Some(title), Some(factory));
    column_view.set_fixed_width(width);
    column_view.set_resizable(true);
    column_view.set_expand(expand);
    column_view
}

pub(super) fn build_stack_view() -> (gtk::ColumnView, gio::ListStore, StackWordInspector) {
    let store = gio::ListStore::new::<glib::BoxedAnyObject>();
    let selection = gtk::SingleSelection::new(Some(store.clone()));
    selection.set_autoselect(true);
    selection.set_can_unselect(false);
    let view = gtk::ColumnView::new(Some(selection.clone()));
    view.add_css_class("debug-table");
    view.add_css_class("stack-table");
    view.set_vexpand(true);
    view.set_reorderable(true);

    for (title, width, expand, column) in [
        ("ANCHOR", 80, false, StackColumn::Anchor),
        ("ADDRESS", 175, false, StackColumn::Address),
        ("VALUE / POINTER CHAIN", 285, true, StackColumn::Value),
        ("OFFSET", 82, false, StackColumn::Offset),
        ("INDEX", 62, false, StackColumn::Index),
        ("REFERENCES", 155, false, StackColumn::References),
        ("REGION", 210, false, StackColumn::Region),
    ] {
        view.append_column(&stack_column(title, width, expand, column, &selection));
    }
    let inspector = build_stack_word_inspector();
    let inspector_for_selection = inspector.clone();
    selection.connect_selected_item_notify(move |selection| {
        let Some(data) = selection
            .selected_item()
            .and_downcast::<glib::BoxedAnyObject>()
        else {
            inspector_for_selection.clear();
            return;
        };
        inspector_for_selection.show(&data.borrow::<StackEntry>());
    });
    (view, store, inspector)
}

pub(super) fn build_stack_word_inspector() -> StackWordInspector {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 2);
    root.add_css_class("stack-word-inspector");
    root.append(&section_title("SELECTED WORD"));
    let grid = gtk::Grid::builder()
        .column_spacing(8)
        .row_spacing(1)
        .build();
    let address = stack_inspector_row(&grid, 0, "ADDRESS");
    let raw = stack_inspector_row(&grid, 1, "RAW");
    let interpretation = stack_inspector_row(&grid, 2, "INTERPRETATION");
    let role = stack_inspector_row(&grid, 3, "ROLE");
    let region = stack_inspector_row(&grid, 4, "REGION");
    for value in [&interpretation, &role, &region] {
        value.set_ellipsize(pango::EllipsizeMode::None);
        value.set_wrap(false);
    }
    let grid_scroll = gtk::ScrolledWindow::new();
    grid_scroll.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Never);
    grid_scroll.set_propagate_natural_height(true);
    grid_scroll.set_child(Some(&grid));
    root.append(&grid_scroll);
    let inspector = StackWordInspector {
        root,
        address,
        raw,
        interpretation,
        role,
        region,
    };
    inspector.clear();
    inspector
}

pub(super) fn stack_inspector_row(grid: &gtk::Grid, row: i32, title: &str) -> gtk::Label {
    let title = gtk::Label::new(Some(title));
    title.add_css_class("stack-inspector-key");
    title.set_halign(gtk::Align::Start);
    grid.attach(&title, 0, row, 1, 1);
    let value = gtk::Label::new(None);
    value.add_css_class("stack-inspector-value");
    value.set_halign(gtk::Align::Start);
    value.set_hexpand(true);
    enable_stable_text_selection(&value);
    value.set_ellipsize(pango::EllipsizeMode::Middle);
    grid.attach(&value, 1, row, 1, 1);
    value
}

impl StackWordInspector {
    fn clear(&self) {
        self.address.set_text("Select a stack word");
        self.raw.set_text("");
        self.interpretation.set_text("");
        self.role.set_text("");
        self.region.set_text("");
        reset_semantic_css(&self.interpretation);
    }

    fn show(&self, entry: &StackEntry) {
        let width = usize::try_from(entry.pointer_bits / 4)
            .unwrap_or(16)
            .clamp(8, 16);
        self.address.set_text(&format!(
            "0x{:0width$x}  ·  SP+0x{:x}  ·  word {}",
            entry.address,
            entry.offset,
            entry.index,
            width = width,
        ));
        self.address.set_tooltip_text(Some(&self.address.text()));
        self.raw.set_text(&entry.value);
        self.raw.set_tooltip_text(Some(&entry.value));
        let interpretation = stack_entry_text(entry);
        self.interpretation.set_text(&interpretation);
        self.interpretation.set_tooltip_text(Some(&interpretation));
        reset_semantic_css(&self.interpretation);
        self.interpretation
            .add_css_class(memory_kind_css(entry.memory_kind));
        let role = stack_word_role(entry);
        self.role.set_text(&role);
        self.role.set_tooltip_text(Some(&role));
        let region = entry.region.as_deref().unwrap_or("unmapped / scalar");
        self.region.set_text(region);
        self.region.set_tooltip_text(Some(region));
    }
}

pub(super) fn stack_column(
    title: &str,
    width: i32,
    expand: bool,
    column: StackColumn,
    selection: &gtk::SingleSelection,
) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    let selection = selection.clone();
    factory.connect_setup(move |_, object| {
        let Some(item) = object.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let label = gtk::Label::new(None);
        label.add_css_class("debug-table-cell");
        label.add_css_class(stack_column_css(column));
        label.set_halign(gtk::Align::Start);
        label.set_ellipsize(pango::EllipsizeMode::End);
        enable_stable_text_selection(&label);
        let click = gtk::GestureClick::new();
        let item_for_click = item.clone();
        let selection = selection.clone();
        click.connect_pressed(move |_, _, _, _| {
            selection.set_selected(item_for_click.position());
        });
        label.add_controller(click);
        item.set_child(Some(&label));
    });
    factory.connect_bind(move |_, object| {
        let Some(item) = object.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let (Some(label), Some(data)) = (
            item.child().and_downcast::<gtk::Label>(),
            item.item().and_downcast::<glib::BoxedAnyObject>(),
        ) else {
            return;
        };
        clear_label_selection(&label);
        let entry = data.borrow::<StackEntry>();
        reset_semantic_css(&label);
        let text = match column {
            StackColumn::Anchor => entry
                .address_registers
                .iter()
                .map(|name| format!("${name}"))
                .collect::<Vec<_>>()
                .join(","),
            StackColumn::Address => {
                label.add_css_class("memory-stack");
                let width = usize::try_from(entry.pointer_bits / 4)
                    .unwrap_or(16)
                    .clamp(8, 16);
                format!("0x{:0width$x}", entry.address, width = width)
            }
            StackColumn::Value => {
                label.add_css_class(memory_kind_css(entry.memory_kind));
                stack_entry_text(&entry)
            }
            StackColumn::Offset => format!("+0x{:04x}", entry.offset),
            StackColumn::Index => format!("+{:03}", entry.index),
            StackColumn::References => stack_references(&entry),
            StackColumn::Region => entry.region.clone().unwrap_or_default(),
        };
        label.set_text(&text);
        label.set_tooltip_text(Some(&stack_tooltip(&entry)));
    });
    let column_view = gtk::ColumnViewColumn::new(Some(title), Some(factory));
    column_view.set_fixed_width(width);
    column_view.set_resizable(true);
    column_view.set_expand(expand);
    column_view
}

pub(super) fn register_column_css(column: RegisterColumn) -> &'static str {
    match column {
        RegisterColumn::Name => "register-name",
        RegisterColumn::Value => "register-value",
        RegisterColumn::Details => "register-details",
    }
}

pub(super) fn stack_column_css(column: StackColumn) -> &'static str {
    match column {
        StackColumn::Anchor => "stack-register-marker",
        StackColumn::Address => "stack-address",
        StackColumn::Value => "stack-value",
        StackColumn::Offset | StackColumn::Index => "stack-position",
        StackColumn::References => "stack-references",
        StackColumn::Region => "stack-region",
    }
}

pub(super) fn reset_semantic_css(label: &gtk::Label) {
    for class in [
        "memory-code",
        "memory-heap",
        "memory-stack",
        "memory-writable",
        "memory-readonly",
        "memory-rwx",
        "memory-string",
        "memory-none",
        "register-zero",
        "modified-register",
    ] {
        label.remove_css_class(class);
    }
}

pub(super) fn instruction_column(
    title: &str,
    width: i32,
    expand: bool,
    class: &'static str,
    selection: &gtk::SingleSelection,
    text: for<'a> fn(&'a InstructionRowData) -> Cow<'a, str>,
) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    let selection = selection.clone();
    factory.connect_setup(move |_, object| {
        let Some(item) = object.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let label = gtk::Label::new(None);
        label.add_css_class("instruction-cell");
        label.add_css_class(class);
        label.set_halign(gtk::Align::Start);
        label.set_ellipsize(pango::EllipsizeMode::End);
        enable_stable_text_selection(&label);
        label.set_cursor_from_name(Some("text"));
        let click = gtk::GestureClick::new();
        click.set_button(gtk::gdk::BUTTON_PRIMARY);
        let item_for_click = item.clone();
        let selection = selection.clone();
        click.connect_pressed(move |_, _, _, _| {
            selection.set_selected(item_for_click.position());
        });
        label.add_controller(click);
        item.set_child(Some(&label));
    });
    factory.connect_bind(move |_, object| {
        let Some(item) = object.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let (Some(label), Some(data)) = (
            item.child().and_downcast::<gtk::Label>(),
            item.item().and_downcast::<glib::BoxedAnyObject>(),
        ) else {
            return;
        };
        clear_label_selection(&label);
        let data = data.borrow::<InstructionRowData>();
        if class == "instruction-address"
            && data.instruction.function != "??"
            && data.instruction.offset == "0"
        {
            label.add_css_class("function-boundary-cell");
        } else {
            label.remove_css_class("function-boundary-cell");
        }
        label.set_text(&text(&data));
        label.set_tooltip_text(Some(&format!(
            "{} · {}\n{}\nSelect text to copy. Press Enter or double-click outside a text selection to toggle an instruction breakpoint",
            data.instruction.address,
            data.instruction.text,
            instruction_symbol_full(&data.instruction),
        )));
    });
    let column = gtk::ColumnViewColumn::new(Some(title), Some(factory));
    column.set_fixed_width(width);
    column.set_resizable(true);
    column.set_expand(expand);
    column
}

pub(super) fn build_editor_panel(notebook: &gtk::Notebook) -> gtk::Box {
    let panel = gtk::Box::new(gtk::Orientation::Vertical, 0);
    panel.add_css_class("panel");
    panel.append(notebook);
    panel
}

pub(super) fn build_terminal_panel(
    terminal: &vte4::Terminal,
    gef_tools_button: &gtk::ToggleButton,
) -> gtk::Box {
    let panel = gtk::Box::new(gtk::Orientation::Vertical, 0);
    panel.add_css_class("panel");

    let header = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    header.add_css_class("panel-header");
    header.add_css_class("terminal-header");
    let title = section_title("");
    title.set_hexpand(true);
    header.append(&title);
    header.append(gef_tools_button);
    panel.append(&header);

    let scrolled = gtk::ScrolledWindow::builder()
        .child(terminal)
        .hexpand(true)
        .vexpand(true)
        .build();
    panel.append(&scrolled);
    panel
}

pub(super) fn build_source_notebook(
    style_scheme: Option<&sourceview5::StyleScheme>,
) -> gtk::Notebook {
    let notebook = gtk::Notebook::new();
    notebook.add_css_class("source-notebook");
    notebook.set_scrollable(true);
    notebook.set_show_border(false);
    notebook.set_hexpand(true);
    notebook.set_vexpand(true);
    append_welcome_source(&notebook, style_scheme);
    notebook
}

pub(super) fn append_welcome_source(
    notebook: &gtk::Notebook,
    style_scheme: Option<&sourceview5::StyleScheme>,
) {
    let buffer = build_source_buffer(INITIAL_SOURCE, None, style_scheme);
    let view = build_source_view(&buffer);
    let page = gtk::ScrolledWindow::builder()
        .child(&view)
        .hexpand(true)
        .vexpand(true)
        .build();
    let tab = gtk::Label::new(Some("welcome.c"));
    tab.add_css_class("source-tab");
    notebook.append_page(&page, Some(&tab));
}

pub(super) fn build_source_buffer(
    contents: &str,
    path: Option<&Path>,
    style_scheme: Option<&sourceview5::StyleScheme>,
) -> sourceview5::Buffer {
    let manager = sourceview5::LanguageManager::default();
    let bundled_languages = format!("resource://{}/language-specs", crate::RESOURCE_PREFIX);
    if !manager
        .search_path()
        .iter()
        .any(|path| path.as_str() == bundled_languages)
    {
        manager.prepend_search_path(&bundled_languages);
    }
    let language = path.map_or_else(
        || manager.language("c"),
        |path| manager.guess_language(Some(path), None),
    );
    let buffer = sourceview5::Buffer::builder()
        .highlight_matching_brackets(true)
        .highlight_syntax(true)
        .text(contents)
        .build();
    buffer.set_language(language.as_ref());
    buffer.set_style_scheme(style_scheme);
    buffer
}

pub(super) fn build_source_view(buffer: &sourceview5::Buffer) -> sourceview5::View {
    sourceview5::View::builder()
        .buffer(buffer)
        .editable(false)
        .highlight_current_line(true)
        .show_line_marks(false)
        .show_line_numbers(false)
        .tab_width(4)
        .top_margin(5)
        .bottom_margin(5)
        .left_margin(4)
        .right_margin(6)
        .monospace(true)
        .build()
}

pub(super) fn build_terminal(theme: &Theme) -> vte4::Terminal {
    let terminal = vte4::Terminal::new();
    terminal.set_hexpand(true);
    terminal.set_vexpand(true);
    terminal.set_scrollback_lines(20_000);
    terminal.set_scroll_on_output(false);
    terminal.set_scroll_on_keystroke(true);
    terminal.set_audible_bell(false);
    terminal.set_cursor_blink_mode(vte4::CursorBlinkMode::On);
    terminal.set_font(Some(&pango::FontDescription::from_string("Monospace 9.5")));
    theme.style_terminal(&terminal);
    connect_terminal_clipboard(&terminal);
    terminal
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TerminalClipboardAction {
    Copy,
    Paste,
}

pub(super) fn terminal_clipboard_action(
    key: gtk::gdk::Key,
    state: gtk::gdk::ModifierType,
    has_selection: bool,
) -> Option<TerminalClipboardAction> {
    if state.intersects(gtk::gdk::ModifierType::ALT_MASK | gtk::gdk::ModifierType::SUPER_MASK) {
        return None;
    }
    let control = state.contains(gtk::gdk::ModifierType::CONTROL_MASK);
    let shift = state.contains(gtk::gdk::ModifierType::SHIFT_MASK);
    let insert = matches!(key, gtk::gdk::Key::Insert | gtk::gdk::Key::KP_Insert);
    if (control && matches!(key, gtk::gdk::Key::v | gtk::gdk::Key::V))
        || (shift && !control && insert)
    {
        return Some(TerminalClipboardAction::Paste);
    }
    if (control && !shift && insert)
        || (control
            && matches!(key, gtk::gdk::Key::c | gtk::gdk::Key::C)
            && (shift || has_selection))
    {
        return Some(TerminalClipboardAction::Copy);
    }
    None
}

fn connect_terminal_clipboard(terminal: &vte4::Terminal) {
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let terminal_for_keys = terminal.clone();
    keys.connect_key_pressed(move |_, key, _, state| {
        match terminal_clipboard_action(key, state, terminal_for_keys.has_selection()) {
            Some(TerminalClipboardAction::Copy) => {
                terminal_for_keys.copy_clipboard_format(vte4::Format::Text);
                gtk::glib::Propagation::Stop
            }
            Some(TerminalClipboardAction::Paste) => {
                terminal_for_keys.paste_clipboard();
                gtk::glib::Propagation::Stop
            }
            None => gtk::glib::Propagation::Proceed,
        }
    });
    terminal.add_controller(keys);

    let right_click = gtk::GestureClick::new();
    right_click.set_button(3);
    right_click.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak_terminal = terminal.downgrade();
    right_click.connect_pressed(move |gesture, _, x, y| {
        let Some(terminal) = weak_terminal.upgrade() else {
            return;
        };
        open_terminal_context_menu(&terminal, x, y);
        gesture.set_state(gtk::EventSequenceState::Claimed);
    });
    terminal.add_controller(right_click);
}

fn open_terminal_context_menu(terminal: &vte4::Terminal, x: f64, y: f64) {
    let popover = gtk::Popover::builder()
        .has_arrow(false)
        .autohide(true)
        .build();
    let menu = gtk::Box::new(gtk::Orientation::Vertical, 1);
    menu.add_css_class("terminal-context-menu");

    let copy = gtk::Button::with_label("Copy");
    copy.set_sensitive(terminal.has_selection());
    copy.set_tooltip_text(Some("Copy selected terminal text · Ctrl+Shift+C"));
    let paste = gtk::Button::with_label("Paste");
    paste.set_tooltip_text(Some("Paste clipboard text · Ctrl+V or Ctrl+Shift+V"));
    let select_all = gtk::Button::with_label("Select all");
    for button in [&copy, &paste, &select_all] {
        button.set_halign(gtk::Align::Fill);
        button.set_hexpand(true);
        menu.append(button);
    }

    let terminal_for_copy = terminal.clone();
    let popover_for_copy = popover.clone();
    copy.connect_clicked(move |_| {
        terminal_for_copy.copy_clipboard_format(vte4::Format::Text);
        popover_for_copy.popdown();
    });
    let terminal_for_paste = terminal.clone();
    let popover_for_paste = popover.clone();
    paste.connect_clicked(move |_| {
        terminal_for_paste.paste_clipboard();
        terminal_for_paste.grab_focus();
        popover_for_paste.popdown();
    });
    let terminal_for_select = terminal.clone();
    let popover_for_select = popover.clone();
    select_all.connect_clicked(move |_| {
        terminal_for_select.select_all();
        popover_for_select.popdown();
    });

    popover.set_child(Some(&menu));
    popover.set_parent(terminal);
    popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(
        x.round() as i32,
        y.round() as i32,
        1,
        1,
    )));
    popover.connect_closed(|popover| popover.unparent());
    popover.popup();
}

pub(super) fn control_button(label: &str, tooltip: &str, suggested: bool) -> gtk::Button {
    let button = gtk::Button::with_label(label);
    button.add_css_class("debug-control");
    button.set_tooltip_text(Some(tooltip));
    button.set_sensitive(false);
    if suggested {
        button.add_css_class("primary-control");
    }
    button
}

pub(super) fn section_title(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.add_css_class("section-title");
    label.set_halign(gtk::Align::Start);
    label
}

pub(super) fn sidebar_row(key: &str, value: &str) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    row.add_css_class("sidebar-row");
    let key = gtk::Label::new(Some(key));
    key.add_css_class("muted");
    key.set_halign(gtk::Align::Start);
    key.set_width_chars(8);
    let value = gtk::Label::new(Some(value));
    value.set_halign(gtk::Align::Start);
    value.set_ellipsize(pango::EllipsizeMode::Middle);
    value.set_hexpand(true);
    row.append(&key);
    row.append(&value);
    row
}
