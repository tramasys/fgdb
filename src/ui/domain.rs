use super::*;

#[derive(Clone, Copy, Debug)]
struct PendingTerminalSynchronization {
    generation: u64,
    last_activity: Instant,
    started: Instant,
    announced: bool,
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
            started: now,
            announced: false,
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

    pub(super) fn announce_wait(&mut self, generation: u64, now: Instant) -> bool {
        let Some(pending) = self.pending.as_mut() else {
            return false;
        };

        if pending.generation != generation
            || pending.announced
            || now.saturating_duration_since(pending.started) < Duration::from_secs(1)
        {
            return false;
        }

        pending.announced = true;
        true
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
/// remain presentation models. Command validation no longer needs to walk the
/// entire rendered tree after every child page or value update.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_wait_is_announced_once_and_superseded_by_new_input() {
        let mut synchronization = TerminalSynchronization::default();
        let now = Instant::now();
        let first = synchronization.begin(now);
        assert!(!synchronization.announce_wait(first, now));
        assert!(synchronization.announce_wait(first, now + Duration::from_secs(1)));
        assert!(!synchronization.announce_wait(first, now + Duration::from_secs(2)));
        let second = synchronization.begin(now);
        assert!(!synchronization.finish(first));
        assert!(synchronization.finish(second));
        assert!(!synchronization.announce_wait(second, now + Duration::from_secs(2)));
    }

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
            .append(&SnapshotRow::new(VariableNode::new(variable(
                "child",
                "var1.child",
            ))));

        let second = VariableNode::new(variable("second", "var2"));
        let mut index = VariableNodeIndex::default();
        index.insert(first.clone());
        index.index_store(&first.children);
        index.insert(second);
        index.replace(&first, &VariableNode::placeholder("unavailable", ""));
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
