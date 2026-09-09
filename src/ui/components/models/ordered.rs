//! Linear reconciliation for sorted tables whose address window can move.

use super::*;

/// Preserve equal rows even when insertions or removals shift their positions.
/// Keys must be strictly increasing. Unexpected input falls back to positional
/// replacement rather than dropping, reordering, or reusing a mismatched row.
pub(in crate::ui) fn replace_sorted_boxed_store_if_changed<T: PartialEq + 'static, K: Ord>(
    store: &gio::ListStore,
    values: impl IntoIterator<Item = T>,
    key: impl Fn(&T) -> K,
) -> bool {
    let values = values
        .into_iter()
        .map(|value| (key(&value), value))
        .collect::<Vec<_>>();

    let old = (0..store.n_items())
        .map(|index| {
            let object = store.item(index)?.downcast::<glib::BoxedAnyObject>().ok()?;
            let key = key(&*object.try_borrow::<T>().ok()?);
            Some((key, object))
        })
        .collect::<Option<Vec<_>>>();

    let Some(old) = old.filter(|old| strictly_sorted(old) && strictly_sorted(&values)) else {
        return replace_boxed_store_if_changed(store, values.into_iter().map(|(_, value)| value));
    };

    let mut old_index = 0;
    let mut removed_from = 0;
    let mut position = 0;
    let mut additions = Vec::new();
    let mut changed = false;

    for (key, value) in values {
        while old
            .get(old_index)
            .is_some_and(|(old_key, _)| *old_key < key)
        {
            old_index += 1;
        }

        let equal = old
            .get(old_index)
            .is_some_and(|(old_key, object)| *old_key == key && *object.borrow::<T>() == value);

        if equal {
            changed |= splice_gap(store, position, old_index - removed_from, &additions);
            position += additions.len() + 1;
            additions.clear();
            old_index += 1;
            removed_from = old_index;
        } else {
            additions.push(glib::BoxedAnyObject::new(value));
        }
    }

    changed | splice_gap(store, position, old.len() - removed_from, &additions)
}

fn strictly_sorted<K: Ord, V>(values: &[(K, V)]) -> bool {
    values.windows(2).all(|pair| pair[0].0 < pair[1].0)
}

fn splice_gap(
    store: &gio::ListStore,
    position: usize,
    removed: usize,
    additions: &[glib::BoxedAnyObject],
) -> bool {
    if removed == 0 && additions.is_empty() {
        return false;
    }

    store.splice(
        u32::try_from(position).expect("list position fits GListModel"),
        u32::try_from(removed).expect("list length fits GListModel"),
        additions,
    );

    true
}

#[cfg(test)]
mod tests;
