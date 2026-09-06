//! Counts-only syscall observation. No debugger commands or target-memory reads.

mod catalog;
mod collector;

pub(crate) use catalog::{Abi, Metadata, catalog};
pub(crate) use collector::{Collector, Counts, Phase, Target};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct Key {
    pub(crate) abi: Abi,
    pub(crate) number: u64,
}

impl Key {
    pub(crate) fn number_text(self) -> String {
        (self.number as i64).to_string()
    }
}
