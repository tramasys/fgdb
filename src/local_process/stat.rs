//! Borrowed Linux process-stat parsing shared by discovery and target inspection.
//!
//! The command name is arbitrary bytes and can contain parentheses or newlines.
//! Only the numeric tail is decoded as text. Parsing does not allocate.

use std::str::SplitWhitespace;

pub(crate) const MAX_STAT_BYTES: usize = 16 * 1024;

pub(crate) struct Stat<'a> {
    pub pid: u32,
    pub name: &'a [u8],
    pub state: u8,
    tail: &'a str,
}

impl<'a> Stat<'a> {
    pub(crate) fn parse(bytes: &'a [u8]) -> Option<Self> {
        let open = bytes.iter().position(|byte| *byte == b'(')?;
        let close = bytes.iter().rposition(|byte| *byte == b')')?;

        if close <= open {
            return None;
        }

        let pid = std::str::from_utf8(&bytes[..open])
            .ok()?
            .trim()
            .parse()
            .ok()?;

        let tail = std::str::from_utf8(&bytes[close + 1..]).ok()?;
        let state = tail.split_whitespace().next()?.as_bytes();

        if state.len() != 1 || pid == 0 || pid > i32::MAX as u32 {
            return None;
        }

        Some(Self {
            pid,
            name: &bytes[open + 1..close],
            state: state[0],
            tail,
        })
    }

    pub(crate) fn fields(&self) -> SplitWhitespace<'a> {
        self.tail.split_whitespace()
    }

    pub(crate) fn start_time(&self) -> Option<u64> {
        self.fields().nth(19)?.parse().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_need_not_be_utf8_or_balanced_parentheses() {
        let bytes = b"42 (a )\xff\n(name)) t 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 12345 0";
        let stat = Stat::parse(bytes).unwrap();
        assert_eq!(stat.pid, 42);
        assert_eq!(stat.name, b"a )\xff\n(name)");
        assert_eq!(stat.state, b't');
        assert_eq!(stat.start_time(), Some(12345));
    }

    #[test]
    fn malformed_headers_and_missing_start_times_are_rejected() {
        for bytes in [
            b"(name) S".as_slice(),
            b"0 (name) S",
            b"42 )name( S",
            b"42 (name) SS",
        ] {
            assert!(Stat::parse(bytes).is_none());
        }

        assert_eq!(Stat::parse(b"42 (name) S 1 2").unwrap().start_time(), None);
    }
}
