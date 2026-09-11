use super::*;

fn at_cursor(marked: &str) -> Input {
    let (before, after) = marked.split_once('|').unwrap();

    Input {
        text: format!("{before}{after}"),
        cursor: before.chars().count() as i32,
        selection: None,
    }
}

#[test]
fn metadata_completion_replaces_only_the_active_tag() {
    for (marked, expected) in [
        ("first, ne|, last", "first, network, last"),
        ("first,  ne|xt  , last", "first,  network  , last"),
        ("étiquette, ne|, 最後", "étiquette, network, 最後"),
        ("ne|xt, last", "network, last"),
    ] {
        let input = at_cursor(marked);
        let query = Query::new(&input, Field::Tags).unwrap();
        let (text, cursor) = query.complete(&input, "network");
        assert_eq!(text, expected);
        let before_cursor = text.chars().take(cursor as usize).collect::<String>();
        assert!(before_cursor.ends_with("network"));
    }
}

#[test]
fn metadata_completion_matches_prefixes_and_excludes_existing_tags() {
    let input = at_cursor("Network, ne|, next");
    let query = Query::new(&input, Field::Tags).unwrap();
    assert!(!query.matches(&input, &Choice::new("network".into())));
    assert!(!query.matches(&input, &Choice::new("NEXT".into())));
    assert!(!query.matches(&input, &Choice::new("other-network".into())));
    assert!(query.matches(&input, &Choice::new("New tag".into())));
    let input = at_cursor("new|, first");
    let query = Query::new(&input, Field::Tags).unwrap();
    assert!(!query.matches(&input, &Choice::new("new".into())));
}

#[test]
fn metadata_completion_keeps_free_form_input_and_handles_tag_boundaries() {
    for marked in ["|", "  |", "first, |"] {
        assert!(Query::new(&at_cursor(marked), Field::Tags).is_none());
    }

    let input = at_cursor("first, ne|  ");
    let query = Query::new(&input, Field::Tags).unwrap();
    let (text, cursor) = query.complete(&input, "new tag");
    assert_eq!(text, "first, new tag, ");
    assert_eq!(cursor as usize, text.chars().count());
    let input = at_cursor("  ne|w group  ");
    let query = Query::new(&input, Field::Group).unwrap();
    assert_eq!(query.complete(&input, "network"), ("network".into(), 7));
    let mut selected = at_cursor("one, tw|o, three");
    selected.selection = Some((2, 9));
    assert!(Query::new(&selected, Field::Tags).is_none());
    selected.selection = Some((8, 5));
    assert!(Query::new(&selected, Field::Tags).is_some());
}

fn descendants<T: IsA<glib::Object> + IsA<gtk::Widget> + Clone + 'static>(
    root: &impl IsA<gtk::Widget>,
) -> Vec<T> {
    let mut found = Vec::new();
    let mut child = root.as_ref().first_child();

    while let Some(widget) = child {
        child = widget.next_sibling();

        if let Ok(value) = widget.clone().downcast::<T>() {
            found.push(value);
        }

        found.extend(descendants::<T>(&widget));
    }

    found
}

fn press(window: &gtk::Window, key: gtk::gdk::Key) -> bool {
    let controller = window
        .observe_controllers()
        .iter::<glib::Object>()
        .filter_map(Result::ok)
        .find_map(|controller| controller.downcast::<gtk::EventControllerKey>().ok())
        .unwrap();

    controller.emit_by_name::<bool>(
        "key-pressed",
        &[&key, &0_u32, &gtk::gdk::ModifierType::empty()],
    )
}

#[test]
#[ignore = "requires a GTK display, run separately from other GTK tests"]
fn metadata_dialog_suggestions_preserve_editing_and_apply_only_on_request() {
    gtk::init().unwrap();
    Theme::graphite().install();
    let parent = gtk::Window::new();
    parent.present();
    let applied = Rc::new(RefCell::new(Vec::new()));
    let captured = Rc::clone(&applied);

    let dialog = open_stop_point_metadata_editor(
        &parent,
        "2",
        &normalized_stop_point_metadata("standard", "existing"),
        vec!["standard".into(), "startup".into()],
        vec!["existing".into(), "network".into(), "next".into()],
        Rc::new(move |metadata| captured.borrow_mut().push(metadata)),
    );

    let entries = descendants::<gtk::Entry>(&dialog);
    assert_eq!(entries.len(), 2);
    let group = &entries[0];
    let tags = &entries[1];
    let group_popup = descendants::<gtk::Popover>(group).pop().unwrap();
    let tag_popup = descendants::<gtk::Popover>(tags).pop().unwrap();
    let main = glib::MainContext::default();
    let settle = || main.block_on(glib::timeout_future(Duration::from_millis(80)));
    settle();
    assert!(!group_popup.is_visible());
    group.set_text("sta");
    group.set_position(-1);
    settle();
    assert!(group_popup.is_visible());
    assert_eq!(descendants::<gtk::ListBoxRow>(&group_popup).len(), 2);
    assert!(press(&dialog, gtk::gdk::Key::Down));
    assert!(press(&dialog, gtk::gdk::Key::Return));
    settle();
    assert_eq!(group.text(), "startup");
    assert!(group.selection_bounds().is_none());
    assert!(!group_popup.is_visible());
    assert!(applied.borrow().is_empty());
    assert!(dialog.is_visible());
    tags.grab_focus_without_selecting();
    tags.set_text("existing, ne, tail");
    tags.set_position(12);
    settle();
    assert!(tag_popup.is_visible());

    let list = tag_popup
        .child()
        .unwrap()
        .downcast::<gtk::ListBox>()
        .unwrap();

    list.emit_by_name::<()>("row-activated", &[&list.row_at_index(1).unwrap()]);
    settle();
    assert_eq!(tags.text(), "existing, next, tail");
    assert_eq!(tags.position(), 14);
    assert!(applied.borrow().is_empty());
    tags.set_text("existing, ne, network");
    tags.set_position(12);
    settle();
    assert_eq!(descendants::<gtk::ListBoxRow>(&tag_popup).len(), 1);
    assert!(press(&dialog, gtk::gdk::Key::Tab));
    assert_eq!(tags.text(), "existing, next, network");
    tags.set_text("existing, n");
    tags.set_position(-1);
    settle();
    assert!(press(&dialog, gtk::gdk::Key::Tab));
    settle();
    assert_eq!(tags.text(), "existing, network, ");
    assert!(!tag_popup.is_visible());
    assert!(applied.borrow().is_empty());
    tags.set_text("ne");
    tags.set_position(-1);
    settle();
    let old_row = list.row_at_index(0).unwrap();
    tags.set_text("unrelated");
    list.emit_by_name::<()>("row-activated", &[&old_row]);
    assert_eq!(tags.text(), "unrelated");
    tags.set_text("n");
    tags.set_position(-1);
    settle();
    assert!(press(&dialog, gtk::gdk::Key::Escape));
    settle();
    assert!(!tag_popup.is_visible());
    assert!(dialog.is_visible());
    tags.set_text("existing, network, ");

    let apply = descendants::<gtk::Button>(&dialog)
        .into_iter()
        .find(|button| button.label().as_deref() == Some("Apply"))
        .unwrap();

    apply.emit_clicked();

    assert_eq!(
        applied.borrow().as_slice(),
        &[normalized_stop_point_metadata(
            "startup",
            "existing, network"
        )]
    );

    assert!(group_popup.parent().is_none());
    assert!(tag_popup.parent().is_none());

    let dialog = open_stop_point_metadata_editor(
        &parent,
        "3",
        &StopPointMetadata::default(),
        (0..20).map(|index| format!("group-{index:02}")).collect(),
        Vec::new(),
        Rc::new(|_| panic!("Escape must not apply metadata")),
    );

    settle();
    let group = descendants::<gtk::Entry>(&dialog).remove(0);
    let popup = descendants::<gtk::Popover>(&group).pop().unwrap();
    group.set_text("group");
    group.set_position(-1);
    assert!(press(&dialog, gtk::gdk::Key::Return));
    assert_eq!(group.text(), "group-00");
    group.set_text("g");
    group.set_position(-1);
    settle();

    assert_eq!(
        descendants::<gtk::ListBoxRow>(&popup).len(),
        MAX_SUGGESTIONS
    );

    assert!(press(&dialog, gtk::gdk::Key::Escape));
    assert!(dialog.is_visible());
    assert!(press(&dialog, gtk::gdk::Key::Escape));
    assert!(!dialog.is_visible());
    parent.close();
}
