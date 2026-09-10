//! Keep table selection, chain inspection and navigation on the same waiter.

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct SelectionState {
    root: WaitKey,
    active: WaitKey,
}

pub(super) fn column<T: 'static>(
    title: &str,
    width: i32,
    text: impl Fn(&T) -> String + 'static,
) -> gtk::ColumnViewColumn {
    marked_column(title, width, text, |_| false)
}

pub(super) fn marked_column<T: 'static>(
    title: &str,
    width: i32,
    text: impl Fn(&T) -> String + 'static,
    cycle: impl Fn(&T) -> bool + 'static,
) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();

    factory.connect_setup(|_, object| {
        let Some(item) = object.downcast_ref::<gtk::ListItem>() else {
            return;
        };

        // Let GTK own row clicks and keyboard focus. Text remains selectable
        // in the detail pane without competing with table selection.
        let label = gtk::Label::new(None);
        label.add_css_class("debug-table-cell");
        label.set_xalign(0.0);
        label.set_ellipsize(pango::EllipsizeMode::Middle);
        item.set_child(Some(&label));
    });

    factory.connect_bind(move |_, object| {
        let Some(item) = object.downcast_ref::<gtk::ListItem>() else {
            return;
        };

        let (Some(label), Some(data)) = (
            item.child().and_downcast::<gtk::Label>(),
            item.item().and_downcast::<glib::BoxedAnyObject>(),
        ) else {
            return;
        };

        let row = data.borrow::<T>();
        let text = text(&row);
        label.set_text(&text);
        label.set_tooltip_text(Some(&text));

        if cycle(&row) {
            label.add_css_class("status-error");
        } else {
            label.remove_css_class("status-error");
        }
    });

    components::table_column(title, width, factory)
}

impl LocksView {
    pub(super) fn connect_selection(self: &Rc<Self>, dependencies: &gtk::ColumnView) {
        let weak = Rc::downgrade(self);

        self.selection.connect_selected_item_notify(move |_| {
            if let Some(view) = weak.upgrade()
                && !view.updating.get()
            {
                view.history.borrow_mut().clear();
                view.chain_root.set(view.selected_key());
                view.render_chain();
                view.emit(LockAction::Selection);
            }
        });

        let weak = Rc::downgrade(self);

        self.dependency_selection
            .connect_selected_item_notify(move |_| {
                if let Some(view) = weak.upgrade()
                    && !view.updating.get()
                {
                    view.select_dependency();
                }
            });

        for (action, button) in &self.actions {
            let weak = Rc::downgrade(self);
            let action = *action;

            button.connect_clicked(move |_| {
                if let Some(view) = weak.upgrade() {
                    view.emit(action);
                }
            });
        }

        let weak = Rc::downgrade(self);

        self.waiters.connect_activate(move |_, position| {
            if let Some(view) = weak.upgrade()
                && !view.updating.get()
                && let Some(key) = view.wait_key_at(position)
            {
                view.selection.set_selected(position);

                if view.selected_key() == Some(key) {
                    view.emit(LockAction::Waiter);
                }
            }
        });

        let weak = Rc::downgrade(self);

        dependencies.connect_activate(move |_, position| {
            if let Some(view) = weak.upgrade()
                && !view.updating.get()
                && let Some(key) = view.dependency_key_at(position)
            {
                view.dependency_selection.set_selected(position);

                if view.selected_key() == Some(key) {
                    view.emit(LockAction::Follow);
                }
            }
        });
    }

    pub(super) fn selection_state(&self) -> Option<SelectionState> {
        Some(SelectionState {
            root: self.chain_root.get()?,
            active: self.selected_key()?,
        })
    }

    pub(super) fn selected_key(&self) -> Option<WaitKey> {
        self.wait_key_at(self.selection.selected())
    }

    fn wait_key_at(&self, position: u32) -> Option<WaitKey> {
        self.store
            .item(position)
            .and_downcast::<glib::BoxedAnyObject>()
            .map(|row| WaitKey::from(&*row.borrow::<LockWait>()))
    }

    fn dependency_key_at(&self, position: u32) -> Option<WaitKey> {
        self.dependency_store
            .item(position)
            .and_downcast::<glib::BoxedAnyObject>()
            .map(|row| row.borrow::<details::ChainRow>().key())
    }

    fn wait_position(&self, key: WaitKey) -> Option<u32> {
        (0..self.store.n_items()).find(|position| self.wait_key_at(*position) == Some(key))
    }

    pub(super) fn restore_selection(&self, state: Option<SelectionState>) {
        let selection = state.and_then(|state| {
            self.wait_position(state.active)
                .map(|position| (state, position))
        });

        let updating = self.updating.replace(true);

        if let Some((state, position)) = selection {
            let root = if self.wait_position(state.root).is_some() {
                state.root
            } else {
                state.active
            };

            self.chain_root.set(Some(root));
            self.selection.set_selected(position);
        } else {
            self.chain_root.set(None);
            self.selection.set_selected(gtk::INVALID_LIST_POSITION);
        }

        self.render_chain();
        self.updating.set(updating);

        if !updating {
            self.reveal_selection();
            self.emit(LockAction::Selection);
        }
    }

    pub(super) fn select(&self, key: WaitKey) {
        if self.wait_position(key).is_none() {
            return;
        }

        self.restore_selection(Some(SelectionState {
            root: key,
            active: key,
        }));
    }

    pub(super) fn remember_selection(&self) {
        let Some(state) = self.selection_state() else {
            return;
        };

        let mut history = self.history.borrow_mut();

        if history.last() == Some(&state) {
            return;
        }

        if history.len() == 64 {
            history.remove(0);
        }

        history.push(state);
    }

    fn select_dependency(&self) {
        let Some(key) = self.dependency_key_at(self.dependency_selection.selected()) else {
            return;
        };

        let Some(position) = self.wait_position(key) else {
            return;
        };

        if self.selection.selected() == position {
            return;
        }

        self.remember_selection();
        self.updating.set(true);
        self.selection.set_selected(position);
        self.updating.set(false);
        self.render_details();
        self.reveal_selection();
        self.emit(LockAction::Selection);
    }

    fn reveal_selection(&self) {
        let position = self.selection.selected();

        if position != gtk::INVALID_LIST_POSITION && self.waiters.is_mapped() {
            self.waiters
                .scroll_to(position, None, gtk::ListScrollFlags::NONE, None);
        }
    }
}
