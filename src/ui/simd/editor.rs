use super::*;

struct VectorEditor {
    window: glib::WeakRef<gtk::Window>,
    register: Register,
    model: Rc<crate::model::DebuggerModel>,
    context: crate::debugger::StopContext,
    handler: Rc<RefCell<Option<VectorAssignmentHandler>>>,
    original: Option<VectorValue>,
    draft: RefCell<Option<VectorValue>>,
    display: Cell<VectorDisplay>,
    controls: VectorControls,
    grid: gtk::Grid,
    entries: RefCell<Vec<(gtk::Entry, String)>>,
    status: gtk::Label,
    apply: gtk::Button,
    revert: gtk::Button,
    updating: Cell<bool>,
    busy: Cell<bool>,
}

impl VectorEditor {
    fn proposed(&self) -> Result<VectorValue, String> {
        let mut draft = self
            .draft
            .borrow()
            .clone()
            .ok_or("GDB did not expose a complete supported register layout")?;

        let display = self.display.get();

        for (index, (entry, rendered)) in self.entries.borrow().iter().enumerate() {
            let text = entry.text();

            // Unedited floating-point text must not round-trip through a parser.
            // In particular, NaN text cannot represent its original payload.
            if text.trim() == rendered {
                continue;
            }

            let raw = display
                .parse(&text)
                .map_err(|error| format!("Lane {index}: {error}"))?;

            if !draft.set_lane(index, display.format.lane_bytes(), raw) {
                return Err(format!("Lane {index} is outside this register"));
            }
        }

        Ok(draft)
    }

    fn update_actions(&self) {
        if self.updating.get() || self.busy.get() {
            return;
        }

        let current = self.model.is_stop_context_current(&self.context)
            && self.model.can_edit_variable(self.context.generation());

        let proposed = self.proposed();
        let changed = proposed
            .as_ref()
            .is_ok_and(|draft| Some(draft) != self.original.as_ref());

        self.apply.set_sensitive(current && changed);
        self.revert
            .set_sensitive(self.original.is_some() && (changed || proposed.is_err()));

        let message = if !current {
            "The debugger context changed. Close and reopen this register before editing."
                .to_owned()
        } else {
            match proposed {
                Ok(_) if changed => {
                    "Unapplied changes. Switching interpretation preserves valid edits.".to_owned()
                }
                Ok(_) => "Lane 0 is least significant. Changes are written only when you apply."
                    .to_owned(),
                Err(error) => error,
            }
        };

        self.status.set_text(&message);
    }

    fn render(self: &Rc<Self>) {
        self.updating.set(true);
        self.entries.borrow_mut().clear();

        while let Some(child) = self.grid.first_child() {
            self.grid.remove(&child);
        }

        let display = self.display.get();
        self.controls.set_display(display);

        if let Some(value) = self.draft.borrow().as_ref() {
            for (column, title) in ["LANE", "BITS", "VALUE"].into_iter().enumerate() {
                let label = components::section_title(title);
                self.grid.attach(&label, column as i32, 0, 1, 1);
            }

            for index in 0..value.bytes() / display.format.lane_bytes() {
                let bits = display.format.lane_bytes() * 8;
                let label = gtk::Label::new(Some(&format!("[{index}]")));
                label.add_css_class("vector-lane-index");
                label.set_xalign(1.0);
                let range = gtk::Label::new(Some(&format!(
                    "{}-{}",
                    index * bits,
                    (index + 1) * bits - 1
                )));

                range.add_css_class("muted");
                range.set_xalign(1.0);
                let entry = gtk::Entry::builder()
                    .hexpand(true)
                    .width_chars(16)
                    .max_length(128)
                    .build();

                let raw = value.lane(index, display.format.lane_bytes()).unwrap();
                let text = display.text(raw);
                entry.set_text(&text);
                entry.set_tooltip_text(Some(&format!(
                    "${} lane {index}, raw bits 0x{raw:0width$x}",
                    self.register.name,
                    width = display.format.lane_bytes() * 2
                )));

                self.grid.attach(&label, 0, index as i32 + 1, 1, 1);
                self.grid.attach(&range, 1, index as i32 + 1, 1, 1);
                self.grid.attach(&entry, 2, index as i32 + 1, 1, 1);
                self.entries.borrow_mut().push((entry.clone(), text));
                let weak = Rc::downgrade(self);

                entry.connect_changed(move |_| {
                    if let Some(editor) = weak.upgrade() {
                        editor.update_actions();
                    }
                });
            }
        } else {
            let raw = gtk::Label::builder()
                .label(&self.register.value)
                .xalign(0.0)
                .wrap(true)
                .wrap_mode(pango::WrapMode::WordChar)
                .build();

            enable_stable_text_selection(&raw);
            self.grid.attach(&raw, 0, 0, 3, 1);
            self.controls.root.set_sensitive(false);
        }

        self.updating.set(false);
        self.update_actions();
    }

    fn switch(self: &Rc<Self>) {
        if self.updating.get() {
            return;
        }

        match self.proposed() {
            Ok(draft) => {
                self.draft.replace(Some(draft));
                self.display.set(self.controls.display());
                self.render();
            }
            Err(error) => {
                self.updating.set(true);
                self.controls.set_display(self.display.get());
                self.updating.set(false);
                self.status.set_text(&error);
            }
        }
    }

    fn apply(self: &Rc<Self>) {
        if self.busy.get()
            || !self.model.is_stop_context_current(&self.context)
            || !self.model.can_edit_variable(self.context.generation())
        {
            self.update_actions();
            return;
        }

        let (Some(original), Ok(edited)) = (self.original.clone(), self.proposed()) else {
            self.update_actions();
            return;
        };

        if original == edited {
            return;
        }

        let Some(handler) = self.handler.borrow().clone() else {
            self.status.set_text("Register editing is unavailable");
            return;
        };

        self.busy.set(true);
        self.apply.set_sensitive(false);
        self.revert.set_sensitive(false);
        self.grid.set_sensitive(false);
        self.controls.root.set_sensitive(false);
        self.status.set_text("Applying register changes...");
        let weak = Rc::downgrade(self);

        handler(
            VectorWrite {
                context: self.context.clone(),
                register: self.register.name.clone(),
                original,
                edited,
            },
            Box::new(move |result| {
                let Some(editor) = weak.upgrade() else {
                    return;
                };

                editor.busy.set(false);

                match result {
                    Ok(()) => {
                        if let Some(window) = editor.window.upgrade() {
                            window.close();
                        }
                    }
                    Err(error) => {
                        editor.grid.set_sensitive(true);
                        editor.controls.root.set_sensitive(true);
                        editor.update_actions();
                        editor.status.set_text(&error);
                    }
                }
            }),
        );
    }
}

pub(in crate::ui) fn open_vector_editor(
    parent: &impl IsA<gtk::Window>,
    register: Register,
    display: VectorDisplay,
    model: Rc<crate::model::DebuggerModel>,
    context: crate::debugger::StopContext,
    handler: Rc<RefCell<Option<VectorAssignmentHandler>>>,
) -> Option<gtk::Window> {
    if !model.is_stop_context_current(&context) {
        return None;
    }

    let (window, _) = build_vector_editor(parent, register, display, model, context, handler);
    window.present();

    Some(window)
}

fn build_vector_editor(
    parent: &impl IsA<gtk::Window>,
    register: Register,
    display: VectorDisplay,
    model: Rc<crate::model::DebuggerModel>,
    context: crate::debugger::StopContext,
    handler: Rc<RefCell<Option<VectorAssignmentHandler>>>,
) -> (gtk::Window, Rc<VectorEditor>) {
    let window = gtk::Window::builder()
        .title(format!("SIMD register ${}", register.name))
        .transient_for(parent)
        .modal(true)
        .default_width(640)
        .default_height(520)
        .build();

    window.add_css_class("value-editor");
    let content = gtk::Box::new(gtk::Orientation::Vertical, components::CONTROL_GAP);
    components::inset(&content, components::DIALOG_INSET);
    let heading = gtk::Label::builder()
        .label(format!(
            "${}    {} bits",
            register.name,
            crate::debugger::vector::register_bytes(&register.name).unwrap_or(0) * 8
        ))
        .xalign(0.0)
        .css_classes(["local-name"])
        .build();

    content.append(&heading);
    let controls = VectorControls::new();
    controls.set_display(display);
    content.append(&controls.root);

    let grid = gtk::Grid::builder()
        .column_spacing(12)
        .row_spacing(4)
        .hexpand(true)
        .valign(gtk::Align::Start)
        .build();

    components::inset(&grid, components::CONTENT_INSET);

    let scroll = gtk::ScrolledWindow::builder()
        .child(&grid)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build();

    content.append(&scroll);
    let status = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .css_classes(["muted"])
        .build();

    content.append(&status);
    let actions = components::control_row();
    let revert = gtk::Button::with_label("Reset edits");
    actions.append(&revert);
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    actions.append(&spacer);
    let close = gtk::Button::with_label("Close");
    let apply = gtk::Button::with_label("Apply changes");
    apply.add_css_class("primary-control");
    actions.append(&close);
    actions.append(&apply);
    content.append(&actions);
    window.set_child(Some(&content));
    connect_escape_to_close(&window);
    let original = VectorValue::parse(&register.name, &register.value);

    let editor = Rc::new(VectorEditor {
        window: window.downgrade(),
        register,
        model,
        context,
        handler,
        draft: RefCell::new(original.clone()),
        original,
        display: Cell::new(display),
        controls,
        grid,
        entries: RefCell::new(Vec::new()),
        status,
        apply,
        revert,
        updating: Cell::new(false),
        busy: Cell::new(false),
    });

    for control in [&editor.controls.interpretation, &editor.controls.base] {
        let weak = Rc::downgrade(&editor);
        control.connect_selected_notify(move |_| {
            if let Some(editor) = weak.upgrade() {
                editor.switch();
            }
        });
    }

    let weak = Rc::downgrade(&editor);
    editor.revert.connect_clicked(move |_| {
        if let Some(editor) = weak.upgrade() {
            editor.draft.replace(editor.original.clone());
            editor.render();
        }
    });

    let weak = Rc::downgrade(&editor);
    editor.apply.connect_clicked(move |_| {
        if let Some(editor) = weak.upgrade() {
            editor.apply();
        }
    });

    let weak = window.downgrade();
    close.connect_clicked(move |_| {
        if let Some(window) = weak.upgrade() {
            window.close();
        }
    });

    editor.render();
    let retained = Rc::clone(&editor);

    window.connect_destroy(move |_| {
        let _ = &retained;
    });

    (window, editor)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires a GTK display, run separately from other GTK tests"]
    fn vector_editor_preserves_drafts_and_handles_failure_and_stale_context() {
        gtk::init().unwrap();
        Theme::graphite().install();
        let model = Rc::new(crate::model::DebuggerModel::new(None));
        model.set_controls_ready(true);
        model.apply_debugger_state_delta(DebuggerStateDelta::establish_stopped_target(
            TargetConnection::Local,
        ));
        model.publish_threads(&[ThreadInfo {
            id: "1".into(),
            group_id: Some("i1".into()),
            target_id: "Thread 1".into(),
            name: None,
            state: "stopped".into(),
            core: None,
            frame: None,
            pc_symbol: None,
            current: true,
        }]);
        model.start_stop_refresh();
        let context = model.bind_stop_context(1).unwrap();
        let parent = gtk::ApplicationWindow::builder().build();
        let register = Register {
            name: "xmm0".into(),
            value: "{v2_int64 = {0xc02000003f800000, 0x7fc0004280000000}}".into(),
            pointer_chain: Vec::new(),
        };

        let summary = VectorDisplay::default().summary(&register);
        assert!(summary.starts_with("[0]\u{a0}0x"));

        let summary = VectorDisplay {
            format: VectorLaneFormat::from_index(0),
            ..VectorDisplay::default()
        }
        .summary(&register);

        assert!(summary.starts_with("[0]\u{a0}\u{a0}0x"));
        assert!(summary.contains("[10]\u{a0}0x"));
        assert!(!summary.contains("[\u{a0}"));
        let calls = Rc::new(Cell::new(0));
        let count = Rc::clone(&calls);
        let handler: VectorAssignmentHandler = Rc::new(move |_, completed| {
            count.set(count.get() + 1);
            completed(Err("A simulated register write failure".into()));
        });

        let (window, editor) = build_vector_editor(
            &parent,
            register,
            VectorDisplay::default(),
            Rc::clone(&model),
            context,
            Rc::new(RefCell::new(Some(handler))),
        );
        window.present();
        let main = glib::MainContext::default();
        main.block_on(glib::timeout_future(Duration::from_millis(50)));
        editor.controls.interpretation.set_selected(4);
        main.block_on(glib::timeout_future(Duration::from_millis(50)));
        let (content, groups) = build_register_view(&crate::ui::ColumnLayouts::default());
        let group = groups
            .iter()
            .find(|group| group.kind == RegisterGroupKind::Vector)
            .unwrap();
        let rows = (0..4)
            .map(|index| RegisterRowData {
                register: Register {
                    name: format!("ymm{index}"),
                    value: "{v4_int64 = {0xc02000003f800000, 0x7fc0004280000000, 0, 0}}".into(),
                    pointer_chain: Vec::new(),
                },
                changed: index == 0,
                ring: None,
                architecture: TargetArchitecture::X86_64,
                endian: Some(TargetEndian::Little),
                pointer_bits: 64,
                vector_display: VectorDisplay::default(),
            })
            .collect::<Vec<_>>();
        populate_register_group(group, rows.clone(), false);
        let identity = group.store.item(0);
        populate_register_group(group, rows, false);
        assert_eq!(group.store.item(0), identity);
        group
            .panel
            .first_child()
            .and_downcast::<gtk::Button>()
            .unwrap()
            .emit_clicked();
        let scroll = gtk::ScrolledWindow::builder()
            .child(&content)
            .overlay_scrolling(false)
            .build();

        parent.set_child(Some(&scroll));
        parent.set_default_size(950, 500);
        parent.present();
        main.block_on(glib::timeout_future(Duration::from_millis(100)));
        let RegisterGroupWidget::Vector(view) = &group.view else {
            unreachable!();
        };
        let list = &view.list;
        list.select_row(list.row_at_index(2).as_ref());
        group
            .vector_controls
            .as_ref()
            .unwrap()
            .interpretation
            .set_selected(0);
        main.block_on(glib::timeout_future(Duration::from_millis(100)));
        assert_eq!(list.selected_row().unwrap().index(), 2);
        let wide_height = list.row_at_index(0).unwrap().height();
        parent.set_default_size(520, 500);
        main.block_on(glib::timeout_future(Duration::from_millis(100)));
        assert!(list.row_at_index(0).unwrap().height() > wide_height);
        assert!(scroll.vadjustment().upper() > scroll.vadjustment().page_size());
        let scrollbar = scroll.vscrollbar().compute_bounds(&scroll).unwrap();
        let inspect = group
            .vector_controls
            .as_ref()
            .unwrap()
            .root
            .last_child()
            .unwrap();

        let button = inspect.compute_bounds(&scroll).unwrap();
        assert!(button.x() + button.width() <= scrollbar.x());
        assert_eq!(
            editor.proposed().unwrap(),
            *editor.original.as_ref().unwrap()
        );
        let entry = editor.entries.borrow()[0].0.clone();
        entry.set_text("1.5");
        assert!(editor.apply.is_sensitive());
        editor.controls.interpretation.set_selected(0);
        assert_eq!(editor.proposed().unwrap().lane(0, 4), Some(0x3fc00000));
        assert_eq!(editor.proposed().unwrap().lane(3, 4), Some(0x7fc00042));
        let entry = editor.entries.borrow()[0].0.clone();
        entry.set_text("invalid");
        editor.controls.interpretation.set_selected(3);
        assert_eq!(editor.controls.interpretation.selected(), 0);
        assert!(!editor.apply.is_sensitive());
        entry.set_text("0x00");
        editor.apply();
        assert_eq!(calls.get(), 1);
        assert!(window.is_visible());
        assert!(editor.status.text().contains("simulated"));
        assert_eq!(editor.proposed().unwrap().lane(0, 4), Some(0x3fc00000));
        model.start_stop_refresh();
        editor.apply();
        assert_eq!(calls.get(), 1);
        assert!(!editor.apply.is_sensitive());
        assert!(editor.status.text().contains("context changed"));

        let weak = Rc::downgrade(&editor);
        window.close();
        drop(editor);
        drop(window);
        main.block_on(glib::timeout_future(Duration::from_millis(20)));
        assert!(weak.upgrade().is_none());
        parent.close();
    }
}
