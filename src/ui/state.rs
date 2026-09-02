use super::*;

impl Ui {
    pub fn build(application: &gtk::Application, config: &LaunchConfig, theme: &Theme) -> Self {
        if let Some(display) = gtk::gdk::Display::default() {
            gtk::IconTheme::for_display(&display)
                .add_resource_path(&format!("{}/icons", crate::RESOURCE_PREFIX));
        }
        gtk::Window::set_default_icon_name(crate::APPLICATION_ID);
        let window = gtk::ApplicationWindow::builder()
            .application(application)
            .title("fgdb")
            .icon_name(crate::APPLICATION_ID)
            .default_width(1380)
            .default_height(820)
            .build();
        crate::install_window_icon(&window);
        window.add_css_class("fgdb-window");

        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("debugger-root");
        let terminal = build_terminal(theme);
        let topbar = build_topbar(config, &window, &terminal);
        window.set_titlebar(Some(&topbar.root));

        let source_style_scheme = theme.source_style_scheme();
        let source_notebook = build_source_notebook(source_style_scheme.as_ref());
        let source_documents = Rc::new(RefCell::new(Vec::new()));
        let breakpoints = Rc::new(RefCell::new(Vec::new()));
        let variable_children_handler = Rc::new(RefCell::new(None));
        let variable_viewer_handler = Rc::new(RefCell::new(None));
        let variable_viewers = Rc::new(VariableViewerRegistry::with_builtins());
        let kernel_refresh_handler = Rc::new(RefCell::new(None));
        let misc_refresh_handler = Rc::new(RefCell::new(None));
        let kernel_section_handler = Rc::new(RefCell::new(None));
        let remembered_disclosures = layout::remembered_disclosures();
        let target_pointer_bits = Rc::new(Cell::new(usize::BITS));
        let target_pointer_bits_known = Rc::new(Cell::new(false));
        let inspector_bindings = InspectorBindings {
            theme,
            variable_children_handler: &variable_children_handler,
            variable_viewer_handler: &variable_viewer_handler,
            variable_viewers: &variable_viewers,
            target_pointer_bits: &target_pointer_bits,
            kernel: KernelViewBindings {
                refresh_handler: &kernel_refresh_handler,
                remembered_disclosures: &remembered_disclosures,
                section_handler: &kernel_section_handler,
            },
            misc: MiscViewBindings {
                refresh_handler: &misc_refresh_handler,
            },
        };

        let workspace = build_workspace(
            &source_notebook,
            &terminal,
            &topbar.gef_tools_button,
            &inspector_bindings,
        );
        root.append(&workspace.root);
        let workspace_footer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        workspace_footer.add_css_class("workspace-footer");
        workspace_footer.append(&topbar.terminal_toggle_button);
        workspace.status_detail.set_hexpand(true);
        workspace_footer.append(&workspace.status_detail);
        root.append(&workspace_footer);
        let terminal_panel = workspace.terminal_panel.clone();
        let terminal_for_toggle = terminal.clone();
        let layout = layout::Persistence::install(&window, workspace.layout_panes.clone());
        layout.bind_notebook("left_sidebar", &workspace.left_navigation);
        let terminal_visible = layout.terminal_visible();
        topbar.terminal_toggle_button.set_active(terminal_visible);
        terminal_panel.set_visible(terminal_visible);
        let layout_for_terminal = layout.clone();
        topbar
            .terminal_toggle_button
            .connect_toggled(move |button| {
                let visible = button.is_active();
                terminal_panel.set_visible(visible);
                layout_for_terminal.set_terminal_visible(visible);
                if visible {
                    terminal_for_toggle.grab_focus();
                }
            });
        window.set_child(Some(&root));
        kernel_section_handler.replace(Some(layout.disclosure_handler()));
        let initial_session = config.initial_session();
        let source_base_roots = source::roots(config);
        let source_tree_base_roots = source::search_roots(config);

        let ui = Self {
            window,
            terminal,
            session_button: topbar.session_button,
            session_popover: topbar.session_popover,
            session_kind_label: topbar.session_kind_label,
            session_target_label: topbar.session_target_label,
            new_session_button: topbar.new_session_button,
            restart_session_button: topbar.restart_session_button,
            kill_session_button: topbar.kill_session_button,
            detach_session_button: topbar.detach_session_button,
            restart_gdb_button: topbar.restart_gdb_button,
            resynchronize_button: topbar.resynchronize_button,
            configuration_button: topbar.configuration_button,
            gdb_capabilities_label: topbar.gdb_capabilities_label,
            target_label: topbar.target_label,
            terminal_toggle_button: topbar.terminal_toggle_button,
            debug_data_button: topbar.debug_data_button,
            run_button: topbar.run_button,
            pause_button: topbar.pause_button,
            next_button: topbar.next_button,
            step_button: topbar.step_button,
            next_instruction_button: topbar.next_instruction_button,
            step_instruction_button: topbar.step_instruction_button,
            finish_button: topbar.finish_button,
            until_button: topbar.until_button,
            until_popover: topbar.until_popover,
            gef_tools_button: topbar.gef_tools_button,
            gef_tools_content: topbar.gef_tools_content,
            gef_tool_controls: topbar.gef_tool_controls,
            gef_tool_groups: topbar.gef_tool_groups,
            status_label: topbar.status_label,
            status_detail: workspace.status_detail,
            status_visual_generation: Rc::new(Cell::new(0)),
            pause_visual_generation: Rc::new(Cell::new(0)),
            inspector_notebook: workspace.inspector_notebook,
            source_notebook,
            source_documents,
            source_navigation: workspace.source_navigation,
            source_tree: workspace.source_tree,
            source_back_history: Rc::new(RefCell::new(Vec::new())),
            source_forward_history: Rc::new(RefCell::new(Vec::new())),
            closed_source_tabs: Rc::new(RefCell::new(Vec::new())),
            source_find_state: Rc::new(RefCell::new(None)),
            source_palette: Rc::new(RefCell::new(None)),
            source_palette_generation: Arc::new(AtomicU64::new(0)),
            source_loaded_generation: Arc::new(AtomicU64::new(0)),
            source_loaded_cache: Rc::new(RefCell::new(None)),
            loaded_source_files: Rc::new(RefCell::new(Vec::new())),
            source_tree_roots: Rc::new(RefCell::new(source_tree_base_roots.clone())),
            source_tree_base_roots,
            source_tree_cache: Rc::new(RefCell::new(None)),
            source_tree_indexing: Rc::new(Cell::new(false)),
            source_tree_generation: Arc::new(AtomicU64::new(0)),
            source_tree_render_generation: Arc::new(AtomicU64::new(0)),
            source_tree_initialized: Rc::new(Cell::new(false)),
            execution_source_path: Rc::new(RefCell::new(None)),
            execution_source_line: Rc::new(Cell::new(None)),
            source_theme: theme.clone(),
            source_style_scheme,
            resolved_source_paths: Rc::new(RefCell::new(HashMap::new())),
            call_stack_list: workspace.call_stack_list,
            frame_buttons: Rc::new(RefCell::new(Vec::new())),
            latest_frames: Rc::new(RefCell::new(Vec::new())),
            latest_frames_generation: Rc::new(Cell::new(None)),
            selected_frame_level: Rc::new(Cell::new(0)),
            threads_list: workspace.threads_list,
            thread_controls: workspace.thread_controls,
            thread_buttons: Rc::new(RefCell::new(Vec::new())),
            latest_threads: Rc::new(RefCell::new(None)),
            selected_thread_id: Rc::new(RefCell::new(None)),
            scheduler_locking: Rc::new(Cell::new(None)),
            non_stop_mode: Rc::new(Cell::new(None)),
            thread_policy_generation: Rc::new(Cell::new(0)),
            modules_list: workspace.modules_list,
            latest_modules: Rc::new(RefCell::new(Vec::new())),
            module_debug_metadata: Rc::new(RefCell::new(HashMap::new())),
            module_debug_generation: Arc::new(AtomicU64::new(0)),
            module_debug_worker_active: Arc::new(AtomicBool::new(false)),
            module_debug_force_pending: Arc::new(AtomicBool::new(false)),
            inferior_controls: workspace.inferior_controls,
            inferiors: Rc::new(RefCell::new(Vec::new())),
            thread_inferior_ids: Rc::new(RefCell::new(HashMap::new())),
            selected_inferior_id: Rc::new(RefCell::new(None)),
            stop_owner_inferior_id: Rc::new(RefCell::new(None)),
            stop_owner_thread_id: Rc::new(RefCell::new(None)),
            inferior_parents: Rc::new(RefCell::new(HashMap::new())),
            pending_fork_parents: Rc::new(RefCell::new(HashMap::new())),
            inferior_refresh_generation: Rc::new(Cell::new(0)),
            execution_context_visual_generation: Rc::new(Cell::new(0)),
            execution_context_visual_pending: Rc::new(Cell::new(false)),
            fork_policy_generation: Rc::new(Cell::new(0)),
            fork_follow_mode: Rc::new(Cell::new(None)),
            detach_on_fork: Rc::new(Cell::new(None)),
            inferior_action_pending: Rc::new(Cell::new(None)),
            inferior_execution_generation: Rc::new(Cell::new(0)),
            pending_execution_inferior: Rc::new(RefCell::new(None)),
            active_thread_execution: Rc::new(RefCell::new(None)),
            thread_execution_exit_candidate: Rc::new(RefCell::new(None)),
            locals_store: workspace.locals_store,
            locals_selection: workspace.locals_selection,
            locals_view: workspace.locals_view,
            locals_empty: workspace.locals_empty,
            locals_summary: workspace.locals_summary,
            locals_edit_button: workspace.locals_edit_button,
            expression_watches_store: workspace.expression_watches_store,
            expression_watches_selection: workspace.expression_watches_selection,
            expression_watches_view: workspace.expression_watches_view,
            expression_watches_empty: workspace.expression_watches_empty,
            expression_watches: Rc::new(RefCell::new(Vec::new())),
            deferred_variable_object_deletions: Rc::new(RefCell::new(HashSet::new())),
            pending_local_variable_objects: Rc::new(RefCell::new(HashSet::new())),
            expression_watch_entry: workspace.expression_watch_entry,
            expression_watch_add_button: workspace.expression_watch_add_button,
            expression_watch_remove_button: workspace.expression_watch_remove_button,
            target_pointer_bits,
            target_pointer_bits_known,
            target_architecture: Rc::new(Cell::new(TargetArchitecture::Unknown)),
            target_endian: Rc::new(Cell::new(None)),
            current_source_is_rust: Rc::new(Cell::new(false)),
            instructions_title: workspace.instructions_title,
            instructions_store: workspace.instructions_store,
            instructions_selection: workspace.instructions_selection,
            instructions_view: workspace.instructions_view,
            instructions_empty: workspace.instructions_empty,
            instruction_flow: workspace.instruction_flow,
            instruction_arguments: workspace.instruction_arguments,
            instruction_memory: workspace.instruction_memory,
            disassembly_controls: workspace.disassembly_controls,
            current_instruction: Rc::new(RefCell::new(None)),
            call_abi_instruction: Rc::new(RefCell::new(None)),
            call_abi_instruction_generation: Rc::new(Cell::new(None)),
            current_instruction_memory_expression: Rc::new(RefCell::new(None)),
            latest_registers: Rc::new(RefCell::new(Vec::new())),
            latest_registers_generation: Rc::new(Cell::new(None)),
            register_details_generation: Rc::new(Cell::new(None)),
            instruction_memory_handler: Rc::new(RefCell::new(None)),
            disassembly_handler: Rc::new(RefCell::new(None)),
            disassembly_source_cache: Rc::new(RefCell::new(HashMap::new())),
            register_groups: workspace.register_groups,
            registers_empty: workspace.registers_empty,
            stack_store: workspace.stack_store,
            latest_stack: Rc::new(RefCell::new(Vec::new())),
            displayed_stack: Rc::new(RefCell::new(Vec::new())),
            latest_stack_generation: Rc::new(Cell::new(None)),
            stack_memory_refresh_generation: Rc::new(Cell::new(None)),
            stack_details_generation: Rc::new(Cell::new(None)),
            stack_empty: workspace.stack_empty,
            breakpoints_list: workspace.breakpoints_list,
            stop_point_filter: workspace.stop_point_filter,
            add_breakpoint_button: workspace.add_breakpoint_button,
            delete_all_breakpoints_button: workspace.delete_all_breakpoints_button,
            delete_all_watchpoints_button: workspace.delete_all_watchpoints_button,
            delete_all_catchpoints_button: workspace.delete_all_catchpoints_button,
            event_catchpoint_buttons: workspace.event_catchpoint_buttons,
            watchpoint_expression: workspace.watchpoint_expression,
            watchpoint_access: workspace.watchpoint_access,
            watchpoint_mask: workspace.watchpoint_mask,
            watchpoint_add_button: workspace.watchpoint_add_button,
            filtered_catchpoint: workspace.filtered_catchpoint,
            signal_detail: workspace.signal_detail,
            signal_buttons: workspace.signal_buttons,
            signal_entry: workspace.signal_entry,
            signal_add_button: workspace.signal_add_button,
            delete_all_signal_catchpoints_button: workspace.delete_all_signal_catchpoints_button,
            until_actions: topbar.until_actions,
            until_condition_entry: topbar.until_condition_entry,
            until_condition_button: topbar.until_condition_button,
            memory_region_store: workspace.memory_region_store,
            memory_regions_view: workspace.memory_regions_view,
            memory_regions_empty: workspace.memory_regions_empty,
            memory_regions: Rc::new(RefCell::new(Vec::new())),
            memory_regions_generation: Rc::new(Cell::new(None)),
            memory_watches_refresh_generation: Rc::new(Cell::new(None)),
            tls_runtime_refresh_generation: Rc::new(Cell::new(None)),
            memory_watches: Rc::new(RefCell::new(Vec::new())),
            memory_watch_container: workspace.memory_watch_container,
            memory_address_entry: workspace.memory_address_entry,
            memory_size: workspace.memory_size,
            memory_format: workspace.memory_format,
            memory_add_button: workspace.memory_add_button,
            memory_watch_handler: Rc::new(RefCell::new(None)),
            kernel_view: workspace.kernel_view,
            kernel_refresh_handler,
            kernel_refresh_generation: Rc::new(Cell::new(0)),
            misc_view: workspace.misc_view,
            misc_refresh_handler,
            misc_refresh_generation: Rc::new(Cell::new(0)),
            debugger_pid: Rc::new(Cell::new(None)),
            inferior_pid: Rc::new(Cell::new(None)),
            layout,
            breakpoints,
            stop_point_filter_rows: Rc::new(RefCell::new(Vec::new())),
            stop_point_metadata: Rc::new(RefCell::new(HashMap::new())),
            previous_registers: Rc::new(RefCell::new(HashMap::new())),
            cached_register_names: Rc::new(RefCell::new(None)),
            stop_refresh_generation: Rc::new(Cell::new(0)),
            thread_refresh_generation: Rc::new(Cell::new(0)),
            breakpoint_refresh_generation: Rc::new(Cell::new(0)),
            breakpoint_refresh_gate: Rc::new(RefreshGate::default()),
            module_refresh_gate: Rc::new(RefreshGate::default()),
            modules_dirty: Rc::new(Cell::new(false)),
            command_pending: Rc::new(Cell::new(false)),
            debugger_state: Rc::new(Cell::new(DebuggerState::default())),
            execution_transition_generation: Rc::new(Cell::new(0)),
            session_pending: Rc::new(Cell::new(false)),
            applied_control_state: Rc::new(RefCell::new(None)),
            gef_available: Rc::new(Cell::new(false)),
            gef_capabilities: Rc::new(RefCell::new(HashSet::new())),
            gef_context_control: Rc::new(Cell::new(GefContextControl::None)),
            gef_context_visible: config.gef_context_visible,
            gef_context_hidden_by_fgdb: Rc::new(Cell::new(false)),
            heap_inspection_handler: Rc::new(RefCell::new(None)),
            source_roots: Rc::new(RefCell::new(source_base_roots.clone())),
            source_base_roots,
            current_session: Rc::new(RefCell::new(initial_session)),
            gdb_capabilities: Rc::new(RefCell::new(GdbCapabilities::default())),
            gdb_recovery_available: Rc::new(Cell::new(false)),
            configuration_report: config.configuration_report().clone(),
            configuration_dialog: Rc::new(RefCell::new(None)),
            debug_data_view: Rc::new(RefCell::new(None)),
            debug_data_state: Rc::new(RefCell::new(debug_data::DebugDataState::default())),
            debug_data_generation: Rc::new(Cell::new(0)),
            debug_data_action_handler: Rc::new(RefCell::new(None)),
            session_handler: Rc::new(RefCell::new(None)),
            session_action_handler: Rc::new(RefCell::new(None)),
            until_action_handler: Rc::new(RefCell::new(None)),
            until_cancel_handler: Rc::new(RefCell::new(None)),
            until_abort_handler: Rc::new(RefCell::new(None)),
            until_stop_handler: Rc::new(RefCell::new(None)),
            native_until_active: Rc::new(Cell::new(false)),
            frame_selection_handler: Rc::new(RefCell::new(None)),
            thread_selection_handler: Rc::new(RefCell::new(None)),
            instruction_handler: Rc::new(RefCell::new(None)),
            variable_editor_handler: Rc::new(RefCell::new(None)),
            variable_assignment_handler: Rc::new(RefCell::new(None)),
            float_assignment_handler: Rc::new(RefCell::new(None)),
            string_assignment_handler: Rc::new(RefCell::new(None)),
            variable_children_handler,
            variable_viewer_handler,
            variable_viewer_windows: Rc::new(RefCell::new(Vec::new())),
            expression_watch_refresh_handler: Rc::new(RefCell::new(None)),
            vector_assignment_handler: Rc::new(RefCell::new(None)),
            breakpoint_insert_handler: Rc::new(RefCell::new(None)),
            source_jump_handler: Rc::new(RefCell::new(None)),
            breakpoint_delete_handler: Rc::new(RefCell::new(None)),
            breakpoint_condition_handler: Rc::new(RefCell::new(None)),
            breakpoint_editor_handler: Rc::new(RefCell::new(None)),
            breakpoint_enabled_handler: Rc::new(RefCell::new(None)),
            breakpoint_bulk_delete_handler: Rc::new(RefCell::new(None)),
            signal_catchpoint_handler: Rc::new(RefCell::new(None)),
            event_catchpoint_handler: Rc::new(RefCell::new(None)),
            filtered_catchpoint_handler: Rc::new(RefCell::new(None)),
            watchpoint_insert_handler: Rc::new(RefCell::new(None)),
            source_symbol_handler: Rc::new(RefCell::new(None)),
            source_discovery_handler: Rc::new(RefCell::new(None)),
            thread_stop_reason: Rc::new(RefCell::new(None)),
            debugger_ready: Rc::new(Cell::new(false)),
        };
        ui.connect_instruction_activation();
        ui.connect_disassembly_controls();
        ui.connect_local_activation();
        ui.connect_expression_watch_controls();
        ui.connect_register_activation();
        ui.connect_memory_controls();
        ui.connect_watchpoint_controls();
        ui.connect_breakpoint_bulk_controls();
        ui.connect_event_catchpoint_controls();
        ui.connect_filtered_catchpoint_controls();
        ui.connect_keyboard_shortcuts();
        ui.update_session_display();
        ui
    }

    pub fn save_layout(&self) {
        self.layout.save();
    }

    pub fn connect_gdb_recovery_handler(&self, handler: impl Fn() + 'static) {
        let popover = self.session_popover.clone();
        self.restart_gdb_button.connect_clicked(move |_| {
            popover.popdown();
            handler();
        });
    }

    pub fn connect_resynchronize_handler(&self, handler: impl Fn() + 'static) {
        let popover = self.session_popover.clone();
        self.resynchronize_button.connect_clicked(move |_| {
            popover.popdown();
            handler();
        });
    }

    pub fn set_gdb_recovery_available(&self, available: bool) {
        if self.gdb_recovery_available.replace(available) == available {
            return;
        }
        self.restart_gdb_button.set_visible(available);
        self.update_control_sensitivity();
    }

    pub(crate) fn gdb_recovery_required(&self) -> bool {
        self.gdb_recovery_available.get()
    }

    pub fn set_gdb_capabilities(&self, capabilities: GdbCapabilities) {
        let summary = capabilities.compatibility_summary();
        let feature_detail = if capabilities.features_known {
            if capabilities.features.is_empty() {
                String::from("GDB returned an empty MI feature list")
            } else {
                format!("GDB/MI features: {}", capabilities.features.join(", "))
            }
        } else {
            String::from("This GDB did not expose an MI feature list")
        };
        let printer_detail = if !capabilities.pretty_printing {
            "Dynamic pretty printing is unavailable in this GDB build"
        } else if capabilities.rust_pretty_printing {
            "The matching Rust toolchain pretty-printers are loaded"
        } else {
            "Dynamic pretty printing is enabled; no Rust toolchain printer is currently loaded"
        };
        let tooltip = format!("{printer_detail}. {feature_detail}");
        self.gdb_capabilities_label.set_text(&summary);
        self.gdb_capabilities_label.set_tooltip_text(Some(&tooltip));
        self.gdb_capabilities.replace(capabilities);
    }

    pub(crate) fn gdb_capabilities(&self) -> GdbCapabilities {
        self.gdb_capabilities.borrow().clone()
    }

    pub fn clear_gdb_capabilities(&self) {
        self.gdb_capabilities.replace(GdbCapabilities::default());
        self.gdb_capabilities_label
            .set_text("GDB capabilities unavailable");
        self.gdb_capabilities_label.set_tooltip_text(None);
    }

    pub fn prepare_full_resynchronization(&self) {
        self.debugger_state
            .set(self.debugger_state.get().with_resynchronizing(true));
        self.reset_target_abi();
        self.invalidate_allocator_probe_cache();
        self.previous_registers.borrow_mut().clear();
        self.disassembly_source_cache.borrow_mut().clear();
        self.start_stop_refresh();
        self.start_thread_refresh();
        self.invalidate_kernel_refresh();
        self.invalidate_misc_refresh();
        self.update_control_sensitivity();
        self.update_thread_control_sensitivity();
        self.render_inferior_controls();
    }

    pub fn finish_full_resynchronization(&self) -> bool {
        let state = self.debugger_state.get();
        let pending = state.resynchronizing();
        self.debugger_state.set(state.with_resynchronizing(false));
        if pending {
            self.update_control_sensitivity();
            self.update_thread_control_sensitivity();
            self.render_inferior_controls();
        }
        pending
    }

    pub fn set_debugger_pid(&self, pid: Option<u32>) {
        self.debugger_pid.set(pid);
    }

    pub fn debugger_pid(&self) -> Option<u32> {
        self.debugger_pid.get()
    }

    pub fn set_inferior_pid(&self, pid: Option<u32>) {
        self.inferior_pid.set(pid);
    }

    pub fn inferior_pid(&self) -> Option<u32> {
        self.inferior_pid.get()
    }

    pub fn set_target_endian(&self, endian: Option<TargetEndian>) {
        self.target_endian.set(endian);
    }

    pub fn set_target_architecture(&self, architecture: TargetArchitecture) {
        let architecture = if self.target_pointer_bits_known.get() {
            architecture.refine_for_pointer_bits(self.target_pointer_bits.get())
        } else {
            if let Some(bits) = architecture.pointer_bits() {
                self.target_pointer_bits.set(bits);
            }
            architecture
        };
        let previous = self.target_architecture.replace(architecture);
        if previous != TargetArchitecture::Unknown
            && architecture != TargetArchitecture::Unknown
            && previous != architecture
        {
            self.cached_register_names.replace(None);
        }
    }

    pub fn target_architecture(&self) -> TargetArchitecture {
        self.target_architecture.get()
    }

    pub fn target_endian(&self) -> Option<TargetEndian> {
        self.target_endian.get()
    }

    pub fn target_pointer_bits(&self) -> u32 {
        self.target_pointer_bits.get()
    }

    pub fn set_target_pointer_bits(&self, bits: u32) {
        if matches!(bits, 32 | 64) {
            self.target_pointer_bits.set(bits);
            self.target_pointer_bits_known.set(true);
            let previous = self.target_architecture.get();
            let refined = previous.refine_for_pointer_bits(bits);
            self.target_architecture.set(refined);
            if previous != TargetArchitecture::Unknown && refined != previous {
                self.cached_register_names.replace(None);
            }
        }
    }

    pub fn reset_target_abi(&self) {
        crate::kernel::invalidate_local_target_abi_cache();
        self.target_architecture.set(TargetArchitecture::Unknown);
        self.target_endian.set(None);
        self.target_pointer_bits.set(usize::BITS);
        self.target_pointer_bits_known.set(false);
        self.cached_register_names.replace(None);
        self.resolved_source_paths.borrow_mut().clear();
    }

    pub fn register_details_visible(&self) -> bool {
        self.inspector_notebook.current_page() == Some(2)
    }

    pub fn stack_details_visible(&self) -> bool {
        self.inspector_notebook.current_page() == Some(3)
    }

    pub fn memory_details_visible(&self) -> bool {
        self.inspector_notebook.current_page() == Some(4)
    }

    pub fn tls_details_visible(&self) -> bool {
        self.kernel_view.active.get()
            && self.kernel_view.pages.visible_child_name().as_deref() == Some("tls")
    }

    pub fn connect_debug_controls(self: &Rc<Self>, client: &Rc<MiClient>) {
        let weak_ui = Rc::downgrade(self);
        let client_for_inspector = Rc::clone(client);
        self.inspector_notebook
            .connect_switch_page(move |_, _, page| {
                if !matches!(page, 2 | 3 | 4 | 7) {
                    return;
                }
                // GTK can emit `switch-page` before `current_page()` exposes
                // the new page. Enrichment checks visibility to avoid doing
                // expensive pointer walks for hidden tabs, so defer it by one
                // main-loop turn and verify that this page is still active.
                let weak_ui = weak_ui.clone();
                let client = Rc::clone(&client_for_inspector);
                glib::idle_add_local_once(move || {
                    let Some(ui) = weak_ui.upgrade() else {
                        return;
                    };
                    if ui.inspector_notebook.current_page() == Some(page)
                        && ui.stopped_inspection_available()
                    {
                        crate::app::refresh_cached_inspector_details(
                            &Rc::downgrade(&ui),
                            &client,
                            page,
                        );
                    }
                });
            });
        let weak_ui = Rc::downgrade(self);
        let client_for_tls = Rc::clone(client);
        self.kernel_view
            .pages
            .connect_visible_child_name_notify(move |_| {
                let weak_ui = weak_ui.clone();
                let client = Rc::clone(&client_for_tls);
                glib::idle_add_local_once(move || {
                    let Some(ui) = weak_ui.upgrade() else {
                        return;
                    };
                    if ui.tls_details_visible() && ui.stopped_inspection_available() {
                        crate::app::refresh_cached_inspector_details(
                            &Rc::downgrade(&ui),
                            &client,
                            7,
                        );
                    }
                });
            });
        let client_for_run = Rc::clone(client);
        let weak_ui = Rc::downgrade(self);
        self.run_button.connect_clicked(move |_| {
            let Some(ui) = weak_ui.upgrade() else {
                return;
            };
            let debugger_state = ui.debugger_state.get();
            if debugger_state.inferior_running()
                || ui.command_pending.get()
                || debugger_state.transition_pending()
                || debugger_state.state_stale()
                || ui.session_pending.get()
                || ui.native_until_active.get()
                || !ui.debugger_ready.get()
            {
                return;
            }
            let (command, detail) = {
                let session = ui.current_session.borrow();
                if debugger_state.inferior_started() {
                    if session
                        .as_ref()
                        .is_some_and(|session| !session.supports_execution())
                    {
                        return;
                    }
                    let command = if let Some(id) = ui.selected_inferior_id() {
                        let Some(id) = crate::debugger::thread_group_argument(&id) else {
                            ui.set_status(
                                "Continue unavailable",
                                "GDB reported an unsupported inferior identifier",
                                Some("status-error"),
                            );
                            return;
                        };
                        format!("-exec-continue --thread-group {id}")
                    } else {
                        String::from("-exec-continue")
                    };
                    (command, "Continuing the selected inferior…")
                } else if configured_target_can_start(
                    session.as_ref(),
                    debugger_state.target_connection(),
                ) {
                    (String::from("-exec-run"), "Starting the inferior…")
                } else {
                    return;
                }
            };
            issue_execution_command(&ui, &client_for_run, &command, detail);
        });
        let client_for_pause = Rc::clone(client);
        let weak_ui = Rc::downgrade(self);
        self.pause_button.connect_clicked(move |_| {
            let Some(ui) = weak_ui.upgrade() else {
                return;
            };
            if ui.native_until_active() {
                ui.cancel_native_until();
            } else if ui.debugger_ready.get()
                && ui.debugger_state.get().inferior_started()
                && ui.debugger_state.get().inferior_running()
                && !ui.command_pending.get()
                && !ui.debugger_state.get().transition_pending()
                && !ui.session_pending.get()
            {
                let command = if let Some(id) = ui.selected_inferior_id() {
                    let Some(id) = crate::debugger::thread_group_argument(&id) else {
                        ui.set_status(
                            "Pause unavailable",
                            "GDB reported an unsupported inferior identifier",
                            Some("status-error"),
                        );
                        return;
                    };
                    format!("-exec-interrupt --thread-group {id}")
                } else {
                    String::from("-exec-interrupt")
                };
                issue_execution_command(
                    &ui,
                    &client_for_pause,
                    &command,
                    "Interrupting the selected inferior…",
                );
            }
        });
        connect_execution_button(
            &self.next_button,
            self,
            client,
            "-exec-next",
            "Stepping over the current source line…",
        );
        connect_execution_button(
            &self.step_button,
            self,
            client,
            "-exec-step",
            "Stepping into the current source line…",
        );
        connect_execution_button(
            &self.next_instruction_button,
            self,
            client,
            "-exec-next-instruction",
            "Stepping over one machine instruction…",
        );
        connect_execution_button(
            &self.step_instruction_button,
            self,
            client,
            "-exec-step-instruction",
            "Stepping into one machine instruction…",
        );
        connect_execution_button(
            &self.finish_button,
            self,
            client,
            "-exec-finish",
            "Running until the current function returns…",
        );
        for (button, action) in &self.until_actions {
            let action = action.clone();
            let until_popover = self.until_popover.clone();
            let weak_ui = Rc::downgrade(self);
            button.connect_clicked(move |_| {
                until_popover.popdown();
                if let Some(ui) = weak_ui.upgrade() {
                    ui.request_native_until(action.clone());
                }
            });
        }
        let condition_entry = self.until_condition_entry.clone();
        let until_popover = self.until_popover.clone();
        let weak_ui = Rc::downgrade(self);
        self.until_condition_button.connect_clicked(move |_| {
            let condition = condition_entry.text().trim().to_owned();
            if condition.is_empty() {
                return;
            }
            until_popover.popdown();
            if let Some(ui) = weak_ui.upgrade() {
                ui.request_native_until(UntilAction::Expression(condition));
            }
        });
        for (button, signal, _) in &self.signal_buttons {
            let signal = (*signal).to_owned();
            let weak_ui = Rc::downgrade(self);
            button.connect_clicked(move |_| {
                if let Some(ui) = weak_ui.upgrade() {
                    request_signal_catchpoint_toggle(&ui, &signal);
                }
            });
        }
        let signal_button = self.signal_add_button.clone();
        self.signal_entry.connect_activate(move |_| {
            if signal_button.is_sensitive() {
                signal_button.emit_clicked();
            }
        });
        let signal_button = self.signal_add_button.clone();
        let ready = Rc::clone(&self.debugger_ready);
        let debugger_state = Rc::clone(&self.debugger_state);
        let pending = Rc::clone(&self.command_pending);
        self.signal_entry.connect_changed(move |entry| {
            signal_button.set_sensitive(
                ready.get()
                    && !debugger_state.get().inferior_running()
                    && !pending.get()
                    && normalized_signal_name(&entry.text()).is_some(),
            );
        });
        let signal_entry = self.signal_entry.clone();
        let weak_ui = Rc::downgrade(self);
        self.signal_add_button.connect_clicked(move |_| {
            if let Some(ui) = weak_ui.upgrade() {
                request_signal_catchpoint_toggle(&ui, &signal_entry.text());
            }
        });
    }

    pub fn connect_source_actions(self: &Rc<Self>) {
        self.connect_open_source();
        self.connect_source_navigation();
    }

    fn connect_keyboard_shortcuts(&self) {
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let run = self.run_button.clone();
        let pause = self.pause_button.clone();
        let next = self.next_button.clone();
        let step = self.step_button.clone();
        let next_instruction = self.next_instruction_button.clone();
        let step_instruction = self.step_instruction_button.clone();
        let finish = self.finish_button.clone();
        let terminal_toggle = self.terminal_toggle_button.clone();
        let resynchronize = self.resynchronize_button.clone();
        let source_back = self.source_navigation.back.clone();
        let source_forward = self.source_navigation.forward.clone();
        let source_quick_open = self.source_navigation.quick_open.clone();
        let source_open_file = self.source_navigation.open_file.clone();
        let source_find = self.source_navigation.find.clone();
        let source_go_to_line = self.source_navigation.go_to_line.clone();
        let source_symbols = self.source_navigation.symbols.clone();
        let source_tree_search = self.source_navigation.tree_search.clone();
        let source_reopen_closed = self.source_navigation.reopen_closed.clone();
        let source_find_close = self.source_navigation.find_close.clone();
        let source_find_bar = self.source_navigation.find_bar.clone();
        let terminal = self.terminal.clone();
        keys.connect_key_pressed(move |_, key, _, state| {
            let control = state.contains(gtk::gdk::ModifierType::CONTROL_MASK);
            let shift = state.contains(gtk::gdk::ModifierType::SHIFT_MASK);
            let alt = state.contains(gtk::gdk::ModifierType::ALT_MASK);
            let blocked = state.contains(gtk::gdk::ModifierType::SUPER_MASK);
            if blocked {
                return gtk::glib::Propagation::Proceed;
            }
            let source_action = match (key, control, shift, alt) {
                (gtk::gdk::Key::Left, false, false, true) => Some(&source_back),
                (gtk::gdk::Key::Right, false, false, true) => Some(&source_forward),
                (gtk::gdk::Key::p | gtk::gdk::Key::P, true, false, false) => {
                    Some(&source_quick_open)
                }
                (gtk::gdk::Key::o | gtk::gdk::Key::O, true, false, false) => {
                    Some(&source_open_file)
                }
                (gtk::gdk::Key::f | gtk::gdk::Key::F, true, false, false) => Some(&source_find),
                (gtk::gdk::Key::g | gtk::gdk::Key::G, true, false, false) => {
                    Some(&source_go_to_line)
                }
                (gtk::gdk::Key::o | gtk::gdk::Key::O, true, true, false) => Some(&source_symbols),
                (gtk::gdk::Key::f | gtk::gdk::Key::F, true, true, false) => {
                    Some(&source_tree_search)
                }
                (gtk::gdk::Key::t | gtk::gdk::Key::T, true, true, false) => {
                    Some(&source_reopen_closed)
                }
                _ => None,
            };
            if terminal.has_focus() && source_action.is_some() {
                return gtk::glib::Propagation::Proceed;
            }
            if let Some(button) = source_action.filter(|button| button.is_sensitive()) {
                button.emit_clicked();
                return gtk::glib::Propagation::Stop;
            }
            if key == gtk::gdk::Key::Escape
                && !control
                && !shift
                && !alt
                && source_find_bar.is_visible()
                && !terminal.has_focus()
            {
                source_find_close.emit_clicked();
                return gtk::glib::Propagation::Stop;
            }
            if alt {
                return gtk::glib::Propagation::Proceed;
            }
            if key == gtk::gdk::Key::grave && control && !shift {
                terminal_toggle.set_active(!terminal_toggle.is_active());
                return gtk::glib::Propagation::Stop;
            }
            if matches!(key, gtk::gdk::Key::r | gtk::gdk::Key::R)
                && control
                && shift
                && resynchronize.is_sensitive()
            {
                resynchronize.emit_clicked();
                return gtk::glib::Propagation::Stop;
            }
            let button = match (key, control, shift) {
                (gtk::gdk::Key::F5, false, false) => Some(&run),
                (gtk::gdk::Key::F6, false, false) => Some(&pause),
                (gtk::gdk::Key::F10, false, false) => Some(&next),
                (gtk::gdk::Key::F10, true, false) => Some(&next_instruction),
                (gtk::gdk::Key::F11, false, false) => Some(&step),
                (gtk::gdk::Key::F11, true, false) => Some(&step_instruction),
                (gtk::gdk::Key::F11, false, true) => Some(&finish),
                _ => None,
            };
            let Some(button) = button.filter(|button| button.is_sensitive()) else {
                return gtk::glib::Propagation::Proceed;
            };
            button.emit_clicked();
            gtk::glib::Propagation::Stop
        });
        self.window.add_controller(keys);
    }

    pub fn set_status(&self, text: &str, detail: &str, class: Option<&str>) {
        self.status_visual_generation
            .set(self.status_visual_generation.get().wrapping_add(1));
        set_status_widgets(&self.status_label, &self.status_detail, text, detail, class);
    }

    pub fn set_execution_status(&self, text: &str, detail: &str) {
        const VISUAL_DELAY: Duration = Duration::from_millis(150);

        let generation = self.status_visual_generation.get().wrapping_add(1);
        self.status_visual_generation.set(generation);
        let current_generation = Rc::clone(&self.status_visual_generation);
        let status = self.status_label.clone();
        let detail_label = self.status_detail.clone();
        let text = text.to_owned();
        let detail = detail.to_owned();
        gtk::glib::timeout_add_local_once(VISUAL_DELAY, move || {
            if current_generation.get() == generation {
                set_status_widgets(
                    &status,
                    &detail_label,
                    &text,
                    &detail,
                    Some("status-running"),
                );
            }
        });
    }

    pub fn set_controls_ready(&self, ready: bool) {
        if !ready && self.native_until_active.get() {
            self.abort_native_until();
        }
        let changed = self.debugger_ready.replace(ready) != ready;
        if !ready {
            self.cancel_running_context_render();
            self.debugger_state
                .set(self.debugger_state.get().reset_backend());
            self.update_run_control_label();
            self.command_pending.set(false);
            self.execution_transition_generation
                .set(self.execution_transition_generation.get().wrapping_add(1));
            self.session_pending.set(false);
            self.native_until_active.set(false);
            self.pending_execution_inferior.borrow_mut().take();
            self.active_thread_execution.borrow_mut().take();
            self.thread_execution_exit_candidate.borrow_mut().take();
            self.inferior_action_pending.set(None);
            self.thread_controls.action_pending.set(None);
            self.reset_thread_analysis();
        }
        if !changed && ready {
            return;
        }
        self.update_control_sensitivity();
        self.update_thread_control_sensitivity();
        if !ready {
            self.render_inferior_controls();
        }
    }

    pub fn set_controls_running(&self, running: bool) {
        let state = self.debugger_state.get();
        if state.inferior_running() == running {
            self.update_run_control_label();
            return;
        }
        let state = state.with_inferior_running(running);
        self.debugger_state.set(state);
        self.update_run_control_label();
        self.update_control_sensitivity();
        self.update_thread_control_sensitivity();
    }

    pub fn inferior_is_running(&self) -> bool {
        self.debugger_state.get().inferior_running()
    }

    pub(super) fn execution_visual_transition_pending(&self) -> bool {
        self.command_pending.get()
            || self.debugger_state.get().transition_pending()
            || self.execution_context_visual_pending.get()
            || self.inferior_action_pending.get() == Some(InferiorActionPending::Execution)
            || self.thread_controls.action_pending.get() == Some(ThreadActionPending::Execution)
    }

    pub fn inferior_has_started(&self) -> bool {
        self.debugger_state.get().inferior_started()
    }

    pub(crate) fn target_connection(&self) -> TargetConnection {
        self.debugger_state.get().target_connection()
    }

    pub(crate) fn configured_session_can_start(&self) -> bool {
        configured_target_can_start(
            self.current_session.borrow().as_ref(),
            self.debugger_state.get().target_connection(),
        )
    }

    pub(crate) fn apply_debugger_state_delta(&self, delta: DebuggerStateDelta) {
        let state = self.debugger_state.get().applying(delta);
        self.debugger_state.set(state);

        if delta.changes_target() {
            self.reset_target_abi();
        }
        self.invalidate_allocator_probe_cache();
        if delta.clears_inferior() {
            self.set_thread_stop_reason(None);
            self.clear_inferiors();
            self.clear_debugger_state();
        }

        self.update_run_control_label();
        self.update_control_sensitivity();
        self.update_thread_control_sensitivity();
        self.render_inferior_controls();
    }

    pub fn movement_commands_available(&self) -> bool {
        self.debugger_ready.get()
            && self.inferior_has_started()
            && !self.inferior_is_running()
            && !self.command_pending.get()
            && !self.debugger_state.get().transition_pending()
            && !self.session_pending.get()
            && !self.debugger_state.get().resynchronizing()
            && self.inferior_action_pending.get().is_none()
            && self.thread_controls.action_pending.get().is_none()
            && !self.debug_state_is_stale()
            && self
                .current_session
                .borrow()
                .as_ref()
                .is_none_or(DebugSession::supports_execution)
    }

    pub(crate) fn stopped_inspection_available(&self) -> bool {
        self.debugger_ready.get()
            && self.inferior_has_started()
            && !self.inferior_is_running()
            && !self.command_pending.get()
            && !self.debugger_state.get().transition_pending()
            && !self.session_pending.get()
            && !self.native_until_active.get()
            && !self.debugger_state.get().resynchronizing()
            && !self.debug_state_is_stale()
    }

    pub(crate) fn stop_point_commands_available(&self) -> bool {
        self.debugger_ready.get()
            && !self.inferior_is_running()
            && !self.command_pending.get()
            && !self.debugger_state.get().transition_pending()
            && !self.session_pending.get()
            && !self.native_until_active.get()
            && !self.debugger_state.get().resynchronizing()
            && !self.debug_state_is_stale()
    }

    pub(crate) fn disassembly_commands_available(&self) -> bool {
        self.stopped_inspection_available() && !self.disassembly_controls.loading.get()
    }

    pub fn set_command_pending(&self, pending: bool) {
        if self.command_pending.replace(pending) == pending {
            return;
        }
        self.update_control_sensitivity();
        self.update_thread_control_sensitivity();
        self.render_inferior_controls();
    }

    pub fn set_session_pending(&self, pending: bool) {
        if self.session_pending.replace(pending) == pending {
            return;
        }
        self.update_control_sensitivity();
        self.update_thread_control_sensitivity();
        self.render_inferior_controls();
    }

    pub(crate) fn begin_execution_transition(&self) -> u64 {
        let generation = self.execution_transition_generation.get().wrapping_add(1);
        self.execution_transition_generation.set(generation);
        self.debugger_state
            .set(self.debugger_state.get().with_transition_pending(true));
        self.update_control_sensitivity();
        self.update_thread_control_sensitivity();
        self.render_inferior_controls();
        generation
    }

    pub(crate) fn finish_execution_transition(&self) {
        let state = self.debugger_state.get();
        if state.transition_pending() {
            self.debugger_state
                .set(state.with_transition_pending(false));
            self.execution_transition_generation
                .set(self.execution_transition_generation.get().wrapping_add(1));
            self.update_control_sensitivity();
            self.update_thread_control_sensitivity();
            self.render_inferior_controls();
        }
    }

    pub(crate) fn execution_transition_is_pending(&self, generation: u64) -> bool {
        self.execution_transition_generation.get() == generation
            && self.debugger_state.get().transition_pending()
    }

    pub(crate) fn execution_transition_matches_thread(
        &self,
        thread_id: Option<&str>,
        all_stopped: bool,
    ) -> bool {
        if !self.debugger_state.get().transition_pending() {
            return false;
        }
        if all_stopped {
            return true;
        }
        super::controls::execution_event_matches_thread(
            self.active_thread_execution.borrow().as_deref(),
            thread_id,
            false,
        )
    }

    pub(crate) fn require_gdb_recovery(&self, title: &str, detail: &str) {
        if self.native_until_active() {
            self.abort_native_until();
        }
        self.set_active_thread_execution(None);
        self.set_thread_execution_exit_candidate(None);
        self.set_pending_execution_inferior(None);
        self.clear_inferior_action_pending();
        self.clear_thread_action_pending();
        self.set_debug_state_stale(true);
        self.clear_debugger_state();
        self.set_controls_ready(false);
        self.set_gdb_recovery_available(true);
        self.set_status(title, detail, Some("status-error"));
    }

    pub fn clear_gef_capabilities(&self) {
        self.gef_context_hidden_by_fgdb.set(false);
        self.set_gef_capabilities(false, &HashSet::new());
    }

    pub fn show_gef_capabilities(&self, capabilities: &HashSet<&'static str>) {
        self.set_gef_capabilities(true, capabilities);
    }

    fn set_gef_capabilities(&self, gef_available: bool, capabilities: &HashSet<&'static str>) {
        self.gef_available.set(gef_available);
        self.gef_capabilities.replace(capabilities.clone());
        let context_control = if !gef_available {
            GefContextControl::None
        } else if capabilities.contains("context off") && capabilities.contains("context on") {
            GefContextControl::ContextCommand
        } else if capabilities.contains("gef config context.enable") {
            GefContextControl::OriginalGef
        } else {
            GefContextControl::None
        };
        self.gef_context_control.set(context_control);
        for control in &self.gef_tool_controls {
            control
                .widget
                .set_visible(gef_available && capabilities.contains(control.capability));
        }
        for group in &self.gef_tool_groups {
            group.widget.set_visible(
                gef_available
                    && group
                        .capabilities
                        .iter()
                        .any(|capability| capabilities.contains(capability)),
            );
        }
        let tools_available = gef_available
            && self
                .gef_tool_controls
                .iter()
                .any(|control| capabilities.contains(control.capability));
        if !tools_available {
            self.gef_tools_button.set_active(false);
        }
        self.gef_tools_button.set_visible(tools_available);
        self.update_control_sensitivity();
    }

    pub fn set_debug_state_stale(&self, stale: bool) {
        let state = self.debugger_state.get();
        if state.state_stale() == stale {
            return;
        }
        self.debugger_state.set(state.with_state_stale(stale));
        self.update_control_sensitivity();
        self.update_thread_control_sensitivity();
        self.render_inferior_controls();
    }

    pub(crate) fn debug_state_is_stale(&self) -> bool {
        self.debugger_state.get().state_stale()
    }

    pub fn set_inferior_started(&self, started: bool) {
        if !started {
            self.inferior_pid.set(None);
        }
        let state = self.debugger_state.get();
        if state.inferior_started() == started {
            self.update_run_control_label();
            return;
        }
        self.debugger_state
            .set(state.with_inferior_started(started));
        self.update_control_sensitivity();
        self.update_thread_control_sensitivity();
        self.update_run_control_label();
    }

    fn update_run_control_label(&self) {
        let label = if self.debugger_state.get().inferior_started() {
            "Continue"
        } else {
            "Run"
        };
        if self.run_button.label().as_deref() != Some(label) {
            self.run_button.set_label(label);
        }
    }

    pub(super) fn update_control_sensitivity(&self) {
        let ready = self.debugger_ready.get();
        let debugger_state = self.debugger_state.get();
        let started = debugger_state.inferior_started();
        let running = debugger_state.inferior_running();
        let stale = debugger_state.state_stale();
        let until_active = self.native_until_active.get();
        let pending = self.command_pending.get()
            || debugger_state.transition_pending()
            || self.session_pending.get()
            || debugger_state.resynchronizing()
            || self.inferior_action_pending.get().is_some()
            || self.thread_controls.action_pending.get().is_some()
            || until_active;
        let busy = running || pending;
        let session = self.current_session.borrow();
        let supports_execution = session
            .as_ref()
            .is_none_or(DebugSession::supports_execution);
        let can_start =
            configured_target_can_start(session.as_ref(), debugger_state.target_connection());
        let can_inspect = ready && started && !running && !pending && !stale;
        let can_move = can_inspect && supports_execution;

        let can_manage_watches = ready && !running && !pending;
        let expression = self.expression_watch_entry.text();
        let can_replace_session =
            !started || matches!(session.as_ref(), Some(DebugSession::CoreDump { .. }));
        let breakpoints = self.breakpoints.borrow();
        let can_edit_stop_points = ready && !running && !pending && !stale;
        let state = ControlState {
            busy,
            run: ready
                && !running
                && !pending
                && ((started && supports_execution) || (!started && can_start && !stale)),
            pause: ready
                && started
                && !self.session_pending.get()
                && (until_active || (running && !self.command_pending.get())),
            move_target: can_move,
            inspect: can_inspect,
            syntax: self.disassembly_controls.syntax_applicable.get(),
            gef_tools: self.gef_available.get() && ready && !running && !pending,
            heap_inspector_in_flight: self.misc_view.heap_inspector_in_flight.get(),
            heap_action_visibility: self
                .misc_view
                .heap_inspector_actions
                .iter()
                .enumerate()
                .fold(0_u64, |mask, (index, (button, _))| {
                    mask | (u64::from(button.is_visible()) << index.min(63))
                }),
            edit_local: can_inspect
                && variable_at(&self.locals_selection, self.locals_selection.selected())
                    .is_some_and(|variable| variable.is_available()),
            manage_watches: can_manage_watches,
            add_watch: can_manage_watches
                && self.expression_watches.borrow().len() < MAX_EXPRESSION_WATCHES
                && !expression.trim().is_empty()
                && !self
                    .expression_watches
                    .borrow()
                    .iter()
                    .any(|existing| existing == expression.trim()),
            remove_watch: can_manage_watches
                && root_variable_at(
                    &self.expression_watches_selection,
                    self.expression_watches_selection.selected(),
                )
                .is_some(),
            add_memory: can_inspect && !self.memory_address_entry.text().trim().is_empty(),
            session: (ready || self.gdb_recovery_available.get()) && !pending,
            new_session: ready && !pending && !running && can_replace_session,
            restart_session: ready
                && started
                && !running
                && !pending
                && session.as_ref().is_some_and(DebugSession::supports_restart),
            kill_session: ready
                && started
                && !running
                && !pending
                && session.as_ref().is_some_and(DebugSession::supports_kill),
            detach_session: ready
                && started
                && !running
                && !pending
                && session.as_ref().is_some_and(DebugSession::supports_detach),
            restart_gdb: self.gdb_recovery_available.get() && !pending,
            resynchronize: can_inspect,
            edit_stop_points: can_edit_stop_points,
            add_signal: can_edit_stop_points
                && normalized_signal_name(&self.signal_entry.text()).is_some(),
            delete_signal_catchpoints: can_edit_stop_points
                && breakpoints.iter().any(Breakpoint::is_signal_catchpoint),
            delete_event_catchpoints: can_edit_stop_points
                && breakpoints.iter().any(|breakpoint| {
                    (breakpoint.is_catchpoint() && !breakpoint.is_signal_catchpoint())
                        || EventCatchpoint::RustPanic.matches(breakpoint)
                }),
            delete_breakpoints: can_edit_stop_points
                && breakpoints
                    .iter()
                    .any(|breakpoint| !breakpoint.is_watchpoint() && !breakpoint.is_catchpoint()),
            delete_watchpoints: can_edit_stop_points
                && breakpoints.iter().any(Breakpoint::is_watchpoint),
        };
        drop(breakpoints);
        drop(session);
        let previous_state = *self.applied_control_state.borrow();
        if previous_state.as_ref() == Some(&state) {
            return;
        }
        self.applied_control_state.replace(Some(state));

        set_transient_execution_sensitive(&self.run_button, state.run, state.busy);
        self.update_pause_control(
            state.pause,
            state.busy,
            previous_state.is_some_and(|previous| previous.pause),
        );
        for button in [
            &self.next_button,
            &self.step_button,
            &self.next_instruction_button,
            &self.step_instruction_button,
            &self.finish_button,
        ] {
            set_transient_execution_sensitive(button, state.move_target, state.busy);
        }
        set_transient_execution_sensitive(&self.until_button, state.move_target, state.busy);
        self.disassembly_controls
            .syntax_intel
            .set_sensitive(state.syntax);
        self.disassembly_controls
            .syntax_att
            .set_sensitive(state.syntax);
        set_transient_execution_sensitive(&self.gef_tools_button, state.gef_tools, state.busy);
        self.gef_tools_content.set_sensitive(state.gef_tools);
        self.misc_view
            .set_heap_inspector_sensitive(state.inspect, state.busy);
        set_execution_sensitive(&self.locals_edit_button, state.edit_local, state.busy);
        set_execution_sensitive(
            &self.expression_watch_entry,
            state.manage_watches,
            state.busy,
        );
        set_execution_sensitive(
            &self.expression_watch_add_button,
            state.add_watch,
            state.busy,
        );
        set_execution_sensitive(
            &self.expression_watch_remove_button,
            state.remove_watch,
            state.busy,
        );
        set_execution_sensitive(&self.memory_add_button, state.add_memory, state.busy);
        set_execution_sensitive(&self.watchpoint_add_button, state.inspect, state.busy);
        set_execution_sensitive(&self.watchpoint_mask, state.inspect, state.busy);
        // Keep the top-level session affordance visually stable during a
        // short execution transition. Its mutating actions remain genuinely
        // insensitive inside the popover until the debugger is ready again.
        set_transient_execution_sensitive(&self.session_button, state.session, state.busy);
        set_execution_sensitive(&self.new_session_button, state.new_session, state.busy);
        set_execution_sensitive(
            &self.restart_session_button,
            state.restart_session,
            state.busy,
        );
        set_execution_sensitive(&self.kill_session_button, state.kill_session, state.busy);
        set_execution_sensitive(
            &self.detach_session_button,
            state.detach_session,
            state.busy,
        );
        set_execution_sensitive(&self.restart_gdb_button, state.restart_gdb, state.busy);
        set_execution_sensitive(&self.resynchronize_button, state.resynchronize, state.busy);
        set_execution_sensitive(
            &self.add_breakpoint_button,
            state.edit_stop_points,
            state.busy,
        );
        for (button, _, _) in &self.signal_buttons {
            set_transient_execution_sensitive(button, state.edit_stop_points, state.busy);
        }
        for (button, _) in &self.event_catchpoint_buttons {
            set_execution_sensitive(button, state.edit_stop_points, state.busy);
        }
        set_execution_sensitive(
            &self.filtered_catchpoint.kind,
            state.edit_stop_points,
            state.busy,
        );
        set_execution_sensitive(
            &self.filtered_catchpoint.filter,
            state.edit_stop_points,
            state.busy,
        );
        set_execution_sensitive(
            &self.filtered_catchpoint.add,
            state.edit_stop_points,
            state.busy,
        );
        set_execution_sensitive(&self.signal_entry, state.edit_stop_points, state.busy);
        set_transient_execution_sensitive(&self.signal_add_button, state.add_signal, state.busy);
        set_transient_execution_sensitive(
            &self.delete_all_signal_catchpoints_button,
            state.delete_signal_catchpoints,
            state.busy,
        );
        set_execution_sensitive(
            &self.delete_all_catchpoints_button,
            state.delete_event_catchpoints,
            state.busy,
        );
        set_execution_sensitive(
            &self.delete_all_breakpoints_button,
            state.delete_breakpoints,
            state.busy,
        );
        set_execution_sensitive(
            &self.delete_all_watchpoints_button,
            state.delete_watchpoints,
            state.busy,
        );
    }

    fn update_pause_control(&self, sensitive: bool, busy: bool, was_sensitive: bool) {
        const VISUAL_DELAY: Duration = Duration::from_millis(150);
        const PENDING_CLASS: &str = "pause-availability-pending";

        set_transient_execution_sensitive(&self.pause_button, sensitive, busy);
        if sensitive {
            if was_sensitive {
                return;
            }
            let generation = self.pause_visual_generation.get().wrapping_add(1);
            self.pause_visual_generation.set(generation);
            self.pause_button.add_css_class(PENDING_CLASS);
            let current_generation = Rc::clone(&self.pause_visual_generation);
            let button = self.pause_button.downgrade();
            let ready = Rc::clone(&self.debugger_ready);
            let debugger_state = Rc::clone(&self.debugger_state);
            let command_pending = Rc::clone(&self.command_pending);
            let session_pending = Rc::clone(&self.session_pending);
            let until_active = Rc::clone(&self.native_until_active);
            gtk::glib::timeout_add_local_once(VISUAL_DELAY, move || {
                let still_available = ready.get()
                    && debugger_state.get().inferior_started()
                    && !session_pending.get()
                    && (until_active.get()
                        || (debugger_state.get().inferior_running() && !command_pending.get()));
                if current_generation.get() == generation
                    && still_available
                    && let Some(button) = button.upgrade()
                {
                    button.remove_css_class(PENDING_CLASS);
                }
            });
        } else if !busy {
            self.pause_visual_generation
                .set(self.pause_visual_generation.get().wrapping_add(1));
            self.pause_button.remove_css_class(PENDING_CLASS);
        }
    }

    pub(crate) fn set_until_action_handler(&self, handler: impl Fn(UntilAction) + 'static) {
        self.until_action_handler.replace(Some(Rc::new(handler)));
    }

    pub(crate) fn set_until_cancel_handler(&self, handler: impl Fn() + 'static) {
        self.until_cancel_handler.replace(Some(Rc::new(handler)));
    }

    pub(crate) fn set_until_abort_handler(&self, handler: impl Fn() + 'static) {
        self.until_abort_handler.replace(Some(Rc::new(handler)));
    }

    pub(crate) fn set_until_stop_handler(
        &self,
        handler: impl Fn(Option<&str>, Option<&str>, Option<&str>) -> bool + 'static,
    ) {
        self.until_stop_handler.replace(Some(Rc::new(handler)));
    }

    fn request_native_until(&self, action: UntilAction) {
        let handler = self.until_action_handler.borrow().clone();
        if let Some(handler) = handler {
            handler(action);
        }
    }

    pub(crate) fn cancel_native_until(&self) {
        let handler = self.until_cancel_handler.borrow().clone();
        if let Some(handler) = handler {
            handler();
        }
    }

    pub(crate) fn abort_native_until(&self) {
        let handler = self.until_abort_handler.borrow().clone();
        if let Some(handler) = handler {
            handler();
        }
    }

    pub(crate) fn handle_native_until_stop(
        &self,
        reason: Option<&str>,
        address: Option<&str>,
        thread_id: Option<&str>,
    ) -> bool {
        let handler = self.until_stop_handler.borrow().clone();
        handler.is_some_and(|handler| handler(reason, address, thread_id))
    }

    pub(crate) fn native_until_active(&self) -> bool {
        self.native_until_active.get()
    }

    pub(crate) fn gef_context_control(&self) -> GefContextControl {
        if self.gef_context_hidden_by_fgdb.get() {
            GefContextControl::None
        } else {
            self.gef_context_control.get()
        }
    }

    pub(crate) fn detected_gef_context_control(&self) -> GefContextControl {
        self.gef_context_control.get()
    }

    pub(crate) const fn gef_context_visible(&self) -> bool {
        self.gef_context_visible
    }

    pub(crate) fn set_gef_context_hidden_by_fgdb(&self, hidden: bool) {
        self.gef_context_hidden_by_fgdb.set(hidden);
    }

    pub(crate) fn set_native_until_active(&self, active: bool) {
        if self.native_until_active.replace(active) == active {
            return;
        }
        self.update_control_sensitivity();
        self.update_thread_control_sensitivity();
        self.render_inferior_controls();
    }

    pub fn set_frame_selection_handler(&self, handler: impl Fn(u32) + 'static) {
        self.frame_selection_handler.replace(Some(Rc::new(handler)));
    }

    pub fn set_thread_selection_handler(&self, handler: impl Fn(String) + 'static) {
        self.thread_selection_handler
            .replace(Some(Rc::new(handler)));
    }

    pub fn set_instruction_handler(&self, handler: impl Fn(String) + 'static) {
        self.instruction_handler.replace(Some(Rc::new(handler)));
    }

    pub(crate) fn set_disassembly_handler(&self, handler: impl Fn(DisassemblyRequest) + 'static) {
        self.disassembly_handler.replace(Some(Rc::new(handler)));
    }

    pub(crate) fn request_disassembly_for_stop(&self, pc: String, architecture: Option<String>) {
        let handler = self.disassembly_handler.borrow().clone();
        if let Some(handler) = handler {
            handler(DisassemblyRequest::Stopped { pc, architecture });
        }
    }

    pub fn set_instruction_memory_handler(&self, handler: impl Fn(String) + 'static) {
        self.instruction_memory_handler
            .replace(Some(Rc::new(handler)));
    }

    pub fn set_memory_watch_handler(&self, handler: impl Fn(u64, String, usize) + 'static) {
        self.memory_watch_handler.replace(Some(Rc::new(handler)));
    }

    pub fn set_variable_object_assignment_handler(
        &self,
        handler: impl Fn(Variable, String) + 'static,
    ) {
        self.variable_assignment_handler
            .replace(Some(Rc::new(handler)));
    }

    pub fn set_variable_editor_handler(&self, handler: impl Fn(Variable) + 'static) {
        self.variable_editor_handler.replace(Some(Rc::new(handler)));
    }

    pub fn set_string_assignment_handler(
        &self,
        handler: impl Fn(Variable, Vec<u8>, StringAssignmentKind) + 'static,
    ) {
        self.string_assignment_handler
            .replace(Some(Rc::new(handler)));
    }

    pub fn set_float_assignment_handler(&self, handler: impl Fn(Variable, Vec<u8>) + 'static) {
        self.float_assignment_handler
            .replace(Some(Rc::new(handler)));
    }

    pub fn set_variable_children_handler(&self, handler: impl Fn(Variable, usize) + 'static) {
        self.variable_children_handler
            .replace(Some(Rc::new(handler)));
    }

    pub(crate) fn set_variable_viewer_handler(
        &self,
        handler: impl Fn(VariableViewerRequest) + 'static,
    ) {
        self.variable_viewer_handler.replace(Some(Rc::new(handler)));
    }

    pub fn set_expression_watch_refresh_handler(&self, handler: impl Fn() + 'static) {
        self.expression_watch_refresh_handler
            .replace(Some(Rc::new(handler)));
    }

    pub fn set_vector_assignment_handler(
        &self,
        handler: impl Fn(String, String, Vec<(usize, String)>) + 'static,
    ) {
        self.vector_assignment_handler
            .replace(Some(Rc::new(handler)));
    }

    pub fn set_breakpoint_insert_handler(&self, handler: impl Fn(PathBuf, u32) + 'static) {
        self.breakpoint_insert_handler
            .replace(Some(Rc::new(handler)));
    }

    pub fn set_source_jump_handler(&self, handler: impl Fn(PathBuf, u32) + 'static) {
        self.source_jump_handler.replace(Some(Rc::new(handler)));
    }

    pub fn set_breakpoint_delete_handler(&self, handler: impl Fn(String) + 'static) {
        self.breakpoint_delete_handler
            .replace(Some(Rc::new(handler)));
    }

    pub fn set_breakpoint_condition_handler(
        &self,
        handler: impl Fn(String, Option<String>) + 'static,
    ) {
        self.breakpoint_condition_handler
            .replace(Some(Rc::new(handler)));
    }

    pub fn set_breakpoint_editor_handler(&self, handler: impl Fn(BreakpointEditRequest) + 'static) {
        self.breakpoint_editor_handler
            .replace(Some(Rc::new(handler)));
    }

    pub fn set_breakpoint_enabled_handler(&self, handler: impl Fn(String, bool) + 'static) {
        self.breakpoint_enabled_handler
            .replace(Some(Rc::new(handler)));
    }

    pub fn set_breakpoint_bulk_delete_handler(&self, handler: impl Fn(Vec<String>) + 'static) {
        self.breakpoint_bulk_delete_handler
            .replace(Some(Rc::new(handler)));
    }

    pub fn set_event_catchpoint_handler(
        &self,
        handler: impl Fn(EventCatchpoint, Option<String>) + 'static,
    ) {
        self.event_catchpoint_handler
            .replace(Some(Rc::new(handler)));
    }

    pub fn set_filtered_catchpoint_handler(
        &self,
        handler: impl Fn(FilteredCatchpointRequest) + 'static,
    ) {
        self.filtered_catchpoint_handler
            .replace(Some(Rc::new(handler)));
    }

    pub fn set_signal_catchpoint_handler(
        &self,
        handler: impl Fn(String, Option<String>) + 'static,
    ) {
        self.signal_catchpoint_handler
            .replace(Some(Rc::new(handler)));
    }

    pub fn set_watchpoint_insert_handler(&self, handler: impl Fn(WatchpointRequest) + 'static) {
        self.watchpoint_insert_handler
            .replace(Some(Rc::new(handler)));
    }

    pub(crate) fn move_stop_point_metadata(&self, from: &str, to: &str) {
        if from == to {
            return;
        }
        let metadata = self.stop_point_metadata.borrow_mut().remove(from);
        if let Some(metadata) = metadata {
            self.stop_point_metadata
                .borrow_mut()
                .insert(to.to_owned(), metadata);
        }
    }

    pub fn set_source_symbol_handler(&self, handler: impl Fn(String) + 'static) {
        self.source_symbol_handler.replace(Some(Rc::new(handler)));
    }

    pub fn set_source_discovery_handler(&self, handler: impl Fn(SourceDiscoveryRequest) + 'static) {
        self.source_discovery_handler
            .replace(Some(Rc::new(handler)));
    }

    pub fn set_thread_stop_reason(&self, reason: Option<&str>) {
        self.thread_stop_reason
            .replace(reason.map(stop_reason_label));
    }
}
