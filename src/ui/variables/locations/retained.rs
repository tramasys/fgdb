//! Last-rendered text, never an authority for location actions or GDB queries.

use super::*;
use crate::debugger::StopContext;

#[derive(Hash, PartialEq, Eq)]
struct Identity {
    varobj: Option<String>,
    index: Option<usize>,
    name: String,
    argument: bool,
    type_name: Option<String>,
}

impl From<&Variable> for Identity {
    fn from(variable: &Variable) -> Self {
        Self {
            varobj: variable.varobj.clone(),
            index: variable.local_index,
            name: variable.name.clone(),
            argument: variable.argument,
            type_name: variable.type_name.clone(),
        }
    }
}

#[derive(Default)]
pub(super) struct Retained {
    context: Option<StopContext>,
    revision: u64,
    entries: HashMap<Identity, presentation::Presentation>,
}

impl Retained {
    pub(super) fn context(&mut self, context: Option<StopContext>, revision: u64) {
        let same_scope = match (&self.context, &context) {
            (Some(old), Some(new)) => {
                old.transport_epoch() == new.transport_epoch()
                    && old.inferior_id() == new.inferior_id()
                    && old.thread_id() == new.thread_id()
                    && old.frame_level() == new.frame_level()
            }
            _ => true,
        };

        if !same_scope || self.revision != revision {
            self.entries.clear();
        }

        self.revision = revision;
        if context.is_some() {
            self.context = context;
        }
    }

    pub(super) fn remember(&mut self, variable: &Variable, display: &presentation::Presentation) {
        let key = Identity::from(variable);
        if let Some(previous) = self.entries.get_mut(&key) {
            previous.clone_from(display);
        } else if self.entries.len() < CACHE_LIMIT {
            self.entries.insert(key, display.clone());
        }
    }

    pub(super) fn prune(&mut self, visible: &HashSet<Variable>) {
        if self.entries.len() >= CACHE_LIMIT && !visible.is_empty() {
            let visible: HashSet<_> = visible.iter().map(Identity::from).collect();
            self.entries.retain(|key, _| visible.contains(key));
        }
    }

    pub(super) fn get(&self, variable: &Variable) -> Option<presentation::Presentation> {
        let mut display = self.entries.get(&Identity::from(variable))?.clone();
        display.tooltip = format!(
            "Previous location, awaiting a current paused value. Location actions are unavailable until verified\n{}",
            display.tooltip
        );

        Some(display)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retention_ignores_value_changes_but_not_identity_or_context_changes() {
        let mut retained = Retained::default();
        let context = |generation, thread, frame| {
            StopContext::new(1, generation, Some("i1".into()), thread, frame)
        };
        retained.context(context(1, "1".into(), 0), 0);
        let mut variable = super::super::tests::variable(0);
        let display = presentation::format(
            Some(&ValueLocation::Memory {
                address: 0x1000,
                referenced: false,
            }),
            true,
            64,
            None,
        );
        retained.remember(&variable, &display);
        retained.context(None, 0);
        assert_eq!(retained.get(&variable).unwrap().text, display.text);
        retained.context(context(2, "1".into(), 0), 0);
        variable.value = "changed".into();
        assert_eq!(retained.get(&variable).unwrap().text, display.text);
        assert!(
            retained
                .get(&variable)
                .unwrap()
                .tooltip
                .contains("Previous location")
        );
        variable.varobj = Some("new root".into());
        assert!(retained.get(&variable).is_none());
        variable.varobj = None;
        retained.context(context(3, "2".into(), 0), 0);
        assert!(retained.get(&variable).is_none());
        retained.remember(&variable, &display);
        retained.context(context(4, "2".into(), 1), 0);
        assert!(retained.get(&variable).is_none());
        retained.remember(&variable, &display);
        retained.context(context(5, "2".into(), 1), 1);
        assert!(retained.get(&variable).is_none());
    }
}
