//! Shared locals/watch tree nodes and child-page acceptance rules.

use super::variable_search_text;
use crate::debugger::{Variable, VariableUpdate};
use gtk::{gio, glib, prelude::*};
use std::{cell::Cell, rc::Rc};

pub(in crate::ui) mod locations;

#[derive(Clone)]
pub(super) struct VariableNode {
    pub(super) variable: Variable,
    /// Descendants retain local-snapshot authority even if GDB gives a
    /// dereference an independent object name rather than a dotted child ID.
    pub(super) local: bool,
    pub(super) search_text: Rc<str>,
    pub(super) children: gio::ListStore,
    pub(super) children_loaded: Rc<Cell<bool>>,
    pub(super) children_loading: Rc<Cell<bool>>,
    pub(super) expanded: Rc<Cell<bool>>,
    pub(super) changed: bool,
    pub(super) load_more: Option<(Variable, usize)>,
    pub(super) placeholder: bool,
}

impl VariableNode {
    pub(super) fn new(variable: Variable) -> Self {
        let search_text = variable_search_text(&variable).into();

        Self {
            local: variable.local_index.is_some(),
            variable,
            search_text,
            children: gio::ListStore::new::<glib::BoxedAnyObject>(),
            children_loaded: Rc::new(Cell::new(false)),
            children_loading: Rc::new(Cell::new(false)),
            expanded: Rc::new(Cell::new(false)),
            changed: false,
            load_more: None,
            placeholder: false,
        }
    }

    pub(super) fn child(&self, mut variable: Variable) -> Self {
        variable.return_value = self.variable.return_value;

        Self {
            local: self.local,
            ..Self::new(variable)
        }
    }

    pub(super) fn placeholder(name: &str, value: &str) -> Self {
        let variable = Variable {
            local_index: None,
            return_value: None,
            name: name.to_owned(),
            value: value.to_owned(),
            type_name: None,
            argument: false,
            varobj: None,
            num_children: 0,
            has_more: false,
            display_hint: None,
            dynamic: false,
        };

        Self {
            local: false,
            search_text: variable_search_text(&variable).into(),
            variable,
            children: gio::ListStore::new::<glib::BoxedAnyObject>(),
            children_loaded: Rc::new(Cell::new(true)),
            children_loading: Rc::new(Cell::new(false)),
            expanded: Rc::new(Cell::new(false)),
            changed: false,
            load_more: None,
            placeholder: true,
        }
    }

    pub(super) fn load_more(parent: Variable, next: usize) -> Self {
        let remaining = parent.num_children.saturating_sub(next);

        let detail = if remaining == 0 {
            String::from("more children are available")
        } else {
            format!(
                "{remaining} child{} remaining",
                if remaining == 1 { "" } else { "ren" }
            )
        };

        let variable = Variable {
            local_index: None,
            return_value: None,
            name: String::from("Load more…"),
            value: detail,
            type_name: None,
            argument: false,
            varobj: None,
            num_children: 0,
            has_more: false,
            display_hint: None,
            dynamic: false,
        };

        Self {
            local: false,
            search_text: variable_search_text(&variable).into(),
            variable,
            children: gio::ListStore::new::<glib::BoxedAnyObject>(),
            children_loaded: Rc::new(Cell::new(true)),
            children_loading: Rc::new(Cell::new(false)),
            expanded: Rc::new(Cell::new(false)),
            changed: false,
            load_more: Some((parent, next)),
            placeholder: true,
        }
    }

    pub(super) fn load_more_error(parent: Variable, next: usize, error: &str) -> Self {
        let mut node = Self::load_more(parent, next);
        node.variable.name = String::from("Retry loading more…");
        node.variable.value = error.to_owned();
        node.search_text = variable_search_text(&node.variable).into();

        node
    }

    pub(super) fn retry_expansion(parent: Variable, error: &str) -> Self {
        let mut node = Self::load_more(parent, 0);
        node.variable.name = String::from("Retry expansion…");
        node.variable.value = error.to_owned();
        node.search_text = variable_search_text(&node.variable).into();

        node
    }

    pub(super) fn updated(&self, variable: Variable, mark_changed: bool) -> Self {
        self.with_update(variable, mark_changed, false)
    }

    pub(super) fn apply_update(&self, update: &VariableUpdate) -> Self {
        let mut variable = self.variable.clone();
        variable.apply_update(update);
        self.with_update(variable, true, update.type_changed)
    }

    fn with_update(&self, variable: Variable, mark_changed: bool, type_changed: bool) -> Self {
        let structure_unchanged = !type_changed && self.variable.has_same_children(&variable);
        let expandable = variable.can_expand();

        let search_text = if self.variable.name == variable.name
            && self.variable.value == variable.value
            && self.variable.type_name == variable.type_name
            && self.variable.argument == variable.argument
            && self.variable.return_value.is_some() == variable.return_value.is_some()
        {
            Rc::clone(&self.search_text)
        } else {
            variable_search_text(&variable).into()
        };

        Self {
            local: self.local,
            changed: if mark_changed {
                self.variable.value != variable.value
            } else {
                self.changed
            },
            search_text,
            variable,
            children: if structure_unchanged {
                self.children.clone()
            } else {
                gio::ListStore::new::<glib::BoxedAnyObject>()
            },
            children_loaded: if structure_unchanged {
                Rc::clone(&self.children_loaded)
            } else {
                Rc::new(Cell::new(false))
            },
            children_loading: if structure_unchanged {
                Rc::clone(&self.children_loading)
            } else {
                Rc::new(Cell::new(false))
            },
            expanded: if expandable {
                Rc::clone(&self.expanded)
            } else {
                Rc::new(Cell::new(false))
            },
            load_more: None,
            placeholder: false,
        }
    }

    pub(super) fn has_changes(&self) -> bool {
        if self.changed {
            return true;
        }

        if self.children.n_items() == 0 {
            return false;
        }

        let mut pending = vec![self.children.clone()];

        while let Some(store) = pending.pop() {
            for position in 0..store.n_items() {
                let Some(item) = store.item(position).and_downcast::<glib::BoxedAnyObject>() else {
                    continue;
                };

                let node = item.borrow::<VariableNode>();

                if node.changed {
                    return true;
                }

                if node.children.n_items() > 0 {
                    pending.push(node.children.clone());
                }
            }
        }

        false
    }

    pub(super) fn without_change_marker(&self) -> Self {
        Self {
            local: self.local,
            variable: self.variable.clone(),
            search_text: Rc::clone(&self.search_text),
            children: self.children.clone(),
            children_loaded: Rc::clone(&self.children_loaded),
            children_loading: Rc::clone(&self.children_loading),
            expanded: Rc::clone(&self.expanded),
            changed: false,
            load_more: self.load_more.clone(),
            placeholder: self.placeholder,
        }
    }

    pub(super) fn rebound(&self) -> Self {
        self.clone()
    }

    pub(super) fn accepts_child_page(&self, parent: &Variable, from: usize) -> bool {
        if !self.variable.can_expand() || !self.variable.has_same_children(parent) {
            return false;
        }

        if from == 0 && !self.children_loaded.get() {
            return true;
        }

        self.children
            .n_items()
            .checked_sub(1)
            .and_then(|index| self.children.item(index))
            .and_downcast::<glib::BoxedAnyObject>()
            .is_some_and(|item| {
                item.borrow::<VariableNode>()
                    .load_more
                    .as_ref()
                    .is_some_and(|(expected, offset)| {
                        *offset == from && expected.has_same_children(parent)
                    })
            })
    }
}

#[cfg(test)]
mod tests;
