use super::*;

pub(super) fn build_topbar(
    config: &LaunchConfig,
    window: &gtk::ApplicationWindow,
    terminal: &vte4::Terminal,
) -> Topbar {
    let topbar = gtk::HeaderBar::new();
    topbar.add_css_class("topbar");
    topbar.set_show_title_buttons(false);

    let title_group = gtk::Box::new(gtk::Orientation::Horizontal, 5);
    title_group.add_css_class("titlebar-identity");
    let title = gtk::Label::new(Some("fgdb"));
    title.add_css_class("app-title");
    title_group.append(&title);
    let title_separator = gtk::Label::new(Some("·"));
    title_separator.add_css_class("muted");
    title_group.append(&title_separator);
    let target = gtk::Label::new(Some(config.target_name()));
    target.add_css_class("target-label");
    target.set_ellipsize(pango::EllipsizeMode::Middle);
    target.set_max_width_chars(32);
    target.set_tooltip_text(Some(config.target_name()));
    title_group.append(&target);
    topbar.set_title_widget(Some(&title_group));

    let leading = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    leading.add_css_class("titlebar-actions");
    let open_source = gtk::Button::with_label("Open source");
    open_source.add_css_class("toolbar-action");
    open_source.set_tooltip_text(Some("Open one or more source files in editor tabs"));
    leading.append(&open_source);
    let load_symbols = gtk::Button::with_label("Load libs");
    load_symbols.add_css_class("toolbar-action");
    load_symbols.set_tooltip_text(Some(
        "Load symbols for shared libraries (useful when auto-solib-add is off)",
    ));
    load_symbols.set_sensitive(false);
    leading.append(&load_symbols);
    let terminal_toggle = gtk::ToggleButton::with_label("Terminal");
    terminal_toggle.add_css_class("toolbar-action");
    terminal_toggle.add_css_class("toolbar-toggle");
    terminal_toggle.set_active(true);
    terminal_toggle.set_tooltip_text(Some("Show or hide the interactive GDB terminal"));
    leading.append(&terminal_toggle);
    let gef_tools = build_gef_tools_menu(terminal, &terminal_toggle);
    leading.append(&gef_tools);
    topbar.pack_start(&leading);

    let controls = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    let run = control_button("Run", "Start or continue the inferior · F5", true);
    let pause = control_button("Pause", "Interrupt the inferior · F6", false);
    let next = control_button("Next", "Step over the current source line · F10", false);
    let step = control_button("Step", "Step into the current source line · F11", false);
    let next_instruction = control_button(
        "Nexti",
        "Execute one machine instruction, stepping over calls · Ctrl+F10",
        false,
    );
    let step_instruction = control_button(
        "Stepi",
        "Execute one machine instruction, stepping into calls · Ctrl+F11",
        false,
    );
    let finish = control_button(
        "Finish",
        "Run until the current function returns · Shift+F11",
        false,
    );
    let until_popover = gtk::Popover::new();
    let until_menu = gtk::Box::new(gtk::Orientation::Vertical, 1);
    until_menu.add_css_class("until-menu");
    until_menu.append(&section_title("RUN UNTIL"));
    let until_actions = [
        ("Current line", "-exec-until"),
        ("Function returns", "-exec-finish"),
        ("Next call", "exec-until call"),
        ("Next return", "exec-until ret"),
        ("Next syscall", "exec-until syscall"),
        ("Next indirect branch", "exec-until indirect-branch"),
        ("Next call / jump / return", "exec-until all-branch"),
        ("Memory access", "exec-until memaccess"),
        ("User code", "exec-until user-code"),
        ("libc code", "exec-until libc-code"),
        ("Region change", "exec-until region-change"),
    ]
    .into_iter()
    .map(|(label, command)| {
        let button = gtk::Button::new();
        let label = gtk::Label::new(Some(label));
        label.set_halign(gtk::Align::Start);
        label.set_hexpand(true);
        button.set_child(Some(&label));
        button.set_halign(gtk::Align::Fill);
        until_menu.append(&button);
        (button, command)
    })
    .collect::<Vec<_>>();
    until_menu.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    let until_condition_entry = gtk::Entry::builder()
        .placeholder_text("$rax == 0")
        .hexpand(true)
        .build();
    until_condition_entry.set_tooltip_text(Some("GDB expression used by GEF exec-until cond"));
    until_menu.append(&until_condition_entry);
    let until_condition_button = gtk::Button::with_label("Expression");
    until_condition_button.add_css_class("inline-action");
    until_menu.append(&until_condition_button);
    until_popover.set_child(Some(&until_menu));
    let until = header_popup_button("Until", &until_popover);
    until.add_css_class("debug-control");
    until.set_tooltip_text(Some("Run until a selected control-flow or memory event"));
    until.set_sensitive(false);
    controls.append(&run);
    controls.append(&pause);
    controls.append(&next);
    controls.append(&step);
    controls.append(&next_instruction);
    controls.append(&step_instruction);
    controls.append(&finish);
    controls.append(&until);
    let status = gtk::Label::new(Some("Starting GDB"));
    status.add_css_class("status-readout");
    let trailing = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    trailing.add_css_class("titlebar-actions");
    trailing.append(&controls);
    trailing.append(&status);
    let window_controls = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    window_controls.add_css_class("window-controls");

    let minimize = window_control_button("−", "Minimize", "minimize");
    let maximize = window_control_button("□", "Maximize or restore", "maximize");
    let close = window_control_button("×", "Close", "close");

    let controlled_window = window.clone();
    minimize.connect_clicked(move |_| controlled_window.minimize());
    let controlled_window = window.clone();
    maximize.connect_clicked(move |_| {
        if controlled_window.is_maximized() {
            controlled_window.unmaximize();
        } else {
            controlled_window.maximize();
        }
    });
    let controlled_window = window.clone();
    close.connect_clicked(move |_| controlled_window.close());

    window_controls.append(&minimize);
    window_controls.append(&maximize);
    window_controls.append(&close);
    trailing.append(&window_controls);
    topbar.pack_end(&trailing);

    Topbar {
        root: topbar,
        open_source_button: open_source,
        load_symbols_button: load_symbols,
        terminal_toggle_button: terminal_toggle,
        run_button: run,
        pause_button: pause,
        next_button: next,
        step_button: step,
        next_instruction_button: next_instruction,
        step_instruction_button: step_instruction,
        finish_button: finish,
        until_button: until,
        until_popover,
        gef_tools_button: gef_tools,
        until_actions,
        until_condition_entry,
        until_condition_button,
        status_label: status,
    }
}

pub(super) fn build_gef_tools_menu(
    terminal: &vte4::Terminal,
    terminal_toggle: &gtk::ToggleButton,
) -> gtk::ToggleButton {
    let popover = gtk::Popover::new();
    let menu = gtk::Box::new(gtk::Orientation::Vertical, 0);
    menu.add_css_class("gef-tools-menu");
    menu.append(&section_title("GEF / LOW-LEVEL TOOLS"));
    let tools = gtk::Notebook::new();
    tools.add_css_class("gef-tools-tabs");
    for (title, commands) in [
        (
            "Context",
            &[
                ("Current instruction", "xinfo $pc", "xinfo $pc"),
                ("Function arguments", "dumpargs", "dumpargs"),
                ("Current syscall", "syscall-args", "syscall-args"),
                ("Future calls", "future-calls", "future-calls"),
                ("Entire stack frame", "stack-frame", "stack-frame"),
            ][..],
        ),
        (
            "Process",
            &[
                ("Virtual memory map", "vmmap", "vmmap"),
                ("Open file descriptors", "fds", "fds"),
                ("ELF auxiliary vector", "auxv", "auxv"),
                ("Current errno", "errno", "errno"),
                ("Thread-local storage", "tls", "tls"),
                ("Fork following", "follow", "follow"),
            ][..],
        ),
        (
            "Binary",
            &[
                ("Binary protections", "checksec", "checksec"),
                ("GOT / PLT", "got", "got"),
                ("Stack canary", "canary", "canary"),
                (
                    "Exception unwind data",
                    "dwarf-exception-handler",
                    "dwarf-exception-handler",
                ),
                ("Dynamic section", "dynamic", "dynamic"),
                ("Runtime link map", "link-map", "link-map"),
            ][..],
        ),
        (
            "Heap",
            &[
                ("Compact bins", "heap bins-simple", "heap bins-simple"),
                ("Heap arenas", "heap arenas", "heap arenas"),
                ("Heap chunks", "heap chunks", "heap chunks"),
                ("Top chunk", "heap top", "heap top"),
                ("Parsed heap", "heap parse", "heap parse"),
            ][..],
        ),
    ] {
        let page = build_gef_tool_page(commands, terminal, terminal_toggle, &popover);
        tools.append_page(&page, Some(&gtk::Label::new(Some(title))));
    }
    menu.append(&tools);

    menu.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    let expression_row = gtk::Box::new(gtk::Orientation::Horizontal, 1);
    let expression = gtk::Entry::builder()
        .placeholder_text("address or expression")
        .hexpand(true)
        .build();
    expression.set_tooltip_text(Some(
        "Address, expression, or type for xinfo, telescope, and dt",
    ));
    let inspect = gtk::Button::with_label("xinfo");
    let telescope = gtk::Button::with_label("telescope");
    let data_type = gtk::Button::with_label("dt");
    for button in [&inspect, &telescope, &data_type] {
        button.add_css_class("inline-action");
    }
    expression_row.append(&expression);
    expression_row.append(&inspect);
    expression_row.append(&telescope);
    expression_row.append(&data_type);
    menu.append(&expression_row);

    let submit = |prefix: &'static str| {
        let terminal = terminal.clone();
        let terminal_toggle = terminal_toggle.clone();
        let popover = popover.clone();
        let expression = expression.clone();
        Rc::new(move || {
            let expression = expression.text().replace(['\r', '\n'], " ");
            let expression = expression.trim();
            if expression.is_empty() {
                return;
            }
            run_terminal_command(
                &terminal,
                &terminal_toggle,
                &popover,
                &format!("{prefix} {expression}"),
            );
        })
    };
    let inspect_submit = submit("xinfo");
    let submit_for_button = Rc::clone(&inspect_submit);
    inspect.connect_clicked(move |_| submit_for_button());
    let submit_for_button = submit("telescope");
    telescope.connect_clicked(move |_| submit_for_button());
    let submit_for_button = submit("dt");
    data_type.connect_clicked(move |_| submit_for_button());
    expression.connect_activate(move |_| inspect_submit());

    popover.set_child(Some(&menu));
    let button = header_popup_button("GEF tools", &popover);
    button.add_css_class("debug-control");
    button.set_tooltip_text(Some(
        "Run useful bata24/GEF investigations in this debugger's terminal",
    ));
    button.set_sensitive(false);
    button
}

pub(super) fn build_gef_tool_page(
    commands: &[(&'static str, &'static str, &'static str)],
    terminal: &vte4::Terminal,
    terminal_toggle: &gtk::ToggleButton,
    popover: &gtk::Popover,
) -> gtk::Box {
    let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
    for (label, detail, command) in commands {
        let button = gef_tool_button(label, detail);
        connect_gef_tool(&button, terminal, terminal_toggle, popover, command);
        page.append(&button);
    }
    page
}

pub(super) fn header_popup_button(label: &str, popover: &gtk::Popover) -> gtk::ToggleButton {
    let button = gtk::ToggleButton::with_label(label);
    button.set_focus_on_click(false);
    popover.set_parent(&button);
    popover.set_position(gtk::PositionType::Bottom);
    let popover_for_toggle = popover.clone();
    button.connect_toggled(move |button| {
        if button.is_active() {
            popover_for_toggle.popup();
        } else {
            popover_for_toggle.popdown();
        }
    });
    let weak_button = button.downgrade();
    popover.connect_closed(move |_| {
        if let Some(button) = weak_button.upgrade()
            && button.is_active()
        {
            button.set_active(false);
        }
    });
    button
}

pub(super) fn gef_tool_button(label: &str, detail: &str) -> gtk::Button {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let label = gtk::Label::new(Some(label));
    label.set_halign(gtk::Align::Start);
    label.set_hexpand(true);
    let detail = gtk::Label::new(Some(detail));
    detail.add_css_class("gef-command");
    detail.set_halign(gtk::Align::End);
    row.append(&label);
    row.append(&detail);
    gtk::Button::builder().child(&row).build()
}

pub(super) fn connect_gef_tool(
    button: &gtk::Button,
    terminal: &vte4::Terminal,
    terminal_toggle: &gtk::ToggleButton,
    popover: &gtk::Popover,
    command: &'static str,
) {
    let terminal = terminal.clone();
    let terminal_toggle = terminal_toggle.clone();
    let popover = popover.clone();
    button.connect_clicked(move |_| {
        run_terminal_command(&terminal, &terminal_toggle, &popover, command);
    });
}

pub(super) fn run_terminal_command(
    terminal: &vte4::Terminal,
    terminal_toggle: &gtk::ToggleButton,
    popover: &gtk::Popover,
    command: &str,
) {
    terminal_toggle.set_active(true);
    popover.popdown();
    terminal.feed_child(format!("\u{15}{command}\n").as_bytes());
    terminal.grab_focus();
}

pub(super) fn window_control_button(label: &str, tooltip: &str, class: &str) -> gtk::Button {
    let button = gtk::Button::with_label(label);
    button.add_css_class("window-control");
    button.add_css_class(class);
    button.set_focus_on_click(false);
    button.set_tooltip_text(Some(tooltip));
    button
}

pub(super) fn build_workspace(
    config: &LaunchConfig,
    theme: &Theme,
    source_notebook: &gtk::Notebook,
    terminal: &vte4::Terminal,
    variable_children_handler: &Rc<RefCell<Option<VariableChildrenHandler>>>,
    target_pointer_bits: &Rc<Cell<u32>>,
) -> Workspace {
    let workspace = gtk::Paned::new(gtk::Orientation::Horizontal);
    workspace.add_css_class("workspace-columns");
    workspace.set_vexpand(true);
    workspace.set_position(980);
    workspace.set_shrink_start_child(false);
    workspace.set_resize_start_child(true);
    let inspector = build_inspector(variable_children_handler, target_pointer_bits);
    workspace.set_end_child(Some(&inspector.root));

    let navigation_and_editor = gtk::Paned::new(gtk::Orientation::Horizontal);
    navigation_and_editor.add_css_class("workspace-columns");
    navigation_and_editor.set_position(260);
    navigation_and_editor.set_shrink_start_child(false);
    navigation_and_editor.set_resize_start_child(false);
    let left_sidebar = build_left_sidebar(config, theme);
    navigation_and_editor.set_start_child(Some(&left_sidebar.root));
    navigation_and_editor.set_end_child(Some(&build_editor_panel(source_notebook)));

    let main_and_terminal = gtk::Paned::new(gtk::Orientation::Vertical);
    main_and_terminal.set_position(515);
    main_and_terminal.set_shrink_start_child(false);
    main_and_terminal.set_resize_start_child(true);
    main_and_terminal.set_start_child(Some(&navigation_and_editor));
    let terminal_panel = build_terminal_panel(terminal);
    main_and_terminal.set_end_child(Some(&terminal_panel));
    workspace.set_start_child(Some(&main_and_terminal));
    let layout_panes = vec![
        layout::Pane::new("workspace_inspector", &workspace),
        layout::Pane::new("navigation_source", &navigation_and_editor),
        layout::Pane::new("workspace_terminal", &main_and_terminal),
        layout::Pane::new("locals_instructions", &inspector.context_split),
    ];
    let mut debug_state_panels = inspector.stale_panels.clone();
    debug_state_panels.push(left_sidebar.root.clone().upcast());
    Workspace {
        root: workspace,
        layout_panes,
        terminal_panel,
        status_detail: inspector.status_detail,
        debug_state_panels,
        call_stack_list: left_sidebar.call_stack_list,
        threads_list: left_sidebar.threads_list,
        modules_list: left_sidebar.modules_list,
        locals_store: inspector.locals_store,
        locals_selection: inspector.locals_selection,
        locals_view: inspector.locals_view,
        locals_empty: inspector.locals_empty,
        locals_edit_button: inspector.locals_edit_button,
        instructions_title: inspector.instructions_title,
        instructions_store: inspector.instructions_store,
        instructions_selection: inspector.instructions_selection,
        instructions_view: inspector.instructions_view,
        instructions_empty: inspector.instructions_empty,
        instruction_flow: inspector.instruction_flow,
        instruction_arguments: inspector.instruction_arguments,
        instruction_memory: inspector.instruction_memory,
        register_groups: inspector.register_groups,
        registers_empty: inspector.registers_empty,
        stack_store: inspector.stack_store,
        stack_empty: inspector.stack_empty,
        breakpoints_list: inspector.breakpoints_list,
        delete_all_breakpoints_button: inspector.delete_all_breakpoints_button,
        delete_all_watchpoints_button: inspector.delete_all_watchpoints_button,
        delete_all_catchpoints_button: inspector.delete_all_catchpoints_button,
        event_catchpoint_buttons: inspector.event_catchpoint_buttons,
        watchpoint_expression: inspector.watchpoint_expression,
        watchpoint_access: inspector.watchpoint_access,
        watchpoint_add_button: inspector.watchpoint_add_button,
        signal_detail: inspector.signal_detail,
        signal_buttons: inspector.signal_buttons,
        signal_entry: inspector.signal_entry,
        signal_add_button: inspector.signal_add_button,
        delete_all_signal_catchpoints_button: inspector.delete_all_signal_catchpoints_button,
        memory_region_store: inspector.memory_region_store,
        memory_regions_empty: inspector.memory_regions_empty,
        memory_watch_list: inspector.memory_watch_list,
        memory_watches_empty: inspector.memory_watches_empty,
        memory_address_entry: inspector.memory_address_entry,
        memory_size: inspector.memory_size,
        memory_format: inspector.memory_format,
        memory_add_button: inspector.memory_add_button,
    }
}

pub(super) fn build_left_sidebar(config: &LaunchConfig, theme: &Theme) -> LeftSidebar {
    let sidebar = gtk::Box::new(gtk::Orientation::Vertical, 4);
    sidebar.add_css_class("sidebar");
    sidebar.set_size_request(190, -1);

    let session_rows = gtk::Box::new(gtk::Orientation::Vertical, 1);
    session_rows.append(&sidebar_row("Target", config.target_name()));
    session_rows.append(&sidebar_row("Debugger", &config.gdb_executable));
    session_rows.append(&sidebar_row("Interface", "GDB/MI 2"));
    session_rows.append(&sidebar_row("Theme", theme.name));
    let session = build_disclosure("SESSION", &session_rows, false, "session-disclosure");
    sidebar.append(&session);
    let call_stack_list = dynamic_list("Frames appear when the target is paused");
    let stack_scrolled = gtk::ScrolledWindow::builder()
        .child(&call_stack_list)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build();
    let threads_list = dynamic_list("Threads appear when the target is paused");
    let threads_scrolled = gtk::ScrolledWindow::builder()
        .child(&threads_list)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build();
    let modules_list = dynamic_list("Modules appear after the inferior starts");
    let modules_scrolled = gtk::ScrolledWindow::builder()
        .child(&modules_list)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build();
    let navigation = gtk::Notebook::new();
    navigation.add_css_class("sidebar-tabs");
    navigation.set_vexpand(true);
    navigation.append_page(&stack_scrolled, Some(&gtk::Label::new(Some("Call Stack"))));
    navigation.append_page(&threads_scrolled, Some(&gtk::Label::new(Some("Threads"))));
    navigation.append_page(&modules_scrolled, Some(&gtk::Label::new(Some("Modules"))));
    sidebar.append(&navigation);
    LeftSidebar {
        root: sidebar,
        call_stack_list,
        threads_list,
        modules_list,
    }
}

pub(super) fn build_inspector(
    variable_children_handler: &Rc<RefCell<Option<VariableChildrenHandler>>>,
    target_pointer_bits: &Rc<Cell<u32>>,
) -> Inspector {
    let notebook = gtk::Notebook::new();
    notebook.set_size_request(260, -1);
    notebook.set_scrollable(true);
    notebook.add_css_class("panel");

    let state = gtk::Box::new(gtk::Orientation::Vertical, 5);
    state.add_css_class("sidebar");
    let detail = gtk::Label::new(Some("Waiting for the MI channel"));
    detail.add_css_class("status-detail");
    detail.set_halign(gtk::Align::Start);
    detail.set_ellipsize(pango::EllipsizeMode::Middle);
    detail.set_single_line_mode(true);
    let (locals_view, locals_store, locals_selection) =
        build_locals_view(variable_children_handler, target_pointer_bits);
    let locals_empty = empty_label("Values appear when the target is paused");
    let locals_scrolled = gtk::ScrolledWindow::builder()
        .child(&locals_view)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .build();
    let (instructions_view, instructions_store, instructions_selection) = build_instruction_view();
    let instructions_empty = empty_label("Paused target required");
    let instructions_scrolled = gtk::ScrolledWindow::builder()
        .child(&instructions_view)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .build();
    let context = gtk::Paned::new(gtk::Orientation::Vertical);
    context.add_css_class("context-split");
    context.set_vexpand(true);
    context.set_position(310);
    context.set_wide_handle(false);
    context.set_resize_start_child(true);
    context.set_shrink_start_child(false);
    let locals_panel = gtk::Box::new(gtk::Orientation::Vertical, 0);
    locals_panel.set_vexpand(true);
    let locals_header = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    locals_header.add_css_class("subpanel-header");
    let locals_title = section_title("LOCALS / ARGUMENTS");
    locals_title.set_hexpand(true);
    locals_header.append(&locals_title);
    let locals_hint = gtk::Label::new(Some("Click name to expand"));
    locals_hint.add_css_class("muted");
    locals_hint.set_tooltip_text(Some(
        "Click an expandable name or its chevron to open it. Double-click a scalar to edit; the Edit button works for every selected value.",
    ));
    locals_header.append(&locals_hint);
    let locals_edit_button = gtk::Button::with_label("Edit");
    locals_edit_button.add_css_class("inline-action");
    locals_edit_button.set_tooltip_text(Some("Edit the selected value"));
    locals_edit_button.set_sensitive(false);
    locals_header.append(&locals_edit_button);
    locals_panel.append(&locals_header);
    locals_panel.append(&locals_empty);
    locals_panel.append(&locals_scrolled);
    let instructions_panel = gtk::Box::new(gtk::Orientation::Vertical, 0);
    instructions_panel.set_vexpand(true);
    let instructions_title = section_title("INSTRUCTIONS");
    instructions_title.set_ellipsize(pango::EllipsizeMode::End);
    instructions_title.set_hexpand(true);
    instructions_title.set_tooltip_text(Some("INSTRUCTIONS"));
    let instructions_header = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    instructions_header.add_css_class("subpanel-header");
    instructions_header.append(&instructions_title);
    instructions_panel.append(&instructions_header);
    let instruction_insight = gtk::Box::new(gtk::Orientation::Vertical, 0);
    instruction_insight.add_css_class("instruction-insight");
    let instruction_flow = insight_label("Flow information appears at a branch or call");
    let instruction_arguments = insight_label("");
    let instruction_memory = insight_label("");
    instruction_insight.append(&instruction_flow);
    instruction_insight.append(&instruction_arguments);
    instruction_insight.append(&instruction_memory);
    instructions_panel.append(&instruction_insight);
    instructions_panel.append(&instructions_empty);
    instructions_panel.append(&instructions_scrolled);
    context.set_start_child(Some(&locals_panel));
    context.set_end_child(Some(&instructions_panel));
    state.append(&context);

    let registers_page = gtk::Box::new(gtk::Orientation::Vertical, 2);
    registers_page.add_css_class("sidebar");
    registers_page.append(&section_title("REGISTERS"));
    let (registers_view, register_groups) = build_register_view();
    let registers_empty = empty_label("Values appear when the target is paused");
    let registers_scrolled = gtk::ScrolledWindow::builder()
        .child(&registers_view)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .build();
    registers_page.append(&registers_empty);
    registers_page.append(&registers_scrolled);

    let stack_page = gtk::Box::new(gtk::Orientation::Vertical, 2);
    stack_page.add_css_class("sidebar");
    stack_page.append(&build_context_legend());
    stack_page.append(&section_title("STACK"));
    let (stack_view, stack_store, stack_word_inspector) = build_stack_view();
    let stack_empty = empty_label("Stack values appear when the target is paused");
    let stack_scrolled = gtk::ScrolledWindow::builder()
        .child(&stack_view)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .build();
    stack_page.append(&stack_empty);
    stack_page.append(&stack_scrolled);
    stack_page.append(&stack_word_inspector.root);

    let memory_page = gtk::Box::new(gtk::Orientation::Vertical, 3);
    memory_page.add_css_class("sidebar");
    memory_page.append(&section_title("ADD MEMORY WATCH"));
    let memory_controls = gtk::Box::new(gtk::Orientation::Vertical, 3);
    memory_controls.add_css_class("memory-watch-command");
    let expression_row = gtk::Box::new(gtk::Orientation::Horizontal, 3);
    let memory_address_entry = gtk::Entry::builder()
        .placeholder_text("$rsp, ptr + 0x20, or 0x404000")
        .hexpand(true)
        .build();
    memory_address_entry
        .set_tooltip_text(Some("Any GDB expression that resolves to a memory address"));
    let memory_add_button = gtk::Button::with_label("Add watch");
    memory_add_button.add_css_class("inline-action");
    memory_add_button.set_sensitive(false);
    expression_row.append(&memory_address_entry);
    expression_row.append(&memory_add_button);

    let memory_options = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    memory_options.add_css_class("memory-watch-options");
    memory_options.append(&section_title("LENGTH"));
    let memory_size = gtk::SpinButton::with_range(8.0, 4096.0, 8.0);
    memory_size.set_value(128.0);
    memory_size.set_width_chars(4);
    memory_size.set_tooltip_text(Some("Bytes to read"));
    let memory_size_unit = gtk::Label::new(Some("bytes"));
    memory_size_unit.add_css_class("muted");
    let memory_options_spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    memory_options_spacer.set_hexpand(true);
    memory_options.append(&memory_size);
    memory_options.append(&memory_size_unit);
    memory_options.append(&memory_options_spacer);
    memory_options.append(&section_title("DISPLAY"));
    let memory_format = gtk::DropDown::from_strings(&["Bytes", "Words", "Pointers"]);
    memory_format.set_selected(0);
    memory_format.set_tooltip_text(Some("How to group and render the memory values"));
    memory_options.append(&memory_format);
    memory_controls.append(&expression_row);
    memory_controls.append(&memory_options);
    memory_page.append(&memory_controls);
    memory_page.append(&section_title("WATCHES"));
    let memory_watches_empty = empty_label("No memory watches. Add an expression above.");
    memory_page.append(&memory_watches_empty);
    let memory_watch_list = gtk::Box::new(gtk::Orientation::Vertical, 3);
    let memory_watches_scrolled = gtk::ScrolledWindow::builder()
        .child(&memory_watch_list)
        .min_content_height(170)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .build();
    memory_page.append(&memory_watches_scrolled);
    memory_page.append(&section_title("VIRTUAL MEMORY MAP"));
    let (memory_regions_view, memory_region_store) = build_memory_region_view();
    let memory_regions_empty = empty_label("Mappings appear when the target is paused");
    let memory_regions_scrolled = gtk::ScrolledWindow::builder()
        .child(&memory_regions_view)
        .min_content_height(190)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .build();
    memory_page.append(&memory_regions_empty);
    memory_page.append(&memory_regions_scrolled);

    let breakpoints_page = gtk::Box::new(gtk::Orientation::Vertical, 3);
    breakpoints_page.add_css_class("sidebar");
    breakpoints_page.append(&section_title("BREAKPOINTS / WATCHPOINTS"));
    let hint = gtk::Label::new(Some(
        "Click the source gutter to add a breakpoint. Conditions are shown on each row.",
    ));
    hint.add_css_class("muted");
    hint.set_halign(gtk::Align::Start);
    hint.set_wrap(true);
    breakpoints_page.append(&hint);
    let breakpoints_list = dynamic_list("No breakpoints or watchpoints set");
    let breakpoints_scrolled = gtk::ScrolledWindow::builder()
        .child(&breakpoints_list)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build();
    let breakpoint_bulk_actions = gtk::Box::new(gtk::Orientation::Horizontal, 2);
    let delete_all_breakpoints_button = gtk::Button::with_label("Delete all BPs");
    delete_all_breakpoints_button.add_css_class("inline-action");
    delete_all_breakpoints_button.add_css_class("danger-action");
    delete_all_breakpoints_button
        .set_tooltip_text(Some("Delete all breakpoints, preserving watchpoints"));
    delete_all_breakpoints_button.set_sensitive(false);
    let delete_all_watchpoints_button = gtk::Button::with_label("Delete all WPs");
    delete_all_watchpoints_button.add_css_class("inline-action");
    delete_all_watchpoints_button.add_css_class("danger-action");
    delete_all_watchpoints_button
        .set_tooltip_text(Some("Delete all watchpoints, preserving breakpoints"));
    delete_all_watchpoints_button.set_sensitive(false);
    let delete_all_catchpoints_button = gtk::Button::with_label("Delete all CPs");
    delete_all_catchpoints_button.add_css_class("inline-action");
    delete_all_catchpoints_button.add_css_class("danger-action");
    delete_all_catchpoints_button.set_tooltip_text(Some(
        "Delete event catchpoints, preserving signal catchpoints",
    ));
    delete_all_catchpoints_button.set_sensitive(false);
    breakpoint_bulk_actions.append(&delete_all_breakpoints_button);
    breakpoint_bulk_actions.append(&delete_all_watchpoints_button);
    breakpoint_bulk_actions.append(&delete_all_catchpoints_button);
    breakpoints_page.append(&breakpoint_bulk_actions);
    breakpoints_page.append(&breakpoints_scrolled);
    breakpoints_page.append(&section_title("QUICK CATCHPOINTS"));
    let event_catchpoint_grid = gtk::Grid::builder()
        .column_spacing(2)
        .row_spacing(2)
        .column_homogeneous(true)
        .build();
    let event_catchpoint_buttons = EventCatchpoint::ALL
        .into_iter()
        .enumerate()
        .map(|(index, (event, label, tooltip))| {
            let button = gtk::Button::with_label(label);
            button.add_css_class("signal-action");
            button.set_tooltip_text(Some(tooltip));
            button.set_sensitive(false);
            event_catchpoint_grid.attach(&button, (index % 3) as i32, (index / 3) as i32, 1, 1);
            (button, event)
        })
        .collect::<Vec<_>>();
    breakpoints_page.append(&event_catchpoint_grid);
    breakpoints_page.append(&section_title("ADD WATCHPOINT"));
    let watchpoint_controls = gtk::Box::new(gtk::Orientation::Horizontal, 2);
    let watchpoint_expression = gtk::Entry::builder()
        .placeholder_text("variable or address expression")
        .hexpand(true)
        .build();
    watchpoint_expression.set_tooltip_text(Some("Examples: counter, *pointer, *(int*)0x404040"));
    let watchpoint_access = gtk::DropDown::from_strings(&["Write", "Read", "Access"]);
    watchpoint_access.set_selected(0);
    watchpoint_access.set_tooltip_text(Some(
        "Stop on writes, reads, or either kind of memory access",
    ));
    let watchpoint_add_button = gtk::Button::with_label("Add");
    watchpoint_add_button.add_css_class("inline-action");
    watchpoint_add_button.set_sensitive(false);
    watchpoint_controls.append(&watchpoint_expression);
    watchpoint_controls.append(&watchpoint_access);
    watchpoint_controls.append(&watchpoint_add_button);
    breakpoints_page.append(&watchpoint_controls);

    let signals_content = gtk::Box::new(gtk::Orientation::Vertical, 4);
    signals_content.add_css_class("sidebar");
    signals_content.append(&section_title("CURRENT STOP"));
    let signal_detail = gtk::Label::new(Some("No signal at the current stop"));
    signal_detail.add_css_class("signal-detail");
    signal_detail.set_halign(gtk::Align::Start);
    signal_detail.set_wrap(true);
    signal_detail.set_xalign(0.0);
    signals_content.append(&signal_detail);
    let signal_actions_header = gtk::Box::new(gtk::Orientation::Horizontal, 3);
    let signal_actions_title = section_title("COMMON CATCHPOINTS");
    signal_actions_title.set_hexpand(true);
    let delete_all_signal_catchpoints_button = gtk::Button::with_label("Clear catches");
    delete_all_signal_catchpoints_button.add_css_class("inline-action");
    delete_all_signal_catchpoints_button.add_css_class("danger-action");
    delete_all_signal_catchpoints_button.set_tooltip_text(Some(
        "Delete every signal catchpoint without affecting breakpoints or watchpoints",
    ));
    delete_all_signal_catchpoints_button.set_sensitive(false);
    signal_actions_header.append(&signal_actions_title);
    signal_actions_header.append(&delete_all_signal_catchpoints_button);
    signals_content.append(&signal_actions_header);
    let signal_hint = gtk::Label::new(Some(
        "Click a signal to add its catchpoint, active signals are green and click again removes them.",
    ));
    signal_hint.add_css_class("muted");
    signal_hint.set_halign(gtk::Align::Start);
    signal_hint.set_wrap(true);
    signals_content.append(&signal_hint);
    let (common_signal_grid, mut signal_buttons) = build_signal_grid(COMMON_SIGNALS);
    signals_content.append(&common_signal_grid);
    let (more_signal_grid, mut more_signal_buttons) = build_signal_grid(MORE_SIGNALS);
    signal_buttons.append(&mut more_signal_buttons);
    signals_content.append(&build_disclosure(
        "MORE POSIX SIGNALS",
        &more_signal_grid,
        false,
        "signal-disclosure",
    ));
    signals_content.append(&section_title("CUSTOM SIGNAL"));
    let custom_signal_row = gtk::Box::new(gtk::Orientation::Horizontal, 2);
    let signal_entry = gtk::Entry::builder()
        .placeholder_text("SIGRTMIN+1 or 35")
        .hexpand(true)
        .build();
    signal_entry.set_tooltip_text(Some(
        "Signal name or number; names without the SIG prefix are normalized",
    ));
    let signal_add_button = gtk::Button::with_label("Toggle catch");
    signal_add_button.add_css_class("inline-action");
    signal_add_button.set_sensitive(false);
    custom_signal_row.append(&signal_entry);
    custom_signal_row.append(&signal_add_button);
    signals_content.append(&custom_signal_row);
    let signals_page = gtk::ScrolledWindow::builder()
        .child(&signals_content)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build();

    notebook.append_page(&state, Some(&gtk::Label::new(Some("Context"))));
    notebook.append_page(&registers_page, Some(&gtk::Label::new(Some("Registers"))));
    notebook.append_page(&stack_page, Some(&gtk::Label::new(Some("Stack"))));
    notebook.append_page(&memory_page, Some(&gtk::Label::new(Some("Memory"))));
    notebook.append_page(
        &breakpoints_page,
        Some(&gtk::Label::new(Some("Breakpoints"))),
    );
    notebook.append_page(&signals_page, Some(&gtk::Label::new(Some("Signals"))));
    let stale_panels = vec![
        state.clone().upcast(),
        registers_page.clone().upcast(),
        stack_page.clone().upcast(),
        memory_page.clone().upcast(),
        signals_page.clone().upcast(),
    ];
    Inspector {
        root: notebook,
        context_split: context,
        status_detail: detail,
        stale_panels,
        locals_store,
        locals_selection,
        locals_view,
        locals_empty,
        locals_edit_button,
        instructions_title,
        instructions_store,
        instructions_selection,
        instructions_view,
        instructions_empty,
        instruction_flow,
        instruction_arguments,
        instruction_memory,
        register_groups,
        registers_empty,
        stack_store,
        stack_empty,
        breakpoints_list,
        delete_all_breakpoints_button,
        delete_all_watchpoints_button,
        delete_all_catchpoints_button,
        event_catchpoint_buttons,
        watchpoint_expression,
        watchpoint_access,
        watchpoint_add_button,
        signal_detail,
        signal_buttons,
        signal_entry,
        signal_add_button,
        delete_all_signal_catchpoints_button,
        memory_region_store,
        memory_regions_empty,
        memory_watch_list,
        memory_watches_empty,
        memory_address_entry,
        memory_size,
        memory_format,
        memory_add_button,
    }
}
