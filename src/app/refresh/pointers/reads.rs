//! One bounded enrichment batch shares reads, including failures and in-flight
//! requests. Nothing survives a new batch or debugger stop.

use std::collections::{HashMap, hash_map::Entry};

#[derive(Clone, Copy)]
pub(in crate::app::refresh) struct Position {
    pub index: usize,
    pub depth: usize,
}

#[derive(Default)]
pub(in crate::app::refresh) struct Reads(HashMap<u64, Read>);

enum Read {
    Pending(Vec<Position>),
    Ready(Option<String>),
}

pub(in crate::app::refresh) enum Lookup {
    Start,
    Pending,
    Ready(Option<String>),
}

impl Reads {
    pub(in crate::app::refresh) fn request(&mut self, address: u64, position: Position) -> Lookup {
        match self.0.entry(address) {
            Entry::Vacant(entry) => {
                entry.insert(Read::Pending(vec![position]));
                Lookup::Start
            }
            Entry::Occupied(mut entry) => match entry.get_mut() {
                Read::Pending(waiters) => {
                    waiters.push(position);
                    Lookup::Pending
                }
                Read::Ready(value) => Lookup::Ready(value.clone()),
            },
        }
    }

    pub(in crate::app::refresh) fn complete(
        &mut self,
        address: u64,
        value: Option<String>,
    ) -> Vec<Position> {
        match self.0.insert(address, Read::Ready(value)) {
            Some(Read::Pending(waiters)) => waiters,
            _ => Vec::new(),
        }
    }

    #[cfg(test)]
    pub(in crate::app::refresh) fn len(&self) -> usize {
        self.0.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shares_in_flight_and_completed_reads_including_failures() {
        for value in [None, Some(String::from("0x1234 <symbol>"))] {
            let mut reads = Reads::default();
            let first = Position { index: 0, depth: 1 };
            let second = Position { index: 3, depth: 2 };
            assert!(matches!(reads.request(0x1000, first), Lookup::Start));
            assert!(matches!(reads.request(0x1000, second), Lookup::Pending));
            let waiters = reads.complete(0x1000, value.clone());
            assert_eq!(waiters.len(), 2);
            assert_eq!((waiters[1].index, waiters[1].depth), (3, 2));
            assert!(
                matches!(reads.request(0x1000, first), Lookup::Ready(cached) if cached == value)
            );

            assert!(matches!(reads.request(0x2000, first), Lookup::Start));
            assert_eq!(reads.len(), 2);
            assert!(matches!(
                Reads::default().request(0x1000, first),
                Lookup::Start
            ));
        }
    }
}
