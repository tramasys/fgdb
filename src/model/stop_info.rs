//! Stop history is evidence, not a saved executable state.

use crate::debugger::stop_info::StopDetails;
use std::{collections::VecDeque, rc::Rc};

const MAX_STOPS: usize = 64;

#[derive(Clone, Debug)]
pub(crate) struct StopEntry {
    pub sequence: u64,
    pub epoch: u64,
    pub reason: String,
    pub inferior: Option<String>,
    pub thread: Option<String>,
    pub address: Option<String>,
    pub signal: Option<String>,
    pub meaning: Option<String>,
    pub details: StopDetails,
}

impl StopEntry {
    pub fn routine(&self) -> bool {
        self.signal.is_none()
            && matches!(
                self.reason.as_str(),
                "end-stepping-range" | "function-finished" | "location-reached"
            )
    }

    pub fn summary(&self) -> String {
        let mut text = self.reason.replace('-', " ");

        if let Some(number) = &self.details.number {
            text.push_str(&format!(" #{number}"));
        }

        if let Some(signal) = &self.signal {
            text.push_str(&format!(" · {signal}"));
        }

        if let Some(thread) = &self.thread {
            text.push_str(&format!(" · thread {thread}"));
        }

        text
    }

    pub fn description(&self) -> String {
        let mut lines = vec![format!("Stop {} · {}", self.sequence, self.summary())];

        lines.extend(
            self.fields()
                .into_iter()
                .map(|(label, value)| format!("{label}  {value}")),
        );

        lines.join("\n")
    }

    pub fn fields(&self) -> Vec<(&'static str, String)> {
        let mut fields = Vec::new();

        for (label, value) in [
            ("Inferior", self.inferior.as_deref()),
            ("Function", self.details.function.as_deref()),
            ("Instruction", self.address.as_deref()),
            ("Expression", self.details.expression.as_deref()),
            ("Previous value", self.details.old.as_deref()),
            ("Current value", self.details.new.as_deref()),
            ("Signal", self.meaning.as_deref()),
            ("Exit code", self.details.exit_code.as_deref()),
        ] {
            if let Some(value) = value {
                fields.push((label, value.to_owned()));
            }
        }

        if let Some(source) = &self.details.source {
            fields.push((
                "Source",
                format!(
                    "{source}{}",
                    self.details
                        .line
                        .map_or_else(String::new, |line| format!(":{line}"))
                ),
            ));
        }

        fields
    }
}

#[derive(Default)]
pub(crate) struct StopHistory {
    pub current: Option<Rc<StopEntry>>,
    pub entries: VecDeque<Rc<StopEntry>>,
    pub include_steps: bool,
}

impl StopHistory {
    pub fn record(&mut self, mut entry: StopEntry) {
        crate::debugger::stop_info::truncate(&mut entry.reason);

        for text in [
            &mut entry.inferior,
            &mut entry.thread,
            &mut entry.address,
            &mut entry.signal,
            &mut entry.meaning,
        ]
        .into_iter()
        .flatten()
        {
            crate::debugger::stop_info::truncate(text);
        }

        let retain = self.include_steps || !entry.routine();
        let entry = Rc::new(entry);
        self.current = Some(Rc::clone(&entry));

        if retain {
            self.entries.push_back(entry);

            if self.entries.len() > MAX_STOPS {
                self.entries.pop_front();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_is_bounded_and_steps_are_opt_in() {
        let mut history = StopHistory::default();
        let mut entry = StopEntry {
            sequence: 1,
            epoch: 1,
            reason: "end-stepping-range".into(),
            inferior: None,
            thread: None,
            address: None,
            signal: None,
            meaning: None,
            details: StopDetails::default(),
        };

        history.record(entry.clone());
        assert!(history.entries.is_empty());
        assert!(history.current.is_some());
        history.include_steps = true;

        for sequence in 0..100 {
            entry.sequence = sequence;
            history.record(entry.clone());
        }

        assert_eq!(history.entries.len(), MAX_STOPS);
        assert_eq!(history.entries.front().unwrap().sequence, 36);
        entry.signal = Some("λ".repeat(8192));
        history.record(entry);
        let current = history.current.unwrap();
        let signal = current.signal.as_ref().unwrap();
        assert!(signal.len() < 2100);
        assert!(signal.capacity() < 2100);
    }
}
