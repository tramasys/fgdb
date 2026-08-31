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
    let target_name = config.target_name();
    let target_label = gtk::Label::new(Some(&target_name));
    target_label.add_css_class("target-label");
    target_label.set_ellipsize(pango::EllipsizeMode::Middle);
    target_label.set_max_width_chars(32);
    target_label.set_tooltip_text(Some(&target_name));
    title_group.append(&target_label);
    topbar.set_title_widget(Some(&title_group));

    let leading = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    leading.add_css_class("titlebar-actions");
    let session_popover = gtk::Popover::new();
    session_popover.set_autohide(true);
    let session_menu = gtk::Box::new(gtk::Orientation::Vertical, 1);
    session_menu.add_css_class("session-menu");
    let session_summary = gtk::Box::new(gtk::Orientation::Vertical, 3);
    session_summary.add_css_class("session-summary");
    let session_caption = gtk::Label::new(Some("DEBUG SESSION"));
    session_caption.add_css_class("session-caption");
    session_caption.set_halign(gtk::Align::Start);
    let session_kind_label = gtk::Label::new(Some("No session"));
    session_kind_label.add_css_class("session-kind");
    session_kind_label.set_halign(gtk::Align::Start);
    let session_target_label = gtk::Label::new(Some("Choose a session"));
    session_target_label.add_css_class("session-target");
    session_target_label.set_halign(gtk::Align::Start);
    session_target_label.set_ellipsize(pango::EllipsizeMode::Middle);
    session_target_label.set_max_width_chars(42);
    session_summary.append(&session_caption);
    session_summary.append(&session_kind_label);
    session_summary.append(&session_target_label);
    session_menu.append(&session_summary);
    session_menu.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    let new_session_button = session_menu_action("New session", "›");
    new_session_button.add_css_class("session-primary-action");
    let restart_session_button = session_menu_action("Restart", "start again");
    let kill_session_button = session_menu_action("Kill inferior", "terminate");
    kill_session_button.add_css_class("danger-action");
    let detach_session_button = session_menu_action("Detach safely", "keep running");
    let resynchronize_button = session_menu_action("Refresh debugger state", "Ctrl+Shift+R");
    resynchronize_button.add_css_class("session-utility-action");
    resynchronize_button.set_tooltip_text(Some(
        "Re-read state after terminal commands or external debugger changes",
    ));
    let configuration_detail = config.configuration_report().menu_detail();
    let configuration_button = session_menu_action("Configuration", &configuration_detail);
    configuration_button.add_css_class("session-utility-action");
    configuration_button.set_tooltip_text(Some(
        "Show loaded files, configuration issues, and effective settings",
    ));
    if !config.configuration_report().issues().is_empty() {
        configuration_button.add_css_class("configuration-warning");
    }
    let restart_gdb_button = session_menu_action("Restart GDB", "recover backend");
    restart_gdb_button.add_css_class("session-primary-action");
    restart_gdb_button.set_visible(false);
    let gdb_capabilities_label = gtk::Label::new(Some("GDB capabilities pending"));
    gdb_capabilities_label.add_css_class("session-target");
    gdb_capabilities_label.add_css_class("session-capabilities");
    gdb_capabilities_label.set_halign(gtk::Align::Start);
    gdb_capabilities_label.set_ellipsize(pango::EllipsizeMode::End);
    session_menu.append(&new_session_button);
    session_menu.append(&restart_session_button);
    session_menu.append(&kill_session_button);
    session_menu.append(&detach_session_button);
    session_menu.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    session_menu.append(&resynchronize_button);
    session_menu.append(&configuration_button);
    session_menu.append(&restart_gdb_button);
    session_menu.append(&gdb_capabilities_label);
    session_popover.set_child(Some(&session_menu));
    let session_button = header_popup_button("Session", &session_popover);
    session_button.add_css_class("toolbar-action");
    session_button.set_tooltip_text(Some(
        "Launch a program, attach to a process, inspect a core, connect remotely, restart, kill, or detach",
    ));
    leading.append(&session_button);
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
    terminal_toggle.add_css_class("toolbar-toggle");
    terminal_toggle.add_css_class("terminal-pane-toggle");
    terminal_toggle.set_active(true);
    terminal_toggle.set_tooltip_text(Some("Show or hide the interactive GDB terminal · Ctrl+`"));
    let gef_tools = build_gef_tools_menu(terminal, &terminal_toggle);
    topbar.pack_start(&leading);

    let controls = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    controls.add_css_class("execution-controls");
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
    until_popover.set_autohide(true);
    let until_menu = gtk::Box::new(gtk::Orientation::Vertical, 1);
    until_menu.add_css_class("until-menu");
    let until_summary = gtk::Box::new(gtk::Orientation::Vertical, 1);
    until_summary.add_css_class("until-summary");
    let until_caption = gtk::Label::new(Some("RUN UNTIL"));
    until_caption.add_css_class("session-caption");
    until_caption.set_halign(gtk::Align::Start);
    let until_kind = gtk::Label::new(Some("Next matching event"));
    until_kind.add_css_class("session-kind");
    until_kind.set_halign(gtk::Align::Start);
    let until_detail = gtk::Label::new(Some("Live execution path · Pause cancels"));
    until_detail.add_css_class("session-target");
    until_detail.set_halign(gtk::Align::Start);
    until_summary.append(&until_caption);
    until_summary.append(&until_kind);
    until_summary.append(&until_detail);
    until_menu.append(&until_summary);
    until_menu.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

    let actions = [
        ("Current line", "source", UntilAction::CurrentLine),
        ("Function returns", "frame", UntilAction::FunctionReturns),
        ("Next call", "instruction", UntilAction::NextCall),
        ("Next return", "instruction", UntilAction::NextReturn),
        ("Next syscall", "instruction", UntilAction::NextSyscall),
        (
            "Next indirect branch",
            "instruction",
            UntilAction::NextIndirectBranch,
        ),
        (
            "Next call / jump / return",
            "control flow",
            UntilAction::NextControlFlow,
        ),
        ("Memory access", "instruction", UntilAction::MemoryAccess),
        ("User code", "mapping", UntilAction::UserCode),
        ("libc code", "mapping", UntilAction::LibcCode),
        ("Region change", "mapping", UntilAction::RegionChange),
    ];
    let mut until_actions = Vec::with_capacity(actions.len());
    for (index, (label, detail, action)) in actions.into_iter().enumerate() {
        if matches!(index, 2 | 8) {
            until_menu.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        }
        let button = session_menu_action(label, detail);
        button.add_css_class("until-action");
        until_menu.append(&button);
        until_actions.push((button, action));
    }

    until_menu.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    let condition_section = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    condition_section.add_css_class("until-condition");
    let until_condition_entry = gtk::Entry::builder()
        .placeholder_text("$rax == 0")
        .hexpand(true)
        .build();
    until_condition_entry.set_tooltip_text(Some(
        "Stop when this side-effect-free GDB expression becomes non-zero",
    ));
    condition_section.append(&until_condition_entry);
    let until_condition_button = gtk::Button::with_label("Run");
    until_condition_button.add_css_class("inline-action");
    condition_section.append(&until_condition_button);
    until_menu.append(&condition_section);
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
    trailing.append(&status);
    trailing.append(&controls);
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
        session_button,
        session_popover,
        session_kind_label,
        session_target_label,
        new_session_button,
        restart_session_button,
        kill_session_button,
        detach_session_button,
        restart_gdb_button,
        resynchronize_button,
        configuration_button,
        gdb_capabilities_label,
        target_label,
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
        gef_tools_button: gef_tools.button,
        gef_tools_content: gef_tools.content,
        gef_tool_controls: gef_tools.controls,
        gef_tool_groups: gef_tools.groups,
        until_actions,
        until_condition_entry,
        until_condition_button,
        status_label: status,
    }
}

fn session_menu_action(label: &str, detail: &str) -> gtk::Button {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    let label = gtk::Label::new(Some(label));
    label.add_css_class("session-action-label");
    label.set_halign(gtk::Align::Start);
    label.set_hexpand(true);
    let detail = gtk::Label::new(Some(detail));
    detail.add_css_class("session-action-detail");
    detail.set_halign(gtk::Align::End);
    row.append(&label);
    row.append(&detail);
    let button = gtk::Button::builder().child(&row).build();
    button.add_css_class("session-action");
    button
}

pub(super) fn build_gef_tools_menu(
    terminal: &vte4::Terminal,
    terminal_toggle: &gtk::ToggleButton,
) -> GefToolsMenu {
    let popover = gtk::Popover::new();
    let menu = gtk::Box::new(gtk::Orientation::Vertical, 0);
    menu.add_css_class("gef-tools-menu");
    menu.append(&section_title("GEF / LOW-LEVEL TOOLS"));
    let tools = gtk::Notebook::new();
    tools.add_css_class("gef-tools-tabs");
    let mut controls = Vec::new();
    let mut groups = Vec::new();
    for (title, commands) in [
        (
            "Context",
            &[
                ("Current instruction", "xinfo $pc", "xinfo $pc", "xinfo"),
                ("Opcode + memory effects", "ii $pc", "ii $pc", "ii"),
                (
                    "Register pointer chains",
                    "registers",
                    "registers",
                    "registers",
                ),
                (
                    "Stack telescope",
                    "telescope $sp",
                    "telescope $sp",
                    "telescope",
                ),
                ("Function arguments", "dumpargs", "dumpargs", "dumpargs"),
                (
                    "Current syscall",
                    "syscall-args",
                    "syscall-args",
                    "syscall-args",
                ),
                (
                    "Future calls",
                    "future-calls",
                    "future-calls",
                    "future-calls",
                ),
                (
                    "Entire stack frame",
                    "stack-frame",
                    "stack-frame",
                    "stack-frame",
                ),
            ][..],
        ),
        (
            "Process",
            &[
                ("Virtual memory map", "vmmap", "vmmap", "vmmap"),
                ("Process information", "proc-info", "proc-info", "proc-info"),
                ("Mapped files", "xfiles", "xfiles", "xfiles"),
                ("Program arguments", "argv", "argv", "argv"),
                ("Environment", "envp", "envp", "envp"),
                ("Open file descriptors", "fds", "fds", "fds"),
                ("ELF auxiliary vector", "auxv", "auxv", "auxv"),
                ("Current errno", "errno", "errno", "errno"),
                ("Thread-local storage", "tls", "tls", "tls"),
                ("Fork following", "follow", "follow", "follow"),
            ][..],
        ),
        (
            "Binary",
            &[
                ("Binary protections", "checksec", "checksec", "checksec"),
                ("ELF information", "elf-info", "elf-info", "elf-info"),
                ("GOT / PLT", "got", "got", "got"),
                ("All GOT entries", "got-all", "got-all", "got-all"),
                ("Stack canary", "canary", "canary", "canary"),
                (
                    "Exception unwind data",
                    "dwarf-exception-handler",
                    "dwarf-exception-handler",
                    "dwarf-exception-handler",
                ),
                ("Dynamic section", "dynamic", "dynamic", "dynamic"),
                ("Runtime link map", "link-map", "link-map", "link-map"),
            ][..],
        ),
    ] {
        let capabilities = commands
            .iter()
            .map(|(_, _, _, capability)| *capability)
            .collect();
        let (page, page_controls) =
            build_gef_tool_page(commands, terminal, terminal_toggle, &popover);
        page.set_visible(false);
        tools.append_page(&page, Some(&gtk::Label::new(Some(title))));
        groups.push(GefCapabilityGroup {
            widget: page.upcast(),
            capabilities,
        });
        controls.extend(page_controls);
    }
    menu.append(&tools);

    let expression_section = gtk::Box::new(gtk::Orientation::Vertical, 0);
    expression_section.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
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
    expression_section.append(&expression_row);
    expression_section.set_visible(false);
    menu.append(&expression_section);
    for (button, capability) in [
        (&inspect, "xinfo"),
        (&telescope, "telescope"),
        (&data_type, "dt"),
    ] {
        controls.push(GefCapabilityControl {
            widget: button.clone().upcast(),
            capability,
        });
    }
    groups.push(GefCapabilityGroup {
        widget: expression_section.upcast(),
        capabilities: vec!["xinfo", "telescope", "dt"],
    });

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
    let telescope_submit = submit("telescope");
    let submit_for_button = Rc::clone(&telescope_submit);
    telescope.connect_clicked(move |_| submit_for_button());
    let data_type_submit = submit("dt");
    let submit_for_button = Rc::clone(&data_type_submit);
    data_type.connect_clicked(move |_| submit_for_button());
    expression.connect_activate(move |_| {
        if inspect.is_visible() {
            inspect_submit();
        } else if telescope.is_visible() {
            telescope_submit();
        } else if data_type.is_visible() {
            data_type_submit();
        }
    });

    popover.set_child(Some(&menu));
    let button = header_popup_button("GEF tools", &popover);
    button.add_css_class("inline-action");
    button.set_tooltip_text(Some(
        "Run investigations supported by the active GEF installation",
    ));
    button.set_visible(false);
    button.set_sensitive(false);
    GefToolsMenu {
        button,
        content: menu,
        controls,
        groups,
    }
}

pub(super) fn build_gef_tool_page(
    commands: &[(&'static str, &'static str, &'static str, &'static str)],
    terminal: &vte4::Terminal,
    terminal_toggle: &gtk::ToggleButton,
    popover: &gtk::Popover,
) -> (gtk::Box, Vec<GefCapabilityControl>) {
    let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let mut controls = Vec::with_capacity(commands.len());
    for (label, detail, command, capability) in commands {
        let button = gef_tool_button(label, detail);
        connect_gef_tool(&button, terminal, terminal_toggle, popover, command);
        page.append(&button);
        controls.push(GefCapabilityControl {
            widget: button.upcast(),
            capability,
        });
    }
    (page, controls)
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
    source_notebook: &gtk::Notebook,
    terminal: &vte4::Terminal,
    gef_tools_button: &gtk::ToggleButton,
    inspector_bindings: &InspectorBindings<'_>,
) -> Workspace {
    let workspace = gtk::Paned::new(gtk::Orientation::Horizontal);
    workspace.add_css_class("workspace-columns");
    workspace.add_css_class("workspace-inspector-split");
    workspace.set_vexpand(true);
    workspace.set_position(980);
    workspace.set_shrink_start_child(false);
    workspace.set_resize_start_child(true);
    workspace.set_wide_handle(false);
    let inspector = build_inspector(inspector_bindings);
    workspace.set_end_child(Some(&inspector.root));
    connect_inspector_responsiveness(&workspace, &inspector);

    let navigation_and_editor = gtk::Paned::new(gtk::Orientation::Horizontal);
    navigation_and_editor.add_css_class("workspace-columns");
    navigation_and_editor.set_position(260);
    navigation_and_editor.set_shrink_start_child(false);
    navigation_and_editor.set_resize_start_child(false);
    let left_sidebar = build_left_sidebar();
    let source_editor = build_editor_panel(source_notebook);
    navigation_and_editor.set_start_child(Some(&left_sidebar.root));
    navigation_and_editor.set_end_child(Some(&source_editor.root));

    let main_and_terminal = gtk::Paned::new(gtk::Orientation::Vertical);
    main_and_terminal.set_position(515);
    main_and_terminal.set_shrink_start_child(false);
    main_and_terminal.set_resize_start_child(true);
    main_and_terminal.set_start_child(Some(&navigation_and_editor));
    let terminal_panel = build_terminal_panel(terminal, gef_tools_button);
    main_and_terminal.set_end_child(Some(&terminal_panel));
    workspace.set_start_child(Some(&main_and_terminal));
    let layout_panes = vec![
        layout::Pane::new("workspace_inspector", &workspace),
        layout::Pane::new("navigation_source", &navigation_and_editor),
        layout::Pane::new("workspace_terminal", &main_and_terminal),
        layout::Pane::new("locals_instructions", &inspector.context_split),
        layout::Pane::with_default_fraction("memory_inspector_map", &inspector.memory_split, 0.5),
        layout::Pane::new("kernel_changes", &inspector.kernel_view.changes_split),
        layout::Pane::with_default_fraction(
            "misc_startup_vectors",
            &inspector.misc_view.startup_split,
            0.42,
        ),
        layout::Pane::with_default_fraction(
            "misc_call_abi",
            &inspector.misc_view.call_abi_split,
            0.42,
        ),
        layout::Pane::with_default_fraction(
            "misc_core_dump",
            &inspector.misc_view.core_split,
            0.34,
        ),
        layout::Pane::with_default_fraction(
            "misc_locks_graph",
            &inspector.misc_view.lock_split,
            0.6,
        ),
    ];
    Workspace {
        root: workspace,
        layout_panes,
        terminal_panel,
        status_detail: inspector.status_detail,
        source_navigation: source_editor.navigation,
        source_tree: left_sidebar.source_tree,
        left_navigation: left_sidebar.navigation,
        inspector_notebook: inspector.notebook.clone(),
        call_stack_list: left_sidebar.call_stack_list,
        threads_list: left_sidebar.threads_list,
        thread_controls: left_sidebar.thread_controls,
        modules_list: left_sidebar.modules_list,
        inferior_controls: left_sidebar.inferior_controls,
        locals_store: inspector.locals_store,
        locals_selection: inspector.locals_selection,
        locals_view: inspector.locals_view,
        locals_empty: inspector.locals_empty,
        locals_edit_button: inspector.locals_edit_button,
        expression_watches_store: inspector.expression_watches_store,
        expression_watches_selection: inspector.expression_watches_selection,
        expression_watches_view: inspector.expression_watches_view,
        expression_watches_empty: inspector.expression_watches_empty,
        expression_watch_entry: inspector.expression_watch_entry,
        expression_watch_add_button: inspector.expression_watch_add_button,
        expression_watch_remove_button: inspector.expression_watch_remove_button,
        instructions_title: inspector.instructions_title,
        instructions_store: inspector.instructions_store,
        instructions_selection: inspector.instructions_selection,
        instructions_view: inspector.instructions_view,
        instructions_empty: inspector.instructions_empty,
        instruction_flow: inspector.instruction_flow,
        instruction_arguments: inspector.instruction_arguments,
        instruction_memory: inspector.instruction_memory,
        disassembly_controls: inspector.disassembly_controls,
        register_groups: inspector.register_groups,
        registers_empty: inspector.registers_empty,
        stack_store: inspector.stack_store,
        stack_empty: inspector.stack_empty,
        breakpoints_list: inspector.breakpoints_list,
        add_breakpoint_button: inspector.add_breakpoint_button,
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
        memory_regions_view: inspector.memory_regions_view,
        memory_regions_empty: inspector.memory_regions_empty,
        memory_watch_container: inspector.memory_watch_container,
        memory_address_entry: inspector.memory_address_entry,
        memory_size: inspector.memory_size,
        memory_format: inspector.memory_format,
        memory_add_button: inspector.memory_add_button,
        kernel_view: inspector.kernel_view,
        misc_view: inspector.misc_view,
    }
}

fn connect_inspector_responsiveness(workspace: &gtk::Paned, inspector: &Inspector) {
    const COMPACT_INSPECTOR_WIDTH: i32 = 620;
    let inspector_notebook = inspector.notebook.clone();
    let compact_inspector_tabs = inspector.compact_tabs.clone();
    let kernel_root = inspector.kernel_view.root.clone();
    let kernel_wide_subtabs = inspector.kernel_view.wide_subtabs.clone();
    let kernel_compact_subtabs = inspector.kernel_view.compact_subtabs.clone();
    let misc_root = inspector.misc_view.root.clone();
    let misc_wide_subtabs = inspector.misc_view.wide_subtabs.clone();
    let misc_compact_subtabs = inspector.misc_view.compact_subtabs.clone();
    let update: Rc<dyn Fn(&gtk::Paned)> = Rc::new(move |workspace| {
        let width = workspace.width().saturating_sub(workspace.position());
        let compact = width > 0 && width < COMPACT_INSPECTOR_WIDTH;
        for root in [&kernel_root, &misc_root] {
            if compact {
                root.add_css_class("inspector-compact");
            } else {
                root.remove_css_class("inspector-compact");
            }
        }
        inspector_notebook.set_show_tabs(!compact);
        compact_inspector_tabs.set_visible(compact);
        kernel_wide_subtabs.set_visible(!compact);
        kernel_compact_subtabs.set_visible(compact);
        misc_wide_subtabs.set_visible(!compact);
        misc_compact_subtabs.set_visible(compact);
    });
    let update_for_position = Rc::clone(&update);
    workspace.connect_position_notify(move |workspace| update_for_position(workspace));
    let update_for_allocation = Rc::clone(&update);
    workspace.connect_max_position_notify(move |workspace| update_for_allocation(workspace));
    let update_for_map = Rc::clone(&update);
    workspace.connect_map(move |workspace| {
        let workspace = workspace.clone();
        let update = Rc::clone(&update_for_map);
        glib::idle_add_local_once(move || update(&workspace));
    });
}

pub(super) fn build_left_sidebar() -> LeftSidebar {
    let sidebar = gtk::Box::new(gtk::Orientation::Vertical, 4);
    sidebar.add_css_class("sidebar");
    sidebar.set_size_request(190, -1);
    let call_stack_list = dynamic_list("Frames appear when the target is paused");
    let stack_scrolled = gtk::ScrolledWindow::builder()
        .child(&call_stack_list)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build();
    let thread_controls = build_thread_controls();
    let threads_list = thread_controls.list.clone();
    let modules_list = dynamic_list("Modules appear after the inferior starts");
    let modules_scrolled = gtk::ScrolledWindow::builder()
        .child(&modules_list)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build();
    let source_tree = build_source_tree_view();
    let inferior_controls = build_inferior_controls();
    let navigation = gtk::Notebook::new();
    navigation.add_css_class("sidebar-tabs");
    navigation.set_vexpand(true);
    navigation.set_scrollable(true);
    navigation.append_page(
        &inferior_controls.page,
        Some(&gtk::Label::new(Some("Inferiors"))),
    );
    navigation.append_page(&stack_scrolled, Some(&gtk::Label::new(Some("Call Stack"))));
    navigation.append_page(
        &thread_controls.root,
        Some(&gtk::Label::new(Some("Threads"))),
    );
    navigation.append_page(&modules_scrolled, Some(&gtk::Label::new(Some("Modules"))));
    navigation.append_page(&source_tree.root, Some(&gtk::Label::new(Some("Sources"))));
    let navigation_for_selection = navigation.clone();
    navigation.connect_switch_page(move |_, _, _| {
        let navigation = navigation_for_selection.clone();
        glib::idle_add_local_once(move || clear_label_selections(&navigation));
    });
    sidebar.append(&inferior_controls.summary);
    sidebar.append(&navigation);
    LeftSidebar {
        root: sidebar,
        navigation,
        call_stack_list,
        threads_list,
        thread_controls,
        modules_list,
        source_tree,
        inferior_controls,
    }
}

pub(super) fn build_inspector(bindings: &InspectorBindings<'_>) -> Inspector {
    let notebook = gtk::Notebook::new();
    notebook.set_size_request(0, 0);
    notebook.set_scrollable(true);
    notebook.add_css_class("panel");

    let state = gtk::Box::new(gtk::Orientation::Vertical, 5);
    state.add_css_class("sidebar");
    let detail = gtk::Label::new(Some("Waiting for the MI channel"));
    detail.add_css_class("status-detail");
    detail.set_halign(gtk::Align::Start);
    detail.set_ellipsize(pango::EllipsizeMode::Middle);
    detail.set_single_line_mode(true);
    let (locals_view, locals_store, locals_selection) = build_locals_view(
        bindings.variable_children_handler,
        bindings.target_pointer_bits,
    );
    let (expression_watches_view, expression_watches_store, expression_watches_selection) =
        build_locals_view(
            bindings.variable_children_handler,
            bindings.target_pointer_bits,
        );
    let locals_empty = empty_label("Values appear when the target is paused");
    let locals_scrolled = gtk::ScrolledWindow::builder()
        .child(&locals_view)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .build();
    let (instructions_view, instructions_store, instructions_selection, source_column) =
        build_instruction_view();
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
        "Click an expandable name or its chevron to open it. Double-click a scalar to edit. The Edit button works for every selected value.",
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
    instructions_title.set_hexpand(true);
    instructions_title.set_xalign(0.0);
    instructions_title.set_tooltip_text(Some("INSTRUCTIONS"));
    let instructions_header = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    instructions_header.add_css_class("subpanel-header");
    instructions_header.append(&instructions_title);
    let disassembly_range = gtk::Label::new(None);
    disassembly_range.add_css_class("disassembly-range");
    disassembly_range.set_ellipsize(pango::EllipsizeMode::Middle);
    disassembly_range.set_halign(gtk::Align::End);
    instructions_header.append(&disassembly_range);
    instructions_panel.append(&instructions_header);
    let disassembly_browser = gtk::Box::new(gtk::Orientation::Vertical, 1);
    disassembly_browser.add_css_class("disassembly-browser");
    let disassembly_navigation = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    disassembly_navigation.add_css_class("disassembly-browser-row");
    let disassembly_actions = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    disassembly_actions.add_css_class("disassembly-browser-row");
    let disassembly_back = compact_instruction_button("‹", "Back in location history");
    let disassembly_forward = compact_instruction_button("›", "Forward in location history");
    let disassembly_previous = compact_instruction_button("‹ Prev", "Show the preceding function");
    let disassembly_next = compact_instruction_button("Next ›", "Show the following function");
    let disassembly_location = gtk::Entry::builder()
        .placeholder_text("Address, symbol, or expression")
        .hexpand(true)
        .build();
    disassembly_location.set_tooltip_text(Some(
        "Examples: $pc, 0x401000, main, malloc, or a register expression",
    ));
    let disassembly_go = compact_instruction_button(
        "Show",
        "Resolve the entered location and disassemble its containing function",
    );
    let disassembly_pc = compact_instruction_button(
        "Current PC",
        "Return to the instruction where execution is currently stopped",
    );
    let disassembly_mixed = gtk::ToggleButton::with_label("Source");
    disassembly_mixed.add_css_class("inline-action");
    disassembly_mixed.set_tooltip_text(Some("Toggle mixed source and assembly display"));
    let disassembly_syntax_intel = gtk::ToggleButton::with_label("Intel");
    disassembly_syntax_intel.add_css_class("inline-action");
    disassembly_syntax_intel.add_css_class("disassembly-syntax");
    disassembly_syntax_intel.set_tooltip_text(Some("Use Intel assembly syntax"));
    disassembly_syntax_intel.set_active(true);
    let disassembly_syntax_att = gtk::ToggleButton::with_label("AT&T");
    disassembly_syntax_att.add_css_class("inline-action");
    disassembly_syntax_att.add_css_class("disassembly-syntax");
    disassembly_syntax_att.set_tooltip_text(Some("Use AT&T assembly syntax"));
    disassembly_syntax_att.set_group(Some(&disassembly_syntax_intel));
    let disassembly_follow = compact_instruction_button(
        "Target",
        "Follow the selected direct or register-indirect call or branch target",
    );
    let disassembly_memory = compact_instruction_button(
        "Memory",
        "Open the selected instruction's effective address in Memory",
    );
    let history_group = disassembly_control_group(
        "HISTORY",
        &[
            disassembly_back.clone().upcast::<gtk::Widget>(),
            disassembly_forward.clone().upcast(),
        ],
    );
    let location_group = disassembly_control_group(
        "LOCATION",
        &[
            disassembly_location.clone().upcast::<gtk::Widget>(),
            disassembly_go.clone().upcast(),
            disassembly_pc.clone().upcast(),
        ],
    );
    location_group.set_hexpand(true);
    disassembly_navigation.append(&history_group);
    disassembly_navigation.append(&location_group);

    let function_group = disassembly_control_group(
        "FUNCTION",
        &[
            disassembly_previous.clone().upcast::<gtk::Widget>(),
            disassembly_next.clone().upcast(),
        ],
    );
    let view_group = disassembly_control_group(
        "VIEW",
        &[
            disassembly_mixed.clone().upcast::<gtk::Widget>(),
            disassembly_syntax_intel.clone().upcast(),
            disassembly_syntax_att.clone().upcast(),
        ],
    );
    let selected_group = disassembly_control_group(
        "SELECTED",
        &[
            disassembly_follow.clone().upcast::<gtk::Widget>(),
            disassembly_memory.clone().upcast(),
        ],
    );
    disassembly_actions.append(&function_group);
    disassembly_actions.append(&view_group);
    disassembly_actions.append(&selected_group);
    disassembly_browser.append(&disassembly_navigation);
    disassembly_browser.append(&disassembly_actions);
    let disassembly_controls = DisassemblyControls {
        back: disassembly_back,
        forward: disassembly_forward,
        previous_function: disassembly_previous,
        next_function: disassembly_next,
        location: disassembly_location,
        go: disassembly_go,
        current_pc: disassembly_pc,
        mixed: disassembly_mixed,
        syntax_intel: disassembly_syntax_intel,
        syntax_att: disassembly_syntax_att,
        follow: disassembly_follow,
        open_memory: disassembly_memory,
        range: disassembly_range,
        source_column,
        scrolled: instructions_scrolled.clone(),
        scroll_generation: Rc::new(Cell::new(0)),
        loading: Rc::new(Cell::new(false)),
        syntax_applicable: Rc::new(Cell::new(false)),
        setting_syntax: Rc::new(Cell::new(false)),
    };
    instructions_panel.append(&disassembly_browser);
    let instruction_insight = gtk::Box::new(gtk::Orientation::Vertical, 2);
    instruction_insight.add_css_class("instruction-insight");
    let instruction_flow = insight_label("Flow information appears at a branch or call");
    instruction_flow.add_css_class("instruction-flow-insight");
    let instruction_arguments = insight_label("");
    instruction_arguments.add_css_class("instruction-arguments-insight");
    let instruction_memory = insight_label("");
    instruction_memory.add_css_class("instruction-memory-insight");
    instruction_insight.append(&instruction_flow);
    instruction_insight.append(&instruction_arguments);
    instruction_insight.append(&instruction_memory);
    instructions_panel.append(&instruction_insight);
    instructions_panel.append(&instructions_empty);
    instructions_panel.append(&instructions_scrolled);
    context.set_start_child(Some(&locals_panel));
    context.set_end_child(Some(&instructions_panel));
    state.append(&context);

    let expression_watches_page = gtk::Box::new(gtk::Orientation::Vertical, 3);
    expression_watches_page.add_css_class("sidebar");
    let expression_watch_header = gtk::Box::new(gtk::Orientation::Horizontal, 3);
    expression_watch_header.add_css_class("subpanel-header");
    let expression_watch_entry = gtk::Entry::builder()
        .placeholder_text("counter, *node, vec.len, …")
        .hexpand(true)
        .build();
    expression_watch_entry.set_tooltip_text(Some(
        "Any C, C++, Rust, or GDB expression that is valid in the selected frame",
    ));
    let expression_watch_add_button = gtk::Button::with_label("Add");
    expression_watch_add_button.add_css_class("inline-action");
    expression_watch_add_button.set_sensitive(false);
    let expression_watch_remove_button = gtk::Button::with_label("Remove");
    expression_watch_remove_button.add_css_class("inline-action");
    expression_watch_remove_button.add_css_class("danger-action");
    expression_watch_remove_button.set_sensitive(false);
    expression_watch_header.append(&expression_watch_entry);
    expression_watch_header.append(&expression_watch_add_button);
    expression_watch_header.append(&expression_watch_remove_button);
    expression_watches_page.append(&expression_watch_header);
    let expression_watch_hint = gtk::Label::new(Some(
        "Structured values expand in place. Double-click a scalar value to edit it.",
    ));
    expression_watch_hint.add_css_class("muted");
    expression_watch_hint.set_halign(gtk::Align::Start);
    expression_watch_hint.set_wrap(true);
    expression_watches_page.append(&expression_watch_hint);
    let expression_watches_empty = empty_label("No watched expressions");
    expression_watches_page.append(&expression_watches_empty);
    let expression_watches_scrolled = gtk::ScrolledWindow::builder()
        .child(&expression_watches_view)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .build();
    expression_watches_page.append(&expression_watches_scrolled);

    let registers_page = gtk::Box::new(gtk::Orientation::Vertical, 2);
    registers_page.add_css_class("sidebar");
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
    let (stack_view, stack_store, stack_word_inspector) = build_stack_view();
    let stack_empty = empty_label("Stack values appear when the target is paused");
    let stack_scrolled = gtk::ScrolledWindow::builder()
        .child(&stack_view)
        .min_content_height(1)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .build();
    stack_page.append(&stack_empty);
    stack_page.append(&stack_scrolled);
    stack_page.append(&stack_word_inspector.root);

    let memory_page = gtk::Box::new(gtk::Orientation::Vertical, 0);
    memory_page.add_css_class("sidebar");
    let memory_controls = gtk::Box::new(gtk::Orientation::Vertical, 3);
    memory_controls.add_css_class("memory-watch-command");
    let memory_command_header = gtk::Box::new(gtk::Orientation::Horizontal, 3);
    let memory_command_title = section_title("MEMORY INSPECTOR");
    memory_command_title.set_hexpand(true);
    memory_command_header.append(&memory_command_title);
    let memory_refresh_all = gtk::Button::with_label("Refresh all");
    memory_refresh_all.add_css_class("inline-action");
    memory_refresh_all.set_tooltip_text(Some("Re-read every open memory inspector"));
    memory_refresh_all.set_sensitive(false);
    let memory_clear_all = gtk::Button::with_label("Close all");
    memory_clear_all.add_css_class("inline-action");
    memory_clear_all.add_css_class("danger-action");
    memory_clear_all.set_tooltip_text(Some("Close every memory inspector"));
    memory_clear_all.set_sensitive(false);
    memory_command_header.append(&memory_refresh_all);
    memory_command_header.append(&memory_clear_all);
    memory_controls.append(&memory_command_header);
    let expression_row = gtk::Box::new(gtk::Orientation::Horizontal, 3);
    let memory_address_entry = gtk::Entry::builder()
        .placeholder_text("$rsp, ptr + 0x20, or 0x404000")
        .hexpand(true)
        .build();
    memory_address_entry
        .set_tooltip_text(Some("Any GDB expression that resolves to a memory address"));
    let memory_add_button = gtk::Button::with_label("Inspect");
    memory_add_button.add_css_class("inline-action");
    memory_add_button.set_sensitive(false);
    expression_row.append(&memory_address_entry);
    expression_row.append(&memory_add_button);

    let memory_options = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    memory_options.add_css_class("memory-watch-options");
    memory_options.append(&section_title("LENGTH"));
    let memory_size = gtk::SpinButton::with_range(1.0, 4096.0, 1.0);
    memory_size.set_value(256.0);
    memory_size.set_width_chars(5);
    memory_size.set_tooltip_text(Some("Bytes to read"));
    let memory_size_unit = gtk::Label::new(Some("bytes"));
    memory_size_unit.add_css_class("muted");
    let memory_options_spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    memory_options_spacer.set_hexpand(true);
    memory_options.append(&memory_size);
    memory_options.append(&memory_size_unit);
    memory_options.append(&memory_options_spacer);
    memory_options.append(&section_title("DISPLAY"));
    let memory_format = gtk::DropDown::from_strings(&[
        "Hex bytes",
        "u16 / i16",
        "u32 / i32",
        "u64 / i64",
        "f32",
        "f64",
        "Pointers",
    ]);
    memory_format.set_selected(0);
    memory_format.set_tooltip_text(Some("How to group and render the memory values"));
    memory_options.append(&memory_format);
    memory_controls.append(&expression_row);
    memory_controls.append(&memory_options);
    memory_page.append(&memory_controls);

    let memory_watch_section = gtk::Box::new(gtk::Orientation::Vertical, 0);
    memory_watch_section.add_css_class("memory-inspector-section");
    let memory_watch_header = gtk::Box::new(gtk::Orientation::Horizontal, 3);
    memory_watch_header.add_css_class("subpanel-header");
    let memory_watch_title = section_title("OPEN INSPECTORS");
    memory_watch_title.set_hexpand(true);
    memory_watch_header.append(&memory_watch_title);
    let memory_watch_hint = gtk::Label::new(Some("changes are compared with the previous read"));
    memory_watch_hint.add_css_class("muted");
    memory_watch_header.append(&memory_watch_hint);
    memory_watch_section.append(&memory_watch_header);
    let memory_watches_empty = empty_label(
        "No memory inspectors. Enter an address or expression above, or open a mapping below.",
    );
    memory_watches_empty.set_vexpand(true);
    memory_watches_empty.set_hexpand(true);
    memory_watches_empty.set_halign(gtk::Align::Fill);
    memory_watches_empty.set_xalign(0.0);
    memory_watches_empty.set_wrap(false);
    memory_watches_empty.set_ellipsize(pango::EllipsizeMode::End);
    memory_watch_section.append(&memory_watches_empty);
    let memory_watch_notebook = gtk::Notebook::new();
    memory_watch_notebook.add_css_class("memory-watch-notebook");
    memory_watch_notebook.set_scrollable(true);
    memory_watch_notebook.set_show_border(false);
    memory_watch_notebook.set_vexpand(true);
    memory_watch_notebook.set_visible(false);
    memory_watch_section.append(&memory_watch_notebook);
    let memory_watch_container = MemoryWatchContainer {
        notebook: memory_watch_notebook,
        empty: memory_watches_empty,
        refresh_all: memory_refresh_all,
        clear_all: memory_clear_all,
    };

    let memory_map_section = gtk::Box::new(gtk::Orientation::Vertical, 0);
    memory_map_section.add_css_class("memory-map-section");
    let memory_map_header = gtk::Box::new(gtk::Orientation::Horizontal, 3);
    memory_map_header.add_css_class("subpanel-header");
    let memory_map_title = section_title("VIRTUAL MEMORY MAP");
    memory_map_title.set_hexpand(true);
    memory_map_header.append(&memory_map_title);
    let memory_map_hint = gtk::Label::new(Some("Double-click a mapping to inspect it"));
    memory_map_hint.add_css_class("muted");
    memory_map_header.append(&memory_map_hint);
    let memory_map_search = gtk::SearchEntry::builder()
        .placeholder_text("Filter mappings")
        .width_request(190)
        .build();
    memory_map_search.add_css_class("memory-map-search");
    memory_map_search.set_tooltip_text(Some(
        "Filter by address, permissions, register annotation, or backing path",
    ));
    memory_map_header.append(&memory_map_search);
    memory_map_section.append(&memory_map_header);
    let (memory_regions_view, memory_region_store) =
        build_memory_region_view(bindings.target_pointer_bits, &memory_map_search);
    let memory_regions_empty = empty_label("Mappings appear when the target is paused");
    let memory_regions_scrolled = gtk::ScrolledWindow::builder()
        .child(&memory_regions_view)
        .min_content_height(48)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .build();
    memory_map_section.append(&memory_regions_empty);
    memory_map_section.append(&memory_regions_scrolled);

    let memory_split = gtk::Paned::new(gtk::Orientation::Vertical);
    memory_split.add_css_class("memory-inspector-split");
    memory_split.set_shrink_start_child(false);
    memory_split.set_shrink_end_child(true);
    memory_split.set_resize_start_child(true);
    memory_split.set_start_child(Some(&memory_watch_section));
    memory_split.set_end_child(Some(&memory_map_section));
    memory_split.set_vexpand(true);
    memory_split.connect_position_notify(|split| {
        // GtkPaned otherwise lets the end child collapse completely. Keep the
        // map header, filter, and first table row reachable while still
        // allowing an actual 50/50 initial split on compact windows.
        const MINIMUM_MAP_HEIGHT: i32 = 92;
        let maximum = split.height().saturating_sub(MINIMUM_MAP_HEIGHT);
        if maximum > 0 && split.position() > maximum {
            split.set_position(maximum);
        }
    });
    memory_page.append(&memory_split);

    let breakpoints_page = gtk::Box::new(gtk::Orientation::Vertical, 3);
    breakpoints_page.add_css_class("sidebar");
    let hint = gtk::Label::new(Some(
        "Use the source gutter for line breakpoints, or Add breakpoint for advanced locations and behavior.",
    ));
    hint.add_css_class("muted");
    hint.set_halign(gtk::Align::Start);
    hint.set_wrap(true);
    breakpoints_page.append(&hint);
    let breakpoints_list = dynamic_list("No breakpoints, catchpoints, or watchpoints set");
    let breakpoints_scrolled = gtk::ScrolledWindow::builder()
        .child(&breakpoints_list)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build();
    let breakpoint_bulk_actions = gtk::Box::new(gtk::Orientation::Horizontal, 2);
    let add_breakpoint_button = gtk::Button::with_label("Add breakpoint");
    add_breakpoint_button.add_css_class("inline-action");
    add_breakpoint_button.add_css_class("primary-control");
    add_breakpoint_button.set_tooltip_text(Some(
        "Add a breakpoint by function, address, source line, or regular expression",
    ));
    add_breakpoint_button.set_sensitive(false);
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
    let breakpoint_bulk_spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    breakpoint_bulk_spacer.set_hexpand(true);
    breakpoint_bulk_actions.append(&add_breakpoint_button);
    breakpoint_bulk_actions.append(&breakpoint_bulk_spacer);
    breakpoint_bulk_actions.append(&delete_all_breakpoints_button);
    breakpoint_bulk_actions.append(&delete_all_watchpoints_button);
    breakpoint_bulk_actions.append(&delete_all_catchpoints_button);
    breakpoints_page.append(&breakpoint_bulk_actions);
    breakpoints_page.append(&breakpoints_scrolled);

    let watchpoint_section = gtk::Box::new(gtk::Orientation::Vertical, 4);
    watchpoint_section.add_css_class("breakpoint-tool-section");
    watchpoint_section.append(&section_title("ADD WATCHPOINT"));
    let watchpoint_controls = gtk::Box::new(gtk::Orientation::Horizontal, 3);
    watchpoint_controls.add_css_class("watchpoint-controls");
    let watchpoint_expression = gtk::Entry::builder()
        .placeholder_text("variable or address expression")
        .hexpand(true)
        .build();
    watchpoint_expression.set_tooltip_text(Some("Examples: counter, *pointer, *(int*)0x404040"));
    let watchpoint_access = gtk::DropDown::from_strings(&["Write", "Read", "Access"]);
    watchpoint_access.add_css_class("watchpoint-access");
    watchpoint_access.set_selected(0);
    watchpoint_access.set_tooltip_text(Some(
        "Stop on writes, reads, or either kind of memory access",
    ));
    let watchpoint_add_button = gtk::Button::with_label("Add");
    watchpoint_add_button.add_css_class("inline-action");
    watchpoint_add_button.add_css_class("watchpoint-add-action");
    watchpoint_add_button.set_tooltip_text(Some("Add this watchpoint"));
    watchpoint_add_button.set_sensitive(false);
    watchpoint_controls.append(&watchpoint_expression);
    watchpoint_controls.append(&watchpoint_access);
    watchpoint_controls.append(&watchpoint_add_button);
    watchpoint_section.append(&watchpoint_controls);
    breakpoints_page.append(&watchpoint_section);

    let catchpoint_section = gtk::Box::new(gtk::Orientation::Vertical, 4);
    catchpoint_section.add_css_class("breakpoint-tool-section");
    catchpoint_section.append(&section_title("QUICK CATCHPOINTS"));
    let event_catchpoint_grid = gtk::Grid::builder()
        .column_spacing(3)
        .row_spacing(3)
        .column_homogeneous(true)
        .build();
    let event_catchpoint_buttons = EventCatchpoint::ALL
        .into_iter()
        .enumerate()
        .map(|(index, (event, label, tooltip))| {
            let button = gtk::Button::with_label(label);
            button.add_css_class("signal-action");
            button.add_css_class("catchpoint-action");
            button.set_tooltip_text(Some(tooltip));
            button.set_sensitive(false);
            event_catchpoint_grid.attach(&button, (index % 3) as i32, (index / 3) as i32, 1, 1);
            (button, event)
        })
        .collect::<Vec<_>>();
    catchpoint_section.append(&event_catchpoint_grid);
    breakpoints_page.append(&catchpoint_section);

    let signals_content = gtk::Box::new(gtk::Orientation::Vertical, 4);
    signals_content.add_css_class("sidebar");
    let current_signal_section = gtk::Box::new(gtk::Orientation::Vertical, 4);
    current_signal_section.add_css_class("signal-tool-section");
    current_signal_section.append(&section_title("CURRENT STOP"));
    let signal_detail = gtk::Label::new(Some("No signal at the current stop"));
    signal_detail.add_css_class("signal-detail");
    signal_detail.set_halign(gtk::Align::Fill);
    signal_detail.set_wrap(true);
    signal_detail.set_xalign(0.0);
    current_signal_section.append(&signal_detail);
    signals_content.append(&current_signal_section);

    let common_signal_section = gtk::Box::new(gtk::Orientation::Vertical, 4);
    common_signal_section.add_css_class("signal-tool-section");
    let signal_actions_header = gtk::Box::new(gtk::Orientation::Horizontal, 3);
    let signal_actions_title = section_title("COMMON CATCHPOINTS");
    signal_actions_title.set_hexpand(true);
    let delete_all_signal_catchpoints_button = gtk::Button::with_label("Clear catches");
    delete_all_signal_catchpoints_button.add_css_class("inline-action");
    delete_all_signal_catchpoints_button.add_css_class("danger-action");
    delete_all_signal_catchpoints_button.add_css_class("signal-clear-action");
    delete_all_signal_catchpoints_button.set_tooltip_text(Some(
        "Delete every signal catchpoint without affecting breakpoints or watchpoints",
    ));
    delete_all_signal_catchpoints_button.set_sensitive(false);
    signal_actions_header.append(&signal_actions_title);
    signal_actions_header.append(&delete_all_signal_catchpoints_button);
    common_signal_section.append(&signal_actions_header);
    let signal_hint = gtk::Label::new(Some(
        "Click a signal to add its catchpoint, active signals are green and click again removes them.",
    ));
    signal_hint.add_css_class("muted");
    signal_hint.set_halign(gtk::Align::Start);
    signal_hint.set_wrap(true);
    common_signal_section.append(&signal_hint);
    let (common_signal_grid, mut signal_buttons) = build_signal_grid(COMMON_SIGNALS);
    common_signal_section.append(&common_signal_grid);
    signals_content.append(&common_signal_section);

    let (more_signal_grid, mut more_signal_buttons) = build_signal_grid(MORE_SIGNALS);
    signal_buttons.append(&mut more_signal_buttons);
    let more_signal_section = build_disclosure(
        "MORE POSIX SIGNALS",
        &more_signal_grid,
        false,
        "signal-disclosure",
    );
    more_signal_section.add_css_class("signal-tool-section");
    signals_content.append(&more_signal_section);

    let custom_signal_section = gtk::Box::new(gtk::Orientation::Vertical, 4);
    custom_signal_section.add_css_class("signal-tool-section");
    custom_signal_section.append(&section_title("CUSTOM SIGNAL"));
    let custom_signal_row = gtk::Box::new(gtk::Orientation::Horizontal, 3);
    custom_signal_row.add_css_class("custom-signal-controls");
    let signal_entry = gtk::Entry::builder()
        .placeholder_text("SIGRTMIN+1 or 35")
        .hexpand(true)
        .build();
    signal_entry.set_tooltip_text(Some(
        "Signal name or number. Names without the SIG prefix are normalized",
    ));
    let signal_add_button = gtk::Button::with_label("Toggle catch");
    signal_add_button.add_css_class("inline-action");
    signal_add_button.add_css_class("signal-toggle-action");
    signal_add_button.set_sensitive(false);
    custom_signal_row.append(&signal_entry);
    custom_signal_row.append(&signal_add_button);
    custom_signal_section.append(&custom_signal_row);
    signals_content.append(&custom_signal_section);
    let signals_page = gtk::ScrolledWindow::builder()
        .child(&signals_content)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build();
    let kernel_view = build_kernel_view(&bindings.kernel);
    let misc_view = build_misc_view();

    append_responsive_inspector_page(&notebook, &state, "Context");
    append_responsive_inspector_page(&notebook, &expression_watches_page, "Watches");
    append_responsive_inspector_page(&notebook, &registers_page, "Registers");
    append_responsive_inspector_page(&notebook, &stack_page, "Stack");
    append_responsive_inspector_page(&notebook, &memory_page, "Memory");
    append_responsive_inspector_page(&notebook, &breakpoints_page, "Breakpoints");
    append_responsive_inspector_page(&notebook, &signals_page, "Signals");
    let kernel_page =
        notebook.append_page(&kernel_view.root, Some(&gtk::Label::new(Some("Kernel"))));
    let misc_page = notebook.append_page(&misc_view.root, Some(&gtk::Label::new(Some("Misc"))));
    let compact_tabs = build_compact_inspector_navigation(&notebook);
    let notebook_for_selection = notebook.clone();
    notebook.connect_switch_page(move |_, _, _| {
        let notebook = notebook_for_selection.clone();
        glib::idle_add_local_once(move || clear_label_selections(&notebook));
    });
    connect_kernel_tab_visibility(
        &notebook,
        kernel_page,
        &kernel_view,
        bindings.kernel.refresh_handler,
    );
    connect_misc_tab_visibility(
        &notebook,
        misc_page,
        &misc_view,
        bindings.misc.refresh_handler,
    );
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.set_size_request(0, 0);
    root.set_vexpand(true);
    root.append(&compact_tabs);
    root.append(&notebook);
    Inspector {
        root,
        notebook,
        compact_tabs,
        context_split: context,
        status_detail: detail,
        locals_store,
        locals_selection,
        locals_view,
        locals_empty,
        locals_edit_button,
        expression_watches_store,
        expression_watches_selection,
        expression_watches_view,
        expression_watches_empty,
        expression_watch_entry,
        expression_watch_add_button,
        expression_watch_remove_button,
        instructions_title,
        instructions_store,
        instructions_selection,
        instructions_view,
        instructions_empty,
        instruction_flow,
        instruction_arguments,
        instruction_memory,
        disassembly_controls,
        register_groups,
        registers_empty,
        stack_store,
        stack_empty,
        breakpoints_list,
        add_breakpoint_button,
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
        memory_regions_view,
        memory_regions_empty,
        memory_watch_container,
        memory_split,
        memory_address_entry,
        memory_size,
        memory_format,
        memory_add_button,
        kernel_view,
        misc_view,
    }
}

fn append_responsive_inspector_page(
    notebook: &gtk::Notebook,
    child: &impl IsA<gtk::Widget>,
    title: &str,
) -> u32 {
    let viewport = gtk::ScrolledWindow::new();
    viewport.add_css_class("inspector-page-viewport");
    viewport.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Never);
    viewport.set_overlay_scrolling(true);
    viewport.set_propagate_natural_width(false);
    viewport.set_min_content_width(0);
    viewport.set_size_request(0, 0);
    viewport.set_hexpand(true);
    viewport.set_vexpand(true);
    viewport.set_child(Some(child));
    notebook.append_page(&viewport, Some(&gtk::Label::new(Some(title))))
}

fn build_compact_inspector_navigation(notebook: &gtk::Notebook) -> gtk::Box {
    const PAGES: [&str; 9] = [
        "Context",
        "Watches",
        "Registers",
        "Stack",
        "Memory",
        "Breakpoints",
        "Signals",
        "Kernel",
        "Misc",
    ];
    let previous = gtk::Button::with_label("‹");
    previous.add_css_class("kernel-tab-nav-button");
    previous.set_tooltip_text(Some("Open the previous inspector"));
    let selector = gtk::DropDown::from_strings(&PAGES);
    selector.add_css_class("kernel-compact-tab-selector");
    selector.set_hexpand(true);
    selector.set_selected(notebook.current_page().unwrap_or(0));
    selector.set_tooltip_text(Some("Select an inspector"));
    let next = gtk::Button::with_label("›");
    next.add_css_class("kernel-tab-nav-button");
    next.set_tooltip_text(Some("Open the next inspector"));
    let root = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    root.add_css_class("kernel-tab-navigation");
    root.add_css_class("kernel-compact-tab-navigation");
    root.set_hexpand(true);
    root.append(&previous);
    root.append(&selector);
    root.append(&next);
    root.set_visible(false);

    let notebook_for_selector = notebook.clone();
    selector.connect_selected_notify(move |selector| {
        let page = selector.selected();
        if page != gtk::INVALID_LIST_POSITION {
            notebook_for_selector.set_current_page(Some(page));
        }
    });
    let notebook_for_previous = notebook.clone();
    previous.connect_clicked(move |_| {
        let page = notebook_for_previous.current_page().unwrap_or(0);
        notebook_for_previous.set_current_page(Some(page.saturating_sub(1)));
    });
    let notebook_for_next = notebook.clone();
    next.connect_clicked(move |_| {
        let page = notebook_for_next.current_page().unwrap_or(0);
        notebook_for_next.set_current_page(Some((page + 1).min(PAGES.len() as u32 - 1)));
    });
    let selector_for_page = selector.clone();
    let previous_for_page = previous.clone();
    let next_for_page = next.clone();
    let update = move |page: u32| {
        selector_for_page.set_selected(page);
        previous_for_page.set_sensitive(page > 0);
        next_for_page.set_sensitive(page + 1 < PAGES.len() as u32);
    };
    update(notebook.current_page().unwrap_or(0));
    notebook.connect_switch_page(move |_, _, page| update(page));
    root
}

fn compact_instruction_button(label: &str, tooltip: &str) -> gtk::Button {
    let button = gtk::Button::with_label(label);
    button.add_css_class("inline-action");
    button.set_tooltip_text(Some(tooltip));
    button
}

fn disassembly_control_group(label: &str, widgets: &[gtk::Widget]) -> gtk::Box {
    let group = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    group.add_css_class("disassembly-control-group");
    let caption = gtk::Label::new(Some(label));
    caption.add_css_class("disassembly-control-label");
    group.append(&caption);
    for widget in widgets {
        group.append(widget);
    }
    group
}
