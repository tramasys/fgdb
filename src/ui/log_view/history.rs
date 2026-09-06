use std::{borrow::Cow, collections::VecDeque, rc::Rc};

pub(super) const MAX_LOG_ENTRIES: usize = 1_000;
pub(super) const MAX_LOG_BYTES: usize = 1024 * 1024;
const MAX_MESSAGE_BYTES: usize = 8 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LogLevel {
    Info,
    Warning,
    Error,
}

impl LogLevel {
    pub(super) const ALL: [Self; 3] = [Self::Info, Self::Warning, Self::Error];

    pub(super) fn index(self) -> usize {
        match self {
            Self::Info => 0,
            Self::Warning => 1,
            Self::Error => 2,
        }
    }

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Info => "INFO",
            Self::Warning => "WARNING",
            Self::Error => "ERROR",
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct LogFilter(u8);

impl LogFilter {
    pub(super) const ALL: Self = Self((1 << LogLevel::ALL.len()) - 1);

    pub(super) fn includes(self, level: LogLevel) -> bool {
        self.0 & (1 << level.index()) != 0
    }

    pub(super) fn with(self, level: LogLevel, enabled: bool) -> Self {
        let bit = 1 << level.index();
        Self(if enabled { self.0 | bit } else { self.0 & !bit })
    }
}

pub(super) struct LogEntry {
    pub(super) id: u64,
    pub(super) level: LogLevel,
    time: String,
    title: String,
    detail: String,
    pub(super) time_chars: i32,
    pub(super) heading_chars: i32,
    pub(super) chars: i32,
    pub(super) bytes: usize,
}

impl LogEntry {
    fn new(id: u64, level: LogLevel, time: String, title: &str, detail: &str) -> Self {
        let title = bounded_log_text(title, 512).into_owned();
        let detail = bounded_log_text(detail, MAX_MESSAGE_BYTES).into_owned();
        let time_chars = time.chars().count() as i32;
        let heading_chars = (level.label().len() + title.chars().count() + 3) as i32;
        let body_spacing = if detail.is_empty() { 1 } else { 2 };
        let chars = time_chars + heading_chars + detail.chars().count() as i32 + body_spacing;
        let bytes = time.len()
            + level.label().len()
            + title.len()
            + 3
            + detail.len()
            + body_spacing as usize;

        Self {
            id,
            level,
            time,
            title,
            detail,
            time_chars,
            heading_chars,
            chars,
            bytes,
        }
    }

    pub(super) fn append_text(&self, text: &mut String) {
        text.push_str(&self.time);
        text.push_str(self.level.label());
        text.push_str("  ");
        text.push_str(&self.title);
        text.push('\n');

        if !self.detail.is_empty() {
            text.push_str(&self.detail);
            text.push('\n');
        }

        text.push('\n');
    }
}

/// Bounded message storage, independent of GTK and the active severity filter.
#[derive(Default)]
pub(super) struct LogHistory {
    entries: VecDeque<Rc<LogEntry>>,
    counts: [usize; LogLevel::ALL.len()],
    bytes: usize,
    discarded: usize,
    next_id: u64,
}

pub(super) struct LogSnapshot {
    pub(super) entries: Vec<Rc<LogEntry>>,
    pub(super) next_id: u64,
    pub(super) summary: String,
    pub(super) retained: usize,
}

impl LogSnapshot {
    pub(super) fn retains(&self, id: u64) -> bool {
        // Wrapping distance stays unambiguous because history is bounded.
        id.wrapping_sub(self.next_id.wrapping_sub(self.retained as u64)) < self.retained as u64
    }
}

impl LogHistory {
    pub(super) fn record(&mut self, level: LogLevel, time: String, title: &str, detail: &str) {
        let entry = Rc::new(LogEntry::new(self.next_id, level, time, title, detail));
        self.next_id = self.next_id.wrapping_add(1);
        self.bytes += entry.bytes;
        self.counts[level.index()] += 1;
        self.entries.push_back(entry);

        while self.entries.len() > MAX_LOG_ENTRIES || self.bytes > MAX_LOG_BYTES {
            let Some(entry) = self.entries.pop_front() else {
                break;
            };

            self.bytes -= entry.bytes;
            self.counts[entry.level.index()] -= 1;
            self.discarded = self.discarded.saturating_add(1);
        }
    }

    pub(super) fn clear(&mut self) {
        // Keep identities distinct from entries still awaiting removal in a view.
        self.entries.clear();
        self.counts.fill(0);
        self.bytes = 0;
        self.discarded = 0;
    }

    /// Clone only the retained entries added since the previous render. None
    /// requests a complete projection when the user changes the filter.
    pub(super) fn snapshot(&self, since: Option<u64>, filter: LogFilter) -> LogSnapshot {
        let added = since.map_or(self.entries.len(), |id| {
            self.next_id.wrapping_sub(id).min(self.entries.len() as u64) as usize
        });

        let entries = self
            .entries
            .range(self.entries.len() - added..)
            .filter(|entry| filter.includes(entry.level))
            .cloned()
            .collect();

        LogSnapshot {
            entries,
            next_id: self.next_id,
            summary: self.summary(filter),
            retained: self.entries.len(),
        }
    }

    fn summary(&self, filter: LogFilter) -> String {
        let total = self.entries.len();
        let shown = LogLevel::ALL
            .iter()
            .filter(|level| filter.includes(**level))
            .map(|level| self.counts[level.index()])
            .sum::<usize>();

        let mut text = match (shown, total) {
            (_, 0) => String::from("No messages"),
            (1, 1) => String::from("1 message"),
            _ if shown == total => format!("{total} messages"),
            _ => format!("{shown} of {total} messages"),
        };

        if self.discarded > 0 {
            text.push_str(&format!("  {} older discarded", self.discarded));
        }

        text
    }
}

fn bounded_log_text(text: &str, max_bytes: usize) -> Cow<'_, str> {
    let text = text.trim();
    let end = text.floor_char_boundary(max_bytes.min(text.len()));
    let prefix = &text[..end];
    let allowed = |character: char| !character.is_control() || matches!(character, '\n' | '\t');

    if end == text.len() && prefix.chars().all(allowed) {
        return Cow::Borrowed(prefix);
    }

    let mut bounded = String::with_capacity(end + 12);
    bounded.extend(prefix.chars().filter(|character| allowed(*character)));

    if end < text.len() {
        bounded.push_str(" [truncated]");
    }

    Cow::Owned(bounded)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_history_preserves_filtered_deltas_across_eviction_and_clear() {
        let mut history = LogHistory::default();

        for _ in 0..MAX_LOG_ENTRIES {
            history.record(LogLevel::Info, String::new(), "Entry", &"é".repeat(256));
        }

        let before = history.snapshot(None, LogFilter::ALL);
        history.record(LogLevel::Error, String::new(), "Error", "é界");
        let after = history.snapshot(Some(before.next_id), LogFilter::ALL);
        assert_eq!(after.retained, MAX_LOG_ENTRIES);
        assert_eq!(after.entries.len(), 1);
        assert!(!after.retains(before.entries[0].id));
        assert!(after.retains(before.entries[1].id));
        assert_eq!(history.discarded, 1);

        let mut history = LogHistory::default();

        for _ in 0..200 {
            history.record(
                LogLevel::Warning,
                String::new(),
                "Entry",
                &"界".repeat(MAX_MESSAGE_BYTES / 3),
            );
        }

        assert!(history.bytes <= MAX_LOG_BYTES);
        assert_eq!(history.entries.len() + history.discarded, 200);
        assert_eq!(bounded_log_text("é界xyz", 4), "é [truncated]");
        assert!(matches!(
            bounded_log_text(" plain ", 100),
            Cow::Borrowed("plain")
        ));

        assert_eq!(
            bounded_log_text("<b>name</b>\0\r\nvalue", 100),
            "<b>name</b>\nvalue"
        );

        let mut history = LogHistory::default();

        for level in LogLevel::ALL {
            history.record(level, String::from("[12:34:56] "), "é\n界", "\0\r\nvalue\t");
        }

        for mask in 0..8 {
            assert_eq!(
                history.snapshot(None, LogFilter(mask)).entries.len(),
                mask.count_ones() as usize,
            );
        }

        let errors_only = LogFilter::ALL
            .with(LogLevel::Info, false)
            .with(LogLevel::Warning, false);

        assert_eq!(
            history.snapshot(None, errors_only).summary,
            "1 of 3 messages"
        );

        for entry in &history.entries {
            let mut rendered = String::new();
            entry.append_text(&mut rendered);
            assert_eq!(rendered.len(), entry.bytes);
            assert_eq!(rendered.chars().count() as i32, entry.chars);
            assert!(!rendered.contains('\0'));
        }

        let before = history.snapshot(None, errors_only);

        for _ in 3..MAX_LOG_ENTRIES {
            history.record(LogLevel::Info, String::new(), "Entry", "Hidden");
        }

        for _ in 0..3 {
            history.record(LogLevel::Error, String::new(), "Error", "New");
        }

        let after = history.snapshot(Some(before.next_id), errors_only);
        assert_eq!(after.entries.len(), 3);
        assert!(!after.retains(before.entries[0].id));
        assert_eq!(after.summary, "3 of 1000 messages  3 older discarded");
        history.clear();
        assert_eq!(history.snapshot(None, errors_only).summary, "No messages");
        assert!(
            !history
                .snapshot(None, errors_only)
                .retains(after.entries[0].id)
        );

        history.record(LogLevel::Error, String::new(), "New", "After clear");
        let cleared = history.snapshot(Some(after.next_id), errors_only);
        assert_eq!(cleared.entries.len(), 1);
        assert!(!cleared.retains(after.entries[0].id));

        // A view that was hidden beyond the retention window gets the current
        // bounded history, not an unbounded backlog of already evicted entries.
        let mut history = LogHistory {
            next_id: u64::MAX - 1,
            ..LogHistory::default()
        };

        let cursor = history.next_id;

        for _ in 0..MAX_LOG_ENTRIES * 2 {
            history.record(LogLevel::Info, String::new(), "Entry", "Wrapped identity");
        }

        let snapshot = history.snapshot(Some(cursor), LogFilter::ALL);
        assert_eq!(snapshot.entries.len(), MAX_LOG_ENTRIES);
        assert!(
            snapshot
                .entries
                .iter()
                .all(|entry| snapshot.retains(entry.id))
        );

        assert!(!snapshot.retains(cursor));
        assert!(!snapshot.retains(snapshot.next_id));
        assert_eq!(history.counts.iter().sum::<usize>(), MAX_LOG_ENTRIES);
    }
}
