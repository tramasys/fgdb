//! Shared table centering and column sizing with draggable header dividers.

use gtk::{glib, pango, prelude::*};
use std::{cell::Cell, rc::Rc};

#[cfg(test)]
mod benchmarks;
#[cfg(test)]
mod tests;

#[derive(Default)]
pub(in crate::ui) struct CenteredScroll {
    pending: Cell<Option<(u32, [f64; 3])>>,
    scheduled: Cell<bool>,
    allocated: Cell<bool>,
}

impl CenteredScroll {
    pub(in crate::ui) fn request(
        self: &Rc<Self>,
        view: &gtk::ColumnView,
        scrolled: &gtk::ScrolledWindow,
        position: u32,
    ) {
        let Some(selection) = view.model().and_downcast::<gtk::SingleSelection>() else {
            return;
        };

        if position >= selection.n_items() || selection.selected() != position {
            return;
        }

        let adjustment = scrolled.vadjustment();
        let row_center =
            adjustment.upper() * (f64::from(position) + 0.5) / f64::from(selection.n_items());
        let nearby = row_center >= adjustment.value()
            && row_center <= adjustment.value() + adjustment.page_size();

        // For nearby rows, center before moving the focus tracker. For a long
        // jump, retire the distant focus first so GTK need not bind both ranges.
        if nearby {
            center_scroll_adjustment(scrolled, position, selection.n_items());
        }

        // Keep keyboard activation on the requested row immediately. Delaying
        // the native focus tracker until a frame can activate the previous row.
        view.scroll_to(position, None, gtk::ListScrollFlags::FOCUS, None);

        if !nearby {
            center_scroll_adjustment(scrolled, position, selection.n_items());
        }

        self.pending.set(Some((
            position,
            [
                adjustment.upper(),
                adjustment.page_size(),
                adjustment.value(),
            ],
        )));
        self.allocated.set(false);

        if self.scheduled.replace(true) {
            return;
        }

        let state = Rc::clone(self);
        let scrolled = scrolled.downgrade();

        view.add_tick_callback(move |view, _| {
            let Some(scrolled) = scrolled.upgrade() else {
                state.scheduled.set(false);
                return gtk::glib::ControlFlow::Break;
            };

            if !state.allocated.replace(true) {
                return gtk::glib::ControlFlow::Continue;
            }

            let adjustment = scrolled.vadjustment();

            if let Some((position, before)) = state.pending.take()
                && [
                    adjustment.upper(),
                    adjustment.page_size(),
                    adjustment.value(),
                ] != before
                && let Some(selection) = view.model().and_downcast::<gtk::SingleSelection>()
                && selection.selected() == position
                && position < selection.n_items()
            {
                // Layout can change the extent or move GTK's scroll anchor.
                // Correct only an actual change, using the latest request.
                center_scroll_adjustment(&scrolled, position, selection.n_items());
            }

            state.scheduled.set(false);
            gtk::glib::ControlFlow::Break
        });
    }
}

fn center_scroll_adjustment(scrolled: &gtk::ScrolledWindow, position: u32, item_count: u32) {
    if item_count == 0 {
        return;
    }

    let adjustment = scrolled.vadjustment();
    let lower = adjustment.lower();
    let upper = adjustment.upper();
    let page_size = adjustment.page_size();

    if !lower.is_finite()
        || !upper.is_finite()
        || !page_size.is_finite()
        || upper <= lower + page_size
    {
        return;
    }

    let row_fraction = (f64::from(position) + 0.5) / f64::from(item_count);
    let row_center = lower + (upper - lower) * row_fraction;
    let maximum = (upper - page_size).max(lower);
    adjustment.set_value((row_center - page_size / 2.0).clamp(lower, maximum));
}

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

pub(in crate::ui) fn label_column(
    title: &str,
    width: i32,
    bind: impl Fn(&glib::BoxedAnyObject, &gtk::Label) + Copy + 'static,
) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();

    factory.connect_setup(|_, object| {
        let Some(item) = object.downcast_ref::<gtk::ListItem>() else {
            return;
        };

        let label = gtk::Label::new(None);
        label.add_css_class("debug-table-cell");
        label.set_halign(gtk::Align::Start);
        label.set_ellipsize(pango::EllipsizeMode::Middle);
        super::super::views::enable_stable_text_selection(&label);
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

        bind(&data, &label);
    });

    table_column(title, width, factory)
}
