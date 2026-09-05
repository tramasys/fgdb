use super::*;

#[derive(Clone)]
struct SourceGutterMenuHandlers {
    jump: Rc<RefCell<Option<SourceJumpHandler>>>,
    enabled: Rc<RefCell<Option<BreakpointEnabledHandler>>>,
    delete: Rc<RefCell<Option<StringSelectionHandler>>>,
}

pub(super) fn dynamic_list(empty_text: &str) -> gtk::Box {
    let list = gtk::Box::new(gtk::Orientation::Vertical, 1);
    list.append(&empty_label(empty_text));

    list
}

pub(super) fn build_signal_grid(
    signals: &'static [(&'static str, &'static str)],
) -> (gtk::Grid, Vec<(gtk::Button, &'static str, &'static str)>) {
    let grid = gtk::Grid::builder()
        .column_homogeneous(true)
        .column_spacing(2)
        .row_spacing(2)
        .hexpand(true)
        .build();

    let buttons = signals
        .iter()
        .enumerate()
        .map(|(index, &(signal, description))| {
            let label = if signal == "all" {
                "ALL SIGNALS"
            } else {
                signal
            };

            let button = gtk::Button::with_label(label);
            button.add_css_class("signal-action");
            button.add_css_class("catchpoint-action");
            button.set_halign(gtk::Align::Fill);
            button.set_hexpand(true);

            button.set_tooltip_text(Some(&format!(
                "{description}\nClick to add a GDB signal catchpoint"
            )));

            grid.attach(&button, (index % 3) as i32, (index / 3) as i32, 1, 1);

            (button, signal, description)
        })
        .collect();

    (grid, buttons)
}

pub(super) fn empty_label(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.add_css_class("muted");
    label.set_halign(gtk::Align::Start);
    label.set_wrap(true);
    label.set_margin_start(4);
    label.set_margin_top(3);

    label
}

pub(super) fn clear_box(container: &gtk::Box) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
}

pub(super) fn replace_boxed_store<T: 'static>(
    store: &gio::ListStore,
    values: impl IntoIterator<Item = T>,
) {
    let values = values
        .into_iter()
        .map(glib::BoxedAnyObject::new)
        .collect::<Vec<_>>();

    store.splice(0, store.n_items(), &values);
}

pub(super) fn replace_boxed_store_if_changed<T: PartialEq + 'static>(
    store: &gio::ListStore,
    values: impl IntoIterator<Item = T>,
) -> bool {
    let values = values.into_iter().collect::<Vec<_>>();
    let old_len = usize::try_from(store.n_items()).unwrap_or(usize::MAX);

    if old_len == values.len() {
        let mut changed = false;
        let mut run_start = None;
        let mut replacements = Vec::new();

        for (index, value) in values.into_iter().enumerate() {
            if boxed_store_item_equals(store, index, &value) {
                if let Some(start) = run_start.take() {
                    store.splice(
                        u32::try_from(start).unwrap_or(u32::MAX),
                        u32::try_from(replacements.len()).unwrap_or(u32::MAX),
                        &replacements,
                    );

                    replacements.clear();
                }
            } else {
                changed = true;
                run_start.get_or_insert(index);
                replacements.push(glib::BoxedAnyObject::new(value));
            }
        }

        if let Some(start) = run_start {
            store.splice(
                u32::try_from(start).unwrap_or(u32::MAX),
                u32::try_from(replacements.len()).unwrap_or(u32::MAX),
                &replacements,
            );
        }

        return changed;
    }

    let common_len = old_len.min(values.len());

    let prefix = values
        .iter()
        .take(common_len)
        .enumerate()
        .take_while(|(index, value)| boxed_store_item_equals(store, *index, *value))
        .count();

    let suffix = values
        .iter()
        .enumerate()
        .rev()
        .take(common_len.saturating_sub(prefix))
        .take_while(|(index, value)| {
            let old_index = old_len - (values.len() - *index);

            boxed_store_item_equals(store, old_index, *value)
        })
        .count();

    let new_middle_len = values.len().saturating_sub(prefix + suffix);
    let old_middle_len = old_len.saturating_sub(prefix + suffix);

    let replacements = values
        .into_iter()
        .skip(prefix)
        .take(new_middle_len)
        .map(glib::BoxedAnyObject::new)
        .collect::<Vec<_>>();

    store.splice(
        u32::try_from(prefix).unwrap_or(u32::MAX),
        u32::try_from(old_middle_len).unwrap_or(u32::MAX),
        &replacements,
    );

    true
}

fn boxed_store_item_equals<T: PartialEq + 'static>(
    store: &gio::ListStore,
    index: usize,
    value: &T,
) -> bool {
    store
        .item(u32::try_from(index).unwrap_or(u32::MAX))
        .and_downcast::<glib::BoxedAnyObject>()
        .is_some_and(|item| *item.borrow::<T>() == *value)
}

pub(super) fn update_selected_frame_buttons(buttons: &[(u32, gtk::Button)], selected: u32) {
    for (level, button) in buttons {
        if *level == selected {
            button.add_css_class("current-debug-item");
        } else {
            button.remove_css_class("current-debug-item");
        }
    }
}

#[derive(Clone, Copy)]
enum SourceTabCloseScope {
    This,
    Others,
    Left,
    Right,
    All,
}

fn source_pages_for_close(
    notebook: &gtk::Notebook,
    documents: &Rc<RefCell<Vec<SourceDocument>>>,
    anchor: &gtk::ScrolledWindow,
    scope: SourceTabCloseScope,
) -> Vec<gtk::ScrolledWindow> {
    let Some(anchor_page) = notebook.page_num(anchor) else {
        return Vec::new();
    };

    documents
        .borrow()
        .iter()
        .filter_map(|document| {
            let page = notebook.page_num(&document.page)?;

            let selected = match scope {
                SourceTabCloseScope::This => document.page == *anchor,
                SourceTabCloseScope::Others => document.page != *anchor,
                SourceTabCloseScope::Left => page < anchor_page,
                SourceTabCloseScope::Right => page > anchor_page,
                SourceTabCloseScope::All => true,
            };

            selected.then(|| document.page.clone())
        })
        .collect()
}

fn close_source_pages(
    notebook: &gtk::Notebook,
    documents: &Rc<RefCell<Vec<SourceDocument>>>,
    pages: &[gtk::ScrolledWindow],
    style_scheme: Option<&sourceview5::StyleScheme>,
    closed_tabs: &Rc<RefCell<Vec<ClosedSourceTab>>>,
    reopen_closed: &gtk::Button,
) {
    if pages.is_empty() {
        return;
    }

    let closed = documents
        .borrow()
        .iter()
        .filter(|document| pages.contains(&document.page))
        .map(|document| ClosedSourceTab {
            path: document.path.clone(),
            line: u32::try_from(
                document
                    .buffer
                    .iter_at_offset(document.buffer.cursor_position())
                    .line()
                    .saturating_add(1),
            )
            .unwrap_or(1),
        })
        .collect::<Vec<_>>();

    if !closed.is_empty() {
        let mut history = closed_tabs.borrow_mut();
        history.extend(closed);

        if history.len() > 32 {
            let remove = history.len() - 32;
            history.drain(..remove);
        }

        reopen_closed.set_sensitive(true);
    }

    for page in pages {
        if let Some(page_number) = notebook.page_num(page) {
            notebook.remove_page(Some(page_number));
        }
    }

    let empty = {
        let mut documents = documents.borrow_mut();
        documents.retain(|document| !pages.contains(&document.page));

        documents.is_empty()
    };

    if empty {
        append_welcome_source(notebook, style_scheme);
    }
}

fn source_tab_menu_button(label: &str) -> gtk::Button {
    let label = gtk::Label::new(Some(label));
    label.set_xalign(0.0);
    let button = gtk::Button::builder().child(&label).hexpand(true).build();
    button.add_css_class("source-tab-menu-action");

    button
}

fn copy_source_text(text: &str) {
    if let Some(display) = gtk::gdk::Display::default() {
        display.clipboard().set_text(text);
    }
}

fn connect_source_tab_context_menu(
    document: &SourceDocument,
    notebook: &gtk::Notebook,
    documents: &Rc<RefCell<Vec<SourceDocument>>>,
    style_scheme: Option<&sourceview5::StyleScheme>,
    closed_tabs: &Rc<RefCell<Vec<ClosedSourceTab>>>,
    reopen_closed: &gtk::Button,
) {
    let popover = gtk::Popover::new();
    popover.add_css_class("source-tab-menu");
    popover.set_autohide(true);
    popover.set_has_arrow(false);
    popover.set_parent(&document.tab);
    let menu = gtk::Box::new(gtk::Orientation::Vertical, 1);
    menu.add_css_class("source-tab-menu-content");
    let close = source_tab_menu_button("Close");
    let close_others = source_tab_menu_button("Close Other Tabs");
    let close_left = source_tab_menu_button("Close Tabs to the Left");
    let close_right = source_tab_menu_button("Close Tabs to the Right");
    let close_all = source_tab_menu_button("Close All Tabs");

    for button in [&close, &close_others, &close_left, &close_right, &close_all] {
        menu.append(button);
    }

    let separator = gtk::Separator::new(gtk::Orientation::Horizontal);
    separator.add_css_class("source-tab-menu-separator");
    menu.append(&separator);
    let copy_name = source_tab_menu_button("Copy File Name");
    let copy_path = source_tab_menu_button("Copy Full Path");
    let copy_path_line = source_tab_menu_button("Copy Path with Line");
    let copy_directory = source_tab_menu_button("Copy Directory Path");

    for button in [&copy_name, &copy_path, &copy_path_line, &copy_directory] {
        menu.append(button);
    }

    popover.set_child(Some(&menu));

    for (button, scope) in [
        (&close, SourceTabCloseScope::This),
        (&close_others, SourceTabCloseScope::Others),
        (&close_left, SourceTabCloseScope::Left),
        (&close_right, SourceTabCloseScope::Right),
        (&close_all, SourceTabCloseScope::All),
    ] {
        let notebook = notebook.clone();
        let documents = Rc::clone(documents);
        let page = document.page.clone();
        let style_scheme = style_scheme.cloned();
        let closed_tabs = Rc::clone(closed_tabs);
        let reopen_closed = reopen_closed.clone();
        let popover = popover.downgrade();

        button.connect_clicked(move |_| {
            let pages = source_pages_for_close(&notebook, &documents, &page, scope);

            if let Some(popover) = popover.upgrade() {
                popover.popdown();
            }

            close_source_pages(
                &notebook,
                &documents,
                &pages,
                style_scheme.as_ref(),
                &closed_tabs,
                &reopen_closed,
            );
        });
    }

    let path = document.path.clone();
    let popover_for_name = popover.downgrade();

    copy_name.connect_clicked(move |_| {
        let name = path
            .file_name()
            .unwrap_or(path.as_os_str())
            .to_string_lossy();

        copy_source_text(&name);

        if let Some(popover) = popover_for_name.upgrade() {
            popover.popdown();
        }
    });

    let path = document.path.clone();
    let popover_for_path = popover.downgrade();

    copy_path.connect_clicked(move |_| {
        copy_source_text(&path.to_string_lossy());

        if let Some(popover) = popover_for_path.upgrade() {
            popover.popdown();
        }
    });

    let path = document.path.clone();
    let buffer = document.buffer.clone();
    let popover_for_line = popover.downgrade();

    copy_path_line.connect_clicked(move |_| {
        let line = buffer
            .iter_at_offset(buffer.cursor_position())
            .line()
            .saturating_add(1);

        copy_source_text(&format!("{}:{line}", path.to_string_lossy()));

        if let Some(popover) = popover_for_line.upgrade() {
            popover.popdown();
        }
    });

    let directory = document.path.parent().map(Path::to_path_buf);
    copy_directory.set_sensitive(directory.is_some());
    let popover_for_directory = popover.downgrade();

    copy_directory.connect_clicked(move |_| {
        if let Some(directory) = directory.as_ref() {
            copy_source_text(&directory.to_string_lossy());
        }

        if let Some(popover) = popover_for_directory.upgrade() {
            popover.popdown();
        }
    });

    let gesture = gtk::GestureClick::new();
    gesture.set_button(gtk::gdk::BUTTON_SECONDARY);
    let notebook = notebook.clone();
    let page = document.page.clone();
    let popover_for_click = popover.downgrade();

    gesture.connect_pressed(move |gesture, _, x, y| {
        let Some(page_number) = notebook.page_num(&page) else {
            return;
        };

        notebook.set_current_page(Some(page_number));
        let page_count = notebook.n_pages();
        close_others.set_sensitive(page_count > 1);
        close_left.set_sensitive(page_number > 0);
        close_right.set_sensitive(page_number + 1 < page_count);

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

    document.tab.add_controller(gesture);
}

pub(super) fn open_source_document(
    path: &Path,
    contents: &str,
    context: SourceOpenContext<'_>,
) -> SourceDocument {
    let path = path.to_path_buf();

    if let Some(document) = context
        .documents
        .borrow()
        .iter()
        .find(|document| document.path == path)
        .cloned()
    {
        if let Some(page) = context.notebook.page_num(&document.page) {
            context.notebook.set_current_page(Some(page));
        }

        document.view.grab_focus();
        return document;
    }

    let buffer = build_source_buffer(contents, Some(&path), context.style_scheme);
    let view = build_source_view(&buffer);
    let breakpoint_renderer = build_breakpoint_gutter(&path, context);

    sourceview5::prelude::ViewExt::gutter(&view, gtk::TextWindowType::Left)
        .insert(&breakpoint_renderer, -30);

    let page = gtk::ScrolledWindow::builder()
        .child(&view)
        .hexpand(true)
        .vexpand(true)
        .build();

    connect_breakpoint_gutter_context_click(&page, &view, &breakpoint_renderer);
    let tab = gtk::Box::new(gtk::Orientation::Horizontal, 3);
    tab.add_css_class("source-tab");
    let tab_label = gtk::Label::new(Some(&source_tab_title(&path)));
    tab_label.set_ellipsize(pango::EllipsizeMode::Middle);
    tab_label.set_width_chars(18);
    tab_label.set_max_width_chars(32);
    tab_label.set_tooltip_text(Some(&path.to_string_lossy()));
    let close = gtk::Button::from_icon_name("window-close-symbolic");
    close.add_css_class("source-tab-close");
    close.set_tooltip_text(Some("Close source tab"));
    tab.append(&tab_label);
    tab.append(&close);

    let document = SourceDocument {
        path,
        buffer,
        view,
        page,
        tab,
        tab_label,
        breakpoint_renderer,
    };

    connect_source_symbol_navigation(&document, context.symbol_handler);

    if context.documents.borrow().is_empty() {
        while context.notebook.n_pages() > 0 {
            context.notebook.remove_page(Some(0));
        }
    }

    let page_number = context
        .notebook
        .append_page(&document.page, Some(&document.tab));

    context.notebook.set_tab_reorderable(&document.page, true);
    context.documents.borrow_mut().push(document.clone());

    connect_source_tab_context_menu(
        &document,
        context.notebook,
        context.documents,
        context.style_scheme,
        context.closed_tabs,
        context.reopen_closed,
    );

    let notebook_for_close = context.notebook.clone();
    let documents_for_close = Rc::clone(context.documents);
    let page_for_close = document.page.clone();
    let style_scheme_for_close = context.style_scheme.cloned();
    let closed_tabs_for_close = Rc::clone(context.closed_tabs);
    let reopen_closed_for_close = context.reopen_closed.clone();

    close.connect_clicked(move |_| {
        close_source_pages(
            &notebook_for_close,
            &documents_for_close,
            std::slice::from_ref(&page_for_close),
            style_scheme_for_close.as_ref(),
            &closed_tabs_for_close,
            &reopen_closed_for_close,
        );
    });

    context.notebook.set_current_page(Some(page_number));
    document.view.grab_focus();

    document
}

pub(super) fn build_breakpoint_gutter(
    path: &Path,
    context: SourceOpenContext<'_>,
) -> BreakpointGutterRenderer {
    let SourceOpenContext {
        theme,
        breakpoints,
        source_index,
        insert_handler,
        jump_handler,
        delete_handler,
        enabled_handler,
        ..
    } = context;

    // Source loading has already resolved the canonical path on a worker.
    let source_id_for_data = source::SourceId::from_indexed_path(path);
    let breakpoints_for_data = Rc::clone(breakpoints);
    let source_index_for_data = Rc::clone(source_index);
    let inactive_foreground = gtk::gdk::RGBA::parse(theme.colors.muted).expect("theme color");
    let disabled_foreground = inactive_foreground;
    let disabled_background = gtk::gdk::RGBA::parse(theme.colors.raised).expect("theme color");
    let enabled_foreground = gtk::gdk::RGBA::parse(theme.colors.background).expect("theme color");
    let enabled_background = gtk::gdk::RGBA::parse(theme.colors.success).expect("theme color");
    let execution_foreground = gtk::gdk::RGBA::parse(theme.colors.warning).expect("theme color");
    let path = path.to_path_buf();
    let source_id = source::SourceId::from_indexed_path(&path);
    let breakpoints = Rc::clone(breakpoints);
    let source_index = Rc::clone(source_index);
    let insert_handler = Rc::clone(insert_handler);
    let jump_handler = Rc::clone(jump_handler);
    let delete_handler = Rc::clone(delete_handler);
    let enabled_handler = Rc::clone(enabled_handler);

    let menu_handlers = SourceGutterMenuHandlers {
        jump: jump_handler,
        enabled: enabled_handler,
        delete: Rc::clone(&delete_handler),
    };

    let renderer = BreakpointGutterRenderer::new(
        move |buffer, line| {
            let source_line = line + 1;

            let executing = i32::try_from(line).ok().is_some_and(|line| {
                !buffer
                    .source_marks_at_line(line, Some(EXECUTION_CATEGORY))
                    .is_empty()
            });

            let breakpoint = breakpoints_for_data
                .borrow()
                .iter()
                .find(|breakpoint| {
                    breakpoint.line == Some(source_line)
                        && breakpoint.source_path().is_some_and(|reported| {
                            source::paths_match_id(
                                source_index_for_data.borrow().as_deref(),
                                &source_id_for_data,
                                reported,
                            )
                        })
                })
                .cloned();

            let text = if executing {
                format!("›{source_line:>3}")
            } else {
                format!("{source_line:>4}")
            };

            match breakpoint {
                Some(breakpoint) if breakpoint.enabled => LineStyle {
                    text,
                    foreground: enabled_foreground,
                    background: Some(enabled_background),
                },
                Some(_) => LineStyle {
                    text,
                    foreground: disabled_foreground,
                    background: Some(disabled_background),
                },
                None => LineStyle {
                    text,
                    foreground: if executing {
                        execution_foreground
                    } else {
                        inactive_foreground
                    },
                    background: None,
                },
            }
        },
        move |renderer, iter, area, button| {
            let line = u32::try_from(iter.line() + 1).ok();

            let existing = breakpoints
                .borrow()
                .iter()
                .find(|breakpoint| {
                    breakpoint.line == line
                        && breakpoint.source_path().is_some_and(|reported| {
                            source::paths_match_id(
                                source_index.borrow().as_deref(),
                                &source_id,
                                reported,
                            )
                        })
                })
                .cloned();

            match (button, existing) {
                (1, Some(breakpoint)) => {
                    let handler = delete_handler.borrow().clone();

                    if let Some(handler) = handler {
                        handler(breakpoint.command_number().to_owned());
                    }
                }
                (1, None) => {
                    let handler = insert_handler.borrow().clone();

                    if let (Some(line), Some(handler)) = (line, handler) {
                        handler(path.clone(), line);
                    }
                }
                (3, breakpoint) => {
                    if let Some(line) = line {
                        open_source_gutter_menu(
                            renderer,
                            area,
                            path.clone(),
                            line,
                            breakpoint,
                            menu_handlers.clone(),
                        );
                    }
                }
                _ => {}
            }
        },
    );

    renderer.set_tooltip_text(Some(
        "Left-click to add or delete a breakpoint · Right-click for line actions",
    ));

    renderer
}

pub(super) fn connect_breakpoint_gutter_context_click(
    page: &gtk::ScrolledWindow,
    view: &sourceview5::View,
    renderer: &BreakpointGutterRenderer,
) {
    let right_click = gtk::GestureClick::new();
    right_click.set_button(3);
    right_click.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak_view = view.downgrade();
    let weak_renderer = renderer.downgrade();

    right_click.connect_pressed(move |gesture, _presses, x, y| {
        let (Some(view), Some(renderer)) = (weak_view.upgrade(), weak_renderer.upgrade()) else {
            return;
        };

        if x < 0.0 || x >= f64::from(renderer.width()) {
            return;
        }

        let (_, buffer_y) = view.window_to_buffer_coords(
            gtk::TextWindowType::Left,
            x.round() as i32,
            y.round() as i32,
        );

        let (iter, _) = view.line_at_y(buffer_y);
        let area = gtk::gdk::Rectangle::new(0, y.round() as i32, renderer.width(), 1);
        renderer.activate_at(&iter, &area, 3);
        gesture.set_state(gtk::EventSequenceState::Claimed);
    });

    page.add_controller(right_click);
}

fn open_source_gutter_menu(
    renderer: &BreakpointGutterRenderer,
    area: &gtk::gdk::Rectangle,
    path: PathBuf,
    line: u32,
    breakpoint: Option<Breakpoint>,
    handlers: SourceGutterMenuHandlers,
) {
    let popover = gtk::Popover::builder()
        .has_arrow(false)
        .autohide(true)
        .build();

    let menu = gtk::Box::new(gtk::Orientation::Vertical, 1);
    menu.add_css_class("gutter-breakpoint-menu");

    let title_text = breakpoint.as_ref().map_or_else(
        || format!("LINE {line}"),
        |breakpoint| format!("LINE {line} · BREAKPOINT #{}", breakpoint.command_number()),
    );

    let title = gtk::Label::new(Some(&title_text));
    title.add_css_class("section-title");
    title.set_halign(gtk::Align::Start);
    menu.append(&title);
    let jump = gtk::Button::with_label("Jump to");

    jump.set_tooltip_text(Some(
        "Continue execution until this line and pause without creating a persistent breakpoint",
    ));

    jump.set_halign(gtk::Align::Fill);
    jump.set_hexpand(true);
    jump.set_sensitive(handlers.jump.borrow().is_some());
    menu.append(&jump);
    let jump_handler = Rc::clone(&handlers.jump);
    let popover_for_jump = popover.clone();

    jump.connect_clicked(move |_| {
        let handler = jump_handler.borrow().clone();

        if let Some(handler) = handler {
            handler(path.clone(), line);
        }

        popover_for_jump.popdown();
    });

    if let Some(breakpoint) = breakpoint {
        let separator = gtk::Separator::new(gtk::Orientation::Horizontal);
        menu.append(&separator);

        let toggle = gtk::Button::with_label(if breakpoint.enabled {
            "Disable"
        } else {
            "Enable"
        });

        toggle.set_halign(gtk::Align::Fill);
        toggle.set_hexpand(true);
        let delete = gtk::Button::with_label("Delete");
        delete.set_halign(gtk::Align::Fill);
        delete.set_hexpand(true);
        delete.add_css_class("danger-action");
        menu.append(&toggle);
        menu.append(&delete);
        let number = breakpoint.command_number().to_owned();
        let enable = !breakpoint.enabled;
        let popover_for_toggle = popover.clone();
        let enabled_handler = Rc::clone(&handlers.enabled);

        toggle.connect_clicked(move |_| {
            let handler = enabled_handler.borrow().clone();

            if let Some(handler) = handler {
                handler(number.clone(), enable);
            }

            popover_for_toggle.popdown();
        });

        let number = breakpoint.command_number().to_owned();
        let popover_for_delete = popover.clone();
        let delete_handler = Rc::clone(&handlers.delete);

        delete.connect_clicked(move |_| {
            let handler = delete_handler.borrow().clone();

            if let Some(handler) = handler {
                handler(number.clone());
            }

            popover_for_delete.popdown();
        });
    }

    popover.set_child(Some(&menu));
    popover.set_parent(renderer);
    popover.set_position(gtk::PositionType::Right);
    popover.set_offset(4, 0);
    popover.set_pointing_to(Some(area));
    popover.connect_closed(|popover| popover.unparent());
    popover.popup();
}

pub(super) fn connect_source_symbol_navigation(
    document: &SourceDocument,
    symbol_handler: &Rc<RefCell<Option<StringSelectionHandler>>>,
) {
    let link_tag = gtk::TextTag::builder()
        .name("ctrl-source-link")
        .underline(pango::Underline::Single)
        .build();

    document.buffer.tag_table().add(&link_tag);
    let highlighted_range = Rc::new(RefCell::new(None::<(i32, i32)>));
    let control_pressed = Rc::new(Cell::new(false));
    let pointer_position = Rc::new(Cell::new((0.0, 0.0)));
    let motion = gtk::EventControllerMotion::new();
    let view_for_motion = document.view.clone();
    let buffer_for_motion = document.buffer.clone();
    let tag_for_motion = link_tag.clone();
    let range_for_motion = Rc::clone(&highlighted_range);
    let control_for_motion = Rc::clone(&control_pressed);
    let position_for_motion = Rc::clone(&pointer_position);

    motion.connect_motion(move |controller, x, y| {
        position_for_motion.set((x, y));

        let active = control_for_motion.get()
            || controller
                .current_event_state()
                .contains(gtk::gdk::ModifierType::CONTROL_MASK);

        update_source_link_highlight(
            &view_for_motion,
            &buffer_for_motion,
            &tag_for_motion,
            &range_for_motion,
            x,
            y,
            active,
        );
    });

    let view_for_leave = document.view.clone();
    let buffer_for_leave = document.buffer.clone();
    let tag_for_leave = link_tag.clone();
    let range_for_leave = Rc::clone(&highlighted_range);

    motion.connect_leave(move |_| {
        clear_source_link_highlight(
            &view_for_leave,
            &buffer_for_leave,
            &tag_for_leave,
            &range_for_leave,
        );
    });

    document.view.add_controller(motion);
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let view_for_press = document.view.clone();
    let buffer_for_press = document.buffer.clone();
    let tag_for_press = link_tag.clone();
    let range_for_press = Rc::clone(&highlighted_range);
    let control_for_press = Rc::clone(&control_pressed);
    let position_for_press = Rc::clone(&pointer_position);

    keys.connect_key_pressed(move |_, key, _, _| {
        if matches!(key, gtk::gdk::Key::Control_L | gtk::gdk::Key::Control_R) {
            control_for_press.set(true);
            let (x, y) = position_for_press.get();

            update_source_link_highlight(
                &view_for_press,
                &buffer_for_press,
                &tag_for_press,
                &range_for_press,
                x,
                y,
                true,
            );
        }

        gtk::glib::Propagation::Proceed
    });

    let view_for_release = document.view.clone();
    let buffer_for_release = document.buffer.clone();
    let tag_for_release = link_tag;
    let range_for_release = Rc::clone(&highlighted_range);

    keys.connect_key_released(move |_, key, _, _| {
        if matches!(key, gtk::gdk::Key::Control_L | gtk::gdk::Key::Control_R) {
            control_pressed.set(false);

            clear_source_link_highlight(
                &view_for_release,
                &buffer_for_release,
                &tag_for_release,
                &range_for_release,
            );
        }
    });

    document.view.add_controller(keys);
    let gesture = gtk::GestureClick::new();
    gesture.set_button(1);
    gesture.set_propagation_phase(gtk::PropagationPhase::Capture);
    let view = document.view.clone();
    let buffer = document.buffer.clone();
    let symbol_handler = Rc::clone(symbol_handler);

    gesture.connect_pressed(move |gesture, _, x, y| {
        if !gesture
            .current_event_state()
            .contains(gtk::gdk::ModifierType::CONTROL_MASK)
        {
            return;
        }

        let (x, y) = view.window_to_buffer_coords(
            gtk::TextWindowType::Widget,
            x.round() as i32,
            y.round() as i32,
        );

        let Some(iter) = view.iter_at_location(x, y) else {
            return;
        };

        let Some(symbol) = source_symbol_at_iter(&buffer, &iter) else {
            return;
        };

        gesture.set_state(gtk::EventSequenceState::Claimed);
        let handler = symbol_handler.borrow().clone();

        if let Some(handler) = handler {
            handler(symbol);
        }
    });

    document.view.add_controller(gesture);
}

pub(super) fn source_symbol_at_iter(
    buffer: &sourceview5::Buffer,
    iter: &gtk::TextIter,
) -> Option<String> {
    source_symbol_span_at_iter(buffer, iter).map(|(symbol, _, _)| symbol)
}

pub(super) fn source_symbol_span_at_iter(
    buffer: &sourceview5::Buffer,
    iter: &gtk::TextIter,
) -> Option<(String, usize, usize)> {
    if ["comment", "string"]
        .iter()
        .any(|context| buffer.iter_has_context_class(iter, context))
    {
        return None;
    }

    let start = buffer.iter_at_line(iter.line())?;
    let mut end = start;
    end.forward_to_line_end();
    let line = buffer.text(&start, &end, false);

    source_symbol_span_at_offset(&line, usize::try_from(iter.line_offset()).ok()?)
}

#[cfg(test)]
pub(super) fn source_symbol_at_offset(line: &str, offset: usize) -> Option<String> {
    source_symbol_span_at_offset(line, offset).map(|(symbol, _, _)| symbol)
}

pub(super) fn source_symbol_span_at_offset(
    line: &str,
    offset: usize,
) -> Option<(String, usize, usize)> {
    let character_count = line.chars().count();
    let offset = offset.min(character_count);

    let byte_offset = line
        .char_indices()
        .nth(offset)
        .map_or(line.len(), |(index, _)| index);

    let is_symbol_character =
        |character: char| character.is_alphanumeric() || matches!(character, '_' | ':' | '$' | '~');

    if byte_offset < line.len()
        && !line[byte_offset..]
            .chars()
            .next()
            .is_some_and(is_symbol_character)
    {
        return None;
    }

    let mut left_byte = byte_offset;

    for (index, character) in line[..byte_offset].char_indices().rev() {
        if !is_symbol_character(character) {
            break;
        }

        left_byte = index;
    }

    let mut right_byte = byte_offset;

    for (index, character) in line[byte_offset..].char_indices() {
        if !is_symbol_character(character) {
            break;
        }

        right_byte = byte_offset + index + character.len_utf8();
    }

    let syntax_right_byte = right_byte;

    while left_byte < right_byte && line.as_bytes()[left_byte] == b':' {
        left_byte += 1;
    }

    while right_byte > left_byte && line.as_bytes()[right_byte - 1] == b':' {
        right_byte -= 1;
    }

    let symbol = &line[left_byte..right_byte];

    if !symbol
        .chars()
        .next()
        .is_some_and(|character| character.is_alphabetic() || matches!(character, '_' | '~'))
        || !is_callable_source_symbol(symbol, line, syntax_right_byte)
    {
        return None;
    }

    let left = line[..left_byte].chars().count();
    let right = left + symbol.chars().count();

    Some((symbol.to_owned(), left, right))
}

pub(super) fn is_callable_source_symbol(symbol: &str, line: &str, cursor: usize) -> bool {
    const NON_CALL_KEYWORDS: &[&str] = &[
        "if", "for", "while", "switch", "catch", "match", "loop", "sizeof", "alignof", "_Alignof",
        "typeof", "decltype", "typeid", "return",
    ];

    let name = symbol.rsplit("::").next().unwrap_or(symbol);

    if NON_CALL_KEYWORDS.contains(&name) {
        return false;
    }

    let mut characters = line[cursor..].chars().peekable();

    while characters
        .next_if(|character| character.is_whitespace())
        .is_some()
    {}
    if characters.next_if_eq(&'<').is_some() {
        let mut depth = 0_u32;

        for character in characters.by_ref() {
            match character {
                '<' => depth = depth.saturating_add(1),
                '>' => {
                    if depth == 0 {
                        break;
                    }

                    depth -= 1;
                }
                _ => {}
            }
        }

        if depth != 0 {
            return false;
        }

        while characters
            .next_if(|character| character.is_whitespace())
            .is_some()
        {}
    }

    characters.next() == Some('(')
}

pub(super) fn update_source_link_highlight(
    view: &sourceview5::View,
    buffer: &sourceview5::Buffer,
    tag: &gtk::TextTag,
    highlighted_range: &Rc<RefCell<Option<(i32, i32)>>>,
    x: f64,
    y: f64,
    active: bool,
) {
    if !active {
        clear_source_link_highlight(view, buffer, tag, highlighted_range);
        return;
    }

    let (x, y) = view.window_to_buffer_coords(
        gtk::TextWindowType::Widget,
        x.round() as i32,
        y.round() as i32,
    );

    let Some(iter) = view.iter_at_location(x, y) else {
        clear_source_link_highlight(view, buffer, tag, highlighted_range);
        return;
    };

    let Some((_, start, end)) = source_symbol_span_at_iter(buffer, &iter) else {
        clear_source_link_highlight(view, buffer, tag, highlighted_range);
        return;
    };

    let Some(line_start) = buffer.iter_at_line(iter.line()) else {
        clear_source_link_highlight(view, buffer, tag, highlighted_range);
        return;
    };

    let Ok(start) = i32::try_from(start) else {
        clear_source_link_highlight(view, buffer, tag, highlighted_range);
        return;
    };

    let Ok(end) = i32::try_from(end) else {
        clear_source_link_highlight(view, buffer, tag, highlighted_range);
        return;
    };

    let start = line_start.offset() + start;
    let end = line_start.offset() + end;

    if highlighted_range.borrow().as_ref() == Some(&(start, end)) {
        view.set_cursor_from_name(Some("pointer"));
        return;
    }

    clear_source_link_highlight(view, buffer, tag, highlighted_range);
    let start_iter = buffer.iter_at_offset(start);
    let end_iter = buffer.iter_at_offset(end);
    buffer.apply_tag(tag, &start_iter, &end_iter);
    highlighted_range.replace(Some((start, end)));
    view.set_cursor_from_name(Some("pointer"));
}

pub(super) fn clear_source_link_highlight(
    view: &sourceview5::View,
    buffer: &sourceview5::Buffer,
    tag: &gtk::TextTag,
    highlighted_range: &Rc<RefCell<Option<(i32, i32)>>>,
) {
    if let Some((start, end)) = highlighted_range.take() {
        buffer.remove_tag(
            tag,
            &buffer.iter_at_offset(start),
            &buffer.iter_at_offset(end),
        );
    }

    view.set_cursor_from_name(None);
}

pub(super) fn source_location_score(symbol: &str, location: &SourceLocation) -> u16 {
    let symbol = without_generic_arguments(symbol);
    let function = without_generic_arguments(&location.function);

    let mut score = if function == symbol {
        100
    } else if function == format!("__GI___libc_{symbol}") {
        98
    } else if function == format!("__GI_{symbol}") {
        97
    } else if function == format!("__libc_{symbol}") {
        95
    } else if function == format!("__{symbol}") {
        90
    } else if function.ends_with(&format!("::{symbol}")) {
        85
    } else if function.contains(&symbol) {
        40
    } else if symbol
        .rsplit("::")
        .next()
        .is_some_and(|name| function.ends_with(&format!("::{name}")))
    {
        30
    } else {
        0
    };

    let source_stem = Path::new(&location.file)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or_default();

    if source_stem == symbol || symbol.ends_with(&format!("::{source_stem}")) {
        score += 25;
    } else if source_stem.contains(&symbol) {
        score += 10;
    }

    score
}

pub(super) fn without_generic_arguments(symbol: &str) -> String {
    let mut depth = 0_u32;

    symbol
        .chars()
        .filter(|character| match *character {
            '<' => {
                depth = depth.saturating_add(1);

                false
            }
            '>' if depth > 0 => {
                depth -= 1;

                false
            }
            _ => depth == 0,
        })
        .collect()
}

pub(super) fn compact_function_name(symbol: &str) -> String {
    if symbol.chars().count() <= 56 || !symbol.contains(['<', '>']) {
        return symbol.to_owned();
    }

    let mut compact = String::with_capacity(symbol.len().min(80));
    let mut depth = 0_u32;

    for character in symbol.chars() {
        match character {
            '<' => {
                if depth == 0 {
                    compact.push('<');
                    compact.push('…');
                }

                depth = depth.saturating_add(1);
            }
            '>' if depth > 0 => {
                depth -= 1;

                if depth == 0 {
                    compact.push('>');
                }
            }
            _ if depth == 0 => compact.push(character),
            _ => {}
        }
    }

    if depth == 0 {
        compact
    } else {
        symbol.to_owned()
    }
}

pub(super) fn scroll_source_document(document: &SourceDocument, line: u32) {
    let Ok(line) = i32::try_from(line.saturating_sub(1)) else {
        return;
    };

    let Some(mut iter) = document.buffer.iter_at_line(line) else {
        return;
    };

    document.buffer.place_cursor(&iter);
    let view = document.view.clone();

    gtk::glib::idle_add_local_once(move || {
        view.scroll_to_iter(&mut iter, 0.15, true, 0.0, 0.35);
    });
}

pub(super) fn source_tab_title(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("source")
        .to_owned()
}

#[cfg(test)]
mod model_update_tests {
    use super::*;

    fn values(store: &gio::ListStore) -> Vec<u32> {
        (0..store.n_items())
            .map(|index| {
                *store
                    .item(index)
                    .and_downcast::<glib::BoxedAnyObject>()
                    .unwrap()
                    .borrow::<u32>()
            })
            .collect()
    }

    #[test]
    fn changed_store_updates_preserve_equal_objects() {
        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        assert!(replace_boxed_store_if_changed(&store, [1_u32, 2, 3]));
        let first = store.item(0).unwrap();
        let last = store.item(2).unwrap();
        assert!(replace_boxed_store_if_changed(&store, [1_u32, 20, 3]));
        assert_eq!(values(&store), [1, 20, 3]);
        assert_eq!(store.item(0).as_ref(), Some(&first));
        assert_eq!(store.item(2).as_ref(), Some(&last));
        assert!(!replace_boxed_store_if_changed(&store, [1_u32, 20, 3]));
        assert!(replace_boxed_store_if_changed(&store, [1_u32, 3]));
        assert_eq!(values(&store), [1, 3]);
        assert_eq!(store.item(0).as_ref(), Some(&first));
        assert_eq!(store.item(1).as_ref(), Some(&last));
    }
}
