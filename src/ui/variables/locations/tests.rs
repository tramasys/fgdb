use super::*;
use crate::{
    config::settings::IntegerDisplay,
    ui::{
        variable_presentation::VariablePresentation, variable_viewers::VariableViewerRegistry,
        views,
    },
};
use std::{collections::VecDeque, time::Duration};

fn variable(index: usize) -> Variable {
    Variable {
        local_index: Some(index),
        name: format!("value_{index}"),
        value: index.to_string(),
        type_name: Some("int".into()),
        argument: false,
        varobj: None,
        num_children: 0,
        has_more: false,
        dynamic: false,
        display_hint: None,
    }
}

#[test]
fn presentation_does_not_confuse_absent_addresses_with_registers_or_pointer_targets() {
    for location in [
        ValueLocation::NonAddressable,
        ValueLocation::OptimizedOut,
        ValueLocation::Unavailable,
        ValueLocation::Unknown("synthetic child".into()),
    ] {
        assert_eq!(location.address(), None);
        let (text, detail) = presentation(Some(&location), true, 64);
        assert!(!text.is_empty());
        assert!(!detail.is_empty());
        assert!(!text.starts_with("0x"));
    }

    let (text, detail) = presentation(
        Some(&ValueLocation::Memory {
            address: 16,
            referenced: false,
        }),
        true,
        32,
    );

    assert_eq!(text, "0x00000010");
    assert!(detail.contains("pointer's own storage"));
    let (text, detail) = presentation(
        Some(&ValueLocation::Memory {
            address: 16,
            referenced: true,
        }),
        true,
        64,
    );

    assert_eq!(text, "0x0000000000000010 (referent)");
    assert!(detail.contains("referenced object"));
    assert_eq!(presentation(None, false, 64).0, "—");
}

fn settle() {
    glib::MainContext::default().block_on(glib::timeout_future(Duration::from_millis(100)));
}

struct Pending {
    variables: Vec<Variable>,
    current: Rc<dyn Fn() -> bool>,
    reply: LocationReply,
}

fn complete(queue: &RefCell<VecDeque<Pending>>) {
    for _ in 0..32 {
        let pending = queue.borrow_mut().pop_front();
        let Some(pending) = pending else {
            return;
        };

        let current = (pending.current)();
        (pending.reply)(current.then(|| {
            pending
                .variables
                .iter()
                .map(|variable| ValueLocation::Memory {
                    address: 0x1000 + variable.local_index.unwrap_or(0) as u64,
                    referenced: false,
                })
                .collect()
        }));

        settle();
    }

    panic!("Unbounded location request loop");
}

fn assert_menu_alignment(menu: &gtk::Box, location: &LocationMenu) {
    let label = location.label.upgrade().unwrap();
    let summary = label.parent().unwrap();
    let caption = summary.first_child().unwrap();
    let address_bounds = label.compute_bounds(menu).unwrap();
    let caption_bounds = caption.compute_bounds(menu).unwrap();
    let summary_bounds = summary.compute_bounds(menu).unwrap();
    let variable_caption = menu.first_child().unwrap().first_child().unwrap();
    let variable_bounds = variable_caption.compute_bounds(menu).unwrap();
    let close = |left: f32, right: f32| assert!((left - right).abs() <= 1.0, "{left} != {right}");
    close(address_bounds.x(), variable_bounds.x());
    close(address_bounds.x(), caption_bounds.x());
    close(address_bounds.x(), 8.0);
    close(
        menu.width() as f32 - address_bounds.x() - address_bounds.width(),
        8.0,
    );

    close(caption_bounds.y() - summary_bounds.y(), 5.0);
    close(
        summary_bounds.y() + summary_bounds.height() - address_bounds.y() - address_bounds.height(),
        5.0,
    );

    close(
        address_bounds.y() - caption_bounds.y() - caption_bounds.height(),
        2.0,
    );

    for button in [&location.copy, &location.inspect, &location.retry] {
        let button = button.upgrade().unwrap();
        let button_bounds = button.compute_bounds(menu).unwrap();
        let text_bounds = button.child().unwrap().compute_bounds(menu).unwrap();
        close(address_bounds.x(), text_bounds.x());
        close(
            text_bounds.y() - button_bounds.y(),
            button_bounds.y() + button_bounds.height() - text_bounds.y() - text_bounds.height(),
        );
    }
}

#[test]
#[ignore = "requires a GTK display"]
fn locations_are_lazy_visible_bounded_and_invalidated_with_their_stop() {
    gtk::init().unwrap();
    crate::theme::Theme::graphite().install();
    let pointer_bits = Rc::new(Cell::new(64));
    let locations = Locations::new(false, Rc::clone(&pointer_bits));
    let pending = Rc::new(RefCell::new(VecDeque::new()));
    let queue = Rc::clone(&pending);
    let calls = Rc::new(Cell::new(0));
    let called = Rc::clone(&calls);
    let opened = Rc::new(Cell::new(None));
    let open = Rc::clone(&opened);

    locations.connect(
        Rc::new(move |variables, current, reply| {
            assert!(variables.len() <= BATCH_LIMIT);
            called.set(called.get() + 1);
            queue.borrow_mut().push_back(Pending {
                variables,
                current,
                reply,
            });
        }),
        Rc::new(|_| true),
        Rc::new(move |address| open.set(Some(address))),
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

    store.extend_from_slice(
        &(0..2000)
            .map(|index| glib::BoxedAnyObject::new(VariableNode::new(variable(index))))
            .collect::<Vec<_>>(),
    );

    let scrolled = gtk::ScrolledWindow::builder()
        .child(&view)
        .vexpand(true)
        .build();

    let window = gtk::Window::builder()
        .default_width(1200)
        .default_height(300)
        .child(&scrolled)
        .build();

    window.present();
    settle();
    assert_eq!(calls.get(), 0, "hidden column performed inspection");

    locations.set_enabled(true);
    settle();
    assert!(!pending.borrow().is_empty());
    complete(&pending);
    let count = locations.cache.borrow().len();
    assert!(
        count > 0 && count < 32,
        "queried {count} of 2000 rows instead of just visible rows"
    );

    let previous_calls = calls.get();
    locations.schedule();
    settle();
    assert_eq!(calls.get(), previous_calls, "cache hit re-queried GDB");

    window.set_default_size(1200, 500);
    settle();
    complete(&pending);
    assert!(
        locations.cache.borrow().len() > count,
        "resized viewport did not resolve newly visible rows"
    );

    let adjustment = scrolled.vadjustment();
    adjustment.set_value(adjustment.upper() - adjustment.page_size());
    settle();
    assert!(pending.borrow().front().is_some_and(|pending| {
        pending
            .variables
            .iter()
            .any(|variable| variable.local_index.unwrap() > 1900)
    }));

    let stale = pending.borrow_mut().pop_front().unwrap();
    locations.set_context(Some((2, 0)));
    assert!(!(stale.current)());
    (stale.reply)(Some(vec![
        ValueLocation::Memory {
            address: 0xdead,
            referenced: false
        };
        stale.variables.len()
    ]));

    settle();
    complete(&pending);
    assert!(
        !locations
            .cache
            .borrow()
            .values()
            .any(|location| location.address() == Some(0xdead))
    );

    locations.set_context(Some((3, 0)));
    settle();
    locations.set_enabled(false);
    complete(&pending);
    assert!(locations.cache.borrow().is_empty());

    let (popover, menu) = crate::ui::build::build_context_menu();
    popover.add_css_class("local-variable-menu");
    let summary = views::variable_menu_summary("VARIABLE");
    let name = gtk::Label::new(Some("record"));
    name.add_css_class("local-variable-menu-name");
    name.set_halign(gtk::Align::Start);
    summary.append(&name);
    menu.append(&summary);
    popover.set_parent(&scrolled);
    locations.append_menu(&menu, &popover, &variable(123));
    popover.popup();
    settle();
    complete(&pending);
    assert_eq!(
        locations.cache.borrow().len(),
        1,
        "menu queried hidden rows"
    );

    let lease = locations.menus.borrow()[0].upgrade().unwrap();
    assert_menu_alignment(&menu, &lease);
    assert_eq!(lease.label.upgrade().unwrap().text(), "0x000000000000107b");
    assert!(lease.copy.upgrade().unwrap().is_sensitive());
    lease.inspect.upgrade().unwrap().emit_clicked();
    assert_eq!(opened.get(), Some(0x107b));
    settle();
    popover.unparent();

    let (copy_popover, copy_menu) = crate::ui::build::build_context_menu();
    copy_popover.set_parent(&scrolled);
    locations.append_menu(&copy_menu, &copy_popover, &variable(123));
    let copy_lease = locations.menus.borrow().last().unwrap().upgrade().unwrap();
    copy_popover.popup();
    settle();
    assert!(copy_popover.is_mapped(), "copy menu unexpectedly closed");
    copy_lease.copy.upgrade().unwrap().emit_clicked();
    let clipboard = glib::MainContext::default()
        .block_on(view.display().clipboard().read_text_future())
        .unwrap();

    assert_eq!(clipboard.as_deref(), Some("0x107b"));
    settle();
    copy_popover.unparent();

    // A failed lookup is cached until an explicit retry; retrying a menu must
    // not start resolving the hidden column.
    locations.cache.borrow_mut().insert(
        variable(123),
        ValueLocation::Unknown("temporary failure".into()),
    );

    let (retry_popover, retry_menu) = crate::ui::build::build_context_menu();
    retry_popover.set_parent(&scrolled);
    locations.append_menu(&retry_menu, &retry_popover, &variable(123));
    let retry_lease = locations.menus.borrow().last().unwrap().upgrade().unwrap();
    retry_popover.popup();
    settle();
    assert!(retry_popover.is_mapped(), "retry menu unexpectedly closed");
    assert_eq!(retry_lease.label.upgrade().unwrap().text(), "Not resolved");
    assert!(retry_lease.retry.upgrade().unwrap().is_sensitive());
    assert!(!retry_lease.copy.upgrade().unwrap().is_sensitive());
    assert!(pending.borrow().is_empty());
    retry_lease.retry.upgrade().unwrap().emit_clicked();
    settle();
    complete(&pending);
    assert_eq!(locations.cache.borrow().len(), 1);
    assert!(!retry_lease.retry.upgrade().unwrap().is_sensitive());
    assert!(retry_lease.copy.upgrade().unwrap().is_sensitive());

    locations.set_context(None);
    assert!(locations.cache.borrow().is_empty());
    assert_eq!(locations.cached(&variable(123)), None);
    opened.set(None);
    retry_lease.inspect.upgrade().unwrap().emit_clicked();
    assert_eq!(
        opened.get(),
        None,
        "menu used a stale address before repaint"
    );

    settle();
    assert!(!retry_lease.inspect.upgrade().unwrap().is_sensitive());
    retry_popover.popdown();
    settle();
    retry_popover.unparent();
    window.close();
    drop((
        lease,
        popover,
        menu,
        copy_lease,
        copy_popover,
        copy_menu,
        retry_lease,
        retry_popover,
        retry_menu,
        window,
        scrolled,
        view,
        store,
    ));

    settle();
    let weak = Rc::downgrade(&locations);
    drop(locations);
    assert!(
        weak.upgrade().is_none(),
        "location presenter retained itself"
    );
}
