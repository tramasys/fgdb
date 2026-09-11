use super::*;

fn breakpoint() -> Breakpoint {
    let record = crate::debugger::parse_record(
        r#"^done,bkpt={number="1",type="breakpoint",enabled="y",addr="0x1000",file="main.c",fullname="/tmp/main.c",line="12"}"#,
    )
    .unwrap();

    crate::debugger::inserted_breakpoints(&record).remove(0)
}

#[test]
fn source_targets_require_resolved_file_and_positive_line() {
    let original = breakpoint();
    let target = Some((Path::new("/tmp/main.c"), 12));
    assert_eq!(source_target(&original), target);
    let mut disabled = original.clone();
    disabled.enabled = false;
    assert_eq!(source_target(&disabled), target);
    let mut child = original.clone();
    child.number = "1.2".into();
    child.parent_number = Some("1".into());
    assert_eq!(source_target(&child), target);
    child.fullname = None;
    assert_eq!(source_target(&child), Some((Path::new("main.c"), 12)));

    for invalid in 0..5 {
        let mut breakpoint = original.clone();

        match invalid {
            0 => breakpoint.line = None,
            1 => breakpoint.line = Some(0),
            2 => breakpoint.pending = Some("main.c:12".into()),
            3 => breakpoint.fullname = Some(String::new()),
            _ => {
                breakpoint.fullname = None;
                breakpoint.file = None;
                breakpoint.original_location = Some("main.c:12".into());
                breakpoint.location_count = 2;
            }
        }

        assert_eq!(source_target(&breakpoint), None, "case {invalid}");
    }
}

#[test]
#[ignore = "requires a GTK display, run separately from other GTK tests"]
fn breakpoint_source_clicks_preserve_buttons_and_text_selection() {
    gtk::init().unwrap();
    Theme::graphite().install();
    let row = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let heading = gtk::Label::new(Some("BREAKPOINT main"));
    let source = gtk::Label::new(Some("/tmp/a<&>.c:12"));
    enable_stable_text_selection(&source);
    let status = gtk::Label::new(Some("2 HITS"));
    enable_stable_text_selection(&status);
    let button = gtk::Button::with_label("Delete");
    row.append(&heading);
    row.append(&source);
    row.append(&status);
    row.append(&button);
    let activations = Rc::new(Cell::new(0));
    let opened = Rc::clone(&activations);
    connect_navigation(&row, &source, move || opened.set(opened.get() + 1));

    let window = gtk::Window::builder()
        .default_width(420)
        .default_height(180)
        .child(&row)
        .build();

    window.present();
    let main = glib::MainContext::default();
    main.block_on(glib::timeout_future(Duration::from_millis(100)));
    assert_eq!(source.text(), "/tmp/a<&>.c:12");
    assert!(source.is_selectable());
    assert!(source.has_css_class("breakpoint-source"));

    assert!(row_navigation_target(
        row.upcast_ref(),
        heading.clone().upcast()
    ));

    assert!(!row_navigation_target(
        row.upcast_ref(),
        source.clone().upcast()
    ));

    assert!(!row_navigation_target(
        row.upcast_ref(),
        status.clone().upcast()
    ));

    assert!(!row_navigation_target(
        row.upcast_ref(),
        button.child().unwrap()
    ));

    let click = row
        .observe_controllers()
        .iter::<glib::Object>()
        .filter_map(Result::ok)
        .find_map(|controller| controller.downcast::<gtk::GestureClick>().ok())
        .unwrap();

    let center = |widget: &gtk::Widget| {
        let bounds = widget.compute_bounds(&row).unwrap();

        (
            f64::from(bounds.center().x()),
            f64::from(bounds.center().y()),
        )
    };

    let (x, y) = center(heading.upcast_ref());
    click.emit_by_name::<()>("pressed", &[&1_i32, &x, &y]);
    click.emit_by_name::<()>("released", &[&1_i32, &x, &y]);
    assert_eq!(activations.get(), 1);

    for widget in [
        button.upcast_ref(),
        source.upcast_ref(),
        status.upcast_ref(),
    ] {
        let (x, y) = center(widget);
        click.emit_by_name::<()>("pressed", &[&1_i32, &x, &y]);
        click.emit_by_name::<()>("released", &[&1_i32, &x, &y]);
    }

    button.emit_clicked();
    button.set_sensitive(false);
    let (button_x, button_y) = center(button.upcast_ref());
    click.emit_by_name::<()>("pressed", &[&1_i32, &button_x, &button_y]);
    click.emit_by_name::<()>("released", &[&1_i32, &button_x, &button_y]);
    button.set_sensitive(true);
    source.select_region(0, -1);
    assert!(source.selection_bounds().is_some());
    assert_eq!(activations.get(), 1);
    clear_label_selection(&source);
    assert!(source.emit_by_name::<bool>("activate-link", &[&"breakpoint-source"]));
    assert_eq!(activations.get(), 2);
    click.emit_by_name::<()>("pressed", &[&1_i32, &x, &y]);
    click.emit_by_name::<()>("stopped", &[]);
    click.emit_by_name::<()>("released", &[&1_i32, &x, &y]);
    assert_eq!(activations.get(), 2);
    click.emit_by_name::<()>("pressed", &[&2_i32, &x, &y]);
    click.emit_by_name::<()>("released", &[&2_i32, &x, &y]);
    assert_eq!(activations.get(), 2);

    let keys = row
        .observe_controllers()
        .iter::<glib::Object>()
        .filter_map(Result::ok)
        .find_map(|controller| controller.downcast::<gtk::EventControllerKey>().ok())
        .unwrap();

    assert!(row.grab_focus());

    assert!(keys.emit_by_name::<bool>(
        "key-pressed",
        &[
            &gtk::gdk::Key::Return,
            &0_u32,
            &gtk::gdk::ModifierType::empty()
        ],
    ));

    assert_eq!(activations.get(), 3);
    assert!(button.grab_focus());

    assert!(!keys.emit_by_name::<bool>(
        "key-pressed",
        &[
            &gtk::gdk::Key::Return,
            &0_u32,
            &gtk::gdk::ModifierType::empty()
        ],
    ));

    assert_eq!(activations.get(), 3);
    window.close();
}
