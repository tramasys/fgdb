//! Mutable presentation slots for frequently replaced table snapshots.
//!
//! A slot is not a debugger-object identity. Callers restore selection by their
//! domain key after an update. Payload changes notify bound cells without
//! destroying GTK's row widgets or its measured scroll geometry.
//!
//! These stores feed direct, unsorted tables. A filter or sorter layered over
//! them must explicitly invalidate its results when a payload changes.

use super::*;
use glib::subclass::prelude::*;
use std::{
    any::Any,
    cell::{Ref, RefCell},
    rc::Rc,
    sync::OnceLock,
};

mod imp {
    use super::*;

    pub struct SnapshotRow {
        pub(super) value: RefCell<Rc<dyn Any>>,
    }

    impl Default for SnapshotRow {
        fn default() -> Self {
            Self {
                value: RefCell::new(Rc::new(())),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for SnapshotRow {
        const NAME: &'static str = "FgdbSnapshotRow";
        type Type = super::SnapshotRow;
    }

    impl ObjectImpl for SnapshotRow {
        fn signals() -> &'static [glib::subclass::Signal] {
            static SIGNALS: OnceLock<Vec<glib::subclass::Signal>> = OnceLock::new();
            SIGNALS.get_or_init(|| vec![glib::subclass::Signal::builder("updated").build()])
        }
    }
}

glib::wrapper! {
    pub struct SnapshotRow(ObjectSubclass<imp::SnapshotRow>);
}

impl SnapshotRow {
    pub(in crate::ui) fn new<T: 'static>(value: T) -> Self {
        let row: Self = glib::Object::new();
        row.imp().value.replace(Rc::new(value));
        row
    }

    pub(in crate::ui) fn borrow<T: 'static>(&self) -> Ref<'_, T> {
        Ref::map(self.imp().value.borrow(), |value| {
            value.downcast_ref().expect("snapshot row type")
        })
    }

    /// Retain a presentation for comparison without copying it for every cell.
    pub(in crate::ui) fn snapshot<T: 'static>(&self) -> Rc<T> {
        Rc::clone(&self.imp().value.borrow())
            .downcast()
            .expect("snapshot row type")
    }

    pub(in crate::ui) fn replace<T: PartialEq + 'static>(&self, value: T) -> bool {
        if *self.borrow::<T>() == value {
            return false;
        }

        self.set(value);
        true
    }

    /// Publish an already validated update, including changes in shared child models.
    pub(in crate::ui) fn set<T: 'static>(&self, value: T) {
        let previous = self.imp().value.replace(Rc::new(value));
        drop(previous);
        // Never hold a payload borrow across a callback into the UI.
        self.emit_by_name::<()>("updated", &[]);
    }

    pub(in crate::ui) fn connect_updated(
        &self,
        update: impl Fn(&Self) + 'static,
    ) -> glib::SignalHandlerId {
        self.connect_local("updated", false, move |values| {
            update(&values[0].get::<Self>().expect("snapshot signal instance"));
            None
        })
    }
}

pub(in crate::ui) fn replace_snapshot_store<T: PartialEq + 'static>(
    store: &gio::ListStore,
    values: impl IntoIterator<Item = T>,
) -> bool {
    let old_len = store.n_items();
    let mut length = 0_u32;
    let mut additions = Vec::new();
    let mut changed = false;

    for value in values {
        if length < old_len {
            let row = store
                .item(length)
                .and_downcast::<SnapshotRow>()
                .expect("snapshot store type");
            changed |= row.replace(value);
        } else {
            additions.push(SnapshotRow::new(value));
        }

        length = length.checked_add(1).expect("snapshot fits GListModel");
    }

    if length != old_len {
        store.splice(
            length.min(old_len),
            old_len.saturating_sub(length),
            &additions,
        );
        changed = true;
    }

    changed
}

#[cfg(test)]
mod tests;
