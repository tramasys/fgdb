//! Owned comparison snapshots. Incomplete cells never establish equality.

use std::collections::BTreeMap;

pub(crate) const MAX_ROWS: usize = 256;
pub(crate) const MAX_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CapturedValue {
    pub text: String,
    pub complete: bool,
}

impl From<String> for CapturedValue {
    fn from(text: String) -> Self {
        Self {
            text,
            complete: true,
        }
    }
}

impl From<&str> for CapturedValue {
    fn from(text: &str) -> Self {
        text.to_owned().into()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ValueSnapshot {
    pub values: BTreeMap<String, CapturedValue>,
    pub complete: bool,
}

pub(crate) type SnapshotReply = Box<dyn FnOnce(Result<ValueSnapshot, String>)>;
