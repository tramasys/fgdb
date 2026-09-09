use super::*;

fn label(locations: &Locations) -> gtk::Label {
    locations
        .items
        .borrow()
        .iter()
        .filter_map(glib::WeakRef::upgrade)
        .find(|item| item_variable(item).is_some())
        .and_then(|item| item.child().and_downcast::<gtk::Label>())
        .unwrap()
}

#[test]
#[ignore = "requires a GTK display, run separately from other GTK tests"]
fn locals_refresh_keeps_verified_text_until_the_replacement_address_arrives() {
    gtk::init().unwrap();
    crate::theme::Theme::graphite().install();
    let pointer_bits = Rc::new(Cell::new(64));
    let model = Rc::new(DebuggerModel::new(None));
    model.set_current_thread_id(Some("1"));
    model.start_stop_refresh();
    model.bind_stop_context(1).unwrap();
    let locations = Locations::new(true, Rc::clone(&pointer_bits), Rc::clone(&model));
    let pending = Rc::new(RefCell::new(VecDeque::new()));
    let queue = Rc::clone(&pending);

    locations.connect(
        Rc::new(move |variables, current, reply| {
            queue.borrow_mut().push_back(Pending {
                variables,
                current,
                reply,
            });
        }),
        Rc::new(|_| true),
        Rc::new(|_| {}),
    );

    locations.set_context(Some((1, 0)));

    let (view, store, _) = views::build_locals_view(
        &crate::ui::ColumnLayouts::default().table(crate::ui::TableId::Locals),
        &Rc::new(RefCell::new(None)),
        &Rc::new(RefCell::new(None)),
        &Rc::new(VariableViewerRegistry::with_builtins()),
        &VariablePresentation::new(IntegerDisplay::Automatic, pointer_bits),
        &locations,
        None,
    );

    let changes = Rc::new(RefCell::new(Vec::new()));

    let factory = view
        .columns()
        .iter::<gtk::ColumnViewColumn>()
        .map(Result::unwrap)
        .find(|column| column.title().as_deref() == Some("LOCATION"))
        .unwrap()
        .factory()
        .and_downcast::<gtk::SignalListItemFactory>()
        .unwrap();

    let observed = Rc::clone(&changes);

    factory.connect_setup(move |_, object| {
        let item = object.downcast_ref::<gtk::ListItem>().unwrap();
        let label = item.child().and_downcast::<gtk::Label>().unwrap();
        let observed = Rc::clone(&observed);
        label.connect_label_notify(move |label| observed.borrow_mut().push(label.text()));
    });

    let mut root = variable(0);
    crate::ui::dialogs::replace_variable_roots_if_changed(&store, &[root.clone()]);
    let scrolled = gtk::ScrolledWindow::builder().child(&view).build();

    let window = gtk::Window::builder()
        .default_width(1200)
        .default_height(200)
        .child(&scrolled)
        .build();

    window.present();
    settle();
    assert!(label(&locations).text().is_empty());

    assert!(
        !pending.borrow().is_empty(),
        "visible empty location cells must still request their address: mapped={}, bounds={:?}, viewport={}x{}, visible={}, current={}",
        label(&locations).is_mapped(),
        label(&locations).compute_bounds(&scrolled),
        scrolled.width(),
        scrolled.height(),
        visible(&label(&locations)),
        locations.can_inspect(&root),
    );

    complete(&pending);
    assert_eq!(label(&locations).text(), "0x0000000000001000");
    changes.borrow_mut().clear();

    for (generation, varobj) in [(2, "created"), (3, "replacement")] {
        locations.set_context(None);
        settle();
        assert_eq!(label(&locations).text(), "0x0000000000001000");
        assert!(locations.cached(&root).is_none());
        assert_eq!(model.start_stop_refresh(), generation);
        model.bind_stop_context(1).unwrap();
        locations.set_context(Some((generation, 0)));
        root.varobj = Some(varobj.into());
        root.value = generation.to_string();
        crate::ui::dialogs::replace_variable_roots_if_changed(&store, &[root.clone()]);
        settle();
        assert_eq!(label(&locations).text(), "0x0000000000001000");
        assert!(locations.cached(&root).is_none());

        assert!(
            changes
                .borrow()
                .iter()
                .all(|text| text == "0x0000000000001000")
        );

        complete(&pending);
        assert_eq!(locations.cached(&root).unwrap().address(), Some(0x1000));
    }

    assert_eq!(model.start_stop_refresh(), 4);
    model.bind_stop_context(1).unwrap();
    locations.set_context(Some((4, 0)));
    settle();
    let reply = pending.borrow_mut().pop_front().unwrap();
    assert!((reply.current)());

    (reply.reply)(Some(vec![ValueLocation::Memory {
        address: 0x2000,
        referenced: false,
    }]));

    settle();
    assert_eq!(label(&locations).text(), "0x0000000000002000");

    assert!(
        changes
            .borrow()
            .iter()
            .all(|text| { matches!(text.as_str(), "0x0000000000001000" | "0x0000000000002000") })
    );

    model.set_current_thread_id(Some("2"));
    model.start_stop_refresh();
    model.bind_stop_context(1).unwrap();
    locations.set_context(Some((5, 0)));
    settle();
    assert!(label(&locations).text().is_empty());
    assert!(locations.cached(&root).is_none());
    window.close();
}
