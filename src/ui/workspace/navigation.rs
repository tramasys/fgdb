use super::*;

pub(in crate::ui) fn build_compact_navigation(notebook: &gtk::Notebook) -> gtk::Box {
    let previous = gtk::Button::with_label("‹");
    previous.add_css_class("kernel-tab-nav-button");
    previous.set_tooltip_text(Some("Open the previous inspector"));
    let names = gtk::StringList::new(&[]);
    let selector = gtk::DropDown::new(Some(names.clone()), None::<gtk::Expression>);
    selector.add_css_class("kernel-compact-tab-selector");
    selector.set_hexpand(true);
    selector.set_tooltip_text(Some("Select an inspector"));
    let next = gtk::Button::with_label("›");
    next.add_css_class("kernel-tab-nav-button");
    next.set_tooltip_text(Some("Open the next inspector"));
    let root = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    root.add_css_class("kernel-tab-navigation");
    root.add_css_class("kernel-compact-tab-navigation");
    root.set_hexpand(true);
    root.append(&previous);
    root.append(&selector);
    root.append(&next);
    root.set_visible(false);
    let entries = Rc::new(RefCell::new(Vec::<glib::WeakRef<gtk::Widget>>::new()));
    let updating = Rc::new(Cell::new(false));
    let book = notebook.downgrade();
    let entries_for_selection = Rc::clone(&entries);
    let updating_for_selection = Rc::clone(&updating);

    selector.connect_selected_notify(move |selector| {
        if updating_for_selection.get() {
            return;
        }

        let selected = entries_for_selection
            .borrow()
            .get(selector.selected() as usize)
            .and_then(glib::WeakRef::upgrade);

        if let (Some(book), Some(selected)) = (book.upgrade(), selected)
            && let Some(position) = book.page_num(&selected)
            && book.current_page() != Some(position)
        {
            book.set_current_page(Some(position));
        }
    });

    let book = notebook.downgrade();

    previous.connect_clicked(move |_| {
        if let Some(book) = book.upgrade() {
            book.prev_page();
        }
    });

    let book = notebook.downgrade();

    next.connect_clicked(move |_| {
        if let Some(book) = book.upgrade() {
            book.next_page();
        }
    });

    let book = notebook.downgrade();
    let selector = selector.downgrade();
    let previous = previous.downgrade();
    let next = next.downgrade();

    let update = Rc::new(move || {
        let (Some(book), Some(selector), Some(previous), Some(next)) = (
            book.upgrade(),
            selector.upgrade(),
            previous.upgrade(),
            next.upgrade(),
        ) else {
            return;
        };

        let count = book.n_pages();
        let unchanged = entries.borrow().len() == count as usize
            && entries
                .borrow()
                .iter()
                .enumerate()
                .all(|(index, root)| root.upgrade() == book.nth_page(Some(index as u32)));

        updating.set(true);

        if !unchanged {
            let roots = (0..count)
                .filter_map(|index| book.nth_page(Some(index)))
                .collect::<Vec<_>>();

            let labels = roots
                .iter()
                .map(|root| book.tab_label_text(root).unwrap_or_default())
                .collect::<Vec<_>>();

            let labels = labels
                .iter()
                .map(|label| label.as_str())
                .collect::<Vec<_>>();

            entries.replace(roots.iter().map(gtk::Widget::downgrade).collect());
            names.splice(0, names.n_items(), &labels);
        }

        let position = book.current_page();
        selector.set_selected(position.unwrap_or(gtk::INVALID_LIST_POSITION));
        previous.set_sensitive(position.is_some_and(|page| page > 0));
        next.set_sensitive(position.is_some_and(|page| page + 1 < count));
        updating.set(false);
    });

    update();
    let pending = Rc::new(Cell::new(false));

    // Notebook signals can precede the final page/selection state. Keep the
    // dropdown's small presentation model independent and synchronize once
    // after structural changes, before the next frame is drawn.
    let schedule = move || {
        if pending.replace(true) {
            return;
        }

        let pending = Rc::clone(&pending);
        let update = Rc::clone(&update);

        glib::idle_add_local_full(glib::Priority::HIGH_IDLE, move || {
            pending.set(false);
            update();
            glib::ControlFlow::Break
        });
    };

    let on_switch = schedule.clone();
    notebook.connect_switch_page(move |_, _, _| on_switch());
    let on_added = schedule.clone();
    notebook.connect_page_added(move |_, _, _| on_added());
    let on_removed = schedule.clone();
    notebook.connect_page_removed(move |_, _, _| on_removed());
    notebook.connect_page_reordered(move |_, _, _| schedule());

    root
}
