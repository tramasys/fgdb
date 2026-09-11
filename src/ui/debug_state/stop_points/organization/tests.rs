use super::*;
use std::rc::Weak;

pub(super) fn breakpoint(number: &str) -> Breakpoint {
    let record = crate::debugger::parse_record(&format!(
        r#"^done,bkpt={{number="{number}",type="breakpoint",enabled="y",addr="0x1000"}}"#,
    ))
    .unwrap();

    crate::debugger::inserted_breakpoints(&record).remove(0)
}

pub(super) fn metadata(group: &str, tags: &str) -> StopPointMetadata {
    normalized_stop_point_metadata(group, tags)
}

#[test]
fn organization_catalog_uses_active_parents_and_deduplicates_tags() {
    let mut child = breakpoint("1.1");
    child.parent_number = Some("1".into());
    let mut breakpoints = vec![breakpoint("1"), child, breakpoint("2"), breakpoint("3")];

    let metadata = HashMap::from([
        ("1".into(), metadata("standard", "Startup, hot")),
        ("1.1".into(), metadata("child-only", "ignored")),
        ("2".into(), metadata("network", "startup, network")),
        ("4".into(), metadata("deleted", "stale")),
    ]);

    let catalog = Catalog::build(&breakpoints, &metadata);
    assert_eq!(catalog.groups, ["network", "standard"]);
    assert_eq!(catalog.tags, ["hot", "network", "Startup"]);
    assert!(catalog.ungrouped);
    breakpoints.reverse();
    assert_eq!(Catalog::build(&breakpoints, &metadata), catalog);
    assert_eq!(Catalog::build(&[], &metadata), Catalog::default());
}

#[test]
fn organization_filters_match_whole_values_and_combine() {
    let filter = Selection {
        group: Some("standard".into()),
        tag: Some("HOT".into()),
    };

    assert!(filter.matches(Some(&metadata("standard", "hot, startup"))));
    assert!(!filter.matches(Some(&metadata("standard-other", "hot"))));
    assert!(!filter.matches(Some(&metadata("standard", "hot-path"))));
    assert!(!filter.matches(None));

    let tags_only = Selection {
        group: None,
        tag: Some("hot".into()),
    };

    assert!(tags_only.matches(Some(&metadata("", "HOT"))));

    let all = Selection {
        group: None,
        tag: None,
    };

    assert!(all.matches(None));
    assert!(all.matches(Some(&metadata("group with spaces", "tag with spaces"))));

    let spaced = Selection {
        group: Some("group with spaces".into()),
        tag: Some("tag with spaces".into()),
    };

    assert!(spaced.matches(Some(&metadata("group with spaces", "tag with spaces"))));
}

fn children(root: &impl IsA<gtk::Widget>) -> Vec<gtk::Widget> {
    let mut children = Vec::new();
    let mut current = root.as_ref().first_child();

    while let Some(child) = current {
        current = child.next_sibling();
        children.push(child);
    }

    children
}

#[test]
#[ignore = "requires a GTK display, run separately from other GTK tests"]
fn breakpoint_groups_and_quick_filters_keep_state_across_refreshes() {
    gtk::init().unwrap();
    Theme::graphite().install();
    let organization = StopPointOrganization::new();
    let events = Rc::new(Cell::new(0));
    let changed = Rc::clone(&events);
    organization.connect_changed(move || changed.set(changed.get() + 1));
    let breakpoints = vec![breakpoint("1"), breakpoint("2"), breakpoint("3")];
    organization.sync(&breakpoints, &HashMap::new());
    assert!(!organization.root.is_visible());

    let mut metadata = HashMap::from([
        ("1".into(), metadata("standard", "hot, startup")),
        ("2".into(), metadata("standard", "startup")),
    ]);

    organization.sync(&breakpoints, &metadata);
    assert!(organization.root.is_visible());
    assert!(organization.groups.widget.is_visible());
    assert!(organization.tags.widget.is_visible());
    organization.groups.widget.set_selected(1);
    organization.tags.widget.set_selected(1);
    assert_eq!(events.get(), 2);
    assert!(organization.selection().matches(metadata.get("1")));
    assert!(!organization.selection().matches(metadata.get("2")));
    let model = organization.groups.widget.model().unwrap();
    organization.sync(&breakpoints, &metadata);
    assert_eq!(organization.groups.widget.model().unwrap(), model);
    assert_eq!(events.get(), 2);
    metadata.insert("3".into(), normalized_stop_point_metadata("aaa", "cold"));
    organization.sync(&breakpoints, &metadata);
    assert_eq!(organization.groups.selected().as_deref(), Some("standard"));
    assert_eq!(organization.tags.selected().as_deref(), Some("hot"));
    assert_eq!(organization.groups.widget.selected(), 2);
    assert_eq!(organization.tags.widget.selected(), 2);
    assert_eq!(events.get(), 2);
    metadata.remove("3");
    organization.sync(&breakpoints, &metadata);
    let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let mut groups = GroupRows::new(&organization, Weak::new());
    let ungrouped = groups.parent_container(None);
    ungrouped.append(&gtk::Label::new(Some("Ungrouped breakpoint")));
    let standard = groups.parent_container(Some("standard"));
    standard.append(&gtk::Label::new(Some("Parent breakpoint")));
    standard.append(&gtk::Label::new(Some("Resolved child location")));
    assert_eq!(groups.parent_container(Some("standard")), standard);
    standard.append(&gtk::Label::new(Some("Second breakpoint")));
    groups.append_to(&list);
    assert_eq!(children(&list).len(), 2);

    let first = list
        .first_child()
        .unwrap()
        .downcast::<gtk::Expander>()
        .unwrap();

    assert_eq!(first.child().unwrap(), standard);
    assert_eq!(children(&standard).len(), 3);
    let heading = first.label_widget().unwrap();

    let count = children(&heading)
        .into_iter()
        .find(|child| child.has_css_class("breakpoint-group-count"))
        .unwrap()
        .downcast::<gtk::Label>()
        .unwrap();

    assert_eq!(count.text(), "2");
    let indicator = heading.first_child().and_downcast::<gtk::Label>().unwrap();
    assert!(indicator.has_css_class("disclosure-arrow"));
    assert_eq!(indicator.text(), DISCLOSURE_EXPANDED_ICON);
    let root = gtk::Box::new(gtk::Orientation::Vertical, 4);
    root.add_css_class("debugger-root");
    root.append(&organization.root);
    root.append(&list);
    let chips = tag_chips(&["startup".into(), "hot path".into()]);
    standard.append(&chips);

    let window = gtk::Window::builder()
        .default_width(380)
        .default_height(300)
        .child(&root)
        .build();

    window.present();
    let main = glib::MainContext::default();
    main.block_on(glib::timeout_future(Duration::from_millis(100)));
    assert!(standard.is_mapped());
    assert!(standard.compute_bounds(&first).unwrap().y() >= 30.0);
    assert!((heading.compute_bounds(&first).unwrap().x() - 8.0).abs() <= 1.0);
    assert!(chips.width() > 0 && chips.width() < standard.width());
    assert!(organization.root.width() <= root.width());

    if let Some(path) = std::env::var_os("FGDB_BREAKPOINT_GROUP_CAPTURE") {
        let paintable = gtk::WidgetPaintable::new(Some(&root));
        let snapshot = gtk::Snapshot::new();

        paintable.snapshot(&snapshot, f64::from(root.width()), f64::from(root.height()));
        let node = snapshot.to_node().expect("rendered breakpoint groups");

        window
            .renderer()
            .unwrap()
            .render_texture(&node, None)
            .save_to_png(path)
            .unwrap();
    }

    first.emit_activate();
    main.block_on(glib::timeout_future(Duration::from_millis(30)));
    assert!(!standard.is_mapped());
    assert_eq!(indicator.text(), DISCLOSURE_COLLAPSED_ICON);

    let menu = organization.group_controls.borrow()[0].1.button.clone();
    menu.set_sensitive(true);
    menu.popup();
    main.block_on(glib::timeout_future(Duration::from_millis(30)));
    assert!(menu.popover().unwrap().is_mapped());
    assert!(!first.is_expanded());
    menu.popdown();

    assert!(
        organization
            .collapsed
            .borrow()
            .contains(&Some("standard".into()))
    );

    clear_box(&list);
    organization.sync(&breakpoints, &metadata);
    let mut groups = GroupRows::new(&organization, Weak::new());
    assert!(!menu.is_sensitive());
    groups.parent_container(Some("standard"));
    groups.append_to(&list);

    let first = list
        .first_child()
        .unwrap()
        .downcast::<gtk::Expander>()
        .unwrap();

    assert!(!first.is_expanded());

    let indicator = first
        .label_widget()
        .unwrap()
        .first_child()
        .and_downcast::<gtk::Label>()
        .unwrap();

    assert_eq!(indicator.text(), DISCLOSURE_COLLAPSED_ICON);
    first.emit_activate();
    assert_eq!(indicator.text(), DISCLOSURE_EXPANDED_ICON);
    assert!(organization.collapsed.borrow().is_empty());

    let tags = (0..20)
        .map(|index| format!("tag-{index}"))
        .collect::<Vec<_>>();

    let chips = tag_chips(&tags);
    assert_eq!(children(&chips).len(), 8);

    let more = chips
        .child_at_index(7)
        .unwrap()
        .child()
        .unwrap()
        .downcast::<gtk::Label>()
        .unwrap();

    assert_eq!(more.text(), "+13");
    assert!(more.tooltip_text().unwrap().contains("tag-19"));
    organization.sync(&breakpoints, &HashMap::new());
    assert!(!organization.root.is_visible());
    assert!(organization.groups.selected().is_none());
    assert!(organization.tags.selected().is_none());
    assert_eq!(events.get(), 2);
    clear_box(&list);
    let mut groups = GroupRows::new(&organization, Weak::new());
    let flat = groups.parent_container(None);
    groups.append_to(&list);
    assert_eq!(list.first_child().unwrap(), flat);
    assert!(!flat.is::<gtk::Expander>());
    window.close();
}

#[test]
#[ignore = "requires a GTK display, run separately from other GTK tests"]
fn deleting_last_group_members_clears_filters_and_collapse_state() {
    gtk::init().unwrap();
    let organization = StopPointOrganization::new();
    let breakpoints = vec![breakpoint("1"), breakpoint("2")];

    let metadata = HashMap::from([
        ("1".into(), metadata("standard", "startup")),
        ("2".into(), metadata("standard", "startup")),
    ]);

    organization.sync(&breakpoints, &metadata);
    organization.groups.widget.set_selected(1);
    organization.tags.widget.set_selected(1);

    organization
        .collapsed
        .borrow_mut()
        .insert(Some("standard".into()));

    // A failed or partial deletion keeps the group while any member survives.
    for snapshot in [&breakpoints[..], &breakpoints[1..]] {
        organization.sync(snapshot, &metadata);
        assert_eq!(organization.groups.selected().as_deref(), Some("standard"));
        assert_eq!(organization.tags.selected().as_deref(), Some("startup"));
        assert!(!organization.collapsed.borrow().is_empty());
    }

    organization.sync(&[], &metadata);
    assert!(organization.groups.values.borrow().is_empty());
    assert!(organization.groups.selected().is_none());
    assert!(organization.tags.selected().is_none());
    assert!(organization.collapsed.borrow().is_empty());
    assert!(!organization.root.is_visible());
}
