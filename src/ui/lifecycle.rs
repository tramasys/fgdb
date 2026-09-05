use gtk::{gio, glib, prelude::*};
use std::{cell::RefCell, rc::Rc};

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
