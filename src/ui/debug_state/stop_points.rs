use super::*;

mod navigation;
pub(super) mod organization;

pub(super) fn breakpoint_layout_matches(current: &[Breakpoint], incoming: &[Breakpoint]) -> bool {
    current.len() == incoming.len()
        && current.iter().zip(incoming).all(|(current, incoming)| {
            let mut current = current.clone();
            let mut incoming = incoming.clone();
            current.hit_count = 0;
            current.ignore_count = 0;
            incoming.hit_count = 0;
            incoming.ignore_count = 0;

            current == incoming
        })
}

pub(super) fn breakpoint_status_text(breakpoint: &Breakpoint) -> String {
    let mut status = Vec::new();

    if breakpoint.hit_count > 0 {
        status.push(format!(
            "{} HIT{}",
            breakpoint.hit_count,
            if breakpoint.hit_count == 1 { "" } else { "S" }
        ));
    }

    if let Some(thread) = breakpoint.thread.as_deref() {
        status.push(format!("THREAD {thread}"));
    }

    if let Some(inferior) = breakpoint.inferior.as_deref() {
        status.push(format!("INFERIOR {inferior}"));
    }

    if breakpoint.ignore_count > 0 {
        status.push(format!(
            "STOP ON HIT {}",
            breakpoint.ignore_count.saturating_add(1)
        ));
    }

    if breakpoint.disposition.as_deref() == Some("del") {
        status.push(String::from("TEMPORARY"));
    }

    if breakpoint.pending.is_some() {
        status.push(String::from("PENDING"));
    }

    if breakpoint.location_count > 0 {
        status.push(format!(
            "{} LOCATION{}",
            breakpoint.location_count,
            if breakpoint.location_count == 1 {
                ""
            } else {
                "S"
            }
        ));
    }

    if breakpoint.is_logpoint() {
        status.push(String::from("AUTO-CONTINUE"));
    } else if !breakpoint.commands.is_empty() {
        status.push(format!(
            "{} COMMAND{}",
            breakpoint.commands.len(),
            if breakpoint.commands.len() == 1 {
                ""
            } else {
                "S"
            }
        ));
    }

    status.join("  ")
}

impl Ui {
    pub(in crate::ui) fn connect_watchpoint_controls(&self) {
        let expression = self.watchpoint_expression.clone();
        let access = self.watchpoint_access.clone();
        let mask = self.watchpoint_mask.clone();
        let handler = Rc::clone(&self.watchpoint_insert_handler);
        let model = Rc::clone(&self.model);

        self.watchpoint_add_button.connect_clicked(move |_| {
            if !stop_point_actions_available(&model) || !model.inferior_has_started() {
                return;
            }

            let expression = expression.text().trim().to_owned();

            if expression.is_empty() {
                return;
            }

            let request = match access.selected() {
                1 => WatchpointRequest::Standard {
                    expression,
                    access: WatchpointAccess::Read,
                },
                2 => WatchpointRequest::Standard {
                    expression,
                    access: WatchpointAccess::Access,
                },
                3 => WatchpointRequest::Masked {
                    expression,
                    mask: mask.text().trim().to_owned(),
                },
                _ => WatchpointRequest::Standard {
                    expression,
                    access: WatchpointAccess::Write,
                },
            };

            let handler = handler.borrow().clone();

            if let Some(handler) = handler {
                handler(request);
            }
        });

        let mask = self.watchpoint_mask.clone();

        self.watchpoint_access
            .connect_selected_notify(move |access| {
                mask.set_visible(access.selected() == 3);
            });

        let button = self.watchpoint_add_button.clone();

        self.watchpoint_expression
            .connect_activate(move |_| button.emit_clicked());

        let button = self.watchpoint_add_button.clone();

        self.watchpoint_mask
            .connect_activate(move |_| button.emit_clicked());
    }

    pub(crate) fn connect_stop_point_search(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        let scheduled = Rc::new(Cell::new(false));
        let refresh = Rc::new(move || {
            if scheduled.replace(true) {
                return;
            }

            let scheduled = Rc::clone(&scheduled);
            let weak = weak.clone();
            // Replacing entry text can emit a transient empty search. Render
            // once after the edit instead of rebuilding an unfiltered pane.
            glib::idle_add_local_once(move || {
                scheduled.set(false);
                if let Some(ui) = weak.upgrade() {
                    let breakpoints = ui.breakpoints.borrow().clone();
                    ui.render_breakpoints(breakpoints, true);
                }
            });
        });

        let on_search = Rc::clone(&refresh);
        self.stop_point_filter
            .search
            .connect_search_changed(move |_| on_search());

        let on_organization = Rc::clone(&refresh);

        self.stop_point_filter
            .organization
            .connect_changed(move || on_organization());

        self.stop_point_filter
            .kind
            .connect_selected_notify(move |_| refresh());
    }

    pub(in crate::ui) fn connect_breakpoint_bulk_controls(&self) {
        let handler = Rc::clone(&self.breakpoint_editor_handler);
        let model = Rc::clone(&self.model);
        let panels = Rc::clone(&self.panels);

        self.add_breakpoint_button.connect_clicked(move |button| {
            let Some(parent) = workspace::host_window(button) else {
                return;
            };

            let pending_supported = model.gdb_supports("pending-breakpoints");
            let editor =
                open_breakpoint_editor(&parent, None, pending_supported, Rc::clone(&handler));

            panels.track_dialog(PanelId::Breakpoints, &editor);
        });

        let breakpoints = Rc::clone(&self.breakpoints);
        let handler = Rc::clone(&self.stop_point_bulk_handler);

        self.delete_all_breakpoints_button
            .connect_clicked(move |_| {
                let numbers = breakpoint_command_numbers(&breakpoints.borrow(), false);
                let handler = handler.borrow().clone();

                if !numbers.is_empty()
                    && let Some(handler) = handler
                {
                    handler(StopPointBulkAction::Delete, numbers);
                }
            });

        let breakpoints = Rc::clone(&self.breakpoints);
        let handler = Rc::clone(&self.stop_point_bulk_handler);

        self.delete_all_watchpoints_button
            .connect_clicked(move |_| {
                let numbers = breakpoint_command_numbers(&breakpoints.borrow(), true);
                let handler = handler.borrow().clone();

                if !numbers.is_empty()
                    && let Some(handler) = handler
                {
                    handler(StopPointBulkAction::Delete, numbers);
                }
            });

        let breakpoints = Rc::clone(&self.breakpoints);
        let handler = Rc::clone(&self.stop_point_bulk_handler);

        self.delete_all_catchpoints_button
            .connect_clicked(move |_| {
                let numbers = event_catchpoint_command_numbers(&breakpoints.borrow());
                let handler = handler.borrow().clone();

                if !numbers.is_empty()
                    && let Some(handler) = handler
                {
                    handler(StopPointBulkAction::Delete, numbers);
                }
            });

        let breakpoints = Rc::clone(&self.breakpoints);
        let handler = Rc::clone(&self.stop_point_bulk_handler);
        let model = Rc::clone(&self.model);

        self.delete_all_signal_catchpoints_button
            .connect_clicked(move |_| {
                if !model.execution().ready
                    || model.execution().state.inferior_running()
                    || model.execution().command_pending
                    || model.execution().session_pending
                    || model.execution().native_until_active
                {
                    return;
                }

                let numbers = signal_catchpoint_command_numbers(&breakpoints.borrow());
                let handler = handler.borrow().clone();

                if !numbers.is_empty()
                    && let Some(handler) = handler
                {
                    handler(StopPointBulkAction::Delete, numbers);
                }
            });
    }

    pub(in crate::ui) fn connect_event_catchpoint_controls(&self) {
        for (button, event) in &self.event_catchpoint_buttons {
            let event = *event;
            let breakpoints = Rc::clone(&self.breakpoints);
            let handler = Rc::clone(&self.event_catchpoint_handler);
            let model = Rc::clone(&self.model);

            button.connect_clicked(move |_| {
                if !stop_point_actions_available(&model) {
                    return;
                }

                let existing = event_catchpoint_command_number(&breakpoints.borrow(), event);
                let handler = handler.borrow().clone();

                if let Some(handler) = handler {
                    handler(event, existing);
                }
            });
        }
    }

    pub(in crate::ui) fn connect_filtered_catchpoint_controls(&self) {
        let filter = self.filtered_catchpoint.filter.clone();
        let kind = self.filtered_catchpoint.kind.clone();
        let handler = Rc::clone(&self.filtered_catchpoint_handler);
        let model = Rc::clone(&self.model);

        self.filtered_catchpoint.add.connect_clicked(move |_| {
            if !stop_point_actions_available(&model) {
                return;
            }

            let filter_text = filter.text().trim().to_owned();

            if filter_text.is_empty() {
                return;
            }

            let kind = match kind.selected() {
                1 => FilteredCatchpointKind::LibraryLoad,
                2 => FilteredCatchpointKind::LibraryUnload,
                _ => FilteredCatchpointKind::Syscall,
            };

            if let Some(handler) = handler.borrow().clone() {
                handler(FilteredCatchpointRequest {
                    kind,
                    filter: filter_text,
                });
            }
        });

        let button = self.filtered_catchpoint.add.clone();

        self.filtered_catchpoint
            .filter
            .connect_activate(move |_| button.emit_clicked());

        let filter = self.filtered_catchpoint.filter.clone();

        self.filtered_catchpoint
            .kind
            .connect_selected_notify(move |kind| {
                filter.set_placeholder_text(Some(if kind.selected() == 0 {
                    "syscall names or numbers"
                } else {
                    "shared-library regular expression"
                }));
            });
    }

    pub fn show_breakpoints(&self, breakpoints: Vec<Breakpoint>) {
        // Reconcile membership when the snapshot is accepted, not when its
        // asynchronous source projection or a filter redraw finishes later.
        let active_numbers = breakpoints
            .iter()
            .filter(|breakpoint| !breakpoint.is_location())
            .map(|breakpoint| breakpoint.command_number())
            .collect::<HashSet<_>>();

        self.stop_point_metadata
            .borrow_mut()
            .retain(|number, _| active_numbers.contains(number.as_str()));

        self.prepare_source_breakpoints(breakpoints, false);
        self.update_stop_point_group_controls();
    }

    pub(in crate::ui) fn render_breakpoints(
        &self,
        breakpoints: Vec<Breakpoint>,
        filter_changed: bool,
    ) {
        if !filter_changed && self.breakpoints.borrow().as_slice() == breakpoints {
            return;
        }

        let render_started = Instant::now();
        let status_only =
            !filter_changed && breakpoint_layout_matches(&self.breakpoints.borrow(), &breakpoints);
        self.breakpoints.replace(breakpoints);

        if status_only {
            let breakpoints = self.breakpoints.borrow();
            let rows = self.stop_point_filter_rows.borrow();

            let rendered_numbers = rows
                .iter()
                .map(|row| row.number.as_str())
                .collect::<HashSet<_>>();

            let by_number = breakpoints
                .iter()
                .filter(|breakpoint| !breakpoint.is_location())
                .filter(|breakpoint| rendered_numbers.contains(breakpoint.command_number()))
                .map(|breakpoint| (breakpoint.command_number(), breakpoint))
                .collect::<HashMap<_, _>>();

            if rows
                .iter()
                .all(|row| by_number.contains_key(row.number.as_str()))
            {
                for row in rows.iter() {
                    let breakpoint = by_number[row.number.as_str()];
                    let status = breakpoint_status_text(breakpoint);
                    set_label_text(&row.status, &status);
                    row.status.set_visible(!status.is_empty());
                }

                drop(rows);
                drop(breakpoints);
                self.update_control_sensitivity();
                self.record_ui_render_duration("stop-point pane", render_started);
                return;
            }
        }

        clear_box(&self.breakpoints_list);
        self.stop_point_filter_rows.borrow_mut().clear();
        let breakpoints = self.breakpoints.borrow();

        let pending_supported = self.model.gdb_supports("pending-breakpoints");

        let query = self
            .stop_point_filter
            .search
            .text()
            .trim()
            .to_ascii_lowercase();

        let terms = query.split_whitespace().collect::<Vec<_>>();
        let metadata = self.stop_point_metadata.borrow();

        self.stop_point_filter
            .organization
            .sync(&breakpoints, &metadata);

        let organization_filter = self.stop_point_filter.organization.selection();

        let matching = breakpoints
            .iter()
            .filter(|breakpoint| {
                let metadata = metadata.get(breakpoint.command_number());

                organization_filter.matches(metadata)
                    && stop_point_matches(
                        breakpoint,
                        metadata,
                        &terms,
                        self.stop_point_filter.kind.selected(),
                    )
            })
            .collect::<Vec<_>>();

        let matching_parents = matching
            .iter()
            .map(|breakpoint| breakpoint.command_number())
            .collect::<HashSet<_>>();

        let directly_matching_parents = matching
            .iter()
            .filter(|breakpoint| !breakpoint.is_location())
            .map(|breakpoint| breakpoint.command_number())
            .collect::<HashSet<_>>();

        let matching_locations = matching
            .iter()
            .filter(|breakpoint| breakpoint.is_location())
            .map(|breakpoint| breakpoint.number.as_str())
            .collect::<HashSet<_>>();

        let location_matches = |location: &&Breakpoint| {
            directly_matching_parents.contains(location.command_number())
                || matching_locations.contains(location.number.as_str())
        };

        let total_stop_points = breakpoints
            .iter()
            .filter(|breakpoint| {
                if breakpoint.is_location() {
                    location_matches(breakpoint)
                } else {
                    matching_parents.contains(breakpoint.command_number())
                }
            })
            .count();

        let mut rendered_stop_points = 0_usize;

        // Search the complete model before bounding widget construction.
        let stop_point_limit = crate::performance::STOP_POINT_WIDGET_BUDGET;

        let mut groups = organization::GroupRows::new(
            &self.stop_point_filter.organization,
            self.self_weak.borrow().clone(),
        );

        if breakpoints.is_empty() {
            self.breakpoints_list.append(&empty_label(
                "No breakpoints, catchpoints, or watchpoints set",
            ));
        } else {
            let rendered_parent_numbers = breakpoints
                .iter()
                .filter(|breakpoint| !breakpoint.is_location())
                .filter(|breakpoint| matching_parents.contains(breakpoint.command_number()))
                .take(stop_point_limit)
                .map(|breakpoint| breakpoint.number.as_str())
                .collect::<HashSet<_>>();

            let mut locations_by_parent: HashMap<&str, Vec<&Breakpoint>> = HashMap::new();
            let mut retained_location_count = 0_usize;

            for location in breakpoints
                .iter()
                .filter(|breakpoint| breakpoint.is_location())
                .filter(location_matches)
            {
                if retained_location_count >= stop_point_limit {
                    break;
                }

                if let Some(parent) = location
                    .parent_number
                    .as_deref()
                    .filter(|parent| rendered_parent_numbers.contains(parent))
                {
                    locations_by_parent
                        .entry(parent)
                        .or_default()
                        .push(location);

                    retained_location_count += 1;
                }
            }

            for breakpoint in breakpoints
                .iter()
                .filter(|breakpoint| !breakpoint.is_location())
                .filter(|breakpoint| matching_parents.contains(breakpoint.command_number()))
            {
                if rendered_stop_points >= stop_point_limit {
                    break;
                }

                rendered_stop_points += 1;
                let current_metadata = metadata.get(breakpoint.command_number());
                let group = current_metadata.and_then(|metadata| metadata.group.as_deref());
                let container = groups.parent_container(group);

                let name = if breakpoint.is_watchpoint() {
                    breakpoint
                        .original_location
                        .as_deref()
                        .or(breakpoint.function.as_deref())
                        .or(breakpoint.address.as_deref())
                        .unwrap_or("unresolved expression")
                } else if breakpoint.is_catchpoint() {
                    breakpoint
                        .original_location
                        .as_deref()
                        .or(breakpoint.catch_type.as_deref())
                        .unwrap_or("event")
                } else {
                    breakpoint
                        .function
                        .as_deref()
                        .or(breakpoint.original_location.as_deref())
                        .or(breakpoint.address.as_deref())
                        .unwrap_or("unresolved")
                };

                let location = match (breakpoint.source_path(), breakpoint.line) {
                    (Some(file), Some(line)) => format!("{file}:{line}"),
                    _ if breakpoint.location_count > 0 => format!(
                        "{} resolved location{}",
                        breakpoint.location_count,
                        if breakpoint.location_count == 1 {
                            ""
                        } else {
                            "s"
                        }
                    ),
                    _ if let Some(pending) = breakpoint.pending.as_deref() => {
                        format!("pending  {pending}")
                    }

                    _ if breakpoint.is_watchpoint() => breakpoint.kind.clone(),
                    _ if breakpoint.is_catchpoint() => {
                        breakpoint.catch_type.as_deref().map_or_else(
                            || String::from("event catchpoint"),
                            |kind| format!("{kind} catchpoint"),
                        )
                    }

                    _ => breakpoint
                        .address
                        .clone()
                        .unwrap_or_else(|| String::from("pending")),
                };

                let row = gtk::Box::new(gtk::Orientation::Vertical, 0);
                row.add_css_class("stack-row");
                row.add_css_class("breakpoint-row");

                if breakpoint.is_watchpoint() {
                    row.add_css_class("watchpoint-row");
                }

                if !breakpoint.enabled {
                    row.add_css_class("breakpoint-row-disabled");
                }

                if breakpoint.pending.is_some() {
                    row.add_css_class("breakpoint-row-pending");
                }

                let heading_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);

                let kind = if breakpoint.is_logpoint() {
                    String::from("LOGPOINT")
                } else if breakpoint.is_hardware_breakpoint() {
                    String::from("HARDWARE BREAKPOINT")
                } else if breakpoint.is_watchpoint() || breakpoint.is_catchpoint() {
                    breakpoint.kind.to_ascii_uppercase()
                } else {
                    String::from("BREAKPOINT")
                };

                let badge = gtk::Button::with_label(&format!("#{}", breakpoint.number));
                badge.add_css_class("breakpoint-badge");
                badge.set_focus_on_click(false);
                badge.set_valign(gtk::Align::Center);

                badge.add_css_class(if breakpoint.enabled {
                    "breakpoint-badge-enabled"
                } else {
                    "breakpoint-badge-disabled"
                });

                badge.set_tooltip_text(Some(if breakpoint.enabled {
                    "Disable this stop point"
                } else {
                    "Enable this stop point"
                }));

                let heading_text = format!("{kind}  {}", compact_function_name(name));
                let heading = gtk::Label::new(Some(&heading_text));
                heading.set_halign(gtk::Align::Start);
                heading.set_ellipsize(pango::EllipsizeMode::End);
                heading.set_hexpand(true);
                heading.set_tooltip_text(Some(&format!("{kind}  {name}")));

                let condition_button = gtk::Button::with_label(
                    if breakpoint.is_watchpoint() || breakpoint.is_catchpoint() {
                        if breakpoint.condition.is_some() {
                            "Edit condition"
                        } else {
                            "Condition"
                        }
                    } else {
                        "Edit"
                    },
                );

                condition_button.add_css_class("inline-action");

                condition_button.set_tooltip_text(Some(
                    if breakpoint.is_watchpoint() || breakpoint.is_catchpoint() {
                        "Add, edit, or clear a GDB condition"
                    } else {
                        "Edit location, behavior, restrictions, commands, or logpoint settings"
                    },
                ));

                let organize_button = gtk::Button::with_label("Organize");
                organize_button.add_css_class("inline-action");

                organize_button.set_tooltip_text(Some(
                    "Assign this stop point to a group and add searchable tags",
                ));

                let delete_button = gtk::Button::with_label("Delete");
                delete_button.add_css_class("inline-action");
                delete_button.add_css_class("danger-action");
                delete_button.set_tooltip_text(Some("Delete this breakpoint"));
                heading_row.append(&badge);
                heading_row.append(&heading);
                heading_row.append(&condition_button);
                heading_row.append(&organize_button);
                heading_row.append(&delete_button);
                let location_text = location;
                let location = gtk::Label::new(Some(&location_text));
                location.add_css_class("muted");
                location.set_halign(gtk::Align::Start);
                location.set_ellipsize(pango::EllipsizeMode::Middle);
                enable_stable_text_selection(&location);
                location.set_tooltip_text(Some(&location_text));
                row.append(&heading_row);
                row.append(&location);
                let tags = current_metadata.map_or(&[][..], |metadata| metadata.tags.as_slice());

                if !tags.is_empty() {
                    row.append(&organization::tag_chips(tags));
                }

                let status_text = breakpoint_status_text(breakpoint);
                let status = gtk::Label::new(Some(&status_text));
                status.add_css_class("breakpoint-metadata");
                status.set_halign(gtk::Align::Start);
                status.set_visible(!status_text.is_empty());
                enable_stable_text_selection(&status);
                row.append(&status);

                if let Some(condition) = breakpoint.condition.as_deref() {
                    let condition = gtk::Label::new(Some(&format!("WHEN  {condition}")));
                    condition.add_css_class("breakpoint-condition");
                    condition.set_halign(gtk::Align::Start);
                    condition.set_ellipsize(pango::EllipsizeMode::End);
                    condition.set_tooltip_text(Some(condition.text().as_str()));
                    row.append(&condition);
                }

                if !breakpoint.commands.is_empty() {
                    let command_text = breakpoint
                        .commands
                        .iter()
                        .map(|command| command.trim())
                        .collect::<Vec<_>>()
                        .join("  ");

                    let commands = gtk::Label::new(Some(&format!("DO  {command_text}")));
                    commands.add_css_class("breakpoint-commands");
                    commands.set_halign(gtk::Align::Start);
                    commands.set_ellipsize(pango::EllipsizeMode::End);
                    commands.set_tooltip_text(Some(&command_text));
                    enable_stable_text_selection(&commands);
                    row.append(&commands);
                }

                let breakpoint_for_condition = breakpoint.clone();
                let condition_handler = Rc::clone(&self.breakpoint_condition_handler);
                let editor_handler = Rc::clone(&self.breakpoint_editor_handler);
                let panels = Rc::clone(&self.panels);

                condition_button.connect_clicked(move |button| {
                    let Some(parent) = workspace::host_window(button) else {
                        return;
                    };

                    let editor = if breakpoint_for_condition.is_watchpoint()
                        || breakpoint_for_condition.is_catchpoint()
                    {
                        open_breakpoint_condition_editor(
                            &parent,
                            breakpoint_for_condition.clone(),
                            Rc::clone(&condition_handler),
                        )
                    } else {
                        open_breakpoint_editor(
                            &parent,
                            Some(breakpoint_for_condition.clone()),
                            pending_supported,
                            Rc::clone(&editor_handler),
                        )
                    };

                    panels.track_dialog(PanelId::Breakpoints, &editor);
                });

                let number = breakpoint.command_number().to_owned();
                let metadata = Rc::clone(&self.stop_point_metadata);
                let filter_controls = self.stop_point_filter.clone();
                let panels = Rc::clone(&self.panels);

                organize_button.connect_clicked(move |button| {
                    let Some(parent) = workspace::host_window(button) else {
                        return;
                    };

                    let current = metadata.borrow().get(&number).cloned().unwrap_or_default();
                    let metadata_for_apply = Rc::clone(&metadata);
                    let controls_for_apply = filter_controls.clone();
                    let number_for_apply = number.clone();
                    let (groups, tags) = filter_controls.organization.suggestions();

                    let editor = open_stop_point_metadata_editor(
                        &parent,
                        &number,
                        &current,
                        groups,
                        tags,
                        Rc::new(move |updated| {
                            if updated == StopPointMetadata::default() {
                                metadata_for_apply.borrow_mut().remove(&number_for_apply);
                            } else {
                                metadata_for_apply
                                    .borrow_mut()
                                    .insert(number_for_apply.clone(), updated);
                            }

                            controls_for_apply
                                .search
                                .emit_by_name::<()>("search-changed", &[]);
                        }),
                    );

                    panels.track_dialog(PanelId::Breakpoints, &editor);
                });

                let number = breakpoint.command_number().to_owned();
                let enable = !breakpoint.enabled;
                let enabled_handler = Rc::clone(&self.breakpoint_enabled_handler);

                badge.connect_clicked(move |_| {
                    let handler = enabled_handler.borrow().clone();

                    if let Some(handler) = handler {
                        handler(number.clone(), enable);
                    }
                });

                let number = breakpoint.command_number().to_owned();
                let delete_handler = Rc::clone(&self.breakpoint_delete_handler);

                delete_button.connect_clicked(move |_| {
                    let handler = delete_handler.borrow().clone();

                    if let Some(handler) = handler {
                        handler(number.clone());
                    }
                });

                self.connect_breakpoint_source(&row, &location, breakpoint);
                container.append(&row);

                for location in locations_by_parent
                    .get(breakpoint.number.as_str())
                    .into_iter()
                    .flatten()
                {
                    if rendered_stop_points >= stop_point_limit {
                        break;
                    }

                    rendered_stop_points += 1;
                    let location_row = gtk::Box::new(gtk::Orientation::Horizontal, 4);
                    location_row.add_css_class("breakpoint-location-row");

                    if !location.enabled {
                        location_row.add_css_class("breakpoint-row-disabled");
                    }

                    let badge = gtk::Button::with_label(&format!("#{}", location.number));
                    badge.add_css_class("breakpoint-badge");
                    badge.add_css_class("breakpoint-location-badge");
                    badge.set_focus_on_click(false);
                    badge.set_valign(gtk::Align::Center);

                    badge.add_css_class(if location.enabled {
                        "breakpoint-badge-enabled"
                    } else {
                        "breakpoint-badge-disabled"
                    });

                    badge.set_tooltip_text(Some(if location.enabled {
                        "Disable only this resolved location"
                    } else {
                        "Enable only this resolved location"
                    }));

                    let details = gtk::Box::new(gtk::Orientation::Vertical, 0);
                    details.set_hexpand(true);
                    let function_text = location.function.as_deref().unwrap_or("resolved location");
                    let function = gtk::Label::new(Some(&compact_function_name(function_text)));
                    function.set_halign(gtk::Align::Start);
                    function.set_ellipsize(pango::EllipsizeMode::End);
                    function.set_tooltip_text(Some(function_text));

                    let source = match (location.source_path(), location.line) {
                        (Some(path), Some(line)) => format!(
                            "{path}:{line}  {}",
                            location.address.as_deref().unwrap_or("resolved")
                        ),
                        _ => location
                            .address
                            .clone()
                            .unwrap_or_else(|| String::from("resolved")),
                    };

                    let source_label = gtk::Label::new(Some(&source));
                    source_label.add_css_class("muted");
                    source_label.set_halign(gtk::Align::Start);
                    source_label.set_ellipsize(pango::EllipsizeMode::Middle);
                    source_label.set_tooltip_text(Some(&source));
                    enable_stable_text_selection(&source_label);
                    details.append(&function);
                    details.append(&source_label);
                    location_row.append(&badge);
                    location_row.append(&details);
                    let number = location.number.clone();
                    let enable = !location.enabled;
                    let enabled_handler = Rc::clone(&self.breakpoint_enabled_handler);

                    badge.connect_clicked(move |_| {
                        let handler = enabled_handler.borrow().clone();

                        if let Some(handler) = handler {
                            handler(number.clone(), enable);
                        }
                    });

                    self.connect_breakpoint_source(&location_row, &source_label, location);
                    container.append(&location_row);
                }

                self.stop_point_filter_rows
                    .borrow_mut()
                    .push(StopPointFilterRow {
                        number: breakpoint.command_number().to_owned(),
                        status,
                    });
            }
        }

        groups.append_to(&self.breakpoints_list);

        if rendered_stop_points < total_stop_points {
            let omitted = total_stop_points - rendered_stop_points;

            let notice = performance_partial_label(&format!(
                "{omitted} additional matching stop point{} not rendered. Refine the search to find them",
                if omitted == 1 { " was" } else { "s were" }
            ));

            self.breakpoints_list.append(&notice);

            self.record_performance_notice(crate::performance::PerformanceNotice::count(
                crate::performance::BudgetOutcome::Partial,
                "stop-point pane",
                rendered_stop_points,
                total_stop_points,
            ));
        }

        self.breakpoints_list.append(&self.stop_point_filter.empty);

        self.stop_point_filter
            .empty
            .set_visible(!breakpoints.is_empty() && total_stop_points == 0);

        for (button, signal, description) in &self.signal_buttons {
            if let Some(number) = signal_catchpoint_command_number(&breakpoints, signal) {
                button.add_css_class("signal-caught");

                button.set_tooltip_text(Some(&format!(
                    "{description}\nCatchpoint #{number} is active. Click to remove it"
                )));
            } else {
                button.remove_css_class("signal-caught");

                button.set_tooltip_text(Some(&format!(
                    "{description}\nClick to add a GDB signal catchpoint"
                )));
            }
        }

        for (button, event) in &self.event_catchpoint_buttons {
            if let Some(number) = event_catchpoint_command_number(&breakpoints, *event) {
                button.add_css_class("signal-caught");

                button.set_tooltip_text(Some(&format!(
                    "{} catchpoint #{number} is active. Click to remove it",
                    event.label()
                )));
            } else {
                button.remove_css_class("signal-caught");

                let description = EventCatchpoint::ALL
                    .iter()
                    .find(|(candidate, _, _)| candidate == event)
                    .map(|(_, _, description)| *description)
                    .unwrap_or("Click to add this catchpoint");

                button.set_tooltip_text(Some(description));
            }
        }

        for document in self.source_documents.borrow().iter() {
            document.breakpoint_renderer.queue_draw();
        }

        drop(breakpoints);
        self.update_control_sensitivity();
        self.record_ui_render_duration("stop-point pane", render_started);
    }

    pub fn start_breakpoint_refresh(&self) -> u64 {
        let generation = self.breakpoint_refresh_generation.get().wrapping_add(1);
        self.breakpoint_refresh_generation.set(generation);

        generation
    }

    pub fn begin_breakpoint_refresh(&self) -> Option<u64> {
        self.breakpoint_refresh_gate
            .begin()
            .then(|| self.start_breakpoint_refresh())
    }

    pub fn finish_breakpoint_refresh(&self) -> bool {
        self.breakpoint_refresh_gate.finish()
    }

    pub fn begin_module_refresh(&self) -> bool {
        self.module_refresh_gate.begin()
    }

    pub fn finish_module_refresh(&self) -> bool {
        self.module_refresh_gate.finish()
    }

    pub fn mark_modules_dirty(&self) {
        self.modules_dirty.set(true);
    }

    pub fn take_modules_dirty(&self) -> bool {
        self.modules_dirty.replace(false)
    }

    pub fn show_breakpoints_for_refresh(&self, generation: u64, breakpoints: Vec<Breakpoint>) {
        if self.is_breakpoint_refresh_current(generation) {
            self.show_breakpoints(breakpoints);
        }
    }

    pub(crate) fn is_breakpoint_refresh_current(&self, generation: u64) -> bool {
        self.breakpoint_refresh_generation.get() == generation
    }

    pub fn set_breakpoint_enabled_pending(&self, number: &str, enabled: bool) -> bool {
        let mut breakpoints = self.latest_source_breakpoints();
        let changed = set_breakpoint_enabled(&mut breakpoints, number, enabled);

        if changed {
            self.start_breakpoint_refresh();
            self.breakpoint_refresh_gate.invalidate();
            self.show_breakpoints(breakpoints);
        }

        changed
    }

    pub fn breakpoint_number_at_address(&self, address: &str) -> Option<String> {
        breakpoint_command_number_at_address(&self.breakpoints.borrow(), address)
    }
}
