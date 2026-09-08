use super::*;
use crate::config::ReplayConfig;
use crate::model::replay::{ExecutionDirection, MAX_CHECKPOINTS, RecordingMethod, ReplayAction};

type ReplayHandler = Rc<RefCell<Option<Rc<dyn Fn(ReplayAction)>>>>;

#[derive(Clone, Copy)]
enum Confirmation {
    Stop,
    Create,
    Restore,
    Delete,
}

#[derive(Clone)]
pub(super) struct ReplayControls {
    pub button: gtk::MenuButton,
    direction_label: gtk::Label,
    popover: gtk::Popover,
    summary: gtk::Label,
    recording_section: gtk::Box,
    checkpoint_note: gtk::Label,
    forward: gtk::Button,
    reverse: gtk::Button,
    method: gtk::DropDown,
    limit: gtk::SpinButton,
    buffer: gtk::SpinButton,
    start: gtk::Button,
    stop: gtk::Button,
    refresh: gtk::Button,
    checkpoints: gtk::DropDown,
    checkpoint_names: gtk::StringList,
    create: gtk::Button,
    restore: gtk::Button,
    delete: gtk::Button,
    handler: ReplayHandler,
    rendered_checkpoints: Rc<RefCell<Vec<crate::model::replay::Checkpoint>>>,
    rendered_checkpoint_owner: Rc<RefCell<Option<String>>>,
    rendering: Rc<Cell<bool>>,
    rendered_direction: Rc<Cell<ExecutionDirection>>,
}

impl ReplayControls {
    pub fn new(config: &crate::config::ReplayConfig) -> Self {
        let popover = gtk::Popover::new();
        popover.set_has_arrow(false);
        popover.set_cascade_popdown(false);
        let root = gtk::Box::new(gtk::Orientation::Vertical, components::CONTENT_INSET);
        root.set_size_request(440, -1);
        components::inset(&root, components::CONTENT_INSET);
        root.add_css_class("session-menu");
        let heading = components::control_row();
        let title = section_title("EXECUTION HISTORY");
        title.set_hexpand(true);
        heading.append(&title);
        let refresh = history_button("Refresh");
        refresh.add_css_class("inline-action");
        heading.append(&refresh);
        root.append(&heading);
        let summary = gtk::Label::new(Some(
            "Start recording at a paused target or open an rr trace",
        ));

        summary.set_xalign(0.0);
        summary.set_wrap(true);
        summary.set_max_width_chars(56);
        summary.add_css_class("muted");
        root.append(&summary);
        let forward = history_button("Forward");
        let reverse = history_button("Reverse");
        root.append(&button_pair(&forward, &reverse));
        let direction_note = gtk::Label::builder()
            .label("Direction applies to Continue and the stepping controls. Until remains forward-only.")
            .wrap(true)
            .max_width_chars(56)
            .xalign(0.0)
            .css_classes(["muted"])
            .build();

        root.append(&direction_note);
        let recording_section =
            gtk::Box::new(gtk::Orientation::Vertical, components::CONTENT_INSET);
        root.append(&recording_section);
        recording_section.append(&section_title("RECORDING"));
        let method = gtk::DropDown::from_strings(&[
            "Full recording",
            "Branch trace (auto)",
            "Intel PT",
            "Intel BTS",
        ]);

        method.set_hexpand(true);
        recording_section.append(&method);
        let limit = gtk::SpinButton::with_range(
            f64::from(*ReplayConfig::FULL_INSTRUCTION_LIMIT.start()),
            f64::from(*ReplayConfig::FULL_INSTRUCTION_LIMIT.end()),
            10_000.0,
        );

        limit.set_value(f64::from(config.full_instruction_limit));
        let buffer = gtk::SpinButton::with_range(
            f64::from(*ReplayConfig::BTRACE_BUFFER_KIB.start()),
            f64::from(*ReplayConfig::BTRACE_BUFFER_KIB.end()),
            4.0,
        );

        buffer.set_value(f64::from(config.btrace_buffer_kib));
        let options = gtk::Grid::builder()
            .column_spacing(components::CONTENT_INSET)
            .row_spacing(components::CONTROL_GAP)
            .hexpand(true)
            .build();

        for (row, (title, input)) in [
            ("Full instruction limit", &limit),
            ("Trace buffer / thread (KiB)", &buffer),
        ]
        .into_iter()
        .enumerate()
        {
            let label = gtk::Label::builder()
                .label(title)
                .xalign(0.0)
                .valign(gtk::Align::Center)
                .css_classes(["field-label"])
                .build();

            input.set_hexpand(true);
            input.set_valign(gtk::Align::Center);
            input.set_width_chars(8);
            options.attach(&label, 0, row as i32, 1, 1);
            options.attach(input, 1, row as i32, 1, 1);
        }

        recording_section.append(&options);
        let start = history_button("Start recording");
        let stop = history_button("Stop recording");
        recording_section.append(&button_pair(&start, &stop));
        let note = gtk::Label::new(Some(
            "Full recording discards the oldest instructions at the limit. It can be slow or reject unsupported instructions. Editing target state can discard future history. Branch tracing has lower overhead but does not preserve past variable values.",
        ));

        note.set_xalign(0.0);
        note.set_wrap(true);
        note.set_max_width_chars(56);
        note.add_css_class("muted");
        recording_section.append(&note);
        root.append(&section_title("CHECKPOINTS"));
        let checkpoint_names = gtk::StringList::new(&[]);
        let checkpoints =
            gtk::DropDown::new(Some(checkpoint_names.clone()), None::<gtk::Expression>);

        checkpoints.set_hexpand(true);
        let checkpoint_factory = gtk::SignalListItemFactory::new();

        checkpoint_factory.connect_setup(|_, object| {
            let Some(item) = object.downcast_ref::<gtk::ListItem>() else {
                return;
            };

            let label = gtk::Label::builder()
                .xalign(0.0)
                .max_width_chars(54)
                .ellipsize(pango::EllipsizeMode::End)
                .build();

            item.set_child(Some(&label));
        });

        checkpoint_factory.connect_bind(|_, object| {
            let Some(item) = object.downcast_ref::<gtk::ListItem>() else {
                return;
            };

            let Some(value) = item.item().and_downcast::<gtk::StringObject>() else {
                return;
            };

            let Some(label) = item.child().and_downcast::<gtk::Label>() else {
                return;
            };

            label.set_text(&value.string());
            label.set_tooltip_text(Some(&value.string()));
        });

        checkpoints.set_factory(Some(&checkpoint_factory));
        checkpoints.set_list_factory(Some(&checkpoint_factory));
        root.append(&checkpoints);
        let create = history_button("Create");
        let restore = history_button("Restore");
        let delete = history_button("Delete");
        let actions = button_pair(&create, &restore);
        delete.set_hexpand(true);
        actions.append(&delete);
        root.append(&actions);
        let note = gtk::Label::new(Some(
            "Native checkpoints require a single-threaded local target without recording. They keep a forked process and cannot undo external I/O. rr uses replay checkpoints.",
        ));

        note.set_xalign(0.0);
        note.set_wrap(true);
        note.set_max_width_chars(56);
        note.add_css_class("muted");
        root.append(&note);
        let checkpoint_note = note;
        let scroll = gtk::ScrolledWindow::builder()
            .child(&root)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .max_content_height(620)
            .min_content_width(440)
            .build();

        popover.set_child(Some(&scroll));
        let direction_label = gtk::Label::new(Some("Forward"));
        let direction_arrow = gtk::Image::from_icon_name("pan-down-symbolic");
        direction_arrow.add_css_class("execution-direction-arrow");
        let direction_content = components::control_row();
        direction_content.set_halign(gtk::Align::Center);
        direction_content.set_valign(gtk::Align::Center);
        direction_content.append(&direction_label);
        direction_content.append(&direction_arrow);
        let button = gtk::MenuButton::builder()
            .child(&direction_content)
            .popover(&popover)
            .always_show_arrow(false)
            .build();

        button.add_css_class("debug-control");
        button.set_tooltip_text(Some(
            "Choose execution direction, recording, and checkpoints",
        ));

        Self {
            button,
            direction_label,
            popover,
            summary,
            recording_section,
            checkpoint_note,
            forward,
            reverse,
            method,
            limit,
            buffer,
            start,
            stop,
            refresh,
            checkpoints,
            checkpoint_names,
            create,
            restore,
            delete,
            handler: Rc::new(RefCell::new(None)),
            rendered_checkpoints: Rc::new(RefCell::new(Vec::new())),
            rendered_checkpoint_owner: Rc::new(RefCell::new(None)),
            rendering: Rc::new(Cell::new(false)),
            rendered_direction: Rc::new(Cell::new(ExecutionDirection::Forward)),
        }
    }

    fn emit(&self, action: ReplayAction) {
        let handler = self.handler.borrow().clone();

        if let Some(handler) = handler {
            handler(action);
        }
    }

    pub(super) fn render(&self, model: &crate::model::DebuggerModel) {
        let direction = model.execution_direction();

        if self.rendered_direction.replace(direction) != direction {
            self.direction_label.set_text(direction.label());
        }

        // The closed menu has no stop-time rendering or checkpoint allocations.
        if !self.button.is_active() {
            return;
        }

        if self.rendering.replace(true) {
            return;
        }

        let ready = model.debugger_synchronization_available();
        let stopped = ready && model.stopped_inspection_available();
        let recording = model.recording_method();
        let reverse_available = model.reverse_available();
        let rr = matches!(
            model.session().as_ref(),
            Some(DebugSession::RrReplay { .. })
        );

        self.recording_section.set_visible(!rr);
        let checkpoint_note = if rr {
            "rr checkpoints save replay positions until this session closes. Restoring a checkpoint does not modify the recorded trace."
        } else {
            "Native checkpoints require a single-threaded local target without recording. They keep a forked process and cannot undo external I/O."
        };

        if self.checkpoint_note.text() != checkpoint_note {
            self.checkpoint_note.set_text(checkpoint_note);
        }

        let group = model.selected_inferior_id().unwrap_or_default();
        let state = model.replay.borrow();
        set_css_class(
            &self.forward,
            "suggested-action",
            direction == ExecutionDirection::Forward,
        );

        set_css_class(
            &self.reverse,
            "suggested-action",
            direction == ExecutionDirection::Reverse,
        );

        self.forward.set_sensitive(ready);
        self.reverse.set_sensitive(stopped && reverse_available);
        self.refresh.set_sensitive(ready && !state.querying);
        let method = self.method.selected();
        self.start.set_sensitive(
            stopped
                && !rr
                && recording.is_none()
                && model.non_stop_mode() == Some(false)
                && matches!(
                    model.target_connection(),
                    crate::model::TargetConnection::Local | crate::model::TargetConnection::Remote
                )
                && state.capabilities.supports_recording(if method == 0 {
                    RecordingMethod::Full
                } else {
                    RecordingMethod::BranchTrace
                }),
        );

        self.stop
            .set_sensitive(stopped && !rr && recording.is_some());
        self.method
            .set_sensitive(ready && !rr && recording.is_none());
        self.limit
            .set_sensitive(ready && !rr && recording.is_none() && method == 0);
        self.buffer
            .set_sensitive(ready && !rr && recording.is_none() && method != 0);

        let summary = if rr {
            "rr replay"
        } else if let Some(method) = recording {
            method.label()
        } else if reverse_available {
            "Reverse-capable target"
        } else {
            "No execution recording"
        };

        if self.summary.text() != summary {
            self.summary.set_text(summary);
        }

        self.summary
            .set_tooltip_text((!state.details.is_empty()).then_some(state.details.as_str()));

        let owned = state.checkpoint_owner.as_deref() == Some(&group);
        self.rendered_checkpoint_owner
            .replace(owned.then_some(group));
        let rows = if owned {
            state.checkpoints.as_slice()
        } else {
            &[]
        };

        let changed = *self.rendered_checkpoints.borrow() != rows;

        if changed {
            let selected = self
                .rendered_checkpoints
                .borrow()
                .get(self.checkpoints.selected() as usize)
                .and_then(|old| rows.iter().position(|row| row.id == old.id))
                .or_else(|| rows.iter().position(|row| !row.active));

            self.rendered_checkpoints.replace(rows.to_vec());
            let labels = rows
                .iter()
                .map(|row| {
                    format!(
                        "{} {}  {}",
                        if row.active { "Current" } else { "Checkpoint" },
                        row.id,
                        row.description
                    )
                })
                .collect::<Vec<_>>();

            self.checkpoint_names.splice(
                0,
                self.checkpoint_names.n_items(),
                &labels.iter().map(String::as_str).collect::<Vec<_>>(),
            );

            if let Some(selected) = selected {
                self.checkpoints.set_selected(selected as u32);
            }
        }

        let can_checkpoint = stopped
            && model.checkpoint_target_available()
            && state.capabilities.checkpoints == Some(true)
            && owned
            && state.checkpoint_query_complete;

        self.create.set_sensitive(
            can_checkpoint && !state.checkpoint_list_capped && rows.len() < MAX_CHECKPOINTS,
        );

        let selected = rows.get(self.checkpoints.selected() as usize);
        self.checkpoints
            .set_tooltip_text(selected.map(|row| row.description.as_str()));

        let can_restore = can_checkpoint && selected.is_some_and(|row| !row.active);
        self.restore.set_sensitive(can_restore);
        self.delete.set_sensitive(can_restore);
        self.rendering.set(false);
    }
}

fn history_button(text: &str) -> gtk::Button {
    let label = gtk::Label::builder()
        .label(text)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .build();

    gtk::Button::builder().child(&label).build()
}

fn button_pair(first: &gtk::Button, second: &gtk::Button) -> gtk::Box {
    let row = components::control_row();
    row.set_homogeneous(true);
    first.set_hexpand(true);
    second.set_hexpand(true);
    row.append(first);
    row.append(second);
    row
}

impl Ui {
    pub(crate) fn restart_rr_replay(&self) {
        self.replay_controls.emit(ReplayAction::RestartReplay);
    }

    pub(crate) fn render_replay_controls(&self) {
        self.update_control_sensitivity();
    }

    pub(crate) fn connect_replay_actions(
        self: &Rc<Self>,
        handler: impl Fn(ReplayAction) + 'static,
    ) {
        let controls = &self.replay_controls;
        controls.handler.replace(Some(Rc::new(handler)));
        components::keep_popover_dismissible(&self.window, &controls.popover);

        for (button, action) in [
            (
                &controls.forward,
                ReplayAction::Direction(ExecutionDirection::Forward),
            ),
            (
                &controls.reverse,
                ReplayAction::Direction(ExecutionDirection::Reverse),
            ),
            (&controls.refresh, ReplayAction::Refresh),
        ] {
            let weak = Rc::downgrade(self);

            button.connect_clicked(move |_| {
                if let Some(ui) = weak.upgrade() {
                    ui.replay_controls.emit(action.clone());
                }
            });
        }

        let weak = Rc::downgrade(self);
        controls.button.connect_active_notify(move |button| {
            if button.is_active()
                && let Some(ui) = weak.upgrade()
            {
                ui.replay_controls.render(&ui.model);
                let weak = Rc::downgrade(&ui);

                glib::idle_add_local_once(move || {
                    if let Some(ui) = weak.upgrade()
                        && ui.replay_controls.button.is_active()
                    {
                        ui.replay_controls.emit(ReplayAction::Refresh);
                    }
                });
            }
        });

        for dropdown in [&controls.method, &controls.checkpoints] {
            let weak = Rc::downgrade(self);
            dropdown.connect_selected_notify(move |_| {
                if let Some(ui) = weak.upgrade()
                    && !ui.replay_controls.rendering.get()
                {
                    let weak = Rc::downgrade(&ui);
                    // Do not splice a dropdown model while GTK is activating it.
                    glib::idle_add_local_once(move || {
                        if let Some(ui) = weak.upgrade() {
                            ui.replay_controls.render(&ui.model);
                        }
                    });
                }
            });
        }

        let weak = Rc::downgrade(self);
        controls.start.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                let controls = &ui.replay_controls;
                let method = match controls.method.selected() {
                    0 => RecordingMethod::Full,
                    1 => RecordingMethod::BranchTrace,
                    2 => RecordingMethod::ProcessorTrace,
                    3 => RecordingMethod::BranchStore,
                    _ => return,
                };

                controls.emit(ReplayAction::Start(
                    method,
                    controls.limit.value_as_int() as u32,
                    controls.buffer.value_as_int() as u32,
                ));
            }
        });

        for (button, kind) in [
            (&controls.stop, Confirmation::Stop),
            (&controls.create, Confirmation::Create),
            (&controls.restore, Confirmation::Restore),
            (&controls.delete, Confirmation::Delete),
        ] {
            let weak = Rc::downgrade(self);
            button.connect_clicked(move |_| {
                let Some(ui) = weak.upgrade() else {
                    return;
                };

                let action = match kind {
                    Confirmation::Stop => ReplayAction::Stop,
                    Confirmation::Create => ReplayAction::CreateCheckpoint,
                    _ => {
                        if *ui.replay_controls.rendered_checkpoint_owner.borrow()
                            != ui.model.selected_inferior_id()
                        {
                            return;
                        }

                        let rows = ui.replay_controls.rendered_checkpoints.borrow();
                        let selected = ui.replay_controls.checkpoints.selected() as usize;
                        let Some(row) = rows.get(selected) else {
                            return;
                        };

                        if matches!(kind, Confirmation::Restore) {
                            ReplayAction::RestoreCheckpoint(row.id.clone())
                        } else {
                            ReplayAction::DeleteCheckpoint(row.id.clone())
                        }
                    }
                };

                let rr = matches!(
                    ui.model.session().as_ref(),
                    Some(DebugSession::RrReplay { .. })
                );

                let (message, detail) = match kind {
                    Confirmation::Stop => (
                        "Stop recording?",
                        "This deletes the recorded execution history. If replaying an earlier position, execution becomes live at that position.",
                    ),
                    Confirmation::Create if rr => (
                        "Create a replay checkpoint?",
                        "Save this replay position. rr keeps the checkpoint until this replay session closes.",
                    ),
                    Confirmation::Create => (
                        "Create a checkpoint?",
                        "A native checkpoint keeps a forked process alive. Checkpoints cannot undo writes to files or external services.",
                    ),
                    Confirmation::Restore if rr => (
                        "Restore this replay checkpoint?",
                        "Seek to the saved replay position. The recorded trace is not modified.",
                    ),
                    Confirmation::Restore => (
                        "Restore this checkpoint?",
                        "This replaces the current target state with the saved state. External I/O is not rolled back.",
                    ),
                    Confirmation::Delete => (
                        "Delete this checkpoint?",
                        "The saved checkpoint will no longer be available.",
                    ),
                };

                let dialog = gtk::AlertDialog::builder()
                    .message(message)
                    .detail(detail)
                    .buttons(["Cancel", "Confirm"])
                    .cancel_button(0)
                    .default_button(0)
                    .modal(true)
                    .build();

                ui.replay_controls.popover.popdown();
                let window = ui.action_window();
                let weak = Rc::downgrade(&ui);
                let Some(context) = ui.model.stop_context(ui.model.current_stop_refresh_generation()) else {
                    return;
                };

                glib::spawn_future_local(async move {
                    if dialog.choose_future(Some(&window)).await == Ok(1)
                        && let Some(ui) = weak.upgrade()
                        && ui.model.is_stop_context_current(&context)
                    {
                        ui.replay_controls.emit(action);
                    }
                });
            });
        }
    }
}
