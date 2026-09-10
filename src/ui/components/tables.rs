//! Shared column sizing for tables with draggable header dividers.

use gtk::prelude::*;

#[cfg(test)]
mod benchmarks;
#[cfg(test)]
mod tests;

/// Only the last visible column absorbs spare width. Expanding a middle column
/// makes GTK reclaim the space released when its right divider is dragged left.
pub(in crate::ui) fn column_view(model: impl IsA<gtk::SelectionModel>) -> gtk::ColumnView {
    let view = gtk::ColumnView::new(Some(model));
    let weak = view.downgrade();

    // Observe the column model, not the row model. Data refreshes and pointer
    // motion do not need callbacks or allocations for this layout policy.
    view.columns().connect_items_changed(move |_, _, _, _| {
        if let Some(view) = weak.upgrade() {
            update_expanding_column(&view);
        }
    });

    view
}

/// Columns used with `column_view` keep their requested widths when reordered
/// or hidden. Visibility changes transfer spare space to the new trailing one.
pub(in crate::ui) fn table_column(
    title: &str,
    width: i32,
    factory: impl IsA<gtk::ListItemFactory>,
) -> gtk::ColumnViewColumn {
    let column = gtk::ColumnViewColumn::new(Some(title), Some(factory));
    column.set_fixed_width(width);
    column.set_resizable(true);

    column.connect_visible_notify(|column| {
        if let Some(view) = column.column_view() {
            update_expanding_column(&view);
        }
    });

    column
}

fn update_expanding_column(view: &gtk::ColumnView) {
    let columns = view.columns();
    let mut trailing = true;

    for position in (0..columns.n_items()).rev() {
        let Some(column) = columns
            .item(position)
            .and_downcast::<gtk::ColumnViewColumn>()
        else {
            continue;
        };

        let expand = trailing && column.is_visible();
        trailing &= !column.is_visible();

        if column.expands() != expand {
            column.set_expand(expand);
        }
    }
}
