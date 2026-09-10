use super::*;
use std::{cell::Cell, rc::Rc};

fn values(store: &gio::ListStore) -> Vec<u32> {
    store
        .iter::<SnapshotRow>()
        .map(|row| *row.unwrap().borrow::<u32>())
        .collect()
}

#[test]
fn changed_payloads_preserve_slots_and_notify_without_a_live_borrow() {
    let store = gio::ListStore::new::<SnapshotRow>();
    assert!(replace_snapshot_store(&store, [1_u32, 2, 3]));
    let row = store.item(1).and_downcast::<SnapshotRow>().unwrap();
    let updates = Rc::new(Cell::new(0));
    let observed = Rc::clone(&updates);
    row.connect_updated(move |row| {
        assert_eq!(*row.borrow::<u32>(), 20);
        assert!(!row.replace(20_u32));
        observed.set(observed.get() + 1);
    });
    let structure = Rc::new(Cell::new(0));
    let changed = Rc::clone(&structure);
    store.connect_items_changed(move |_, _, _, _| changed.set(changed.get() + 1));
    assert!(replace_snapshot_store(&store, [1_u32, 20, 3]));
    assert_eq!(values(&store), [1, 20, 3]);
    assert_eq!(store.item(1).as_ref(), Some(row.upcast_ref()));
    assert_eq!(updates.get(), 1);
    assert_eq!(structure.get(), 0);
    assert!(!replace_snapshot_store(&store, [1_u32, 20, 3]));
    assert_eq!(updates.get(), 1);
    assert!(replace_snapshot_store(&store, [1_u32, 20, 3, 4]));
    assert_eq!(structure.get(), 1);
    assert!(replace_snapshot_store(&store, [1_u32, 20]));
    assert_eq!(structure.get(), 2);
    assert_eq!(values(&store), [1, 20]);
}

#[test]
fn snapshots_keep_every_value_and_release_removed_slots() {
    let store = gio::ListStore::new::<SnapshotRow>();

    for before in 0..10_u32 {
        for after in 0..10_u32 {
            replace_snapshot_store(&store, 0..before);
            let slots = store
                .iter::<SnapshotRow>()
                .map(|row| row.unwrap().downgrade())
                .collect::<Vec<_>>();
            let expected = (0..after).rev().map(|value| value % 3).collect::<Vec<_>>();
            let previous = values(&store);
            assert_eq!(
                replace_snapshot_store(&store, expected.iter().copied()),
                previous != expected
            );
            assert_eq!(values(&store), expected);
            assert!(!replace_snapshot_store(&store, expected));

            for (index, slot) in slots.into_iter().enumerate() {
                if index < after as usize {
                    assert_eq!(
                        store.item(index as u32),
                        slot.upgrade().map(|row| row.upcast())
                    );
                } else {
                    assert!(slot.upgrade().is_none());
                }
            }
        }
    }
}
