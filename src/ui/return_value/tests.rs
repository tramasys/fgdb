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
    let pointer_bits = Rc::new(Cell::new(64));
    let model = Rc::new(DebuggerModel::new(None));
    model.set_controls_ready(true);

    model.apply_debugger_state_delta(DebuggerStateDelta::establish_stopped_target(
        TargetConnection::Local,
    ));

    model.set_current_thread_id(Some("1"));

    let locations =
        variables::locations::Locations::new(false, Rc::clone(&pointer_bits), Rc::clone(&model));

    let presentation = Rc::new(variable_presentation::VariablePresentation::new(
        crate::config::settings::IntegerDisplay::Automatic,
        Rc::clone(&pointer_bits),
    ));

    let viewers = Rc::new(VariableViewerRegistry::with_builtins());
    let children = Rc::new(RefCell::new(None));
    let viewer_handler = Rc::new(RefCell::new(None));
    let kernel_handler = Rc::new(RefCell::new(None));
    let section_handler = Rc::new(RefCell::new(None));
    let misc_handler = Rc::new(RefCell::new(None));
    let disclosures = HashMap::new();

    let bindings = InspectorBindings {
        columns: &columns,
        theme: &theme,
        variable_children_handler: &children,
        variable_viewer_handler: &viewer_handler,
        variable_viewers: &viewers,
        target_pointer_bits: &pointer_bits,
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

    for (history, value) in [("$1", "42"), ("$2", "{x = 7, y = 11}")] {
        model.record_return_value(
            Some(ReturnValue {
                value: value.into(),
                history_variable: Some(history.into()),
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
    assert_eq!(view.store.n_items(), 2);
    assert_eq!(view.selection.n_items(), 2);
    assert_eq!(view.view.columns().n_items(), 4);
    assert!(view.root.is_visible());

    let filter = view
        .root
        .last_child()
        .and_then(|content| content.first_child())
        .and_then(|body| body.first_child())
        .and_then(|tools| tools.first_child())
        .and_downcast::<gtk::Entry>()
        .unwrap();

    filter.set_text("$1");
    assert_eq!(view.selection.n_items(), 1);
    assert_eq!(view.store.n_items(), 2);
    assert_eq!(variable_at(&view.selection, 0).unwrap().name, "$1");
    filter.set_text("");
    assert_eq!(view.selection.n_items(), 2);
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.add_css_class("debugger-root");
    root.append(&view.root);

    let window = gtk::Window::builder()
        .default_width(900)
        .default_height(240)
        .child(&root)
        .build();
    window.present();
    let main = glib::MainContext::default();
    main.block_on(glib::timeout_future(Duration::from_millis(100)));
    assert_eq!(requested.borrow().len(), 2);
    let original = view.store.item(0).unwrap();
    assert!(view.sync(model.return_values(), 1, true).is_none());
    assert_eq!(view.store.item(0).unwrap(), original);
    assert!(
        view.sync(model.return_values(), 1, false)
            .unwrap()
            .is_empty()
    );
    assert_eq!(view.store.item(0).unwrap(), original);

    {
        let mut state = view.state.borrow_mut();
        let variable = &mut state.entries[0].variable;
        variable.type_name = Some("ReturnPair".into());
        variable.varobj = Some("returned-pair".into());
        variable.num_children = 2;
    }

    view.render();
    let node = variable_root_node(&view.store, 0).unwrap();
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
    view.enabled.set(false);
    view.sync(Vec::new(), 2, true);
    assert!(!view.root.is_visible());
    assert_eq!(view.store.n_items(), 0);
    window.close();
}
