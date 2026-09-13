use gtk::{gio, glib, prelude::*};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

/// Observe only the snapshot currently bound to a recycled list item. The
/// subscription disconnects on unbind and destruction, without retaining a
/// row, item, or cell. Hidden tables defer rendering; column comparisons share
/// immutable payloads. An update callback must only change cell presentation.
pub(super) fn connect_snapshot_cell<T: 'static>(
    item: &gtk::ListItem,
    label: &gtk::Label,
    same: impl Fn(&T, &T) -> bool + 'static,
    update: impl Fn(&T) + 'static,
) {
    use super::components::SnapshotRow;

    let subscription = RefCell::new(None::<SignalSubscription>);
    let previous = Rc::new(RefCell::new(None::<Rc<T>>));
    let pending = Rc::new(Cell::new(false));
    let weak_label = label.downgrade();
    let rendered = Rc::clone(&previous);
    let dirty = Rc::clone(&pending);

    let update = Rc::new(move |row: &SnapshotRow| {
        let Some(label) = weak_label.upgrade() else {
            return;
        };

        // Populate new cells for measurement. Existing hidden cells keep only
        // the latest model payload until they map again.
        if !label.is_mapped()
            && rendered.borrow().is_some()
            && label
                .ancestor(gtk::ColumnView::static_type())
                .is_some_and(|view| !view.is_mapped())
        {
            dirty.set(true);
            return;
        }

        dirty.set(false);
        super::views::clear_label_selection(&label);
        let value = row.snapshot::<T>();

        let unchanged = rendered
            .borrow()
            .as_ref()
            .is_some_and(|old| same(old, &value));
        rendered.replace(Some(Rc::clone(&value)));

        if !unchanged {
            update(&value);
        }
    });

    let on_map = Rc::clone(&update);
    let weak_item = item.downgrade();

    label.connect_map(move |_| {
        if pending.replace(false)
            && let Some(row) = weak_item.upgrade().and_then(|item| snapshot_item(&item))
        {
            on_map(&row);
        }
    });

    item.connect_item_notify(move |item| {
        drop(subscription.borrow_mut().take());
        let Some(row) = snapshot_item(item) else {
            previous.borrow_mut().take();
            return;
        };

        update(&row);
        let update = Rc::clone(&update);
        let item = item.downgrade();
        let handler = row.connect_updated(move |row| {
            if item
                .upgrade()
                .is_some_and(|item| snapshot_item(&item).as_ref() == Some(row))
            {
                update(row);
            }
        });

        subscription.replace(Some(SignalSubscription::new(&row, handler)));
    });
}

fn snapshot_item(item: &gtk::ListItem) -> Option<super::components::SnapshotRow> {
    let object = item.item()?;
    let object = if let Some(row) = object.downcast_ref::<gtk::TreeListRow>() {
        row.item()?
    } else {
        object
    };

    object.downcast().ok()
}

/// Disconnect on rebinding or teardown without retaining the observed object.
#[must_use]
pub(super) struct SignalSubscription {
    object: glib::WeakRef<glib::Object>,
    handler: Option<glib::SignalHandlerId>,
}

impl SignalSubscription {
    pub(super) fn new(object: &impl IsA<glib::Object>, handler: glib::SignalHandlerId) -> Self {
        Self {
            object: object.as_ref().downgrade(),
            handler: Some(handler),
        }
    }
}

impl Drop for SignalSubscription {
    fn drop(&mut self) {
        if let (Some(object), Some(handler)) = (self.object.upgrade(), self.handler.take()) {
            object.disconnect(handler);
        }
    }
}

/// A weak identity for a row in a particular presentation model. Position alone
/// is not an identity because filtering, expansion and recycling move rows.
pub(super) struct TreeRowBinding {
    row: glib::WeakRef<gtk::TreeListRow>,
    model: glib::WeakRef<gio::ListModel>,
}

impl TreeRowBinding {
    pub(super) fn is_current(row: &gtk::TreeListRow, model: &impl IsA<gio::ListModel>) -> bool {
        let position = row.position();
        position != gtk::INVALID_LIST_POSITION
            && model.item(position).as_ref() == Some(row.upcast_ref())
    }

    pub(super) fn new(row: &gtk::TreeListRow, model: &impl IsA<gio::ListModel>) -> Option<Self> {
        Self::is_current(row, model).then(|| Self {
            row: row.downgrade(),
            model: model.as_ref().downgrade(),
        })
    }

    fn current(&self) -> Option<gtk::TreeListRow> {
        let row = self.row.upgrade()?;
        let model = self.model.upgrade()?;
        Self::is_current(&row, &model).then_some(row)
    }

    /// Run structural changes after the current GTK binding or event callback.
    /// Removed rows and destroyed models cancel the action without retaining
    /// their children. Callers should read the current payload in the action.
    pub(super) fn defer(self, action: impl FnOnce(&gtk::TreeListRow) + 'static) {
        glib::idle_add_local_once(move || {
            if let Some(row) = self.current() {
                action(&row);
            }
        });
    }

    pub(super) fn defer_for_expander(
        self,
        expander: &gtk::TreeExpander,
        action: impl FnOnce(&gtk::TreeListRow) + 'static,
    ) {
        let expander = expander.downgrade();
        self.defer(move |row| {
            if expander
                .upgrade()
                .is_some_and(|expander| expander.list_row().as_ref() == Some(row))
            {
                action(row);
            }
        });
    }
}

/// A factory installs this once during setup. Unbinding disconnects the old
/// row, and rebinding installs exactly one subscription for the new identity.
pub(super) fn connect_bound_expansion(
    expander: &gtk::TreeExpander,
    model: &impl IsA<gio::ListModel>,
    action: impl Fn(&gtk::TreeListRow) + 'static,
) {
    let subscription = RefCell::new(None::<SignalSubscription>);
    let model = model.as_ref().downgrade();
    let action = Rc::new(action);

    expander.connect_list_row_notify(move |expander| {
        let previous = subscription.borrow_mut().take();
        drop(previous);

        let (Some(row), Some(model)) = (expander.list_row(), model.upgrade()) else {
            return;
        };

        let Some(binding) = TreeRowBinding::new(&row, &model) else {
            return;
        };

        let action = Rc::clone(&action);
        let expander = expander.downgrade();

        let handler = row.connect_expanded_notify(move |row| {
            if binding.current().is_some()
                && expander
                    .upgrade()
                    .is_some_and(|expander| expander.list_row().as_ref() == Some(row))
            {
                action(row);
            }
        });
        subscription.replace(Some(SignalSubscription::new(&row, handler)));
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    #[ignore = "requires a GTK display"]
    fn snapshot_cells_skip_unrelated_changes_and_render_latest_on_map() {
        use super::super::components::SnapshotRow;

        gtk::init().unwrap();
        let store = gio::ListStore::new::<SnapshotRow>();
        let row = SnapshotRow::new((1_u32, 0_u32));
        store.append(&row);
        let updates = Rc::new(RefCell::new(Vec::new()));
        let factory = gtk::SignalListItemFactory::new();
        let observed = Rc::clone(&updates);

        factory.connect_setup(move |_, object| {
            let item = object.downcast_ref::<gtk::ListItem>().unwrap();
            let label = gtk::Label::new(None);
            label.set_selectable(true);
            item.set_child(Some(&label));
            let weak_label = label.downgrade();
            let observed = Rc::clone(&observed);

            connect_snapshot_cell(
                item,
                &label,
                |old: &(u32, u32), new| old.0 == new.0,
                move |value| {
                    observed.borrow_mut().push(value.0);
                    weak_label.upgrade().unwrap().set_text(&value.0.to_string());
                },
            );
        });

        let view = gtk::ColumnView::new(Some(gtk::SingleSelection::new(Some(store.clone()))));
        view.append_column(&gtk::ColumnViewColumn::new(Some("Value"), Some(factory)));
        let window = gtk::Window::builder()
            .child(&view)
            .default_width(300)
            .default_height(200)
            .build();
        window.present();
        let settle = || {
            glib::MainContext::default()
                .block_on(glib::timeout_future(std::time::Duration::from_millis(50)))
        };
        settle();
        assert!(!updates.borrow().is_empty());
        updates.borrow_mut().clear();
        row.replace((1_u32, 1_u32));
        assert!(updates.borrow().is_empty());
        row.replace((2_u32, 1_u32));
        assert_eq!(*updates.borrow(), [2]);
        window.set_visible(false);
        updates.borrow_mut().clear();

        for value in 3..10_u32 {
            row.replace((value, value));
        }

        assert_eq!(*row.borrow::<(u32, u32)>(), (9, 9));
        assert!(updates.borrow().is_empty());
        window.present();
        settle();
        assert_eq!(*updates.borrow(), [9]);
        updates.borrow_mut().clear();
        store.remove_all();
        store.append(&SnapshotRow::new((20_u32, 0_u32)));
        settle();
        assert!(updates.borrow().contains(&20));
        updates.borrow_mut().clear();
        row.replace((30_u32, 0_u32));
        assert!(updates.borrow().is_empty());
        let weak_view = view.downgrade();
        window.set_child(gtk::Widget::NONE);
        window.close();
        drop(view);
        drop(window);
        settle();
        assert!(weak_view.upgrade().is_none());
    }

    #[test]
    #[ignore = "requires a GTK display"]
    fn recycled_rows_cancel_deferred_actions_and_disconnect_subscriptions() {
        gtk::init().unwrap();
        let roots = gio::ListStore::new::<glib::BoxedAnyObject>();
        roots.append(&glib::BoxedAnyObject::new(1_u32));
        roots.append(&glib::BoxedAnyObject::new(2_u32));
        let tree = gtk::TreeListModel::new(roots.clone(), false, false, |_| {
            Some(gio::ListStore::new::<glib::BoxedAnyObject>().upcast())
        });
        let selection = gtk::SingleSelection::new(Some(tree.clone()));
        let first = tree.row(0).unwrap();
        let second = tree.row(1).unwrap();
        let expander = gtk::TreeExpander::new();
        let notifications = Rc::new(Cell::new(0));
        let observed = Rc::clone(&notifications);
        connect_bound_expansion(&expander, &selection, move |_| {
            observed.set(observed.get() + 1);
        });

        for _ in 0..8 {
            expander.set_list_row(Some(&first));
            expander.set_list_row(Some(&second));
        }

        first.set_expanded(true);
        assert_eq!(notifications.get(), 0);
        second.set_expanded(true);
        assert_eq!(notifications.get(), 1);
        expander.set_list_row(None::<&gtk::TreeListRow>);
        second.set_expanded(false);
        assert_eq!(notifications.get(), 1);

        let completed = Rc::new(Cell::new(0));
        let observed = Rc::clone(&completed);
        TreeRowBinding::new(&first, &selection)
            .unwrap()
            .defer(move |_| observed.set(observed.get() + 1));
        roots.remove(0);
        let observed = Rc::clone(&completed);
        TreeRowBinding::new(&second, &selection)
            .unwrap()
            .defer(move |_| observed.set(observed.get() + 1));
        expander.set_list_row(Some(&second));
        TreeRowBinding::new(&second, &selection)
            .unwrap()
            .defer_for_expander(&expander, |_| panic!("recycled binding ran"));
        expander.set_list_row(None::<&gtk::TreeListRow>);

        let context = glib::MainContext::default();
        while context.pending() {
            context.iteration(false);
        }

        assert_eq!(completed.get(), 1);

        expander.set_list_row(Some(&second));
        drop(expander);
        second.set_expanded(true);
        assert_eq!(notifications.get(), 1);
    }
}
