//! Stack-memory paging controls and incremental presentation updates.

use super::*;
use crate::model::stack::{StackPage, StackPageStatus, StackStop};

#[cfg(test)]
mod tests;

#[derive(Clone)]
pub(super) struct Paging {
    pub root: gtk::Box,
    scrolled: gtk::ScrolledWindow,
    status: gtk::Label,
    more: gtk::Button,
    can_load: Rc<Cell<bool>>,
    was_available: Rc<Cell<bool>>,
}

impl Paging {
    pub(super) fn new(scrolled: &gtk::ScrolledWindow) -> Self {
        let root = components::control_row();
        components::inset(&root, components::CONTENT_INSET);
        let status = gtk::Label::builder()
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(pango::EllipsizeMode::End)
            .css_classes(["muted"])
            .build();
        let more = gtk::Button::with_label("Load more");
        more.add_css_class("inline-action");
        more.set_sensitive(false);
        more.set_tooltip_text(Some(
            "Read the next stack words. Scrolling to the bottom also loads more",
        ));

        root.append(&status);
        root.append(&more);
        Self {
            root,
            scrolled: scrolled.clone(),
            status,
            more,
            can_load: Rc::new(Cell::new(false)),
            was_available: Rc::new(Cell::new(false)),
        }
    }

    fn connect(&self, handler: impl Fn(bool) + 'static) {
        let handler = Rc::new(handler);
        let clicked = Rc::clone(&handler);
        let can_load = Rc::clone(&self.can_load);
        self.more.connect_clicked(move |_| {
            if can_load.get() {
                clicked(false);
            }
        });
        let can_load = Rc::clone(&self.can_load);
        self.scrolled.connect_edge_reached(move |scrolled, edge| {
            if can_load.get() && edge == gtk::PositionType::Bottom && scrolled.is_mapped() {
                handler(true);
            }
        });
    }

    fn update(&self, status: &StackPageStatus) {
        self.can_load.set(status.can_load);
        let refreshing = status.refreshing || status.loading;
        if !refreshing {
            self.was_available.set(status.can_load);
        }

        // Match the existing search/execution controls: preserve hover and
        // focus during a transient refresh, while rejecting every activation.
        set_transient_execution_sensitive(
            &self.more,
            status.can_load,
            refreshing && self.was_available.get(),
        );
        self.more.update_state(&[
            gtk::accessible::State::Disabled(!status.can_load),
            gtk::accessible::State::Busy(refreshing),
        ]);

        if refreshing && status.loaded == 0 && self.was_available.get() {
            return;
        }

        let loaded = status.total.map_or_else(
            || format!("{} words loaded", status.loaded),
            |total| format!("{} / {total} words loaded", status.loaded),
        );

        let text = if status.loading {
            format!("{loaded} · Loading…")
        } else if let Some(stop) = status.stop {
            let reason = match stop {
                StackStop::MappingEnd => "End of stack mapping",
                StackStop::UnknownBoundary => "Stack mapping boundary unavailable",
                StackStop::TableCapacity => "Table capacity reached",
            };

            format!("{loaded} · {reason}")
        } else if let Some(reason) = &status.error {
            format!("{loaded} · {reason}")
        } else if status.loaded > 0 {
            loaded
        } else {
            String::from("Stack memory is loaded on demand")
        };
        if self.status.text() != text {
            self.status.set_text(&text);
        }

        let detail = if let Some(range) = &status.range {
            format!(
                "{text}\nStack storage from SP 0x{:x} to mapping end 0x{:x} (exclusive)",
                range.start, range.end
            )
        } else if status.stop == Some(StackStop::UnknownBoundary) {
            format!(
                "{text}\nOnly the initial preview is available without a verified stack mapping"
            )
        } else {
            text
        };

        if self.status.tooltip_text().as_deref() != Some(&detail) {
            self.status.set_tooltip_text(Some(&detail));
        }

        let label = if status.error.is_some() {
            "Retry"
        } else {
            "Load more"
        };
        if self.more.label().as_deref() != Some(label) {
            self.more.set_label(label);
        }
    }
}

impl Ui {
    pub(crate) fn connect_stack_paging(&self, handler: impl Fn(bool) + 'static) {
        self.stack_paging.connect(handler);
    }

    pub(crate) fn update_stack_paging(&self) {
        self.stack_paging.update(&self.model.stack_page_status());
    }

    pub(crate) fn show_stack_page_start(&self) {
        if self.model.stack_page_status().stop.is_some() {
            self.stack_store.remove_all();
            self.displayed_stack.borrow_mut().clear();
            self.stack_empty
                .set_text("No complete stack words in the readable range");

            self.stack_empty.set_visible(true);
            self.update_stack_paging();
            return;
        }

        self.stack_empty.set_text("Loading stack memory…");
        self.stack_empty
            .set_visible(self.stack_store.n_items() == 0);

        self.update_stack_paging();
    }

    pub(crate) fn show_stack_page(&self, page: StackPage, entries: &[StackEntry]) -> bool {
        if !self.model.append_stack_page(page, entries) {
            return false;
        }

        append_page(
            &self.stack_store,
            &self.displayed_stack,
            page.index,
            entries,
        );

        self.stack_empty.set_visible(false);
        self.update_stack_paging();
        true
    }

    pub(crate) fn show_stack_page_error(&self, page: StackPage, reason: &str) {
        if !self.model.fail_stack_page(page, reason) {
            return;
        }

        if page.index == 0 {
            self.stack_store.remove_all();
            self.displayed_stack.borrow_mut().clear();
            self.stack_empty.set_text(reason);
            self.stack_empty.set_visible(true);
        }

        self.update_stack_paging();
    }

    pub(crate) fn show_stack_details(&self, generation: u64, entries: &[StackEntry]) {
        if !self.model.publish_stack_details(generation, entries) {
            return;
        }

        update_details(&self.stack_store, &self.displayed_stack, entries);
    }
}

fn append_page(
    store: &gio::ListStore,
    displayed: &RefCell<Vec<StackEntry>>,
    index: usize,
    entries: &[StackEntry],
) {
    if index == 0 {
        // Keep the existing refresh behavior for unchanged words. Retained
        // annotations are presentation-only and final details replace them.
        let mut rendered = entries.to_vec();
        debug_state::preserve_stack_render_details(&mut rendered, &displayed.borrow());
        components::replace_snapshot_store(store, rendered.iter().cloned());
        displayed.replace(rendered);
    } else {
        let objects = entries
            .iter()
            .cloned()
            .map(components::SnapshotRow::new)
            .collect::<Vec<_>>();

        store.splice(index as u32, 0, &objects);
        displayed.borrow_mut().extend_from_slice(entries);
    }
}

fn update_details(
    store: &gio::ListStore,
    displayed: &RefCell<Vec<StackEntry>>,
    entries: &[StackEntry],
) {
    for entry in entries {
        let changed = displayed
            .borrow()
            .get(entry.index)
            .is_some_and(|current| current != entry);
        if changed {
            let Some(row) = store
                .item(entry.index as u32)
                .and_downcast::<components::SnapshotRow>()
            else {
                continue;
            };
            row.replace(entry.clone());

            displayed.borrow_mut()[entry.index].clone_from(entry);
        }
    }
}
