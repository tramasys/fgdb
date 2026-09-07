//! Shared list-model updates that preserve unchanged GTK object identities.

use gtk::{gio, glib, prelude::*};

pub(in crate::ui) fn replace_boxed_store<T: 'static>(
    store: &gio::ListStore,
    values: impl IntoIterator<Item = T>,
) {
    let values = values
        .into_iter()
        .map(glib::BoxedAnyObject::new)
        .collect::<Vec<_>>();

    store.splice(0, store.n_items(), &values);
}

pub(in crate::ui) fn replace_boxed_store_if_changed<T: PartialEq + 'static>(
    store: &gio::ListStore,
    values: impl IntoIterator<Item = T>,
) -> bool {
    let values = values.into_iter().collect::<Vec<_>>();
    let old_len = usize::try_from(store.n_items()).unwrap_or(usize::MAX);

    if old_len == values.len() {
        let mut changed = false;
        let mut run_start = None;
        let mut replacements = Vec::new();

        for (index, value) in values.into_iter().enumerate() {
            if boxed_store_item_equals(store, index, &value) {
                if let Some(start) = run_start.take() {
                    store.splice(
                        u32::try_from(start).unwrap_or(u32::MAX),
                        u32::try_from(replacements.len()).unwrap_or(u32::MAX),
                        &replacements,
                    );

                    replacements.clear();
                }
            } else {
                changed = true;
                run_start.get_or_insert(index);
                replacements.push(glib::BoxedAnyObject::new(value));
            }
        }

        if let Some(start) = run_start {
            store.splice(
                u32::try_from(start).unwrap_or(u32::MAX),
                u32::try_from(replacements.len()).unwrap_or(u32::MAX),
                &replacements,
            );
        }

        return changed;
    }

    let common_len = old_len.min(values.len());

    let prefix = values
        .iter()
        .take(common_len)
        .enumerate()
        .take_while(|(index, value)| boxed_store_item_equals(store, *index, *value))
        .count();

    let suffix = values
        .iter()
        .enumerate()
        .rev()
        .take(common_len.saturating_sub(prefix))
        .take_while(|(index, value)| {
            let old_index = old_len - (values.len() - *index);

            boxed_store_item_equals(store, old_index, *value)
        })
        .count();

    let new_middle_len = values.len().saturating_sub(prefix + suffix);
    let old_middle_len = old_len.saturating_sub(prefix + suffix);

    let replacements = values
        .into_iter()
        .skip(prefix)
        .take(new_middle_len)
        .map(glib::BoxedAnyObject::new)
        .collect::<Vec<_>>();

    store.splice(
        u32::try_from(prefix).unwrap_or(u32::MAX),
        u32::try_from(old_middle_len).unwrap_or(u32::MAX),
        &replacements,
    );

    true
}

fn boxed_store_item_equals<T: PartialEq + 'static>(
    store: &gio::ListStore,
    index: usize,
    value: &T,
) -> bool {
    store
        .item(u32::try_from(index).unwrap_or(u32::MAX))
        .and_downcast::<glib::BoxedAnyObject>()
        .is_some_and(|item| *item.borrow::<T>() == *value)
}

#[cfg(test)]
mod model_update_tests {
    use super::*;

    fn values(store: &gio::ListStore) -> Vec<u32> {
        (0..store.n_items())
            .map(|index| {
                *store
                    .item(index)
                    .and_downcast::<glib::BoxedAnyObject>()
                    .unwrap()
                    .borrow::<u32>()
            })
            .collect()
    }

    #[test]
    fn changed_store_updates_preserve_equal_objects() {
        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        assert!(replace_boxed_store_if_changed(&store, [1_u32, 2, 3]));
        let first = store.item(0).unwrap();
        let last = store.item(2).unwrap();
        assert!(replace_boxed_store_if_changed(&store, [1_u32, 20, 3]));
        assert_eq!(values(&store), [1, 20, 3]);
        assert_eq!(store.item(0).as_ref(), Some(&first));
        assert_eq!(store.item(2).as_ref(), Some(&last));
        assert!(!replace_boxed_store_if_changed(&store, [1_u32, 20, 3]));
        assert!(replace_boxed_store_if_changed(&store, [1_u32, 3]));
        assert_eq!(values(&store), [1, 3]);
        assert_eq!(store.item(0).as_ref(), Some(&first));
        assert_eq!(store.item(1).as_ref(), Some(&last));
    }
}
