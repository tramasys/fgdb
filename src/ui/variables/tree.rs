//! Variable tree identities, payload updates and row lookup.

use super::VariableNode;
use crate::debugger::{Variable, VariableUpdate};
use crate::ui::components::SnapshotRow;
use gtk::{gio, prelude::*};
use std::{cell::RefCell, collections::HashMap, rc::Rc};

pub(in crate::ui) fn variable_at(
    selection: &gtk::SingleSelection,
    position: u32,
) -> Option<Variable> {
    variable_row_at(selection, position).map(|(_, variable)| variable)
}

pub(in crate::ui) fn root_variable_at(
    selection: &gtk::SingleSelection,
    position: u32,
) -> Option<Variable> {
    let (mut row, _) = variable_node_at(selection, position)?;

    while let Some(parent) = row.parent() {
        row = parent;
    }

    let item = row.item()?.downcast::<SnapshotRow>().ok()?;
    let node = item.borrow::<VariableNode>();

    (!node.placeholder).then(|| node.variable.clone())
}

pub(in crate::ui) fn variable_row_at(
    selection: &gtk::SingleSelection,
    position: u32,
) -> Option<(gtk::TreeListRow, Variable)> {
    variable_node_at(selection, position)
        .and_then(|(row, node)| (!node.placeholder).then_some((row, node.variable)))
}

pub(in crate::ui) fn variable_node_at(
    selection: &gtk::SingleSelection,
    position: u32,
) -> Option<(gtk::TreeListRow, VariableNode)> {
    selection
        .item(position)
        .and_then(|item| item.downcast::<gtk::TreeListRow>().ok())
        .and_then(|row| {
            let item = row
                .item()
                .and_then(|item| item.downcast::<SnapshotRow>().ok())?;

            let node = item.borrow::<VariableNode>();

            Some((row, node.clone()))
        })
}

pub(in crate::ui) fn index_variable_nodes(
    store: &gio::ListStore,
    index: &mut HashMap<String, VariableNode>,
) {
    let mut pending = vec![store.clone()];

    while let Some(store) = pending.pop() {
        for position in 0..store.n_items() {
            let Some(item) = store.item(position).and_downcast::<SnapshotRow>() else {
                continue;
            };

            let node = item.borrow::<VariableNode>().clone();

            if let Some(varobj) = node.variable.varobj.as_ref() {
                index.insert(varobj.clone(), node.clone());
            }

            if node.children.n_items() > 0 {
                pending.push(node.children);
            }
        }
    }
}

pub(in crate::ui) fn remove_indexed_variable_nodes(
    store: &gio::ListStore,
    index: &mut HashMap<String, VariableNode>,
) {
    let mut pending = vec![store.clone()];

    while let Some(store) = pending.pop() {
        for position in 0..store.n_items() {
            let Some(item) = store.item(position).and_downcast::<SnapshotRow>() else {
                continue;
            };

            let node = item.borrow::<VariableNode>().clone();

            if let Some(varobj) = node.variable.varobj.as_ref() {
                index.remove(varobj);
            }

            if node.children.n_items() > 0 {
                pending.push(node.children);
            }
        }
    }
}

pub(in crate::ui) fn variable_root_node(
    store: &gio::ListStore,
    position: usize,
) -> Option<VariableNode> {
    store
        .item(u32::try_from(position).ok()?)
        .and_downcast::<SnapshotRow>()
        .map(|item| item.borrow::<VariableNode>().clone())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::ui) enum VariableRootChange {
    Unchanged,
    Updated,
    Rebuilt,
}

pub(in crate::ui) fn replace_variable_roots(
    store: &gio::ListStore,
    variables: &[Variable],
    mark_changed: bool,
    node_index: &RefCell<VariableNodeIndex>,
) -> VariableRootChange {
    let same_roots = usize::try_from(store.n_items()).ok() == Some(variables.len())
        && variables.iter().enumerate().all(|(index, variable)| {
            store
                .item(u32::try_from(index).unwrap_or(u32::MAX))
                .and_downcast::<SnapshotRow>()
                .is_some_and(|item| {
                    let node = item.borrow::<VariableNode>();

                    !node.placeholder
                        && node.variable.local_index == variable.local_index
                        && node.variable.return_value == variable.return_value
                        && node.variable.name == variable.name
                        && node.variable.argument == variable.argument
                })
        });

    if !same_roots {
        let rows = variables
            .iter()
            .cloned()
            .map(VariableNode::new)
            .map(SnapshotRow::new)
            .collect::<Vec<_>>();

        {
            let mut index = node_index.borrow_mut();
            index.remove_store(store);

            for row in &rows {
                index.insert(row.borrow::<VariableNode>().clone());
            }
        }

        store.splice(0, store.n_items(), &rows);
        return VariableRootChange::Rebuilt;
    }

    let mut changed = false;

    for (index, variable) in variables.iter().enumerate() {
        let position = u32::try_from(index).unwrap_or(u32::MAX);

        let Some(item) = store.item(position).and_downcast::<SnapshotRow>() else {
            continue;
        };

        let node = item.borrow::<VariableNode>().clone();

        let value_changed = if mark_changed {
            node.variable.value != variable.value
        } else {
            node.changed
        };

        if node.variable == *variable && node.changed == value_changed {
            continue;
        }

        let updated = node.updated(variable.clone(), mark_changed);
        node_index.borrow_mut().replace(&node, &updated);
        update_variable_node(store, position, updated);
        changed = true;
    }

    if changed {
        VariableRootChange::Updated
    } else {
        VariableRootChange::Unchanged
    }
}

pub(in crate::ui) fn replace_variable_root(
    store: &gio::ListStore,
    index: usize,
    variable: &Variable,
    mark_changed: bool,
    node_index: &RefCell<VariableNodeIndex>,
) -> Option<VariableRootChange> {
    let Ok(position) = u32::try_from(index) else {
        return None;
    };

    let item = store.item(position).and_downcast::<SnapshotRow>()?;

    let node = item.borrow::<VariableNode>().clone();

    if node.placeholder
        || node.variable.local_index != variable.local_index
        || node.variable.name != variable.name
        || node.variable.argument != variable.argument
    {
        return None;
    }

    let target_changed = if mark_changed {
        node.variable.value != variable.value
    } else {
        node.changed
    };

    if node.variable == *variable && node.changed == target_changed {
        return Some(VariableRootChange::Unchanged);
    }

    let updated = node.updated(variable.clone(), mark_changed);
    node_index.borrow_mut().replace(&node, &updated);
    update_variable_node(store, position, updated);

    Some(VariableRootChange::Updated)
}

fn update_variable_node(store: &gio::ListStore, position: u32, node: VariableNode) {
    let Some(row) = store.item(position).and_downcast::<SnapshotRow>() else {
        return;
    };

    let same_structure = {
        let previous = row.borrow::<VariableNode>();
        previous.children == node.children
            && previous.variable.can_expand() == node.variable.can_expand()
    };

    if same_structure {
        row.set(node);
    } else {
        store.splice(position, 1, &[SnapshotRow::new(node)]);
    }
}

pub(in crate::ui) fn changed_variable_roots(store: &gio::ListStore) -> usize {
    (0..store.n_items())
        .filter(|position| {
            store
                .item(*position)
                .and_downcast::<SnapshotRow>()
                .is_some_and(|item| item.borrow::<VariableNode>().has_changes())
        })
        .count()
}

pub(in crate::ui) fn apply_variable_updates(
    store: &gio::ListStore,
    updates: &[VariableUpdate],
    mut on_updated: impl FnMut(&VariableNode, &VariableNode),
) -> usize {
    let updates = updates
        .iter()
        .map(|update| (update.varobj.as_str(), update))
        .collect::<HashMap<_, _>>();

    apply_variable_updates_to_store(store, &updates, &mut on_updated)
}

pub(in crate::ui) fn clear_variable_change_markers(
    roots: &gio::ListStore,
    index: &RefCell<VariableNodeIndex>,
) {
    let mut changed_roots = vec![false; roots.n_items() as usize];
    let mut pending = Vec::new();

    for position in 0..roots.n_items() {
        let Some(item) = roots.item(position).and_downcast::<SnapshotRow>() else {
            continue;
        };

        let node = item.borrow::<VariableNode>();
        changed_roots[position as usize] = node.changed;

        if node.children.n_items() > 0 {
            pending.push((node.children.clone(), position));
        }
    }

    while let Some((store, root)) = pending.pop() {
        for position in 0..store.n_items() {
            let Some(item) = store.item(position).and_downcast::<SnapshotRow>() else {
                continue;
            };

            let node = item.borrow::<VariableNode>();

            if node.children.n_items() > 0 {
                pending.push((node.children.clone(), root));
            }

            let replacement = node.changed.then(|| node.without_change_marker());
            drop(node);

            if let Some(replacement) = replacement {
                changed_roots[root as usize] = true;

                index.borrow_mut().insert(replacement.clone());

                update_variable_node(&store, position, replacement);
            }
        }
    }

    // Root filters do not observe descendant-store changes. Notify them only
    // after every child marker is cleared, and only for affected roots.
    for (position, changed) in changed_roots.into_iter().enumerate() {
        if !changed {
            continue;
        }

        let position = position as u32;

        if let Some(item) = roots.item(position).and_downcast::<SnapshotRow>() {
            let replacement = item.borrow::<VariableNode>().without_change_marker();

            index.borrow_mut().insert(replacement.clone());

            update_variable_node(roots, position, replacement);
        }
    }
}

fn apply_variable_updates_to_store(
    store: &gio::ListStore,
    updates: &HashMap<&str, &VariableUpdate>,
    on_updated: &mut impl FnMut(&VariableNode, &VariableNode),
) -> usize {
    let mut applied = 0;
    let mut pending = vec![store.clone()];

    while let Some(store) = pending.pop() {
        for position in 0..store.n_items() {
            let Some(item) = store.item(position).and_downcast::<SnapshotRow>() else {
                continue;
            };

            let node = item.borrow::<VariableNode>();

            let update = node
                .variable
                .varobj
                .as_deref()
                .and_then(|varobj| updates.get(varobj).copied());

            let children = if let Some(update) = update {
                let updated = node.apply_update(update);
                let children = updated.children.clone();
                on_updated(&node, &updated);
                drop(node);
                update_variable_node(&store, position, updated);
                applied += 1;

                children
            } else {
                node.children.clone()
            };

            if children.n_items() > 0 {
                pending.push(children);
            }
        }
    }

    applied
}

pub(in crate::ui) fn root_variable_position(
    selection: &gtk::SingleSelection,
    name: &str,
    argument: bool,
    local_index: Option<usize>,
) -> Option<u32> {
    let model = selection.model()?;

    (0..model.n_items()).find(|position| {
        variable_node_at(selection, *position).is_some_and(|(row, node)| {
            row.depth() == 0
                && node.variable.name == name
                && node.variable.argument == argument
                && node.variable.local_index == local_index
        })
    })
}

pub(in crate::ui) fn remove_load_more_rows(store: &gio::ListStore) {
    for position in (0..store.n_items()).rev() {
        let is_load_more = store
            .item(position)
            .and_then(|item| item.downcast::<SnapshotRow>().ok())
            .is_some_and(|item| item.borrow::<VariableNode>().load_more.is_some());

        if is_load_more {
            store.remove(position);
        }
    }
}

/// Each tree owns its lookup index. Mutations publish rows and invalidate the
/// root filter only after the complete batch has been applied.
#[derive(Clone)]
pub(in crate::ui) struct VariableTree {
    pub(in crate::ui) store: gio::ListStore,
    pub(in crate::ui) selection: gtk::SingleSelection,
    index: Rc<RefCell<VariableNodeIndex>>,
}

impl VariableTree {
    pub(in crate::ui) fn new(store: gio::ListStore, selection: gtk::SingleSelection) -> Self {
        let mut index = VariableNodeIndex::default();
        index.index_store(&store);

        Self {
            store,
            selection,
            index: Rc::new(RefCell::new(index)),
        }
    }

    pub(in crate::ui) fn get(&self, varobj: &str) -> Option<VariableNode> {
        self.index.borrow().get(varobj)
    }

    pub(in crate::ui) fn contains(&self, varobj: &str) -> bool {
        self.index.borrow().contains(varobj)
    }

    pub(in crate::ui) fn replace_roots(
        &self,
        variables: &[Variable],
        mark_changed: bool,
    ) -> VariableRootChange {
        let changed = replace_variable_roots(&self.store, variables, mark_changed, &self.index);

        if changed != VariableRootChange::Unchanged {
            crate::ui::views::invalidate_variable_filter(&self.selection);
        }

        changed
    }

    pub(in crate::ui) fn replace_root(
        &self,
        position: usize,
        variable: &Variable,
        mark_changed: bool,
    ) -> Option<VariableRootChange> {
        let changed =
            replace_variable_root(&self.store, position, variable, mark_changed, &self.index);

        if changed == Some(VariableRootChange::Updated) {
            crate::ui::views::invalidate_variable_filter(&self.selection);
        }

        changed
    }

    pub(in crate::ui) fn apply_updates(&self, updates: &[VariableUpdate]) -> usize {
        let changed = apply_variable_updates(&self.store, updates, |previous, updated| {
            self.index.borrow_mut().replace(previous, updated);
        });

        if changed > 0 {
            crate::ui::views::invalidate_variable_filter(&self.selection);
        }

        changed
    }

    pub(in crate::ui) fn clear_change_markers(&self) {
        clear_variable_change_markers(&self.store, &self.index);
        crate::ui::views::invalidate_variable_filter(&self.selection);
    }

    pub(in crate::ui) fn replace_children(
        &self,
        node: &VariableNode,
        parent: &Variable,
        from: usize,
        variables: &[Variable],
        has_more: bool,
    ) {
        if from == 0 {
            self.index.borrow_mut().remove_store(&node.children);
        } else {
            remove_load_more_rows(&node.children);
        }

        let mut additions = Vec::with_capacity(variables.len() + usize::from(has_more));

        for variable in variables {
            let child = node.child(variable.clone());
            self.index.borrow_mut().insert(child.clone());
            additions.push(SnapshotRow::new(child));
        }

        if has_more {
            additions.push(SnapshotRow::new(VariableNode::load_more(
                parent.clone(),
                from.saturating_add(variables.len()),
            )));
        }

        if from == 0 {
            node.children.splice(0, node.children.n_items(), &additions);
        } else {
            node.children.extend_from_slice(&additions);
        }

        node.children_loading.set(false);
        node.children_loaded.set(true);
        crate::ui::views::invalidate_variable_filter(&self.selection);
    }

    pub(in crate::ui) fn children_error(
        &self,
        node: &VariableNode,
        parent: &Variable,
        from: usize,
        error: &str,
    ) {
        if from == 0 {
            self.index.borrow_mut().remove_store(&node.children);
        }

        apply_variable_children_page_error(node, parent, from, error);
        crate::ui::views::invalidate_variable_filter(&self.selection);
    }
}

#[derive(Default)]
pub(in crate::ui) struct VariableNodeIndex {
    nodes: HashMap<String, VariableNode>,
}

impl VariableNodeIndex {
    pub(in crate::ui) fn get(&self, varobj: &str) -> Option<VariableNode> {
        self.nodes.get(varobj).cloned()
    }

    pub(in crate::ui) fn contains(&self, varobj: &str) -> bool {
        self.nodes.contains_key(varobj)
    }

    pub(in crate::ui) fn insert(&mut self, node: VariableNode) {
        if let Some(varobj) = node.variable.varobj.as_ref() {
            self.nodes.insert(varobj.clone(), node);
        }
    }

    pub(in crate::ui) fn replace(&mut self, previous: &VariableNode, updated: &VariableNode) {
        if previous.variable.varobj != updated.variable.varobj
            && let Some(varobj) = previous.variable.varobj.as_ref()
        {
            self.nodes.remove(varobj);
        }

        if previous.children != updated.children {
            self.remove_store(&previous.children);
        }

        self.insert(updated.clone());
    }

    pub(in crate::ui) fn index_store(&mut self, store: &gio::ListStore) {
        index_variable_nodes(store, &mut self.nodes);
    }

    pub(in crate::ui) fn remove_store(&mut self, store: &gio::ListStore) {
        remove_indexed_variable_nodes(store, &mut self.nodes);
    }
}

pub(in crate::ui) fn apply_variable_children_page_error(
    node: &VariableNode,
    parent: &Variable,
    from: usize,
    error: &str,
) {
    if from == 0 {
        node.children.splice(
            0,
            node.children.n_items(),
            &[SnapshotRow::new(VariableNode::retry_expansion(
                parent.clone(),
                error,
            ))],
        );
    } else {
        remove_load_more_rows(&node.children);

        node.children
            .append(&SnapshotRow::new(VariableNode::load_more_error(
                parent.clone(),
                from,
                error,
            )));
    }

    node.children_loading.set(false);
    node.children_loaded.set(true);
}

#[cfg(test)]
mod tests {
    use super::*;
    fn variable(name: &str, value: &str, varobj: Option<&str>, children: usize) -> Variable {
        Variable {
            local_index: None,
            return_value: None,
            name: name.to_owned(),
            value: value.to_owned(),
            type_name: Some(String::from("demo::Value")),
            argument: false,
            varobj: varobj.map(str::to_owned),
            num_children: children,
            has_more: false,
            display_hint: None,
            dynamic: false,
        }
    }

    #[test]
    fn incremental_child_updates_preserve_expansion_and_clear_per_stop_markers() {
        let root = VariableNode::new(variable("root", "{...}", Some("var1"), 1));

        root.children
            .append(&SnapshotRow::new(VariableNode::new(variable(
                "field",
                "1",
                Some("var1.field"),
                0,
            ))));

        root.children_loaded.set(true);
        root.expanded.set(true);
        let store = gio::ListStore::new::<SnapshotRow>();
        store.append(&SnapshotRow::new(root));

        let index = RefCell::new(VariableNodeIndex::default());
        index.borrow_mut().index_store(&store);

        let applied = apply_variable_updates(
            &store,
            &[VariableUpdate {
                varobj: String::from("var1.field"),
                value: Some(String::from("2")),
                in_scope: Some(true),
                type_changed: false,
                new_type: None,
                new_num_children: None,
                has_more: None,
                display_hint: None,
                dynamic: None,
            }],
            |previous, updated| index.borrow_mut().replace(previous, updated),
        );

        assert_eq!(applied, 1);
        assert_eq!(
            index.borrow().get("var1.field").unwrap().variable.value,
            "2"
        );

        let root = store.item(0).and_downcast::<SnapshotRow>().unwrap();

        let root = root.borrow::<VariableNode>();
        assert!(root.expanded.get());
        assert!(root.has_changes());

        let child = root.children.item(0).and_downcast::<SnapshotRow>().unwrap();

        assert_eq!(child.borrow::<VariableNode>().variable.value, "2");
        assert!(child.borrow::<VariableNode>().changed);
        drop(root);
        clear_variable_change_markers(&store, &index);
        assert!(!index.borrow().get("var1.field").unwrap().changed);

        let root = store.item(0).and_downcast::<SnapshotRow>().unwrap();

        let root = root.borrow::<VariableNode>();
        assert!(root.expanded.get());
        assert!(!root.has_changes());

        assert_eq!(
            root.children
                .item(0)
                .and_downcast::<SnapshotRow>()
                .unwrap()
                .borrow::<VariableNode>()
                .variable
                .value,
            "2"
        );
    }

    #[test]
    fn argument_scope_is_part_of_a_root_identity() {
        let store = gio::ListStore::new::<SnapshotRow>();

        store.append(&SnapshotRow::new(VariableNode::new(variable(
            "value", "1", None, 0,
        ))));

        let index = RefCell::new(VariableNodeIndex::default());
        index.borrow_mut().index_store(&store);
        let mut argument = variable("value", "1", None, 0);
        argument.argument = true;

        assert_eq!(
            replace_variable_roots(&store, &[argument], true, &index),
            VariableRootChange::Rebuilt
        );
    }

    #[test]
    fn variable_node_index_includes_loaded_descendants() {
        let root = VariableNode::new(variable("root", "{...}", Some("var1"), 1));

        root.children
            .append(&SnapshotRow::new(VariableNode::new(variable(
                "field",
                "1",
                Some("var1.field"),
                0,
            ))));

        let store = gio::ListStore::new::<SnapshotRow>();
        store.append(&SnapshotRow::new(root));
        let mut index = HashMap::new();
        index_variable_nodes(&store, &mut index);
        assert_eq!(index.len(), 2);
        assert_eq!(index["var1.field"].variable.name, "field");
    }
}
