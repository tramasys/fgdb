use super::*;
use crate::debugger::ReturnValue;
use crate::model::{DebuggerModel, DebuggerStateDelta, TargetConnection};

#[test]
fn only_absolute_history_references_can_be_inspected() {
    for reference in [
        None,
        Some(""),
        Some("$"),
        Some("$$"),
        Some("$0"),
        Some("$01"),
        Some("$1; call foo()"),
        Some("$-1"),
    ] {
        assert!(!valid_history_reference(reference));
    }

    for reference in ["$1", "$12345"] {
        assert!(valid_history_reference(Some(reference)));
    }
}

#[test]
#[ignore = "requires a GTK display, run separately from other GTK tests"]
fn return_table_reuses_variable_controls_and_retires_stop_objects() {
    gtk::init().unwrap();
    let theme = Theme::graphite();
    theme.install();
    let columns = ColumnLayouts::default();
    let model = Rc::new(DebuggerModel::new(None));
    model.set_controls_ready(true);

    model.apply_debugger_state_delta(DebuggerStateDelta::establish_stopped_target(
        TargetConnection::Local,
    ));

    model.set_current_thread_id(Some("1"));

    let locations = variables::locations::Locations::new(false, Rc::clone(&model));

    let presentation = Rc::new(variable_presentation::VariablePresentation::new(
        crate::config::settings::IntegerDisplay::Automatic,
        Rc::clone(&model),
    ));

    let viewers = Rc::new(VariableViewerRegistry::with_builtins());
    let children = Rc::new(RefCell::new(None));
    let viewer_handler = Rc::new(RefCell::new(None));
    let kernel_handler = Rc::new(RefCell::new(None));
    let section_handler = Rc::new(RefCell::new(None));
    let misc_handler = Rc::new(RefCell::new(None));
    let disclosures = HashMap::new();

    let stop_info = stop_info::StopInfoView::new();

    let bindings = InspectorBindings {
        stop_info: &stop_info,
        columns: &columns,
        theme: &theme,
        variable_children_handler: &children,
        variable_viewer_handler: &viewer_handler,
        variable_viewers: &viewers,
        model: &model,
        variable_presentation: &presentation,
        variable_locations: &locations,
        kernel: KernelViewBindings {
            columns: &columns,
            refresh_handler: &kernel_handler,
            remembered_disclosures: &disclosures,
            section_handler: &section_handler,
        },
        misc: MiscViewBindings {
            refresh_handler: &misc_handler,
        },
    };

    let view = ReturnValueView::new(&bindings);
    assert!(!view.root.is_visible());
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.add_css_class("debugger-root");
    let split = gtk::Paned::new(gtk::Orientation::Vertical);
    let locals = gtk::Label::new(Some("Locals"));
    locals.set_vexpand(true);
    split.set_start_child(Some(&view.root));
    split.set_end_child(Some(&locals));
    split.set_resize_start_child(false);
    split.set_resize_end_child(true);
    split.set_shrink_start_child(false);
    split.set_shrink_end_child(false);
    split.set_position(180);
    view.fit_content(&split);
    root.append(&split);
    assert_eq!(split.position(), 0);

    for (history, value) in [("$1", "42"), ("$2", "{x = 7, y = 11}")] {
        model.record_return_value(
            Some(ReturnValue {
                value: value.into(),
                history_variable: Some(history.into()),
                function: None,
            }),
            Some("1"),
            None,
        );
    }

    let requested = Rc::new(RefCell::new(Vec::new()));
    let captured = Rc::clone(&requested);
    view.refresh.replace(Some(Rc::new(move |variable| {
        captured.borrow_mut().push(variable)
    })));
    assert!(view.sync(model.return_values(), 1, true).is_some());
    assert_eq!(view.tree.store.n_items(), 2);
    assert_eq!(view.tree.selection.n_items(), 2);
    assert_eq!(view.view.columns().n_items(), 5);
    assert!(view.root.is_visible());
    assert_eq!(split.position(), 180);

    let filter = view
        .root
        .last_child()
        .and_then(|content| content.first_child())
        .and_then(|body| body.first_child())
        .and_then(|tools| tools.first_child())
        .and_downcast::<gtk::Entry>()
        .unwrap();

    filter.set_text("$1");
    assert_eq!(view.tree.selection.n_items(), 1);
    assert_eq!(view.tree.store.n_items(), 2);
    assert_eq!(variable_at(&view.tree.selection, 0).unwrap().name, "$1");
    filter.set_text("");
    assert_eq!(view.tree.selection.n_items(), 2);
    let window = gtk::Window::builder()
        .default_width(900)
        .default_height(240)
        .child(&root)
        .build();
    window.present();
    let main = glib::MainContext::default();
    main.block_on(glib::timeout_future(Duration::from_millis(100)));
    assert_eq!(requested.borrow().len(), 2);
    let original = view.tree.store.item(0).unwrap();
    assert!(view.sync(model.return_values(), 1, true).is_none());
    assert_eq!(view.tree.store.item(0).unwrap(), original);
    assert!(
        view.sync(model.return_values(), 1, false)
            .unwrap()
            .is_empty()
    );
    assert_eq!(view.tree.store.item(0).unwrap(), original);

    {
        let mut state = view.state.borrow_mut();
        let variable = &mut state.entries[0].variable;
        variable.type_name = Some("ReturnPair".into());
        variable.varobj = Some("returned-pair".into());
        variable.num_children = 2;
    }

    view.render();
    let node = variable_root_node(&view.tree.store, 0).unwrap();
    assert!(node.variable.can_expand());
    let mut field = node.variable.clone();
    field.name = "x".into();
    field.varobj = Some("returned-pair.x".into());
    field.return_value = None;
    assert_eq!(
        node.child(field).variable.return_value,
        node.variable.return_value
    );

    if let Some(path) = std::env::var_os("FGDB_RETURN_VALUE_CAPTURE") {
        main.block_on(glib::timeout_future(Duration::from_millis(100)));
        let paintable = gtk::WidgetPaintable::new(Some(&root));
        let snapshot = gtk::Snapshot::new();
        paintable.snapshot(&snapshot, f64::from(root.width()), f64::from(root.height()));
        let node = snapshot.to_node().unwrap();
        window
            .renderer()
            .unwrap()
            .render_texture(&node, None)
            .save_to_png(path)
            .unwrap();
    }

    let retired = view.sync(model.return_values(), 2, true).unwrap();
    assert_eq!(retired, ["returned-pair"]);
    assert!(view.state.borrow().entries[0].variable.varobj.is_none());
    assert_eq!(
        view.state.borrow().entries[0].variable.type_name.as_deref(),
        Some("ReturnPair")
    );
    let height = split.position();
    let expanded_locals_height = locals.height();

    let header = view
        .root
        .first_child()
        .and_downcast::<gtk::Button>()
        .unwrap();

    header.emit_clicked();
    main.block_on(glib::timeout_future(Duration::from_millis(100)));
    assert!(!view.content.is_visible());

    let collapsed_height = header
        .measure(gtk::Orientation::Vertical, view.root.width())
        .0;

    assert_eq!(view.root.height(), collapsed_height);
    assert!(locals.height() > expanded_locals_height);
    view.sync(Vec::new(), 2, true);
    view.sync(model.return_values(), 2, true);
    main.block_on(glib::timeout_future(Duration::from_millis(100)));
    assert_eq!(view.root.height(), collapsed_height);
    header.emit_clicked();
    main.block_on(glib::timeout_future(Duration::from_millis(100)));
    assert_eq!(split.position(), height);
    assert_eq!(locals.height(), expanded_locals_height);
    split.set_position(height - 20);
    main.block_on(glib::timeout_future(Duration::from_millis(100)));
    let resized_height = split.position();
    view.sync(Vec::new(), 2, true);
    assert_eq!(split.position(), 0);
    assert!(view.availability.is_visible());
    assert!(!view.scrolled.is_visible());
    main.block_on(glib::timeout_future(Duration::from_millis(100)));
    let empty_height = view.root.height();
    assert!(empty_height < resized_height);
    header.emit_clicked();
    main.block_on(glib::timeout_future(Duration::from_millis(100)));
    assert_eq!(view.root.height(), collapsed_height);
    header.emit_clicked();
    main.block_on(glib::timeout_future(Duration::from_millis(100)));
    assert_eq!(view.root.height(), empty_height);
    view.sync(model.return_values(), 2, true);
    main.block_on(glib::timeout_future(Duration::from_millis(100)));
    assert_eq!(split.position(), resized_height);
    view.root.set_visible(false);
    main.block_on(glib::timeout_future(Duration::from_millis(100)));
    assert_eq!(locals.height(), split.height());
    view.root.set_visible(true);
    main.block_on(glib::timeout_future(Duration::from_millis(100)));
    assert_eq!(split.position(), resized_height);
    view.enabled.set(false);
    view.sync(Vec::new(), 2, true);
    assert!(!view.root.is_visible());
    assert_eq!(view.tree.store.n_items(), 0);
    window.close();
}
