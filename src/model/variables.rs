//! Accepted variable snapshots and expression intent, independent of GTK rows.

use super::DebuggerModel;
use crate::debugger::Variable;
use std::{
    cell::{Cell, Ref, RefCell},
    collections::HashSet,
};

const MAX_EXPRESSION_WATCHES: usize = 256;

#[derive(Default)]
pub(super) struct VariableState {
    locals: RefCell<LocalVariableCatalog>,
    generation: Cell<Option<u64>>,
    local_revision: Cell<u64>,
    watches: RefCell<Vec<Variable>>,
    watch_revision: Cell<u64>,
    expressions: RefCell<Vec<String>>,
    pub(super) pending_locals: RefCell<HashSet<(u64, usize)>>,
}

impl DebuggerModel {
    pub(crate) fn locals(&self) -> Ref<'_, LocalVariableCatalog> {
        self.variables.locals.borrow()
    }

    pub(crate) fn publish_locals(
        &self,
        generation: Option<u64>,
        variables: &[Variable],
    ) -> Option<bool> {
        if generation.is_some_and(|generation| !self.is_stop_refresh_current(generation)) {
            return None;
        }

        let new_snapshot = self.variables.generation.replace(generation) != generation;
        self.variables.local_revision.set(self.symbols.revision());
        self.variables.locals.borrow_mut().replace(variables);
        Some(new_snapshot)
    }

    pub(crate) fn locals_are_current(&self) -> bool {
        self.variables
            .generation
            .get()
            .is_some_and(|generation| self.is_stop_refresh_current(generation))
    }

    pub(crate) fn locals_inspection_available(&self) -> bool {
        self.locals_are_current()
            && self.stopped_inspection_available()
            && self.execution().inferior_action_pending.is_none()
            && self.execution().thread_action_pending.is_none()
    }

    pub(crate) fn has_local_variable_identity(&self, variable: &Variable) -> bool {
        self.locals_are_current()
            && variable
                .local_index
                .and_then(|index| {
                    self.variables
                        .locals
                        .borrow()
                        .entries
                        .get(index)
                        .map(|entry| entry.root(index).has_same_children(variable))
                })
                .unwrap_or(false)
    }

    pub(crate) fn update_local_root(
        &self,
        generation: u64,
        index: usize,
        variable: &Variable,
    ) -> bool {
        if self.variables.generation.get() != Some(generation)
            || !self.is_stop_refresh_current(generation)
        {
            return false;
        }

        let mut locals = self.variables.locals.borrow_mut();
        let Some(entry) = locals.entries.get_mut(index) else {
            return false;
        };

        if entry.variable.name != variable.name || entry.variable.argument != variable.argument {
            return false;
        }

        locals.update(index, variable);
        true
    }

    pub(crate) fn claim_local_variable_object(&self, generation: u64, variable: &Variable) -> bool {
        self.is_stop_refresh_current(generation)
            && self.has_local_variable_identity(variable)
            && variable.local_index.is_some_and(|index| {
                self.variables
                    .pending_locals
                    .borrow_mut()
                    .insert((generation, index))
            })
    }

    pub(crate) fn finish_local_variable_object(&self, generation: u64, variable: &Variable) {
        if let Some(index) = variable.local_index {
            self.variables
                .pending_locals
                .borrow_mut()
                .remove(&(generation, index));
        }
    }

    pub(crate) fn watch_expressions(&self) -> Ref<'_, Vec<String>> {
        self.variables.expressions.borrow()
    }

    pub(crate) fn can_add_watch(&self, expression: &str) -> bool {
        let expression = expression.trim();
        let expressions = self.variables.expressions.borrow();
        !expression.is_empty()
            && expressions.len() < MAX_EXPRESSION_WATCHES
            && !expressions.iter().any(|existing| existing == expression)
    }

    pub(crate) fn add_watch(&self, expression: &str) -> bool {
        if !self.can_add_watch(expression) {
            return false;
        }

        self.variables
            .expressions
            .borrow_mut()
            .push(expression.trim().to_owned());

        true
    }

    pub(crate) fn remove_watch(&self, expression: &str) {
        self.variables
            .expressions
            .borrow_mut()
            .retain(|existing| existing != expression);
    }

    pub(crate) fn replace_watch_expressions(
        &self,
        expressions: &[String],
    ) -> Result<(), &'static str> {
        if expressions.len() > MAX_EXPRESSION_WATCHES
            || expressions
                .iter()
                .any(|expression| expression.trim().is_empty())
        {
            return Err("Watch expressions must be nonempty and limited to 256");
        }

        // Saved workspaces retain their exact expressions and order.
        expressions.clone_into(&mut self.variables.expressions.borrow_mut());
        Ok(())
    }

    pub(crate) fn watch_variables(&self) -> Vec<Variable> {
        self.variables.watches.borrow().clone()
    }

    pub(crate) fn publish_watches(&self, generation: u64, variables: &[Variable]) -> bool {
        if !self.is_stop_refresh_current(generation) {
            return false;
        }

        self.variables.watch_revision.set(self.symbols.revision());
        variables.clone_into(&mut self.variables.watches.borrow_mut());
        true
    }

    pub(crate) fn update_watch_root(
        &self,
        generation: u64,
        index: usize,
        variable: &Variable,
    ) -> bool {
        if !self.is_stop_refresh_current(generation) {
            return false;
        }

        let mut watches = self.variables.watches.borrow_mut();
        let Some(root) = watches.get_mut(index) else {
            return false;
        };

        if root.name != variable.name || root.argument != variable.argument {
            return false;
        }

        root.clone_from(variable);
        true
    }

    /// Return reusable roots and objects to retire without erasing the old display.
    pub(crate) fn variable_objects_for_refresh(
        &self,
        watches: bool,
    ) -> (Vec<Variable>, Vec<String>) {
        let (variables, revision) = if watches {
            (self.watch_variables(), self.variables.watch_revision.get())
        } else {
            (self.locals().to_vec(), self.variables.local_revision.get())
        };

        if revision == self.symbols.revision() {
            (variables, Vec::new())
        } else {
            (
                Vec::new(),
                variables
                    .into_iter()
                    .filter_map(|variable| variable.varobj)
                    .collect(),
            )
        }
    }
}

pub(crate) fn local_refresh_indices(
    variables: &[Variable],
    query: &str,
    limit: usize,
) -> HashSet<usize> {
    let terms = query.split_whitespace().collect::<Vec<_>>();
    variables
        .iter()
        .enumerate()
        .filter(|(_, variable)| {
            terms.is_empty() || {
                let text = variable_search_text(variable);
                terms.iter().all(|term| text.contains(term))
            }
        })
        .take(limit)
        .map(|(index, _)| index)
        .collect()
}

#[derive(Default)]
pub(crate) struct LocalVariableCatalog {
    entries: Vec<LocalVariableEntry>,
}

struct LocalVariableEntry {
    variable: Variable,
    search_text: String,
}

impl LocalVariableEntry {
    fn new(variable: &Variable) -> Self {
        Self {
            variable: variable.clone(),
            search_text: variable_search_text(variable),
        }
    }

    fn update(&mut self, variable: &Variable) {
        if self.variable == *variable {
            return;
        }

        // Object identity and child availability do not affect text search.
        if self.variable.name != variable.name
            || self.variable.value != variable.value
            || self.variable.type_name != variable.type_name
            || self.variable.argument != variable.argument
        {
            self.search_text = variable_search_text(variable);
        }

        self.variable.clone_from(variable);
    }

    fn root(&self, index: usize) -> Variable {
        let mut variable = self.variable.clone();
        // Occurrence identity belongs to this catalog, not the incoming value.
        variable.local_index = Some(index);
        variable
    }
}

impl LocalVariableCatalog {
    pub(crate) fn replace(&mut self, variables: &[Variable]) {
        self.entries.truncate(variables.len());

        for (index, variable) in variables.iter().enumerate() {
            if let Some(entry) = self.entries.get_mut(index) {
                entry.update(variable);
            } else {
                self.entries.push(LocalVariableEntry::new(variable));
            }
        }
    }

    pub(crate) fn update(&mut self, index: usize, variable: &Variable) {
        if let Some(entry) = self.entries.get_mut(index) {
            entry.update(variable);
        }
    }

    pub(crate) fn filtered(&self, query: &str, limit: usize) -> (Vec<Variable>, usize) {
        if query.is_empty() {
            return (
                self.entries
                    .iter()
                    .take(limit)
                    .enumerate()
                    .map(|(index, entry)| entry.root(index))
                    .collect(),
                self.entries.len(),
            );
        }

        let terms = query.split_whitespace().collect::<Vec<_>>();
        let mut total = 0_usize;
        let mut rendered = Vec::with_capacity(limit.min(self.entries.len()));

        for (index, entry) in self.entries.iter().enumerate() {
            if terms.iter().all(|term| entry.search_text.contains(term)) {
                total += 1;

                if rendered.len() < limit {
                    rendered.push(entry.root(index));
                }
            }
        }

        (rendered, total)
    }

    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    pub(crate) fn argument_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|entry| entry.variable.argument)
            .count()
    }

    pub(crate) fn to_vec(&self) -> Vec<Variable> {
        self.entries
            .iter()
            .enumerate()
            .map(|(index, entry)| entry.root(index))
            .collect()
    }
}

pub(crate) fn variable_search_text(variable: &Variable) -> String {
    let mut text = format!(
        "{} {} {} {} {}",
        variable.name,
        variable.type_name.as_deref().unwrap_or_default(),
        compact_variable_type(variable.type_name.as_deref().unwrap_or_default()),
        variable.value,
        if variable.return_value.is_some() {
            "return result"
        } else if variable.argument {
            "argument arg"
        } else {
            "local"
        },
    );

    text.make_ascii_lowercase();
    text
}

pub(crate) fn compact_variable_type(type_name: &str) -> String {
    let mut compact = type_name
        .trim()
        .replace("std::__cxx11::", "std::")
        .replace("std::__1::", "std::")
        .replace("std::__debug::", "std::");

    for (qualified, short) in [
        ("alloc::string::String", "String"),
        ("alloc::vec::Vec<", "Vec<"),
        ("alloc::boxed::Box<", "Box<"),
        ("alloc::rc::Rc<", "Rc<"),
        ("alloc::sync::Arc<", "Arc<"),
        ("std::boxed::Box<", "Box<"),
        ("std::rc::Rc<", "Rc<"),
        ("std::sync::Arc<", "Arc<"),
        ("std::vec::Vec<", "Vec<"),
        ("core::cell::RefCell<", "RefCell<"),
        ("std::cell::RefCell<", "RefCell<"),
        ("core::cell::Cell<", "Cell<"),
        ("std::cell::Cell<", "Cell<"),
        ("core::option::Option<", "Option<"),
        ("std::option::Option<", "Option<"),
        ("core::result::Result<", "Result<"),
        ("std::result::Result<", "Result<"),
        ("alloc::collections::vec_deque::VecDeque<", "VecDeque<"),
        ("alloc::collections::btree::map::BTreeMap<", "BTreeMap<"),
        ("std::collections::hash::map::HashMap<", "HashMap<"),
    ] {
        compact = compact.replace(qualified, short);
    }

    compact = compact.replace(
        "std::basic_string<char, std::char_traits<char>, std::allocator<char> >",
        "std::string",
    );

    compact = compact.replace(
        "std::basic_string<char, std::char_traits<char>, std::allocator<char>>",
        "std::string",
    );

    compact = compact.replace(
        ", std::hash::random::RandomState, alloc::alloc::Global>",
        ">",
    );

    compact = compact.replace(", alloc::alloc::Global>", ">");

    while compact.contains("> >") {
        compact = compact.replace("> >", ">>");
    }

    compact
}

#[cfg(test)]
mod tests {
    use super::*;
    fn variable(name: &str, varobj: &str) -> Variable {
        Variable {
            local_index: None,
            return_value: None,
            name: name.to_owned(),
            value: String::from("1"),
            type_name: Some(String::from("int")),
            argument: false,
            varobj: Some(varobj.to_owned()),
            num_children: 0,
            has_more: false,
            display_hint: None,
            dynamic: false,
        }
    }

    #[test]
    fn local_catalog_searches_every_root_without_reformatting_pages() {
        let mut catalog = LocalVariableCatalog::default();

        let variables = (0..600)
            .map(|index| variable(&format!("value_{index}"), &format!("var{index}")))
            .collect::<Vec<_>>();

        catalog.replace(&variables);
        let (rendered, total) = catalog.filtered("value_599 int", 64);
        assert_eq!(total, 1);
        assert_eq!(rendered[0].name, "value_599");
        assert_eq!(catalog.filtered("value", 64).1, 600);
        assert_eq!(rendered[0].local_index, Some(599));
        let search = catalog.entries[599].search_text.as_ptr();
        let value = catalog.entries[599].variable.value.as_ptr();
        catalog.replace(&variables);
        assert_eq!(search, catalog.entries[599].search_text.as_ptr());
        assert_eq!(value, catalog.entries[599].variable.value.as_ptr());

        let mut changed = variables[599].clone();
        changed.num_children = 4;
        changed.varobj = Some(String::from("new_object"));
        catalog.update(599, &changed);
        assert_eq!(search, catalog.entries[599].search_text.as_ptr());
        assert_eq!(catalog.to_vec()[599].num_children, 4);

        catalog.replace(&variables[..2]);
        assert_eq!(catalog.filtered("value_599", 64).1, 0);
        assert_eq!(catalog.len(), 2);
        catalog.replace(&variables);
        assert_eq!(catalog.filtered("value_599", 0).1, 1);
        assert!(catalog.filtered("value_599", 0).0.is_empty());

        // Preparation must use the incoming snapshot and the same page/filter
        // rules without replacing the previous values still being displayed.
        for (query, limit) in [("", 64), ("value_599 int", 64), ("value_599", 0)] {
            let expected = catalog
                .filtered(query, limit)
                .0
                .iter()
                .filter_map(|variable| variable.local_index)
                .collect::<HashSet<_>>();
            assert_eq!(local_refresh_indices(&variables, query, limit), expected);
        }
        let mut incoming = variables.clone();
        incoming.insert(0, variable("new_local", "new"));
        assert_eq!(
            local_refresh_indices(&incoming, "value_599", 64),
            HashSet::from([600])
        );
        assert_eq!(
            catalog.filtered("value_599", 64).0[0].local_index,
            Some(599)
        );
    }

    #[test]
    fn filtered_root_updates_keep_full_catalog_identity_and_duplicates() {
        let mut catalog = LocalVariableCatalog::default();
        let first = variable("value", "var1");
        let mut second = variable("value", "var2");
        second.type_name = Some("OtherType".into());
        catalog.replace(&[first, second]);

        let (rendered, count) = catalog.filtered("othertype", 64);
        assert_eq!(count, 1);
        let mut selected = rendered[0].clone();
        assert_eq!(selected.local_index, Some(1));
        selected.value = "updated".into();
        selected.local_index = Some(999);
        catalog.update(1, &selected);

        let (roots, count) = catalog.filtered("", 64);
        assert_eq!(count, 2);
        assert_eq!(roots[0].varobj.as_deref(), Some("var1"));
        assert_eq!(roots[0].value, "1");
        assert_eq!(roots[1].value, "updated");
        assert_eq!(roots[1].local_index, Some(1));
        assert_eq!(catalog.filtered("updated", 64).0[0].local_index, Some(1));
    }

    #[test]
    fn accepted_variables_and_pending_creation_follow_stop_and_symbol_identity() {
        let model = crate::model::tests::stopped_model();
        let generation = model.start_stop_refresh();
        model.bind_stop_context(1).unwrap();
        let mut root = variable("value", "var1");
        root.local_index = Some(0);
        assert_eq!(
            model.publish_locals(Some(generation), &[root.clone()]),
            Some(true)
        );
        assert!(model.has_local_variable_identity(&root));
        assert!(model.claim_local_variable_object(generation, &root));
        assert!(!model.claim_local_variable_object(generation, &root));
        model.finish_local_variable_object(generation, &root);
        assert!(model.claim_local_variable_object(generation, &root));
        assert!(model.add_watch(" value "));
        assert!(!model.add_watch("value"));
        assert!(!model.add_watch(" "));
        assert!(model.publish_watches(generation, &[root.clone()]));
        let (reusable, retired) = model.variable_objects_for_refresh(false);
        assert_eq!(reusable, vec![root.clone()]);
        assert!(retired.is_empty());
        model
            .variables
            .local_revision
            .set(model.symbols.revision().wrapping_add(1));
        let (reusable, retired) = model.variable_objects_for_refresh(false);
        assert!(reusable.is_empty());
        assert_eq!(retired, ["var1"]);
        assert_eq!(model.locals().to_vec(), vec![root.clone()]);
        let next = model.start_stop_refresh();
        model.bind_stop_context(1).unwrap();
        assert!(!model.has_local_variable_identity(&root));
        assert!(!model.claim_local_variable_object(generation, &root));
        assert_eq!(model.publish_locals(Some(generation), &[]), None);
        assert!(!model.publish_watches(generation, &[]));
        assert!(!model.update_local_root(generation, 0, &root));
        assert!(!model.update_watch_root(generation, 0, &root));
        assert_eq!(model.watch_variables(), vec![root.clone()]);
        assert_eq!(
            model.publish_locals(Some(next), &[root.clone()]),
            Some(true)
        );
        assert!(model.claim_local_variable_object(next, &root));
        let before = model.watch_expressions().clone();
        assert!(
            model
                .replace_watch_expressions(&["value".into(), " ".into()])
                .is_err()
        );
        assert_eq!(*model.watch_expressions(), before);
        let saved = vec![String::from(" value "), String::from("value")];
        model.replace_watch_expressions(&saved).unwrap();
        assert_eq!(*model.watch_expressions(), saved);
    }
}
