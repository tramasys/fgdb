use super::*;

mod disassembly;
mod locals;
mod stop_points;

use locals::locals_summary_text;

#[cfg(test)]
use {
    locals::apply_variable_children_page_error,
    stop_points::{breakpoint_layout_matches, breakpoint_status_text},
};

pub(super) fn update_selected_frame_buttons(buttons: &[(u32, gtk::Button)], selected: u32) {
    for (level, button) in buttons {
        if *level == selected {
            button.add_css_class("current-debug-item");
        } else {
            button.remove_css_class("current-debug-item");
        }
    }
}

fn center_scroll_adjustment(scrolled: &gtk::ScrolledWindow, position: u32, item_count: u32) {
    if item_count == 0 {
        return;
    }

    let adjustment = scrolled.vadjustment();
    let lower = adjustment.lower();
    let upper = adjustment.upper();
    let page_size = adjustment.page_size();

    if !lower.is_finite()
        || !upper.is_finite()
        || !page_size.is_finite()
        || upper <= lower + page_size
    {
        return;
    }

    let row_fraction = (f64::from(position) + 0.5) / f64::from(item_count);
    let row_center = lower + (upper - lower) * row_fraction;
    let maximum = (upper - page_size).max(lower);
    adjustment.set_value((row_center - page_size / 2.0).clamp(lower, maximum));
}

pub(super) fn preserve_stack_render_details(entries: &mut [StackEntry], previous: &[StackEntry]) {
    if previous.is_empty() || entries.iter().all(|entry| !entry.pointer_chain.is_empty()) {
        return;
    }

    let previous = previous
        .iter()
        .map(|entry| (entry.address, entry))
        .collect::<HashMap<_, _>>();

    for entry in entries {
        if !entry.pointer_chain.is_empty() {
            continue;
        }

        let Some(previous) = previous.get(&entry.address).copied() else {
            continue;
        };

        if previous.pointer_chain.is_empty()
            || previous.value != entry.value
            || previous.pointer_bits != entry.pointer_bits
            || previous.endian != entry.endian
            || previous.region != entry.region
        {
            continue;
        }

        entry.pointer_chain.clone_from(&previous.pointer_chain);

        if previous.memory_kind == MemoryKind::String {
            entry.memory_kind = MemoryKind::String;
        }
    }
}

fn same_register_render_context(
    previous: Option<&crate::debugger::StopContext>,
    current: Option<&crate::debugger::StopContext>,
) -> bool {
    matches!((previous, current), (Some(previous), Some(current))
        if previous.transport_epoch() == current.transport_epoch()
            && previous.inferior_id() == current.inferior_id()
            && previous.thread_id() == current.thread_id()
            && previous.frame_level() == current.frame_level())
}

fn bounded_stack_frames(
    frames: &[StackFrame],
    limit: usize,
    selected_level: u32,
) -> Vec<&StackFrame> {
    let mut visible = frames.iter().take(limit).collect::<Vec<_>>();

    if limit > 0
        && let Some(selected) = frames.iter().find(|frame| frame.level == selected_level)
        && !visible.iter().any(|frame| frame.level == selected_level)
    {
        visible.pop();
        visible.insert(0, selected);
    }

    visible
}

fn frame_layout_matches<'a>(
    displayed_levels: impl Iterator<Item = u32>,
    frames: impl Iterator<Item = &'a StackFrame>,
) -> bool {
    displayed_levels.eq(frames.map(|frame| frame.level))
}

impl Ui {
    pub fn show_frames(&self, frames: &[StackFrame]) {
        self.model.publish_frames(None, frames);
        self.render_frames();
    }

    fn render_frames(&self) {
        let render_started = Instant::now();
        let snapshot = self.model.frames();
        let frames = snapshot.as_ref();
        let selected_level = self.model.selected_frame_level();

        let frames_unchanged = {
            let displayed = self.displayed_frames.borrow();
            Rc::ptr_eq(&displayed, &snapshot) || displayed.as_ref() == frames
        };

        if frames_unchanged
            && (self
                .frame_buttons
                .borrow()
                .iter()
                .any(|(level, _)| *level == selected_level)
                || !frames.iter().any(|frame| frame.level == selected_level))
        {
            update_selected_frame_buttons(&self.frame_buttons.borrow(), selected_level);
            self.update_thread_control_sensitivity();

            return;
        }

        let widget_limit = self.adaptive_render_limit(
            "call-stack pane",
            crate::performance::STACK_FRAME_WIDGET_BUDGET,
            64,
        );

        let rendered_frames = bounded_stack_frames(frames, widget_limit, selected_level);

        let can_update_in_place = {
            let latest = self.displayed_frames.borrow();
            let previous_frames = bounded_stack_frames(&latest, widget_limit, selected_level);
            let buttons = self.frame_buttons.borrow();

            previous_frames.len() == rendered_frames.len()
                && frame_layout_matches(
                    buttons.iter().map(|(level, _)| *level),
                    rendered_frames.iter().copied(),
                )
                && previous_frames
                    .iter()
                    .zip(&rendered_frames)
                    .all(|(previous, current)| previous.level == current.level)
        };

        if can_update_in_place {
            let latest = self.displayed_frames.borrow();
            let previous_frames = bounded_stack_frames(&latest, widget_limit, selected_level);

            for (((level, button), previous), frame) in self
                .frame_buttons
                .borrow()
                .iter()
                .zip(previous_frames)
                .zip(rendered_frames.iter().copied())
            {
                debug_assert_eq!(*level, frame.level);

                if previous != frame {
                    update_frame_button(button, frame);
                }
            }

            drop(latest);
            self.displayed_frames.replace(Rc::clone(&snapshot));

            update_selected_frame_buttons(
                &self.frame_buttons.borrow(),
                self.model.selected_frame_level(),
            );

            self.update_thread_control_sensitivity();
            self.record_ui_render_duration("call-stack pane", render_started);
            return;
        }

        self.displayed_frames.replace(Rc::clone(&snapshot));
        clear_box(&self.call_stack_list);
        self.frame_buttons.borrow_mut().clear();

        if frames.is_empty() {
            self.call_stack_list
                .append(&empty_label("No stack frames available"));

            self.record_ui_render_duration("call-stack pane", render_started);
            return;
        }

        for frame in &rendered_frames {
            let row = gtk::Box::new(gtk::Orientation::Vertical, 0);
            let displayed_function = compact_function_name(&frame.function);

            let function =
                gtk::Label::new(Some(&format!("#{}  {displayed_function}", frame.level)));

            function.set_halign(gtk::Align::Start);
            function.set_ellipsize(pango::EllipsizeMode::End);
            function.set_tooltip_text(Some(&frame.function));
            let location_text = frame_location_text(frame);
            let location = gtk::Label::new(Some(&location_text));
            location.add_css_class("muted");
            location.set_halign(gtk::Align::Start);
            location.set_ellipsize(pango::EllipsizeMode::Middle);
            location.set_tooltip_text(Some(&location_text));
            row.append(&function);
            row.append(&location);
            let button = gtk::Button::builder().child(&row).build();
            button.add_css_class("stack-frame");

            if frame.level == self.model.selected_frame_level() {
                button.add_css_class("current-debug-item");
            }

            let level = frame.level;
            let handler = Rc::clone(&self.frame_selection_handler);

            button.connect_clicked(move |_| {
                let handler = handler.borrow().clone();

                if let Some(handler) = handler {
                    handler(level);
                }
            });

            self.frame_buttons
                .borrow_mut()
                .push((level, button.clone()));

            self.call_stack_list.append(&button);
        }

        if rendered_frames.len() < frames.len() {
            let omitted = frames.len() - rendered_frames.len();

            let notice = performance_partial_label(&format!(
                "{omitted} deeper frame{} not rendered",
                if omitted == 1 { " was" } else { "s were" }
            ));

            self.call_stack_list.append(&notice);

            self.record_performance_notice(crate::performance::PerformanceNotice::count(
                crate::performance::BudgetOutcome::Partial,
                "call-stack pane",
                rendered_frames.len(),
                frames.len(),
            ));
        }

        self.update_thread_control_sensitivity();
        self.record_ui_render_duration("call-stack pane", render_started);
    }

    pub(crate) fn select_frame_in_view(&self, level: u32) {
        self.model.select_frame(level);
        self.render_frames();
    }

    pub fn show_frames_for_refresh(&self, generation: u64, frames: &[StackFrame]) {
        if self.model.publish_frames(Some(generation), frames) {
            self.render_frames();
        }
    }

    pub(super) fn connect_register_activation(&self) {
        for group in &self.register_groups {
            let panel = group.panel.downgrade();
            let panels = Rc::clone(&self.panels);
            let store = group.store.clone();
            let handler = Rc::clone(&self.variable_assignment_handler);
            let float_handler = Rc::clone(&self.float_assignment_handler);
            let string_handler = Rc::clone(&self.string_assignment_handler);
            let vector_handler = Rc::clone(&self.vector_assignment_handler);
            let register_context = Rc::clone(&self.register_render_context);
            let target_pointer_bits = Rc::clone(&self.target_pointer_bits);
            let target_architecture = Rc::clone(&self.target_architecture);
            let current_source_language = Rc::clone(&self.current_source_language);
            let model = Rc::clone(&self.model);

            group.view.connect_activate(&group.store, move |position| {
                if !model.execution().ready
                    || !model.execution().state.inferior_started()
                    || model.execution().state.inferior_running()
                    || model.execution().command_pending
                    || model.execution().session_pending
                {
                    return;
                }

                let Some(item) = store
                    .item(position)
                    .and_then(|item| item.downcast::<glib::BoxedAnyObject>().ok())
                else {
                    return;
                };

                let row = item.borrow::<RegisterRowData>();
                let register = row.register.clone();
                let display = row.vector_display;
                drop(row);

                let Some(parent) = panel
                    .upgrade()
                    .and_then(|panel| workspace::host_window(&panel))
                else {
                    return;
                };

                let editor = if matches!(register.name.as_str(), "eflags" | "rflags") {
                    open_flag_editor(&parent, register, Rc::clone(&handler))
                } else if vector_register_bytes(&register.name).is_some() {
                    let Some(context) = register_context.borrow().clone() else {
                        return;
                    };

                    open_vector_editor(
                        &parent,
                        register,
                        display,
                        Rc::clone(&model),
                        context,
                        Rc::clone(&vector_handler),
                    )
                } else {
                    Some(open_variable_editor(
                        &parent,
                        Variable {
                            local_index: None,
                            name: format!("${}", register.name),
                            value: register.value,
                            type_name: None,
                            argument: false,
                            varobj: None,
                            num_children: 0,
                            has_more: false,
                            display_hint: None,
                            dynamic: false,
                        },
                        target_pointer_bits.get(),
                        target_architecture.get(),
                        current_source_language.get(),
                        None,
                        ValueEditorHandlers {
                            model: Rc::clone(&model),
                            assignment: Rc::clone(&handler),
                            float: Rc::clone(&float_handler),
                            string: Rc::clone(&string_handler),
                        },
                    ))
                };

                if let Some(editor) = editor {
                    panels.track_dialog(PanelId::Registers, &editor);
                }
            });
        }
    }

    pub fn show_threads(&self, threads: &[ThreadInfo]) {
        self.model.publish_threads(threads);
        self.render_threads();
    }

    pub(super) fn render_threads(&self) {
        let render_started = Instant::now();
        let threads = self.model.threads();
        let threads = threads.as_ref();

        let executable_name = self
            .model
            .session()
            .as_ref()
            .and_then(DebugSession::executable)
            .and_then(std::path::Path::file_name)
            .and_then(std::ffi::OsStr::to_str)
            .map(str::to_owned);

        let stop_reason = self.model.thread_stop_reason();
        let (query, state_filter, sort) = self.current_thread_filter_state();

        let thread_limit =
            self.adaptive_render_limit("thread pane", crate::performance::THREAD_WIDGET_BUDGET, 32);

        let (rendered_threads, visible_thread_count) =
            Self::filtered_sorted_thread_page(threads, &query, state_filter, sort, thread_limit);

        let rendered_thread_count = rendered_threads.len();
        let omitted_thread_count = visible_thread_count.saturating_sub(rendered_thread_count);

        if self.latest_threads.borrow().as_ref().is_some_and(|state| {
            state.source_threads.as_ref() == threads
                && state.rendered_threads == rendered_threads
                && state.stop_reason == stop_reason
                && state.executable_name == executable_name
                && state.query == query
                && state.state_filter == state_filter
                && state.sort == sort
        }) {
            return;
        }

        self.kernel_view
            .set_tls_thread(threads, executable_name.as_deref());

        let can_update_in_place = self
            .latest_threads
            .borrow()
            .as_ref()
            .is_some_and(|previous| {
                previous.rendered_threads.len() == rendered_threads.len()
                    && self.thread_buttons.borrow().len() == rendered_threads.len()
                    && previous
                        .rendered_threads
                        .iter()
                        .zip(&rendered_threads)
                        .all(|(previous, current)| previous.id == current.id)
            });

        if can_update_in_place {
            let latest = self.latest_threads.borrow();

            let Some(previous) = latest.as_ref() else {
                return;
            };

            for (((_, button), old_thread), thread) in self
                .thread_buttons
                .borrow()
                .iter()
                .zip(previous.rendered_threads.iter())
                .zip(&rendered_threads)
            {
                let reason = thread
                    .current
                    .then(|| stop_reason.as_deref().unwrap_or("STOPPED"));

                let old_reason = old_thread
                    .current
                    .then(|| previous.stop_reason.as_deref().unwrap_or("STOPPED"));

                if old_thread != thread || old_reason != reason {
                    update_thread_button(button, thread, reason);
                }
            }

            drop(latest);

            self.latest_threads.replace(Some(ThreadRenderState {
                source_threads: self.model.threads(),
                rendered_threads,
                stop_reason,
                executable_name,
                query,
                state_filter,
                sort,
            }));

            self.sync_thread_controls(threads, visible_thread_count);
            sync_thread_partial_notice(&self.threads_list, omitted_thread_count);

            if omitted_thread_count > 0 {
                self.record_performance_notice(crate::performance::PerformanceNotice::count(
                    crate::performance::BudgetOutcome::Partial,
                    "thread pane",
                    rendered_thread_count,
                    visible_thread_count,
                ));
            }

            self.record_ui_render_duration("thread pane", render_started);
            return;
        }

        self.latest_threads.replace(Some(ThreadRenderState {
            source_threads: self.model.threads(),
            rendered_threads: rendered_threads.clone(),
            stop_reason: stop_reason.clone(),
            executable_name,
            query,
            state_filter,
            sort,
        }));

        self.sync_thread_controls(threads, visible_thread_count);
        clear_box(&self.threads_list);
        self.thread_buttons.borrow_mut().clear();

        if rendered_threads.is_empty() {
            self.threads_list
                .append(&empty_label(if threads.is_empty() {
                    "No threads available"
                } else {
                    "No threads match the current filter"
                }));

            self.record_ui_render_duration("thread pane", render_started);
            return;
        }

        for thread in &rendered_threads {
            let reason = thread
                .current
                .then(|| stop_reason.as_deref().unwrap_or("STOPPED"));

            let row = thread_button_content(thread, reason);
            let button = gtk::Button::builder().child(&row).build();
            button.add_css_class("stack-frame");

            if thread.current {
                button.add_css_class("current-debug-item");
            }

            let id = thread.id.clone();
            let handler = Rc::clone(&self.thread_selection_handler);

            button.connect_clicked(move |_| {
                let handler = handler.borrow().clone();

                if let Some(handler) = handler {
                    handler(id.clone());
                }
            });

            self.thread_buttons
                .borrow_mut()
                .push((thread.id.clone(), button.clone()));

            self.threads_list.append(&button);
        }

        sync_thread_partial_notice(&self.threads_list, omitted_thread_count);

        if omitted_thread_count > 0 {
            self.record_performance_notice(crate::performance::PerformanceNotice::count(
                crate::performance::BudgetOutcome::Partial,
                "thread pane",
                rendered_thread_count,
                visible_thread_count,
            ));
        }

        self.update_thread_control_sensitivity();
        self.record_ui_render_duration("thread pane", render_started);
    }

    pub fn show_threads_for_refresh(&self, generation: u64, threads: &[ThreadInfo]) {
        if self.model.is_thread_refresh_current(generation) {
            self.show_threads(threads);
        }
    }

    pub fn show_signal(&self, name: Option<&str>, meaning: Option<&str>) {
        let text = match (name, meaning) {
            (Some(name), Some(meaning)) => format!("{name}  {meaning}"),
            (Some(name), None) => name.to_owned(),
            (None, _) => String::from("No signal at the current stop"),
        };

        self.signal_detail.set_text(&text);

        if name.is_some() {
            self.signal_detail.add_css_class("signal-active");
        } else {
            self.signal_detail.remove_css_class("signal-active");
        }
    }

    pub fn show_registers(&self, registers: &[Register]) -> bool {
        let changed = self.model.publish_registers(registers);
        self.render_registers(registers, false);

        changed
    }

    fn render_registers(&self, registers: &[Register], details_pending: bool) {
        let context = self
            .model
            .stop_context(self.model.current_stop_refresh_generation());
        let preserve_details = details_pending
            && same_register_render_context(
                self.register_render_context.borrow().as_ref(),
                context.as_ref(),
            );
        self.register_render_context.replace(context);

        if registers.is_empty() {
            for group in &self.register_groups {
                if group.store.n_items() != 0 {
                    group.store.remove_all();
                }

                if group.panel.is_visible() {
                    group.panel.set_visible(false);
                }
            }

            if !self.registers_empty.is_visible() {
                self.registers_empty.set_visible(true);
            }
        } else {
            if self.registers_empty.is_visible() {
                self.registers_empty.set_visible(false);
            }

            let architecture = self.target_architecture();
            let endian = self.target_endian();
            let pointer_bits = self.target_pointer_bits();
            let previous = self.model.previous_registers();

            let ring = registers
                .iter()
                .find(|register| register.name == "cs")
                .and_then(|register| hex_value(&register.value))
                .map(|value| value & 0x3);

            for group in self.register_groups.iter() {
                let grouped = registers.iter().filter(|register| {
                    register_in_group(group.kind, &register.name, architecture)
                        && (group.kind != RegisterGroupKind::Other
                            || !self.register_groups.iter().any(|candidate| {
                                candidate.kind != RegisterGroupKind::Other
                                    && register_in_group(
                                        candidate.kind,
                                        &register.name,
                                        architecture,
                                    )
                            }))
                });

                let rows = grouped.map(|register| RegisterRowData {
                    register: register.clone(),
                    changed: register_changed(register, &previous),
                    ring: if is_flags_register(&register.name) {
                        ring
                    } else {
                        None
                    },
                    architecture,
                    endian,
                    pointer_bits,
                    vector_display: group
                        .vector_controls
                        .as_ref()
                        .map_or_else(VectorDisplay::default, VectorControls::display),
                });
                populate_register_group(group, rows, preserve_details);
            }
        }

        self.update_instruction_insight();
    }

    pub fn start_stop_refresh(&self) -> u64 {
        let generation = self.model.start_stop_refresh();

        self.memory_watch_container
            .refresh_batch
            .borrow_mut()
            .clear();

        update_memory_container_state(&self.memory_watch_container, false);
        self.pending_local_variable_objects.borrow_mut().clear();
        clear_variable_change_markers(&self.locals_store);
        clear_variable_change_markers(&self.expression_watches_store);
        let roots = self.local_variables.borrow();
        let arguments = roots.argument_count();

        self.locals_summary.set_text(&locals_summary_text(
            roots.len().saturating_sub(arguments),
            arguments,
            0,
            self.locals_store.n_items() as usize,
            roots.len(),
        ));

        self.call_abi_instruction.replace(None);
        self.call_abi_instruction_generation.set(None);
        self.misc_view.show_call_abi_pending();
        self.update_control_sensitivity();

        generation
    }

    pub(crate) fn begin_stop_refresh(
        &self,
        transport_epoch: u64,
    ) -> Option<crate::debugger::StopContext> {
        self.start_stop_refresh();
        let context = self.model.bind_stop_context(transport_epoch)?;
        let tooltip =
            "Refreshing locals and arguments. Previous values remain visible and cannot be edited";
        self.locals_view.set_tooltip_text(Some(tooltip));
        self.locals_summary.set_tooltip_text(Some(tooltip));

        if self.locals_store.n_items() == 0 {
            self.locals_empty.set_text("Loading locals and arguments…");
        }

        update_selected_frame_buttons(&self.frame_buttons.borrow(), context.frame_level());
        self.update_thread_control_sensitivity();

        Some(context)
    }

    pub fn show_registers_for_refresh(&self, generation: u64, registers: &[Register]) {
        self.update_registers_for_refresh(generation, registers, true);
    }

    pub(crate) fn show_register_details_for_refresh(
        &self,
        generation: u64,
        registers: &[Register],
    ) {
        self.update_registers_for_refresh(generation, registers, false);
    }

    fn update_registers_for_refresh(
        &self,
        generation: u64,
        registers: &[Register],
        details_pending: bool,
    ) {
        if let Some(refresh_transfer) = self
            .model
            .publish_registers_for_refresh(generation, registers)
        {
            // Retained pointer annotations are presentation-only. Publish the
            // actual response to the model, and accept empty final details too.
            self.render_registers(registers, details_pending);

            if refresh_transfer {
                self.refresh_call_abi_transfer();
            }
        }
    }

    pub fn show_stack(&self, entries: &[StackEntry]) {
        self.model.publish_stack(None, entries);
        self.render_stack(Cow::Borrowed(entries));
    }

    fn render_stack(&self, entries: Cow<'_, [StackEntry]>) {
        self.update_stack_paging();
        if self.displayed_stack.borrow().as_slice() != entries.as_ref() {
            replace_boxed_store_if_changed(&self.stack_store, entries.iter().cloned());
            self.displayed_stack.replace(entries.into_owned());
        }

        if self.displayed_stack.borrow().is_empty() {
            self.stack_empty
                .set_text("Stack values appear when the target is paused");

            self.stack_empty.set_visible(true);
            return;
        }

        self.stack_empty.set_visible(false);
    }

    pub fn show_stack_for_refresh(&self, generation: u64, entries: &[StackEntry]) {
        if self.model.publish_stack(Some(generation), entries) {
            let mut rendered = entries.to_vec();
            preserve_stack_render_details(&mut rendered, &self.displayed_stack.borrow());
            self.render_stack(Cow::Owned(rendered));
        }
    }

    pub fn show_stack_unavailable_for_refresh(&self, generation: u64, reason: &str) {
        if !self.model.publish_stack(Some(generation), &[]) {
            return;
        }

        self.displayed_stack.replace(Vec::new());
        self.stack_store.remove_all();
        self.stack_empty.set_text(reason);
        self.stack_empty.set_visible(true);
        self.update_stack_paging();
    }

    pub fn show_memory_regions_for_refresh(&self, generation: u64, regions: &[MemoryRegion]) {
        let Some(changed) = self.model.publish_memory_regions(generation, regions) else {
            return;
        };

        if changed {
            replace_boxed_store_if_changed(&self.memory_region_store, regions.iter().cloned());
        }

        self.memory_regions_empty.set_visible(regions.is_empty());
        self.variable_locations.schedule();
    }

    pub(super) fn connect_memory_controls(&self) {
        let container = self.memory_watch_container.clone();
        let watches = Rc::clone(&self.memory_watches);
        let handler = Rc::clone(&self.memory_watch_handler);
        let expression = self.memory_address_entry.clone();
        let size = self.memory_size.clone();
        let format = self.memory_format.clone();
        let weak_ui = Rc::clone(&self.self_weak);

        self.memory_add_button.connect_clicked(move |_| {
            let expression_text = expression.text().trim().to_owned();

            if expression_text.is_empty() {
                return;
            }

            let byte_count = usize::try_from(size.value_as_int()).unwrap_or(128);

            let format = match format.selected() {
                1 => MemoryWatchFormat::U16,
                2 => MemoryWatchFormat::U32,
                3 => MemoryWatchFormat::U64,
                4 => MemoryWatchFormat::F32,
                5 => MemoryWatchFormat::F64,
                6 => MemoryWatchFormat::Pointers,
                _ => MemoryWatchFormat::Bytes,
            };

            let added = add_memory_watch(
                &container,
                &watches,
                &handler,
                expression_text,
                byte_count,
                format,
            );

            if added {
                expression.set_text("");
            } else if let Some(ui) = weak_ui.borrow().upgrade() {
                ui.set_status(
                    "Memory watch limit",
                    "Remove a memory watch before adding another (limit 256)",
                    Some("status-error"),
                );
            }

            expression.grab_focus();
        });

        let button = self.memory_add_button.clone();

        self.memory_address_entry.connect_activate(move |_| {
            if button.is_sensitive() {
                button.emit_clicked();
            }
        });

        let button = self.memory_add_button.clone();
        let model = Rc::clone(&self.model);

        self.memory_address_entry.connect_changed(move |entry| {
            button.set_sensitive(
                model.execution().ready
                    && model.execution().state.inferior_started()
                    && !model.execution().state.inferior_running()
                    && !model.execution().command_pending
                    && !entry.text().trim().is_empty(),
            );
        });

        let ui = self.clone();

        self.memory_watch_container
            .refresh_all
            .connect_clicked(move |_| ui.refresh_memory_watches());

        let container = self.memory_watch_container.clone();
        let watches = Rc::clone(&self.memory_watches);

        self.memory_watch_container
            .clear_all
            .connect_clicked(move |_| clear_memory_watches(&container, &watches));

        self.memory_regions_view.set_single_click_activate(false);
        let container = self.memory_watch_container.clone();
        let watches = Rc::clone(&self.memory_watches);
        let handler = Rc::clone(&self.memory_watch_handler);
        let size = self.memory_size.clone();
        let search = Rc::downgrade(&self.memory_search);

        self.memory_regions_view
            .connect_activate(move |view, position| {
                let Some(region) = view
                    .model()
                    .and_then(|model| model.item(position))
                    .and_downcast::<glib::BoxedAnyObject>()
                else {
                    return;
                };

                let region = region.borrow::<MemoryRegion>();
                let requested = usize::try_from(size.value_as_int()).unwrap_or(128);

                let region_size =
                    usize::try_from(region.end.saturating_sub(region.start)).unwrap_or(usize::MAX);

                let byte_count = requested.min(region_size).max(1);

                if add_memory_watch(
                    &container,
                    &watches,
                    &handler,
                    format!("0x{:x}", region.start),
                    byte_count,
                    MemoryWatchFormat::Bytes,
                ) && let Some(search) = search.upgrade()
                {
                    search.show_inspector();
                }
            });
    }

    pub fn refresh_memory_watches(&self) {
        if !self.model.stopped_inspection_available() {
            update_memory_container_state(&self.memory_watch_container, false);
            return;
        }

        let requests = {
            let watches = self.memory_watches.borrow();

            if watches.is_empty() {
                return;
            }

            self.memory_watch_container
                .refresh_batch
                .borrow_mut()
                .begin(watches.iter().map(|watch| watch.id));

            update_memory_container_state(&self.memory_watch_container, true);

            watches
                .iter()
                .map(begin_memory_watch_request)
                .collect::<Vec<_>>()
        };

        let handler = self.memory_watch_handler.borrow().clone();

        if let Some(handler) = handler {
            for request in requests {
                handler(request);
            }
        } else {
            self.memory_watch_container
                .refresh_batch
                .borrow_mut()
                .clear();

            update_memory_container_state(&self.memory_watch_container, false);
        }
    }

    pub(crate) fn memory_watch_request_is_current(&self, id: u64, revision: u64) -> bool {
        self.memory_watches
            .borrow()
            .iter()
            .any(|watch| watch.id == id && watch.request_revision.get() == revision)
    }

    pub fn show_memory_watch(&self, id: u64, result: Result<MemoryBlock, &str>) {
        let reading = self
            .memory_watch_container
            .refresh_batch
            .borrow_mut()
            .finish(id);

        let watch = self
            .memory_watches
            .borrow()
            .iter()
            .find(|watch| watch.id == id)
            .cloned();

        let Some(watch) = watch else {
            update_memory_container_state(&self.memory_watch_container, reading);
            return;
        };

        match result {
            Ok(memory) => {
                let endian = self.target_endian().or_else(|| {
                    (watch.format == MemoryWatchFormat::Bytes).then_some(TargetEndian::Little)
                });

                let Some(endian) = endian else {
                    show_memory_watch_error(
                        &watch,
                        "Target byte order is unavailable. Use Hex bytes display",
                    );

                    update_memory_container_state(&self.memory_watch_container, reading);
                    return;
                };

                show_memory_watch_data(
                    &watch,
                    memory,
                    &self.model.memory_regions(),
                    self.target_pointer_bits.get(),
                    endian,
                );
            }
            Err(error) => {
                show_memory_watch_error(&watch, error);
                self.application_log
                    .record(LogLevel::Error, "Memory watch failed", error);
            }
        }

        update_memory_container_state(&self.memory_watch_container, reading);
    }
}

pub(super) fn frame_location_text(frame: &StackFrame) -> String {
    frame.line.map_or_else(
        || frame.address.clone(),
        |line| {
            format!(
                "{}:{line}",
                frame.source_path().unwrap_or(frame.address.as_str())
            )
        },
    )
}

fn update_frame_button(button: &gtk::Button, frame: &StackFrame) {
    let Some(row) = button.child().and_downcast::<gtk::Box>() else {
        return;
    };

    let Some(function) = row.first_child().and_downcast::<gtk::Label>() else {
        return;
    };

    let Some(location) = row.last_child().and_downcast::<gtk::Label>() else {
        return;
    };

    function.set_text(&format!(
        "#{}  {}",
        frame.level,
        compact_function_name(&frame.function)
    ));

    function.set_tooltip_text(Some(&frame.function));
    let location_text = frame_location_text(frame);
    location.set_text(&location_text);
    location.set_tooltip_text(Some(&location_text));
}

fn thread_button_content(thread: &ThreadInfo, stop_reason: Option<&str>) -> gtk::Box {
    let marker = if thread.current { "*" } else { " " };
    let tid = thread_os_id(&thread.target_id).unwrap_or_else(|| String::from("?"));
    let name = thread.name.as_deref().unwrap_or("<unnamed>");
    let detail = thread_detail(thread, stop_reason);
    let row = gtk::Box::new(gtk::Orientation::Vertical, 0);

    let heading = gtk::Label::new(Some(&format!(
        "[{marker}Thread Id:{}, tid:{tid}]",
        thread.id
    )));

    heading.add_css_class("thread-heading");
    heading.set_halign(gtk::Align::Start);
    heading.set_ellipsize(pango::EllipsizeMode::End);
    let name = gtk::Label::new(Some(&format!("Name: \"{name}\"")));
    name.add_css_class("thread-name");
    name.set_halign(gtk::Align::Fill);
    name.set_xalign(0.0);
    name.set_wrap(true);
    name.set_wrap_mode(pango::WrapMode::WordChar);
    name.set_tooltip_text(thread.name.as_deref());
    let detail_widget = thread_detail_widget(thread, stop_reason);
    let full_symbol = thread.frame.as_ref().map(|frame| frame.function.as_str());

    detail_widget.set_tooltip_text(Some(&match full_symbol {
        Some(symbol) => format!(
            "{detail}\nFull symbol: {symbol}\nGDB target: {}",
            thread.target_id
        ),
        None => format!("{detail}\nGDB target: {}", thread.target_id),
    }));

    row.append(&heading);
    row.append(&name);
    row.append(&detail_widget);

    row
}

fn update_thread_button(button: &gtk::Button, thread: &ThreadInfo, stop_reason: Option<&str>) {
    let Some(row) = button.child().and_downcast::<gtk::Box>() else {
        return;
    };

    let Some(heading) = row.first_child().and_downcast::<gtk::Label>() else {
        return;
    };

    let Some(name) = heading.next_sibling().and_downcast::<gtk::Label>() else {
        return;
    };

    let Some(detail_widget) = row.last_child().and_downcast::<gtk::Box>() else {
        return;
    };

    let marker = if thread.current { "*" } else { " " };
    let tid = thread_os_id(&thread.target_id).unwrap_or_else(|| String::from("?"));
    let thread_name = thread.name.as_deref().unwrap_or("<unnamed>");

    set_label_text(
        &heading,
        &format!("[{marker}Thread Id:{}, tid:{tid}]", thread.id),
    );

    set_label_text(&name, &format!("Name: \"{thread_name}\""));
    name.set_tooltip_text(thread.name.as_deref());

    if !update_thread_detail_widget(&detail_widget, thread, stop_reason) {
        return;
    }

    let detail = thread_detail(thread, stop_reason);
    let full_symbol = thread.frame.as_ref().map(|frame| frame.function.as_str());

    detail_widget.set_tooltip_text(Some(&match full_symbol {
        Some(symbol) => format!(
            "{detail}\nFull symbol: {symbol}\nGDB target: {}",
            thread.target_id
        ),
        None => format!("{detail}\nGDB target: {}", thread.target_id),
    }));

    set_css_class(button, "current-debug-item", thread.current);
}

fn sync_thread_partial_notice(container: &gtk::Box, omitted: usize) {
    let existing = container
        .last_child()
        .filter(|child| child.has_css_class("performance-partial"));

    if omitted == 0 {
        if let Some(existing) = existing {
            container.remove(&existing);
        }

        return;
    }

    let text = format!(
        "{omitted} matching thread{} not rendered. Narrow the filter to inspect them",
        if omitted == 1 { " was" } else { "s were" }
    );

    if let Some(label) = existing.and_downcast::<gtk::Label>() {
        label.set_text(&text);
    } else {
        let label = performance_partial_label(&text);
        container.append(&label);
    }
}

pub(super) fn performance_partial_label(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.add_css_class("performance-partial");
    label.set_halign(gtk::Align::Fill);
    label.set_xalign(0.0);
    label.set_wrap(true);

    label
}

#[cfg(test)]
mod render_tests {
    use super::*;

    fn breakpoint(number: &str) -> Breakpoint {
        Breakpoint {
            number: number.to_owned(),
            kind: String::from("breakpoint"),
            enabled: true,
            condition: None,
            address: Some(String::from("0x1000")),
            function: Some(String::from("main")),
            file: Some(String::from("main.c")),
            fullname: Some(String::from("/tmp/main.c")),
            line: Some(12),
            original_location: Some(String::from("main")),
            catch_type: None,
            disposition: Some(String::from("keep")),
            hit_count: 0,
            ignore_count: 0,
            thread: None,
            inferior: None,
            pending: None,
            commands: Vec::new(),
            parent_number: None,
            location_count: 0,
        }
    }

    fn stack_entry(value: &str, chain: &[&str], region: Option<&str>) -> StackEntry {
        StackEntry {
            address: 0x1000,
            offset: 0,
            index: 0,
            pointer_bits: 64,
            endian: TargetEndian::Little,
            value: value.to_owned(),
            pointer_chain: chain.iter().map(|value| (*value).to_owned()).collect(),
            address_registers: vec![String::from("rsp")],
            value_registers: Vec::new(),
            return_frame: None,
            memory_kind: MemoryKind::Heap,
            region: region.map(str::to_owned),
        }
    }

    fn frame(level: u32) -> StackFrame {
        StackFrame {
            level,
            address: format!("0x{level:x}"),
            function: format!("frame_{level}"),
            architecture: None,
            file: None,
            fullname: None,
            line: None,
        }
    }

    #[test]
    fn bounded_stack_frames_keep_a_deep_selected_frame() {
        let frames = (0..20).map(frame).collect::<Vec<_>>();
        let visible = bounded_stack_frames(&frames, 5, 19);
        assert_eq!(visible.len(), 5);
        assert_eq!(visible[0].level, 19);
        assert_eq!(visible[1].level, 0);
    }

    #[test]
    fn a_new_deep_selection_cannot_reuse_the_old_frame_button_layout() {
        let frames = [frame(0), frame(1), frame(2), frame(3)];
        let initial = bounded_stack_frames(&frames, 2, 0);
        let selected = bounded_stack_frames(&frames, 2, 3);
        assert!(!frame_layout_matches(
            initial.iter().map(|frame| frame.level),
            selected.iter().copied()
        ));
        assert!(frame_layout_matches(
            [3, 0].into_iter(),
            selected.iter().copied()
        ));
    }

    #[test]
    fn local_summary_reports_the_visible_page_without_losing_totals() {
        assert_eq!(
            locals_summary_text(900, 100, 0, 512, 1_000),
            "900 locals  100 args  512/1000 shown"
        );
    }

    #[test]
    fn preserves_only_stable_stack_pointer_details_between_refresh_phases() {
        let mut previous = stack_entry(
            "0x2000",
            &["0x2000", "0x3000", "0x6f6c6c6568"],
            Some("heap"),
        );

        previous.memory_kind = MemoryKind::String;
        let mut stable = vec![stack_entry("0x2000", &[], Some("heap"))];
        preserve_stack_render_details(&mut stable, std::slice::from_ref(&previous));
        assert_eq!(stable[0].pointer_chain, previous.pointer_chain);
        assert_eq!(stable[0].memory_kind, MemoryKind::String);
        let mut changed_value = vec![stack_entry("0x2008", &[], Some("heap"))];
        preserve_stack_render_details(&mut changed_value, std::slice::from_ref(&previous));
        assert!(changed_value[0].pointer_chain.is_empty());
        let mut changed_region = vec![stack_entry("0x2000", &[], Some("unmapped"))];
        preserve_stack_render_details(&mut changed_region, &[previous]);
        assert!(changed_region[0].pointer_chain.is_empty());
    }

    #[test]
    fn stable_register_rows_survive_raw_refresh_but_accept_final_detail_changes() {
        let previous = RegisterRowData {
            register: Register {
                name: String::from("rax"),
                value: String::from("0x2000"),
                pointer_chain: vec![String::from("0x2000 <buffer>"), String::from("0x3000")],
            },
            changed: false,
            ring: None,
            architecture: TargetArchitecture::X86_64,
            endian: Some(TargetEndian::Little),
            pointer_bits: 64,
            vector_display: VectorDisplay::default(),
        };
        let mut raw = previous.clone();
        raw.register.pointer_chain.clear();
        let mut pending = raw.clone();
        pending.preserve_details_from(&previous);
        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        replace_boxed_store_if_changed(&store, [previous.clone()]);
        let original = store.item(0).unwrap();
        assert!(!replace_boxed_store_if_changed(&store, [pending]));
        assert_eq!(store.item(0).unwrap(), original);

        // Memory behind an unchanged register can change or become unreadable.
        let mut completed = previous.clone();
        completed.register.pointer_chain[1] = String::from("0x4000");
        assert!(replace_boxed_store_if_changed(&store, [completed]));
        assert!(replace_boxed_store_if_changed(&store, [raw.clone()]));
        assert!(!replace_boxed_store_if_changed(&store, [raw.clone()]));

        for change in 0..5 {
            let mut different = raw.clone();
            match change {
                0 => different.register.value = String::from("0x2008"),
                1 => different.register.name = String::from("rbx"),
                2 => different.pointer_bits = 32,
                3 => different.endian = Some(TargetEndian::Big),
                _ => different.architecture = TargetArchitecture::AArch64,
            }
            different.preserve_details_from(&previous);
            assert!(different.register.pointer_chain.is_empty());
        }

        let context = |epoch, generation, inferior: &str, thread: &str, frame| {
            crate::debugger::StopContext::new(
                epoch,
                generation,
                Some(inferior.to_owned()),
                thread.to_owned(),
                frame,
            )
            .unwrap()
        };
        let previous = context(1, 10, "i1", "1", 0);
        assert!(same_register_render_context(
            Some(&previous),
            Some(&context(1, 11, "i1", "1", 0))
        ));
        for different in [
            None,
            Some(context(2, 11, "i1", "1", 0)),
            Some(context(1, 11, "i2", "1", 0)),
            Some(context(1, 11, "i1", "2", 0)),
            Some(context(1, 11, "i1", "1", 1)),
        ] {
            assert!(!same_register_render_context(
                Some(&previous),
                different.as_ref()
            ));
        }
    }

    #[test]
    fn breakpoint_counter_changes_do_not_require_row_reconstruction() {
        let current = breakpoint("1");
        let mut updated = current.clone();
        updated.hit_count = 2;
        updated.ignore_count = 3;

        assert!(breakpoint_layout_matches(
            std::slice::from_ref(&current),
            std::slice::from_ref(&updated)
        ));

        assert_eq!(breakpoint_status_text(&updated), "2 HITS  STOP ON HIT 4");
        updated.enabled = false;
        assert!(!breakpoint_layout_matches(&[current], &[updated]));
    }

    #[test]
    fn page_errors_preserve_loaded_children_and_offer_a_retry() {
        let parent = Variable {
            local_index: None,
            name: String::from("items"),
            value: String::from("{...}"),
            type_name: Some(String::from("Item [256]")),
            argument: false,
            varobj: Some(String::from("var1")),
            num_children: 256,
            has_more: true,
            display_hint: Some(String::from("array")),
            dynamic: true,
        };

        let node = VariableNode::new(parent.clone());

        node.children
            .append(&glib::BoxedAnyObject::new(VariableNode::new(Variable {
                local_index: None,
                name: String::from("[0]"),
                value: String::from("1"),
                type_name: Some(String::from("int")),
                argument: false,
                varobj: Some(String::from("var1.0")),
                num_children: 0,
                has_more: false,
                display_hint: None,
                dynamic: false,
            })));

        node.children
            .append(&glib::BoxedAnyObject::new(VariableNode::load_more(
                parent.clone(),
                128,
            )));

        apply_variable_children_page_error(&node, &parent, 128, "temporary failure");
        assert_eq!(node.children.n_items(), 2);

        let first = node
            .children
            .item(0)
            .and_downcast::<glib::BoxedAnyObject>()
            .unwrap();

        assert_eq!(first.borrow::<VariableNode>().variable.name, "[0]");

        let retry = node
            .children
            .item(1)
            .and_downcast::<glib::BoxedAnyObject>()
            .unwrap();

        let retry = retry.borrow::<VariableNode>();
        assert_eq!(retry.variable.name, "Retry loading more…");
        assert_eq!(retry.load_more.as_ref().map(|(_, from)| *from), Some(128));
    }

    #[test]
    fn initial_expansion_errors_can_be_retried() {
        let parent = Variable {
            local_index: None,
            name: String::from("head"),
            value: String::from("0x20"),
            type_name: Some(String::from("Node *")),
            argument: false,
            varobj: Some(String::from("var1")),
            num_children: 1,
            has_more: false,
            display_hint: None,
            dynamic: false,
        };

        let node = VariableNode::new(parent.clone());
        apply_variable_children_page_error(&node, &parent, 0, "temporary failure");
        assert_eq!(node.children.n_items(), 1);

        let retry = node
            .children
            .item(0)
            .and_downcast::<glib::BoxedAnyObject>()
            .unwrap();

        let retry = retry.borrow::<VariableNode>();
        assert_eq!(retry.variable.name, "Retry expansion…");
        assert_eq!(retry.load_more.as_ref().map(|(_, from)| *from), Some(0));
    }
}
