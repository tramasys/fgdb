use super::*;

#[derive(Default)]
pub(super) struct LocalVariableCatalog {
    roots: Vec<Variable>,
    search_text: Vec<Rc<str>>,
}

impl LocalVariableCatalog {
    pub(super) fn replace(&mut self, variables: &[Variable]) {
        let mut roots = variables.to_vec();

        for (index, variable) in roots.iter_mut().enumerate() {
            variable.local_index = Some(index);
        }

        let search_text = roots
            .iter()
            .enumerate()
            .map(|(index, variable)| {
                self.roots
                    .get(index)
                    .filter(|previous| *previous == variable)
                    .and_then(|_| self.search_text.get(index).cloned())
                    .unwrap_or_else(|| variable_search_text(variable).into())
            })
            .collect();

        self.roots = roots;
        self.search_text = search_text;
    }

    pub(super) fn update(&mut self, index: usize, variable: &Variable) {
        let Some(existing) = self.roots.get_mut(index) else {
            return;
        };

        if existing != variable {
            existing.clone_from(variable);
            existing.local_index = Some(index);

            if let Some(search_text) = self.search_text.get_mut(index) {
                *search_text = variable_search_text(variable).into();
            }
        }
    }

    pub(super) fn filtered(&self, query: &str, limit: usize) -> (Vec<Variable>, usize) {
        if query.is_empty() {
            return (
                self.roots.iter().take(limit).cloned().collect(),
                self.roots.len(),
            );
        }

        let terms = query.split_whitespace().collect::<Vec<_>>();
        let mut total = 0_usize;
        let mut rendered = Vec::with_capacity(limit.min(self.roots.len()));

        for (variable, search_text) in self.roots.iter().zip(&self.search_text) {
            if terms.iter().all(|term| search_text.contains(term)) {
                total += 1;

                if rendered.len() < limit {
                    rendered.push(variable.clone());
                }
            }
        }

        (rendered, total)
    }

    pub(super) fn len(&self) -> usize {
        self.roots.len()
    }

    pub(super) fn argument_count(&self) -> usize {
        self.roots
            .iter()
            .filter(|variable| variable.argument)
            .count()
    }

    pub(super) fn to_vec(&self) -> Vec<Variable> {
        self.roots.clone()
    }
}

#[derive(Clone, Copy, Debug)]
struct PendingTerminalSynchronization {
    generation: u64,
    last_activity: Instant,
}

/// Coordinates commands entered through GDB's interactive terminal with the
/// structured MI state used by the rest of the application. Terminal output
/// is deliberately treated as activity rather than parsed for a particular
/// prompt: users can customize prompts and pretty-printers may write output of
/// their own.
#[derive(Default)]
pub(super) struct TerminalSynchronization {
    generation: u64,
    pending: Option<PendingTerminalSynchronization>,
    prompt: Option<String>,
}

impl TerminalSynchronization {
    pub(super) fn begin(&mut self, now: Instant) -> u64 {
        self.generation = self.generation.wrapping_add(1);

        self.pending = Some(PendingTerminalSynchronization {
            generation: self.generation,
            last_activity: now,
        });

        self.generation
    }

    pub(super) fn note_activity(&mut self, now: Instant) {
        if let Some(pending) = self.pending.as_mut() {
            pending.last_activity = now;
        }
    }

    pub(super) fn is_quiet(
        &self,
        generation: u64,
        now: Instant,
        quiet_period: Duration,
    ) -> Option<bool> {
        let pending = self.pending.as_ref()?;

        (pending.generation == generation)
            .then(|| now.saturating_duration_since(pending.last_activity) >= quiet_period)
    }

    pub(super) fn finish(&mut self, generation: u64) -> bool {
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.generation == generation)
        {
            self.pending = None;

            true
        } else {
            false
        }
    }

    pub(super) fn cancel(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.pending = None;
    }

    pub(super) fn set_prompt(&mut self, prompt: &str) {
        let prompt = prompt.trim();
        self.prompt = (!prompt.is_empty() && prompt.len() <= 256).then(|| prompt.to_owned());
    }

    pub(super) fn is_prompt(&self, text: &str) -> bool {
        let text = text.trim();

        known_gdb_prompt(text) || self.prompt.as_deref() == Some(text)
    }
}

/// Tracks a multi-request refresh independently from the widgets presenting
/// it. A response enables the aggregate action only after every member has
/// either completed or been removed.
#[derive(Default)]
pub(super) struct MemoryRefreshBatch {
    pending: HashSet<u64>,
}

impl MemoryRefreshBatch {
    pub(super) fn begin(&mut self, ids: impl IntoIterator<Item = u64>) {
        self.pending.clear();
        self.pending.extend(ids);
    }

    pub(super) fn finish(&mut self, id: u64) -> bool {
        self.pending.remove(&id);

        !self.pending.is_empty()
    }

    pub(super) fn remove(&mut self, id: u64) -> bool {
        self.pending.remove(&id);

        !self.pending.is_empty()
    }

    pub(super) fn clear(&mut self) {
        self.pending.clear();
    }

    pub(super) fn is_pending(&self) -> bool {
        !self.pending.is_empty()
    }
}

/// Authoritative lookup for instantiated GDB variable objects. GTK stores
/// remain presentation models; command validation no longer needs to walk the
/// entire rendered tree after every child page or value update.
#[derive(Default)]
pub(super) struct VariableNodeIndex {
    nodes: HashMap<String, VariableNode>,
}

impl VariableNodeIndex {
    pub(super) fn get(&self, varobj: &str) -> Option<VariableNode> {
        self.nodes.get(varobj).cloned()
    }

    pub(super) fn contains(&self, varobj: &str) -> bool {
        self.nodes.contains_key(varobj)
    }

    pub(super) fn insert(&mut self, node: VariableNode) {
        if let Some(varobj) = node.variable.varobj.as_ref() {
            self.nodes.insert(varobj.clone(), node);
        }
    }

    pub(super) fn remove_node(&mut self, node: &VariableNode) {
        if let Some(varobj) = node.variable.varobj.as_ref() {
            self.nodes.remove(varobj);
        }

        self.remove_store(&node.children);
    }

    pub(super) fn index_store(&mut self, store: &gio::ListStore) {
        index_variable_nodes(store, &mut self.nodes);
    }

    pub(super) fn remove_store(&mut self, store: &gio::ListStore) {
        remove_indexed_variable_nodes(store, &mut self.nodes);
    }

    pub(super) fn rebuild(&mut self, locals: &gio::ListStore, watches: &gio::ListStore) {
        self.nodes.clear();
        self.index_store(locals);
        self.index_store(watches);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn variable(name: &str, varobj: &str) -> Variable {
        Variable {
            local_index: None,
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
        let search = Rc::clone(&catalog.search_text[599]);
        catalog.replace(&variables);
        assert!(Rc::ptr_eq(&search, &catalog.search_text[599]));
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
        catalog.update(selected.local_index.unwrap(), &selected);

        let (roots, count) = catalog.filtered("", 64);
        assert_eq!(count, 2);
        assert_eq!(roots[0].varobj.as_deref(), Some("var1"));
        assert_eq!(roots[0].value, "1");
        assert_eq!(roots[1].value, "updated");
        assert_eq!(catalog.filtered("updated", 64).0[0].local_index, Some(1));
    }

    #[test]
    fn memory_batch_finishes_only_after_every_member() {
        let mut batch = MemoryRefreshBatch::default();
        batch.begin([1, 2, 3]);
        assert!(batch.finish(2));
        assert!(batch.remove(1));
        assert!(!batch.finish(3));
    }

    #[test]
    fn variable_index_removes_only_the_replaced_subtree() {
        let first = VariableNode::new(variable("first", "var1"));

        first
            .children
            .append(&glib::BoxedAnyObject::new(VariableNode::new(variable(
                "child",
                "var1.child",
            ))));

        let second = VariableNode::new(variable("second", "var2"));
        let mut index = VariableNodeIndex::default();
        index.insert(first.clone());
        index.index_store(&first.children);
        index.insert(second);
        index.remove_node(&first);
        assert!(!index.contains("var1"));
        assert!(!index.contains("var1.child"));
        assert!(index.contains("var2"));
    }

    #[test]
    fn newer_terminal_commands_supersede_older_synchronization_barriers() {
        let start = Instant::now();
        let mut synchronization = TerminalSynchronization::default();
        let first = synchronization.begin(start);
        synchronization.note_activity(start + Duration::from_millis(20));

        assert_eq!(
            synchronization.is_quiet(
                first,
                start + Duration::from_millis(100),
                Duration::from_millis(100)
            ),
            Some(false)
        );

        let second = synchronization.begin(start + Duration::from_millis(120));

        assert_eq!(
            synchronization.is_quiet(
                first,
                start + Duration::from_millis(250),
                Duration::from_millis(100)
            ),
            None
        );

        assert_eq!(
            synchronization.is_quiet(
                second,
                start + Duration::from_millis(250),
                Duration::from_millis(100)
            ),
            Some(true)
        );

        assert!(synchronization.finish(second));
        assert!(!synchronization.finish(second));
    }

    #[test]
    fn terminal_synchronization_accepts_the_prompt_reported_by_gdb() {
        let mut synchronization = TerminalSynchronization::default();
        synchronization.set_prompt("debugger ready> ");
        assert!(synchronization.is_prompt("debugger ready>  "));
        assert!(!synchronization.is_prompt("confirmation>"));
    }
}
