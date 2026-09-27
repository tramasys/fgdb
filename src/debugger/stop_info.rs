//! Bounded stop-event evidence, without additional debugger requests.

use super::mi::{MiRecord, MiResult, MiValue, result_field};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct StopDetails {
    pub number: Option<String>,
    pub expression: Option<String>,
    pub old: Option<String>,
    pub new: Option<String>,
    pub function: Option<String>,
    pub source: Option<String>,
    pub line: Option<u32>,
    pub exit_code: Option<String>,
}

pub(crate) fn bounded(text: &str) -> String {
    let mut result = text[..text.floor_char_boundary(2048)].to_owned();

    if text.len() > result.len() {
        result.push_str("… [truncated]");
    }

    result
}

pub(crate) fn truncate(text: &mut String) {
    const LIMIT: usize = 2048;

    if text.len() > LIMIT {
        text.truncate(text.floor_char_boundary(LIMIT));
        text.push_str("… [truncated]");
        text.shrink_to_fit();
    }
}

impl StopDetails {
    pub(super) fn from_record(record: &MiRecord) -> Self {
        let field = |name| record.field(name).and_then(MiValue::as_const).map(bounded);
        let tuple = |name| record.field(name).and_then(MiValue::as_tuple);
        let watch = tuple("wpt")
            .or_else(|| tuple("hw-rwpt"))
            .or_else(|| tuple("hw-awpt"));
        let value = tuple("value");
        let frame = tuple("frame");
        let text = |fields: Option<&[MiResult]>, name| {
            fields
                .and_then(|fields| result_field(fields, name))
                .and_then(MiValue::as_const)
                .map(bounded)
        };

        Self {
            number: field("bkptno").or_else(|| text(watch, "number")),
            expression: text(watch, "exp"),
            old: text(value, "old"),
            new: text(value, "new").or_else(|| text(value, "value")),
            function: text(frame, "func"),
            source: text(frame, "fullname").or_else(|| text(frame, "file")),
            line: text(frame, "line")
                .and_then(|line| line.parse().ok())
                .filter(|line| *line > 0),
            exit_code: field("exit-code"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "manual release-mode stop evidence benchmark"]
    fn benchmark_stop_evidence() {
        let record = super::super::mi::parse_record(r#"*stopped,reason="watchpoint-trigger",wpt={number="2",exp="counter"},value={old="3",new="4"},frame={func="main",fullname="/tmp/main.c",line="12"}"#).unwrap();

        crate::benchmarks::measure("stop/evidence", || {
            StopDetails::from_record(std::hint::black_box(&record))
        });
    }

    #[test]
    fn preserves_watchpoint_evidence_and_handles_missing_values() {
        let record = super::super::mi::parse_record(r#"*stopped,reason="watchpoint-trigger",wpt={number="2",exp="counter"},value={old="3",new="4"},frame={func="main",fullname="/tmp/main.c",line="12"}"#).unwrap();
        let details = StopDetails::from_record(&record);
        assert_eq!(details.number.as_deref(), Some("2"));
        assert_eq!(details.old.as_deref(), Some("3"));
        assert_eq!(details.new.as_deref(), Some("4"));
        assert_eq!(details.line, Some(12));

        let record = super::super::mi::parse_record(
            r#"*stopped,hw-rwpt={number="3",exp="counter"},value={value="4"}"#,
        )
        .unwrap();
        let details = StopDetails::from_record(&record);
        assert_eq!(details.number.as_deref(), Some("3"));
        assert_eq!(details.old, None);
        assert_eq!(details.new.as_deref(), Some("4"));
        assert!(bounded(&"λ".repeat(4096)).len() < 2100);
    }
}
