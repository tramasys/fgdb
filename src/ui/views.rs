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

#[derive(Clone)]
struct VariableMenuContext {
    selection: gtk::SingleSelection,
    handler: Rc<RefCell<Option<VariableViewerHandler>>>,
    viewers: Rc<VariableViewerRegistry>,
}

pub(super) fn build_locals_view(
    children_handler: &Rc<RefCell<Option<VariableChildrenHandler>>>,
    viewer_handler: &Rc<RefCell<Option<VariableViewerHandler>>>,
    viewers: &Rc<VariableViewerRegistry>,
    target_pointer_bits: &Rc<Cell<u32>>,
    filter_controls: Option<(&gtk::Entry, &gtk::ToggleButton)>,
) -> (gtk::ColumnView, gio::ListStore, gtk::SingleSelection) {
    let store = gio::ListStore::new::<glib::BoxedAnyObject>();
    let roots: gio::ListModel = if let Some((search, changed_toggle)) = filter_controls {
        let query = Rc::new(RefCell::new(String::new()));
        let changed_only = Rc::new(Cell::new(false));
        let query_for_filter = Rc::clone(&query);
        let changed_for_filter = Rc::clone(&changed_only);
        let filter = gtk::CustomFilter::new(move |object| {
            let Some(item) = object.downcast_ref::<glib::BoxedAnyObject>() else {
                return false;
            };
            let node = item.borrow::<VariableNode>();
            (!changed_for_filter.get() || node.has_changes())
                && variable_matches_filter(&node.variable, &query_for_filter.borrow())
        });
        let query_for_search = Rc::clone(&query);
        let filter_for_search = filter.clone();
        search.connect_changed(move |search| {
            query_for_search.replace(search.text().trim().to_ascii_lowercase());
            filter_for_search.changed(gtk::FilterChange::Different);
        });
        let filter_for_changed = filter.clone();
        changed_toggle.connect_toggled(move |toggle| {
            changed_only.set(toggle.is_active());
            filter_for_changed.changed(gtk::FilterChange::Different);
        });
        gtk::FilterListModel::new(Some(store.clone()), Some(filter)).upcast()
    } else {
        store.clone().upcast()
    };
    let tree = gtk::TreeListModel::new(roots, false, false, |item| {
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

    let variable_menu = VariableMenuContext {
        selection: selection.clone(),
        handler: Rc::clone(viewer_handler),
        viewers: Rc::clone(viewers),
    };
    view.append_column(&local_name_column(children_handler, &variable_menu));
    view.append_column(&local_text_column(
        "VALUE",
        360,
        true,
        LocalColumn::Value,
        Rc::clone(target_pointer_bits),
        &variable_menu,
    ));
    view.append_column(&local_text_column(
        "TYPE",
        260,
        false,
        LocalColumn::Type,
        Rc::clone(target_pointer_bits),
        &variable_menu,
    ));
    (view, store, selection)
}

pub(super) fn variable_matches_filter(variable: &Variable, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    let search_text = format!(
        "{} {} {} {}",
        variable.name,
        variable.type_name.as_deref().unwrap_or_default(),
        compact_variable_type(variable.type_name.as_deref().unwrap_or_default()),
        variable.value,
    )
    .to_ascii_lowercase();
    query
        .split_whitespace()
        .all(|term| search_text.contains(term))
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

fn local_name_column(
    children_handler: &Rc<RefCell<Option<VariableChildrenHandler>>>,
    variable_menu: &VariableMenuContext,
) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    let selection = variable_menu.selection.clone();
    let children_handler = Rc::clone(children_handler);
    let variable_menu = variable_menu.clone();
    factory.connect_bind(move |_, object| {
        let Some(item) = object.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(row) = item.item().and_downcast::<gtk::TreeListRow>() else {
            return;
        };
        let Some(data) = row.item().and_downcast::<glib::BoxedAnyObject>() else {
            return;
        };
        let node = data.borrow::<VariableNode>().clone();
        let expandable = node.variable.can_expand();
        let load_more = node.load_more.is_some();

        let content = gtk::Box::new(gtk::Orientation::Horizontal, 5);
        content.add_css_class("local-name-cell");
        content.set_hexpand(true);
        let disclosure = gtk::Label::new(Some(if expandable {
            if row.is_expanded() {
                DISCLOSURE_EXPANDED_ICON
            } else {
                DISCLOSURE_COLLAPSED_ICON
            }
        } else if load_more {
            DISCLOSURE_COLLAPSED_ICON
        } else {
            ""
        }));
        disclosure.add_css_class("local-disclosure");
        disclosure.set_width_chars(1);
        if expandable {
            row.bind_property("expanded", &disclosure, "label")
                .transform_to(|_, expanded: bool| {
                    Some(if expanded {
                        DISCLOSURE_EXPANDED_ICON
                    } else {
                        DISCLOSURE_COLLAPSED_ICON
                    })
                })
                .sync_create()
                .build();
        }
        content.append(&disclosure);
        let scope = gtk::Label::new((row.depth() == 0).then_some(if node.variable.argument {
            "ARG"
        } else {
            "LOCAL"
        }));
        scope.add_css_class("local-scope");
        scope.set_visible(row.depth() == 0 && !node.placeholder);
        content.append(&scope);
        let changed = gtk::Label::new(Some("●"));
        changed.add_css_class("local-changed-marker");
        changed.set_tooltip_text(Some("Value changed since the previous stop"));
        changed.set_opacity(if node.changed { 1.0 } else { 0.0 });
        changed.set_visible(!node.placeholder);
        content.append(&changed);
        let label = gtk::Label::new(Some(&node.variable.name));
        label.add_css_class("debug-table-cell");
        label.add_css_class("local-name");
        label.set_halign(gtk::Align::Start);
        label.set_xalign(0.0);
        label.set_ellipsize(pango::EllipsizeMode::End);
        label.set_hexpand(true);
        content.append(&label);

        let tooltip = if node.placeholder {
            format!("{}\n{}", node.variable.name, node.variable.value)
        } else {
            variable_tooltip(&node.variable)
        };
        content.set_tooltip_text(Some(&tooltip));
        if expandable || load_more {
            content.add_css_class("local-expandable");
            content.set_cursor_from_name(Some("pointer"));
        }
        if load_more {
            label.remove_css_class("local-name");
            label.add_css_class("local-load-more");
        } else if node.placeholder {
            label.remove_css_class("local-name");
            label.add_css_class("muted");
        }

        let click = gtk::GestureClick::new();
        click.set_button(gtk::gdk::BUTTON_PRIMARY);
        click.set_propagation_phase(gtk::PropagationPhase::Capture);
        let selection_for_click = selection.clone();
        let row_for_click = row.clone();
        let node_for_click = node.clone();
        let children_handler_for_click = Rc::clone(&children_handler);
        click.connect_pressed(move |gesture, presses, _, _| {
            if presses != 1 {
                return;
            }
            if !row_for_click.is_expandable() && node_for_click.load_more.is_none() {
                return;
            }
            selection_for_click.set_selected(row_for_click.position());
            if row_for_click.is_expandable() {
                row_for_click.set_expanded(!row_for_click.is_expanded());
            } else {
                request_next_variable_page_if_needed(&node_for_click, &children_handler_for_click);
            }
            gesture.set_state(gtk::EventSequenceState::Claimed);
        });
        content.add_controller(click);
        if !node.placeholder {
            connect_current_variable_context_menu(&content, item, &variable_menu);
        }

        if expandable && !node.expansion_observer_attached.replace(true) {
            let node = node.clone();
            let children_handler = Rc::clone(&children_handler);
            row.connect_expanded_notify(move |row| {
                node.expanded.set(row.is_expanded());
                if row.is_expanded() {
                    request_variable_children_if_needed(&node, &children_handler);
                }
            });
        }
        if expandable && node.expanded.get() && !row.is_expanded() {
            row.set_expanded(true);
        }
        let expander = gtk::TreeExpander::new();
        expander.set_list_row(Some(&row));
        expander.set_hide_expander(true);
        expander.set_indent_for_icon(false);
        expander.set_child(Some(&content));
        item.set_child(Some(&expander));
    });
    factory.connect_unbind(|_, object| {
        if let Some(item) = object.downcast_ref::<gtk::ListItem>() {
            item.set_child(None::<&gtk::Widget>);
        }
    });
    let column = gtk::ColumnViewColumn::new(Some("NAME / EXPRESSION"), Some(factory));
    column.set_fixed_width(230);
    column.set_resizable(true);
    column
}

fn connect_current_variable_context_menu(
    row_widget: &impl IsA<gtk::Widget>,
    item: &gtk::ListItem,
    variable_menu: &VariableMenuContext,
) {
    let click = gtk::GestureClick::new();
    click.set_button(gtk::gdk::BUTTON_SECONDARY);
    click.set_propagation_phase(gtk::PropagationPhase::Capture);
    let row_widget = row_widget.as_ref().clone();
    let row_widget_for_click = row_widget.downgrade();
    let item = item.downgrade();
    let variable_menu = variable_menu.clone();
    click.connect_pressed(move |gesture, presses, x, y| {
        if presses != 1 {
            return;
        }
        let Some(row_widget_for_click) = row_widget_for_click.upgrade() else {
            return;
        };
        let Some(item) = item.upgrade() else {
            return;
        };
        let Some(row) = item.item().and_downcast::<gtk::TreeListRow>() else {
            return;
        };
        let Some(data) = row.item().and_downcast::<glib::BoxedAnyObject>() else {
            return;
        };
        let node = data.borrow::<VariableNode>();
        if node.placeholder {
            return;
        }
        let variable = node.variable.clone();
        drop(node);
        variable_menu.selection.set_selected(row.position());
        show_variable_context_menu(
            &row_widget_for_click,
            &variable,
            &variable_menu.handler,
            &variable_menu.viewers,
            x,
            y,
        );
        gesture.set_state(gtk::EventSequenceState::Claimed);
    });
    row_widget.add_controller(click);
}

fn show_variable_context_menu(
    row_widget: &gtk::Widget,
    variable: &Variable,
    viewer_handler: &Rc<RefCell<Option<VariableViewerHandler>>>,
    viewers: &VariableViewerRegistry,
    x: f64,
    y: f64,
) {
    let popover = gtk::Popover::new();
    popover.add_css_class("local-variable-menu");
    popover.set_autohide(true);
    popover.set_has_arrow(false);
    popover.set_parent(row_widget);
    popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(
        x.round() as i32,
        y.round() as i32,
        1,
        1,
    )));
    let menu = gtk::Box::new(gtk::Orientation::Vertical, 1);
    menu.add_css_class("local-variable-menu-content");
    let summary = gtk::Box::new(gtk::Orientation::Vertical, 3);
    summary.add_css_class("local-variable-menu-summary");
    let caption = gtk::Label::new(Some(if variable.argument {
        "ARGUMENT"
    } else {
        "VARIABLE"
    }));
    caption.add_css_class("local-variable-menu-caption");
    caption.set_halign(gtk::Align::Start);
    let name = gtk::Label::new(Some(&variable.name));
    name.add_css_class("local-variable-menu-name");
    name.set_halign(gtk::Align::Start);
    name.set_ellipsize(pango::EllipsizeMode::Middle);
    let type_name =
        compact_variable_type(variable.type_name.as_deref().unwrap_or("<unknown type>"));
    let type_label = gtk::Label::new(Some(&type_name));
    type_label.add_css_class("local-variable-menu-type");
    type_label.set_halign(gtk::Align::Start);
    type_label.set_ellipsize(pango::EllipsizeMode::Middle);
    let value = gtk::Label::new(Some(&variable.value));
    value.add_css_class("local-variable-menu-value");
    value.set_halign(gtk::Align::Start);
    value.set_ellipsize(pango::EllipsizeMode::Middle);
    summary.append(&caption);
    summary.append(&name);
    summary.append(&type_label);
    summary.append(&value);
    menu.append(&summary);

    let matching_viewers = viewers.matching(variable);
    if !matching_viewers.is_empty() {
        menu.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        menu.append(&variable_menu_section("VIEW AS"));
        for descriptor in matching_viewers {
            let button = variable_menu_action(&descriptor.title, &descriptor.detail);
            button.add_css_class("local-variable-viewer-action");
            let request = VariableViewerRequest {
                descriptor,
                variable: variable.clone(),
            };
            let viewer_handler = Rc::clone(viewer_handler);
            let popover = popover.downgrade();
            button.connect_clicked(move |_| {
                let handler = viewer_handler.borrow().clone();
                if let Some(handler) = handler {
                    handler(request.clone());
                }
                if let Some(popover) = popover.upgrade() {
                    popover.popdown();
                }
            });
            menu.append(&button);
        }
    }

    menu.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    menu.append(&variable_menu_section("COPY"));
    for (label, detail, text) in [
        ("Copy name", "expression", variable.name.clone()),
        ("Copy value", "formatted value", variable.value.clone()),
        (
            "Copy type",
            "full debugger type",
            variable
                .type_name
                .clone()
                .unwrap_or_else(|| String::from("<unknown>")),
        ),
    ] {
        let button = variable_menu_action(label, detail);
        let display = row_widget.display();
        let popover = popover.downgrade();
        button.connect_clicked(move |_| {
            display.clipboard().set_text(&text);
            if let Some(popover) = popover.upgrade() {
                popover.popdown();
            }
        });
        menu.append(&button);
    }
    popover.set_child(Some(&menu));
    popover.connect_closed(|popover| {
        if popover.parent().is_some() {
            popover.unparent();
        }
    });
    popover.popup();
}

fn variable_menu_section(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.add_css_class("local-variable-menu-section");
    label.set_halign(gtk::Align::Start);
    label
}

fn variable_menu_action(label: &str, detail: &str) -> gtk::Button {
    let row = gtk::Box::new(gtk::Orientation::Vertical, 1);
    let label = gtk::Label::new(Some(label));
    label.add_css_class("local-variable-menu-action-label");
    label.set_halign(gtk::Align::Start);
    let detail = gtk::Label::new(Some(detail));
    detail.add_css_class("local-variable-menu-action-detail");
    detail.set_halign(gtk::Align::Start);
    row.append(&label);
    row.append(&detail);
    let button = gtk::Button::builder().child(&row).build();
    button.add_css_class("local-variable-menu-action");
    button.set_halign(gtk::Align::Fill);
    button
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
    let handler = children_handler.borrow().clone();
    if let Some(handler) = handler {
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
    let handler = children_handler.borrow().clone();
    if let Some(handler) = handler {
        handler(parent.clone(), *from);
    } else {
        node.children_loading.set(false);
    }
}

fn local_text_column(
    title: &str,
    width: i32,
    expand: bool,
    column: LocalColumn,
    target_pointer_bits: Rc<Cell<u32>>,
    variable_menu: &VariableMenuContext,
) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    let variable_menu = variable_menu.clone();
    factory.connect_setup(move |_, object| {
        let Some(item) = object.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let label = gtk::Label::new(None);
        label.add_css_class("debug-table-cell");
        label.add_css_class(match column {
            LocalColumn::Type => "local-type",
            LocalColumn::Value => "local-value",
        });
        label.set_halign(gtk::Align::Start);
        label.set_ellipsize(pango::EllipsizeMode::End);
        enable_stable_text_selection(&label);
        connect_current_variable_context_menu(&label, item, &variable_menu);
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
        label.remove_css_class("local-changed-value");
        match column {
            LocalColumn::Type => {
                label.set_text(&compact_variable_type(
                    variable.type_name.as_deref().unwrap_or("<unknown>"),
                ));
            }
            LocalColumn::Value => {
                let display =
                    variable_display_value(variable, value, details, target_pointer_bits.get());
                label.set_text(&display);
                if display.contains("<error:") {
                    label.add_css_class("local-details-error");
                } else if node.changed {
                    label.add_css_class("local-changed-value");
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

pub(super) fn variable_display_value(
    variable: &Variable,
    value: &str,
    details: &str,
    target_pointer_bits: u32,
) -> String {
    let decimal = integer_decimal_value(variable, value, target_pointer_bits);
    match (details.is_empty(), decimal) {
        (true, Some(decimal)) if decimal != value => format!("{value}  ({decimal})"),
        (false, Some(decimal)) => format!("{value}  {details}  ({decimal})"),
        (false, None) => format!("{value}  {details}"),
        _ => compact_pretty_value(variable, value),
    }
}

fn compact_pretty_value(variable: &Variable, value: &str) -> String {
    let Some(type_name) = variable.type_name.as_deref() else {
        return value.to_owned();
    };
    let type_name = type_name
        .trim_start_matches("const ")
        .trim_start_matches(['&', '*'])
        .trim_start();
    let Some((namespace, _)) = type_name.rsplit_once("::") else {
        return value.to_owned();
    };
    value
        .strip_prefix(namespace)
        .and_then(|value| value.strip_prefix("::"))
        .unwrap_or(value)
        .to_owned()
}

pub(crate) fn compact_variable_type(type_name: &str) -> String {
    let mut compact = type_name.trim().replace("std::__cxx11::", "std::");
    for (qualified, short) in [
        ("alloc::string::String", "String"),
        ("alloc::vec::Vec<", "Vec<"),
        ("alloc::boxed::Box<", "Box<"),
        ("alloc::rc::Rc<", "Rc<"),
        ("alloc::sync::Arc<", "Arc<"),
        ("core::cell::RefCell<", "RefCell<"),
        ("core::option::Option<", "Option<"),
        ("core::result::Result<", "Result<"),
        ("alloc::collections::vec_deque::VecDeque<", "VecDeque<"),
        ("alloc::collections::btree::map::BTreeMap<", "BTreeMap<"),
        ("std::collections::hash::map::HashMap<", "HashMap<"),
    ] {
        compact = compact.replace(qualified, short);
    }
    compact = compact.replace(
        "std::basic_string<char, std::char_traits<char>, std::allocator<char> >",
        "std::string",
    );
    compact = compact.replace(
        ", std::hash::random::RandomState, alloc::alloc::Global>",
        ">",
    );
    compact = compact.replace(", alloc::alloc::Global>", ">");
    while compact.contains("> >") {
        compact = compact.replace("> >", ">>");
    }
    compact
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

#[cfg(test)]
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
    let view = gtk::ColumnView::new(Some(selection));
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

pub(super) fn build_editor_panel(notebook: &gtk::Notebook) -> SourceEditorPanel {
    let panel = gtk::Box::new(gtk::Orientation::Vertical, 0);
    panel.add_css_class("panel");
    let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 1);
    toolbar.add_css_class("source-navigation-toolbar");
    let back = gtk::Button::with_label("‹");
    back.set_tooltip_text(Some("Back in source navigation history · Alt+Left"));
    back.set_sensitive(false);
    let forward = gtk::Button::with_label("›");
    forward.set_tooltip_text(Some("Forward in source navigation history · Alt+Right"));
    forward.set_sensitive(false);
    let quick_open = gtk::Button::with_label("Quick open");
    quick_open.set_tooltip_text(Some("Find a loaded or project source file · Ctrl+P"));
    for button in [&back, &forward, &quick_open] {
        button.add_css_class("source-navigation-action");
        toolbar.append(button);
    }

    let popover = gtk::Popover::new();
    popover.set_autohide(true);
    let menu = gtk::Box::new(gtk::Orientation::Vertical, 1);
    menu.add_css_class("source-navigation-menu");
    let find = source_navigation_menu_action("Find in file", "Ctrl+F");
    let go_to_line = source_navigation_menu_action("Go to line", "Ctrl+G");
    let symbols = source_navigation_menu_action("Functions and symbols", "Ctrl+Shift+O");
    let loaded_search = source_navigation_menu_action("Search loaded source files", "");
    let tree_search = source_navigation_menu_action("Search source tree", "Ctrl+Shift+F");
    let reopen_closed = source_navigation_menu_action("Reopen closed tab", "Ctrl+Shift+T");
    reopen_closed.set_sensitive(false);
    for button in [
        &find,
        &go_to_line,
        &symbols,
        &loaded_search,
        &tree_search,
        &reopen_closed,
    ] {
        let popover = popover.clone();
        button.connect_clicked(move |_| popover.popdown());
        menu.append(button);
    }
    popover.set_child(Some(&menu));
    let source_menu = gtk::MenuButton::new();
    source_menu.set_child(Some(&gtk::Label::new(Some("Source"))));
    source_menu.set_popover(Some(&popover));
    source_menu.add_css_class("source-navigation-menu-button");
    toolbar.append(&source_menu);
    panel.append(&toolbar);

    let find_bar = gtk::Box::new(gtk::Orientation::Horizontal, 2);
    find_bar.add_css_class("source-find-bar");
    find_bar.set_visible(false);
    let find_entry = source_search_entry("Find in current source file");
    find_entry.set_hexpand(true);
    let find_count = gtk::Label::new(None);
    find_count.add_css_class("source-find-count");
    let find_previous = gtk::Button::with_label("Prev");
    let find_next = gtk::Button::with_label("Next");
    let find_case = gtk::ToggleButton::with_label("Aa");
    find_case.set_tooltip_text(Some("Match case"));
    let find_close = gtk::Button::with_label("Close");
    for button in [&find_previous, &find_next, &find_close] {
        button.add_css_class("inline-action");
    }
    find_case.add_css_class("inline-action");
    find_bar.append(&find_entry);
    find_bar.append(&find_count);
    find_bar.append(&find_previous);
    find_bar.append(&find_next);
    find_bar.append(&find_case);
    find_bar.append(&find_close);
    panel.append(&find_bar);
    panel.append(notebook);
    SourceEditorPanel {
        root: panel,
        navigation: SourceNavigationControls {
            back,
            forward,
            quick_open,
            find,
            go_to_line,
            symbols,
            loaded_search,
            tree_search,
            reopen_closed,
            find_bar,
            find_entry,
            find_count,
            find_previous,
            find_next,
            find_case,
            find_close,
        },
    }
}

fn source_navigation_menu_action(label: &str, shortcut: &str) -> gtk::Button {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let label = gtk::Label::new(Some(label));
    label.set_halign(gtk::Align::Start);
    label.set_hexpand(true);
    let shortcut = gtk::Label::new(Some(shortcut));
    shortcut.add_css_class("muted");
    shortcut.set_halign(gtk::Align::End);
    row.append(&label);
    row.append(&shortcut);
    let button = gtk::Button::builder().child(&row).hexpand(true).build();
    button.add_css_class("source-navigation-menu-action");
    button
}

pub(super) fn source_search_entry(placeholder: &str) -> gtk::Entry {
    let entry = gtk::Entry::builder()
        .placeholder_text(placeholder)
        .primary_icon_name("system-search-symbolic")
        .build();
    entry.add_css_class("source-search-entry");
    entry.connect_changed(|entry| {
        entry.set_secondary_icon_name((!entry.text().is_empty()).then_some("edit-clear-symbolic"));
    });
    entry.connect_icon_release(|entry, position| {
        if position == gtk::EntryIconPosition::Secondary {
            entry.set_text("");
        }
    });
    entry
}

pub(super) fn build_inferior_controls() -> InferiorControls {
    let selector_model = gtk::StringList::new(&["No inferiors"]);
    let selector = gtk::DropDown::builder()
        .model(&selector_model)
        .hexpand(true)
        .build();
    selector.add_css_class("inferior-selector");
    selector.set_sensitive(false);
    let selected_state = gtk::Label::new(Some("idle"));
    selected_state.add_css_class("inferior-selected-state");
    selected_state.set_xalign(1.0);
    let summary_heading = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let caption = section_title("CURRENT INFERIOR");
    caption.set_hexpand(true);
    summary_heading.append(&caption);
    summary_heading.append(&selected_state);
    let stop_owner = gtk::Label::new(None);
    stop_owner.add_css_class("inferior-stop-owner");
    stop_owner.set_halign(gtk::Align::Fill);
    stop_owner.set_xalign(0.0);
    stop_owner.set_ellipsize(pango::EllipsizeMode::End);
    stop_owner.set_visible(false);
    let summary = gtk::Box::new(gtk::Orientation::Vertical, 4);
    summary.add_css_class("inferior-summary");
    summary.append(&summary_heading);
    summary.append(&selector);
    summary.append(&stop_owner);

    let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
    page.add_css_class("inferior-page");
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    header.add_css_class("inferior-page-header");
    let heading = section_title("PROCESS DEBUGGING");
    heading.set_hexpand(true);
    let refresh = gtk::Button::from_icon_name("view-refresh-symbolic");
    refresh.add_css_class("inferior-refresh");
    refresh.set_tooltip_text(Some("Refresh inferiors and fork settings"));
    header.append(&heading);
    header.append(&refresh);
    page.append(&header);

    let navigation = gtk::Box::new(gtk::Orientation::Vertical, 5);
    navigation.add_css_class("inferior-navigation");
    navigation.append(&section_title("RELATIONSHIP NAVIGATION"));
    let navigation_actions = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    navigation_actions.set_homogeneous(true);
    let switch_parent = gtk::Button::with_label("Switch parent");
    let switch_child = gtk::Button::with_label("Switch child");
    for button in [&switch_parent, &switch_child] {
        button.add_css_class("inferior-inline-action");
        button.set_sensitive(false);
        navigation_actions.append(button);
    }
    navigation.append(&navigation_actions);
    page.append(&navigation);

    let policy = gtk::Box::new(gtk::Orientation::Vertical, 5);
    policy.add_css_class("inferior-policy");
    policy.append(&section_title("FORK POLICY"));
    let follow = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    follow.set_homogeneous(true);
    let follow_parent = gtk::ToggleButton::with_label("Follow parent");
    let follow_child = gtk::ToggleButton::with_label("Follow child");
    follow_child.set_group(Some(&follow_parent));
    follow_parent.add_css_class("inferior-policy-choice");
    follow_child.add_css_class("inferior-policy-choice");
    follow_parent.set_sensitive(false);
    follow_child.set_sensitive(false);
    follow.append(&follow_parent);
    follow.append(&follow_child);
    policy.append(&follow);
    let detach_on_fork = gtk::CheckButton::with_label("Detach the process not being followed");
    detach_on_fork.add_css_class("inferior-detach-policy");
    detach_on_fork.set_sensitive(false);
    detach_on_fork.set_tooltip_text(Some(
        "Turn this off to retain both parent and child as GDB inferiors",
    ));
    policy.append(&detach_on_fork);
    page.append(&policy);

    let list_title = section_title("INFERIORS");
    list_title.add_css_class("inferior-list-title");
    page.append(&list_title);
    let list = dynamic_list("Inferiors appear when GDB reports thread groups");
    list.add_css_class("inferior-list");
    let scrolled = gtk::ScrolledWindow::builder()
        .child(&list)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build();
    scrolled.add_css_class("inferior-list-scroll");
    page.append(&scrolled);

    InferiorControls {
        summary,
        page,
        selector,
        selector_model,
        selector_ids: Rc::new(RefCell::new(Vec::new())),
        selector_updating: Rc::new(Cell::new(false)),
        selected_state,
        stop_owner,
        list,
        cards: Rc::new(RefCell::new(Vec::new())),
        follow_parent,
        follow_child,
        detach_on_fork,
        switch_parent,
        switch_child,
        refresh,
        action_handler: Rc::new(RefCell::new(None)),
    }
}

pub(super) fn build_source_tree_view() -> SourceTreeControls {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.add_css_class("source-tree-panel");
    let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 2);
    toolbar.add_css_class("source-tree-toolbar");
    let search = source_search_entry("Filter source files");
    search.set_hexpand(true);
    let refresh = gtk::Button::from_icon_name("view-refresh-symbolic");
    refresh.add_css_class("source-tree-refresh");
    refresh.set_halign(gtk::Align::End);
    refresh.set_tooltip_text(Some("Refresh source tree"));
    toolbar.append(&search);
    toolbar.append(&refresh);
    root.append(&toolbar);

    let status = gtk::Label::new(Some("Open Sources to index the source tree"));
    status.add_css_class("source-tree-status");
    status.set_halign(gtk::Align::Start);
    status.set_ellipsize(pango::EllipsizeMode::End);
    root.append(&status);

    let roots = gio::ListStore::new::<glib::BoxedAnyObject>();
    let model = gtk::TreeListModel::new(roots.clone(), false, false, |item| {
        let item = item.downcast_ref::<glib::BoxedAnyObject>()?;
        let node = item.borrow::<SourceTreeNode>();
        if node.data.children.is_empty() {
            return None;
        }
        let children = gio::ListStore::new::<glib::BoxedAnyObject>();
        for child in &node.data.children {
            children.append(&glib::BoxedAnyObject::new(SourceTreeNode {
                data: Arc::new(child.clone()),
            }));
        }
        Some(children.upcast())
    });
    let selection = gtk::SingleSelection::new(Some(model.clone()));
    selection.set_autoselect(false);
    selection.set_can_unselect(true);
    let open_handler = Rc::new(RefCell::new(None::<SourceTreePathHandler>));
    let search_handler = Rc::new(RefCell::new(None::<SourceTreePathHandler>));
    let refresh_handler = Rc::new(RefCell::new(None::<SourceTreeRefreshHandler>));
    let factory = gtk::SignalListItemFactory::new();
    let open_for_bind = Rc::clone(&open_handler);
    let search_for_bind = Rc::clone(&search_handler);
    let refresh_for_bind = Rc::clone(&refresh_handler);
    factory.connect_bind(move |_, object| {
        let Some(item) = object.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(tree_row) = item.item().and_downcast::<gtk::TreeListRow>() else {
            return;
        };
        let Some(data) = tree_row.item().and_downcast::<glib::BoxedAnyObject>() else {
            return;
        };
        let node = data.borrow::<SourceTreeNode>().clone();
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        row.add_css_class("source-tree-row");
        row.set_tooltip_text(Some(&node.data.path.display().to_string()));
        let disclosure = gtk::Label::new(Some(if node.data.directory {
            if tree_row.is_expanded() {
                DISCLOSURE_EXPANDED_ICON
            } else {
                DISCLOSURE_COLLAPSED_ICON
            }
        } else {
            ""
        }));
        disclosure.add_css_class("source-tree-disclosure");
        disclosure.set_width_chars(1);
        if node.data.directory {
            tree_row
                .bind_property("expanded", &disclosure, "label")
                .transform_to(|_, expanded: bool| {
                    Some(if expanded {
                        DISCLOSURE_EXPANDED_ICON
                    } else {
                        DISCLOSURE_COLLAPSED_ICON
                    })
                })
                .sync_create()
                .build();
        }
        row.append(&disclosure);
        let icon = gtk::Image::from_icon_name(if node.data.directory {
            "folder-symbolic"
        } else {
            "text-x-generic-symbolic"
        });
        icon.add_css_class("source-tree-icon");
        row.append(&icon);
        let name = gtk::Label::new(Some(&node.data.name));
        name.add_css_class("source-tree-name");
        name.set_halign(gtk::Align::Start);
        name.set_xalign(0.0);
        name.set_hexpand(true);
        name.set_ellipsize(pango::EllipsizeMode::End);
        row.append(&name);
        let loaded = gtk::Label::new(Some("●"));
        loaded.add_css_class("source-tree-loaded");
        loaded.set_tooltip_text(Some(if node.data.directory {
            "Contains source files known to GDB"
        } else {
            "Source file known to GDB"
        }));
        loaded.set_visible(node.data.loaded);
        row.append(&loaded);
        connect_source_tree_context_menu(
            &row,
            &node,
            &open_for_bind,
            &search_for_bind,
            &refresh_for_bind,
        );
        let expander = gtk::TreeExpander::new();
        expander.set_list_row(Some(&tree_row));
        expander.set_hide_expander(true);
        expander.set_indent_for_icon(false);
        expander.set_child(Some(&row));
        item.set_child(Some(&expander));
    });
    factory.connect_unbind(|_, object| {
        if let Some(item) = object.downcast_ref::<gtk::ListItem>() {
            item.set_child(None::<&gtk::Widget>);
        }
    });
    let view = gtk::ListView::new(Some(selection.clone()), Some(factory));
    view.add_css_class("source-tree-view");
    view.set_single_click_activate(true);
    let model_for_activate = model.clone();
    let open_for_activate = Rc::clone(&open_handler);
    view.connect_activate(move |_, position| {
        let Some(row) = model_for_activate
            .item(position)
            .and_downcast::<gtk::TreeListRow>()
        else {
            return;
        };
        let Some(item) = row.item().and_downcast::<glib::BoxedAnyObject>() else {
            return;
        };
        let node = item.borrow::<SourceTreeNode>();
        if node.data.directory {
            row.set_expanded(!row.is_expanded());
        } else {
            let handler = open_for_activate.borrow().clone();
            if let Some(handler) = handler {
                handler(node.data.path.clone());
            }
        }
    });
    let refresh_for_click = Rc::clone(&refresh_handler);
    refresh.connect_clicked(move |_| {
        let handler = refresh_for_click.borrow().clone();
        if let Some(handler) = handler {
            handler();
        }
    });
    let scrolled = gtk::ScrolledWindow::builder()
        .child(&view)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build();
    root.append(&scrolled);
    SourceTreeControls {
        root,
        search,
        status,
        roots,
        model,
        selection,
        view,
        open_handler,
        search_handler,
        refresh_handler,
    }
}

fn connect_source_tree_context_menu(
    row: &gtk::Box,
    node: &SourceTreeNode,
    open_handler: &Rc<RefCell<Option<SourceTreePathHandler>>>,
    search_handler: &Rc<RefCell<Option<SourceTreePathHandler>>>,
    refresh_handler: &Rc<RefCell<Option<SourceTreeRefreshHandler>>>,
) {
    let popover = gtk::Popover::new();
    popover.add_css_class("source-tree-menu");
    popover.set_autohide(true);
    popover.set_has_arrow(false);
    popover.set_parent(row);
    let menu = gtk::Box::new(gtk::Orientation::Vertical, 1);
    let open = source_tree_menu_button("Open");
    open.set_sensitive(!node.data.directory);
    let search = source_tree_menu_button("Search within folder");
    let copy_name = source_tree_menu_button("Copy name");
    let copy_path = source_tree_menu_button("Copy full path");
    let refresh = source_tree_menu_button("Refresh source tree");
    for button in [&open, &search, &copy_name, &copy_path, &refresh] {
        menu.append(button);
    }
    popover.set_child(Some(&menu));
    let path = node.data.path.clone();
    let open_handler = Rc::clone(open_handler);
    let popover_for_open = popover.clone();
    open.connect_clicked(move |_| {
        let handler = open_handler.borrow().clone();
        if let Some(handler) = handler {
            handler(path.clone());
        }
        popover_for_open.popdown();
    });
    let directory = if node.data.directory {
        Some(node.data.path.clone())
    } else {
        node.data.path.parent().map(Path::to_path_buf)
    };
    search.set_sensitive(directory.is_some());
    let search_handler = Rc::clone(search_handler);
    let popover_for_search = popover.clone();
    search.connect_clicked(move |_| {
        let handler = search_handler.borrow().clone();
        if let (Some(directory), Some(handler)) = (directory.as_ref(), handler) {
            handler(directory.clone());
        }
        popover_for_search.popdown();
    });
    let name = node.data.name.clone();
    let popover_for_name = popover.clone();
    copy_name.connect_clicked(move |_| {
        if let Some(display) = gtk::gdk::Display::default() {
            display.clipboard().set_text(&name);
        }
        popover_for_name.popdown();
    });
    let path = node.data.path.display().to_string();
    let popover_for_path = popover.clone();
    copy_path.connect_clicked(move |_| {
        if let Some(display) = gtk::gdk::Display::default() {
            display.clipboard().set_text(&path);
        }
        popover_for_path.popdown();
    });
    let refresh_handler = Rc::clone(refresh_handler);
    let popover_for_refresh = popover.clone();
    refresh.connect_clicked(move |_| {
        let handler = refresh_handler.borrow().clone();
        if let Some(handler) = handler {
            handler();
        }
        popover_for_refresh.popdown();
    });
    let gesture = gtk::GestureClick::new();
    gesture.set_button(gtk::gdk::BUTTON_SECONDARY);
    let popover_for_click = popover.downgrade();
    gesture.connect_pressed(move |gesture, _, x, y| {
        let Some(popover) = popover_for_click.upgrade() else {
            return;
        };
        popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(
            x.round() as i32,
            y.round() as i32,
            1,
            1,
        )));
        gesture.set_state(gtk::EventSequenceState::Claimed);
        popover.popup();
    });
    row.add_controller(gesture);
}

fn source_tree_menu_button(text: &str) -> gtk::Button {
    let label = gtk::Label::new(Some(text));
    label.set_halign(gtk::Align::Start);
    label.set_xalign(0.0);
    gtk::Button::builder()
        .child(&label)
        .hexpand(true)
        .css_classes(["source-tree-menu-action"])
        .build()
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
