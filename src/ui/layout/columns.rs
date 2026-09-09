//! Explicit column identities and requested widths, independent of allocation.

use gtk::{glib, prelude::*};
use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, HashMap},
    fmt::Write as _,
    rc::Rc,
};

mod ids;
pub(in crate::ui) use ids::TableId;

const MIN_WIDTH: i32 = 16;
const MAX_WIDTH: i32 = 8192;
const MAX_COLUMNS: usize = 64;
const PREFIX: &str = "column.";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Widths(BTreeMap<TableId, BTreeMap<String, i32>>);

impl Widths {
    pub(super) fn parse(&mut self, key: &str, value: &str) -> bool {
        let Some(key) = key.strip_prefix(PREFIX) else {
            return false;
        };

        if let Some((table, column)) = key.split_once('.')
            && let Some(table) = TableId::parse(table)
            && valid_column(column)
            && let Ok(width) = value.trim().parse::<i32>()
            && valid_width(width)
        {
            let columns = self.0.entry(table).or_default();

            if columns.len() < MAX_COLUMNS || columns.contains_key(column) {
                columns.insert(column.to_owned(), width);
            }
        }

        true
    }

    pub(super) fn write(&self, output: &mut String) {
        for (table, columns) in &self.0 {
            for (column, width) in columns {
                let _ = writeln!(output, "{PREFIX}{}.{column}={width}", table.key());
            }
        }
    }

    fn get(&self, table: TableId, column: &str) -> Option<i32> {
        self.0.get(&table)?.get(column).copied()
    }
}

fn valid_column(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 64
        && key
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn valid_width(width: i32) -> bool {
    (MIN_WIDTH..=MAX_WIDTH).contains(&width)
}

#[derive(Clone, Default)]
pub(in crate::ui) struct ColumnLayouts(Rc<State>);

#[derive(Default)]
struct State {
    widths: RefCell<Widths>,
    bindings: RefCell<HashMap<(TableId, &'static str), Vec<Binding>>>,
    changed: RefCell<Option<Rc<dyn Fn()>>>,
    applying: Cell<bool>,
    finished: Cell<bool>,
}

struct Binding {
    column: glib::WeakRef<gtk::ColumnViewColumn>,
    default_width: i32,
}

pub(in crate::ui) struct TableLayout {
    layouts: ColumnLayouts,
    id: TableId,
}

impl ColumnLayouts {
    pub(in crate::ui) fn table(&self, id: TableId) -> TableLayout {
        TableLayout {
            layouts: self.clone(),
            id,
        }
    }

    pub(super) fn restore(&self, widths: Widths) {
        self.0.widths.replace(widths);
        self.0.apply_all();
    }

    pub(super) fn on_changed(&self, callback: impl Fn() + 'static) {
        self.0.changed.replace(Some(Rc::new(callback)));
    }

    pub(super) fn write(&self, output: &mut String) {
        self.0.widths.borrow().write(output);
    }

    pub(super) fn finish(&self) {
        self.0.finished.set(true);
        self.0.changed.borrow_mut().take();
    }

    pub(in crate::ui) fn reset(&self) {
        if self.0.finished.get() {
            return;
        }

        self.0.widths.borrow_mut().0.clear();
        self.0.apply_all();
        self.0.notify();
    }
}

impl TableLayout {
    /// Bind at construction, before headings or column positions can change.
    pub(in crate::ui) fn append(
        &self,
        view: &gtk::ColumnView,
        key: &'static str,
        column: &gtk::ColumnViewColumn,
    ) {
        assert!(valid_column(key), "invalid persistent column ID: {key}");
        assert!(
            column.column_view().is_none(),
            "persistent columns must be bound before insertion"
        );

        let state = &self.layouts.0;
        let default_width = column.fixed_width();
        assert!(
            valid_width(default_width),
            "persistent columns require a bounded default width"
        );

        let saved = state.widths.borrow().get(self.id, key);

        if let Some(width) = saved {
            column.set_fixed_width(width);
        }

        let mut bindings = state.bindings.borrow_mut();
        assert!(
            bindings.contains_key(&(self.id, key))
                || bindings
                    .keys()
                    .filter(|(table, _)| *table == self.id)
                    .count()
                    < MAX_COLUMNS,
            "persistent table exceeds its column budget"
        );

        let bound = bindings.entry((self.id, key)).or_default();
        bound.retain(|binding| binding.column.upgrade().is_some());
        assert!(
            !bound.iter().any(|binding| {
                binding
                    .column
                    .upgrade()
                    .and_then(|column| column.column_view())
                    .as_ref()
                    == Some(view)
            }),
            "duplicate persistent column ID within a table"
        );

        debug_assert!(
            bound
                .iter()
                .all(|binding| binding.default_width == default_width),
            "shared table columns have conflicting defaults"
        );

        bound.push(Binding {
            column: column.downgrade(),
            default_width,
        });

        drop(bindings);
        let weak = Rc::downgrade(state);
        let table = self.id;

        column.connect_fixed_width_notify(move |column| {
            if let Some(state) = weak.upgrade() {
                state.remember(table, key, column);
            }
        });

        view.append_column(column);
    }
}

impl State {
    fn remember(&self, table: TableId, key: &'static str, column: &gtk::ColumnViewColumn) {
        let width = column.fixed_width();

        if self.applying.get() || self.finished.get() || !valid_width(width) {
            return;
        }

        if self.widths.borrow().get(table, key) == Some(width) {
            return;
        }

        {
            let mut widths = self.widths.borrow_mut();
            let columns = widths.0.entry(table).or_default();

            if let Some(saved) = columns.get_mut(key) {
                *saved = width;
            } else {
                // A bounded file can contain obsolete column IDs. Make room
                // for a current column instead of writing an entry that the
                // next bounded load could discard.
                if columns.len() >= MAX_COLUMNS {
                    let bindings = self.bindings.borrow();
                    let obsolete = columns
                        .keys()
                        .find(|candidate| {
                            !bindings
                                .keys()
                                .any(|&(id, key)| id == table && key == candidate.as_str())
                        })
                        .cloned();

                    if let Some(obsolete) = obsolete {
                        columns.remove(&obsolete);
                    }
                }

                columns.insert(key.to_owned(), width);
            }
        }

        let peers = self
            .bindings
            .borrow()
            .get(&(table, key))
            .into_iter()
            .flatten()
            .filter_map(|binding| binding.column.upgrade())
            .filter(|peer| peer != column)
            .collect::<Vec<_>>();

        self.apply(peers.into_iter().map(|column| (column, width)));
        self.notify();
    }

    fn apply_all(&self) {
        let widths = self.widths.borrow();
        let mut bindings = self.bindings.borrow_mut();
        let mut updates = Vec::new();

        bindings.retain(|&(table, key), bindings| {
            bindings.retain(|binding| {
                let Some(column) = binding.column.upgrade() else {
                    return false;
                };

                let width = widths.get(table, key).unwrap_or(binding.default_width);
                updates.push((column, width));
                true
            });

            !bindings.is_empty()
        });

        drop(bindings);
        drop(widths);
        self.apply(updates);
    }

    fn apply(&self, updates: impl IntoIterator<Item = (gtk::ColumnViewColumn, i32)>) {
        let previous = self.applying.replace(true);

        for (column, width) in updates {
            if column.fixed_width() != width {
                column.set_fixed_width(width);
            }
        }

        self.applying.set(previous);
    }

    fn notify(&self) {
        let changed = self.changed.borrow().clone();

        if let Some(changed) = changed {
            changed();
        }
    }
}

#[cfg(test)]
mod tests;
