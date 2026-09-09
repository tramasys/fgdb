use super::*;

fn snapshot(store: &gio::ListStore) -> Vec<(u32, u32)> {
    (0..store.n_items())
        .map(|index| {
            *store
                .item(index)
                .and_downcast::<glib::BoxedAnyObject>()
                .unwrap()
                .borrow::<(u32, u32)>()
        })
        .collect()
}

fn replace(store: &gio::ListStore, rows: &[(u32, u32)]) -> bool {
    replace_sorted_boxed_store_if_changed(store, rows.iter().copied(), |row| row.0)
}

#[test]
fn moving_windows_preserve_overlap_but_refresh_changed_values() {
    let store = gio::ListStore::new::<glib::BoxedAnyObject>();
    assert!(replace(&store, &[(1, 10), (2, 20), (3, 30), (4, 40)]));
    let retained = store.item(2).unwrap();
    let updated = store.item(3).unwrap();

    assert!(replace(&store, &[(3, 30), (4, 41), (5, 50)]));
    assert_eq!(snapshot(&store), [(3, 30), (4, 41), (5, 50)]);
    assert_eq!(store.item(0).unwrap(), retained);
    assert_ne!(store.item(1).unwrap(), updated);
    assert!(!replace(&store, &[(3, 30), (4, 41), (5, 50)]));

    assert!(replace(&store, &[(0, 0), (1, 10), (3, 30)]));
    assert_eq!(snapshot(&store), [(0, 0), (1, 10), (3, 30)]);
    assert_eq!(store.item(2).unwrap(), retained);
    assert!(replace(&store, &[]));
    assert!(!replace(&store, &[]));
}

#[test]
fn unsorted_or_duplicate_keys_are_preserved_without_matching_by_key() {
    let store = gio::ListStore::new::<glib::BoxedAnyObject>();

    for rows in [
        vec![(3, 30), (1, 10), (2, 20)],
        vec![(1, 10), (1, 11), (2, 20)],
        vec![(0, 0), (2, 20), (4, 40)],
        vec![(5, 50), (1, 10)],
    ] {
        assert!(replace(&store, &rows));
        assert_eq!(snapshot(&store), rows);
        assert!(!replace(&store, &rows));
    }
}

#[test]
fn all_small_sorted_edits_match_replacement_and_retain_equal_rows() {
    let store = gio::ListStore::new::<glib::BoxedAnyObject>();

    for old_mask in 0..64_u32 {
        let old = (0..6)
            .filter(|key| old_mask & (1 << key) != 0)
            .map(|key| (key, key * 10))
            .collect::<Vec<_>>();

        for new_mask in 0..64_u32 {
            replace_boxed_store(&store, old.iter().copied());
            let identities = (0..store.n_items())
                .map(|index| (old[index as usize], store.item(index).unwrap()))
                .collect::<Vec<_>>();

            let new = (0..6)
                .filter(|key| new_mask & (1 << key) != 0)
                .map(|key| (key, key * 10 + u32::from(key == 3)))
                .collect::<Vec<_>>();

            assert_eq!(replace(&store, &new), old != new);
            assert_eq!(snapshot(&store), new);

            for (index, row) in new.iter().enumerate() {
                if let Some((_, object)) = identities.iter().find(|(old, _)| old == row) {
                    assert_eq!(store.item(index as u32).as_ref(), Some(object));
                }
            }
        }
    }
}
