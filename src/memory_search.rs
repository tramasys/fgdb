//! Bounded streaming search over explicitly supplied memory, independent of GTK and GDB.

mod pattern;

#[cfg(test)]
mod benchmarks;

use crate::debugger::{MemoryBlock, TargetEndian};
pub(crate) use pattern::{Pattern, SearchKind, parse_address};
use std::{collections::VecDeque, ops::Range};

pub(crate) const MAX_PATTERN: usize = 256;
pub(crate) const MAX_RESULTS: usize = 10_000;
pub(crate) const MAX_RANGES: usize = 4096;
pub(crate) const MAX_SCAN_BYTES: u64 = 4 * 1024 * 1024 * 1024;
pub(crate) const READ_BYTES: u64 = 64 * 1024;
const RETRY_BYTES: u64 = 4096;
const MAX_ISSUES: usize = 128;

pub(crate) struct Query {
    pub kind: SearchKind,
    pub value: String,
    pub ranges: Vec<Range<u64>>,
    pub aligned: bool,
    pub max_results: usize,
    pub max_bytes: u64,
}

pub(crate) enum Action {
    Start(Query),
    Cancel,
    Inspect(u64),
}

#[derive(Clone)]
pub(crate) struct Hit {
    pub address: u64,
    pub bytes: Vec<u8>,
}

pub(crate) struct Issue {
    pub range: Range<u64>,
    pub reason: String,
}

#[derive(Default)]
pub(crate) struct Progress {
    pub total: u64,
    pub searched: u64,
    pub skipped: u64,
    pub hits: Vec<Hit>,
    pub issues: Vec<Issue>,
    pub omitted_issues: u64,
}

impl Progress {
    pub(crate) fn remaining(&self) -> u64 {
        self.total - self.searched - self.skipped
    }
}

pub(crate) struct Scan {
    pattern: Pattern,
    ranges: VecDeque<Range<u64>>,
    previous_end: Option<u64>,
    alignment: u64,
    max_results: usize,
    max_bytes: u64,
    pub progress: Progress,
}

impl Scan {
    pub(crate) fn new(
        query: Query,
        pointer_bits: Option<u32>,
        endian: Option<TargetEndian>,
    ) -> Result<Self, String> {
        if !(1..=MAX_RESULTS).contains(&query.max_results)
            || !(1..=MAX_SCAN_BYTES).contains(&query.max_bytes)
            || query.ranges.is_empty()
            || query.ranges.len() > MAX_RANGES
        {
            return Err(String::from("Invalid memory search limits or range count"));
        }

        let pattern = Pattern::parse(query.kind, &query.value, pointer_bits, endian)?;

        let mut ranges = query.ranges;

        if ranges.iter().any(|range| range.start >= range.end) {
            return Err(String::from("Each range must end after its start address"));
        }

        ranges.sort_unstable_by_key(|range| range.start);
        let mut merged: Vec<Range<u64>> = Vec::with_capacity(ranges.len());

        for range in ranges {
            if let Some(last) = merged.last_mut()
                && range.start <= last.end
            {
                last.end = last.end.max(range.end);
            } else {
                merged.push(range);
            }
        }

        let total = merged.iter().map(|range| range.end - range.start).sum();
        let alignment = if query.aligned {
            pattern.width() as u64
        } else {
            1
        };

        Ok(Self {
            pattern,
            ranges: merged.into(),
            previous_end: None,
            alignment,
            max_results: query.max_results,
            max_bytes: query.max_bytes,
            progress: Progress {
                total,
                ..Progress::default()
            },
        })
    }

    pub(crate) fn limit_reached(&self) -> Option<&'static str> {
        if self.progress.hits.len() >= self.max_results {
            Some("Result limit reached")
        } else if self.progress.searched + self.progress.skipped >= self.max_bytes
            && self.progress.remaining() != 0
        {
            Some("Scan byte limit reached")
        } else {
            None
        }
    }

    pub(crate) fn next_read(&mut self) -> Option<Range<u64>> {
        if self.limit_reached().is_some() {
            return None;
        }

        let range = self.ranges.pop_front()?;
        let remaining = self.max_bytes - self.progress.searched - self.progress.skipped;
        let count = (range.end - range.start).min(READ_BYTES).min(remaining);
        let end = range.start + count;

        if end < range.end {
            self.ranges.push_front(end..range.end);
        }

        Some(range.start..end)
    }

    /// The adapter supplies sorted, non-overlapping blocks contained in the request.
    /// Missing bytes break the matcher, never becoming zero-filled search input.
    pub(crate) fn accept(&mut self, range: Range<u64>, blocks: &[MemoryBlock]) {
        let mut cursor = range.start;

        for block in blocks {
            if self.limit_reached().is_some() {
                return;
            }

            if cursor < block.begin {
                self.skip(cursor..block.begin, "Not returned by GDB");
            }

            if self.previous_end != Some(block.begin) {
                self.pattern.reset();
            }

            let alignment = self.alignment;
            let hits = &mut self.progress.hits;
            let max_results = self.max_results;

            let consumed = self.pattern.search(&block.bytes, |end, pattern| {
                let start = block.begin + end as u64 - pattern.len() as u64;

                if start.is_multiple_of(alignment) {
                    hits.push(Hit {
                        address: start,
                        bytes: pattern.matched_bytes(),
                    });

                    return hits.len() < max_results;
                }

                true
            });

            self.progress.searched += consumed as u64;

            if self.progress.hits.len() == self.max_results {
                return;
            }

            cursor = block.begin + block.bytes.len() as u64;
            self.previous_end = Some(cursor);
        }

        if cursor < range.end {
            self.skip(cursor..range.end, "Not returned by GDB");
        }
    }

    pub(crate) fn failed(&mut self, range: Range<u64>, reason: &str) {
        if range.end - range.start > RETRY_BYTES {
            // A large failed read can hide readable islands. Retry bounded pieces
            // once, without issuing a request for every individual address.
            let mut end = range.end;

            while end > range.start {
                let start = end.saturating_sub(RETRY_BYTES).max(range.start);
                self.ranges.push_front(start..end);
                end = start;
            }
        } else {
            self.skip(range, reason);
        }
    }

    fn skip(&mut self, range: Range<u64>, reason: &str) {
        self.progress.skipped += range.end - range.start;
        self.previous_end = None;
        self.pattern.reset();

        if let Some(last) = self.progress.issues.last_mut()
            && last.range.end == range.start
            && last.reason == reason
        {
            last.range.end = range.end;
        } else if self.progress.issues.len() < MAX_ISSUES {
            self.progress.issues.push(Issue {
                range,
                reason: reason.chars().take(512).collect(),
            });
        } else {
            self.progress.omitted_issues += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::debugger::TargetEndian;

    fn scan(pattern: &str, ranges: Vec<Range<u64>>, max_results: usize, max_bytes: u64) -> Scan {
        let query = Query {
            kind: SearchKind::Bytes,
            value: pattern.to_owned(),
            ranges,
            aligned: false,
            max_results,
            max_bytes,
        };

        Scan::new(query, None, None).unwrap()
    }

    #[test]
    fn accelerated_skips_preserve_alignment_and_partial_result_accounting() {
        let query = Query {
            kind: SearchKind::Unsigned(16),
            value: String::from("0x6666"),
            ranges: std::iter::once(1..1101).collect(),
            aligned: true,
            max_results: 2,
            max_bytes: 1100,
        };

        let mut scan = Scan::new(query, None, Some(TargetEndian::Little)).unwrap();
        let mut bytes = vec![0; 1100];
        bytes[95..103].fill(0x66);
        let range = scan.next_read().unwrap();
        scan.accept(range, &[MemoryBlock { begin: 1, bytes }]);
        assert_eq!(
            scan.progress
                .hits
                .iter()
                .map(|hit| hit.address)
                .collect::<Vec<_>>(),
            [96, 98]
        );

        assert!(
            scan.progress
                .hits
                .iter()
                .all(|hit| hit.bytes == [0x66, 0x66])
        );

        assert_eq!(scan.progress.searched, 99);
        assert_eq!(scan.progress.skipped, 0);
        assert_eq!(scan.progress.remaining(), 1001);
        assert_eq!(scan.limit_reached(), Some("Result limit reached"));
        assert!(scan.next_read().is_none());
    }

    #[test]
    fn streaming_matches_overlap_cross_chunks_and_never_bridge_holes() {
        let mut scan = scan("61 ?? 61", std::iter::once(0..12).collect(), 100, 100);
        scan.accept(
            0..4,
            &[MemoryBlock {
                begin: 0,
                bytes: b"aaaa".to_vec(),
            }],
        );

        scan.accept(
            4..7,
            &[MemoryBlock {
                begin: 4,
                bytes: b"aaa".to_vec(),
            }],
        );
        assert_eq!(
            scan.progress
                .hits
                .iter()
                .map(|hit| hit.address)
                .collect::<Vec<_>>(),
            [0, 1, 2, 3, 4]
        );

        scan.accept(
            7..12,
            &[MemoryBlock {
                begin: 10,
                bytes: b"aa".to_vec(),
            }],
        );
        assert_eq!(scan.progress.hits.len(), 5);
        assert_eq!(scan.progress.searched, 9);
        assert_eq!(scan.progress.skipped, 3);
        assert_eq!(scan.progress.remaining(), 0);
        assert_eq!(scan.progress.issues[0].range, 7..10);
        assert_eq!(scan.progress.hits[0].bytes, b"aaa");
    }

    #[test]
    fn range_union_limits_retry_and_address_extremes_are_bounded() {
        let mut scan = scan("ff", vec![20..30, 0..10, 5..20], 2, 8);
        assert_eq!(scan.progress.total, 30);
        let range = scan.next_read().unwrap();
        assert_eq!(range, 0..8);
        scan.accept(
            range,
            &[MemoryBlock {
                begin: 0,
                bytes: vec![0; 8],
            }],
        );
        assert!(scan.next_read().is_none());
        assert_eq!(scan.limit_reached(), Some("Scan byte limit reached"));
        assert_eq!(scan.progress.remaining(), 22);

        let mut scan = self::scan("ff", std::iter::once(0..16_384).collect(), 2, 16_384);
        let range = scan.next_read().unwrap();
        scan.failed(range, "unreadable");

        while let Some(range) = scan.next_read() {
            assert!(range.end - range.start <= RETRY_BYTES);
            scan.failed(range, "unreadable");
        }

        assert_eq!(scan.progress.skipped, 16_384);
        assert_eq!(scan.progress.issues.len(), 1);
        assert_eq!(scan.progress.remaining(), 0);

        let mut scan = self::scan(
            "ff",
            std::iter::once(u64::MAX - 4..u64::MAX).collect(),
            2,
            4,
        );

        let range = scan.next_read().unwrap();
        scan.accept(
            range.clone(),
            &[MemoryBlock {
                begin: range.start,
                bytes: vec![255; 4],
            }],
        );
        assert_eq!(scan.progress.hits.len(), 2);
        assert_eq!(scan.progress.remaining(), 2);
        assert_eq!(scan.limit_reached(), Some("Result limit reached"));
    }

    #[test]
    fn patterns_match_naive_search_across_all_machine_word_boundaries() {
        for len in [2, 63, 64, 65, 127, 128, 129, 255, 256] {
            let pattern: Vec<_> = (0..len)
                .map(|index| {
                    if index % 7 == 0 {
                        None
                    } else {
                        Some((index % 5) as u8)
                    }
                })
                .collect();

            let text = pattern
                .iter()
                .map(|byte| byte.map_or(String::from("??"), |byte| format!("{byte:02x}")))
                .collect::<Vec<_>>()
                .join(" ");

            let mut compiled = Pattern::parse(SearchKind::Bytes, &text, None, None).unwrap();
            let data: Vec<_> = (0..len * 4).map(|index| (index % len % 5) as u8).collect();
            let expected: Vec<_> = data
                .windows(len)
                .enumerate()
                .filter_map(|(index, window)| {
                    pattern
                        .iter()
                        .zip(window)
                        .all(|(expected, actual)| {
                            expected.is_none_or(|expected| expected == *actual)
                        })
                        .then_some(index)
                })
                .collect();

            let actual: Vec<_> = data
                .iter()
                .enumerate()
                .filter(|&(_, &byte)| compiled.push(byte))
                .map(|(index, _)| index + 1 - len)
                .collect();

            assert_eq!(actual, expected, "pattern length {len}");
        }
    }

    #[test]
    fn typed_patterns_use_checked_widths_target_endian_and_exact_float_bits() {
        for (kind, input, width, endian, bytes) in [
            (
                SearchKind::Unsigned(16),
                "0x1234",
                None,
                TargetEndian::Big,
                vec![0x12, 0x34],
            ),
            (
                SearchKind::Signed(16),
                "-2",
                None,
                TargetEndian::Little,
                vec![0xfe, 0xff],
            ),
            (
                SearchKind::Pointer,
                "0x12345678",
                Some(32),
                TargetEndian::Little,
                vec![0x78, 0x56, 0x34, 0x12],
            ),
            (
                SearchKind::Float(32),
                "-0.0",
                None,
                TargetEndian::Big,
                vec![0x80, 0, 0, 0],
            ),
        ] {
            let mut pattern = Pattern::parse(kind, input, width, Some(endian)).unwrap();

            for (index, &byte) in bytes.iter().enumerate() {
                assert_eq!(pattern.push(byte), index + 1 == bytes.len());
            }

            assert_eq!(pattern.matched_bytes(), bytes);
            assert_eq!(pattern.width(), bytes.len());
        }

        let query = Query {
            kind: SearchKind::Unsigned(16),
            value: String::from("0xaaaa"),
            ranges: std::iter::once(1..7).collect(),
            aligned: true,
            max_results: 100,
            max_bytes: 100,
        };

        let mut scan = Scan::new(query, None, Some(TargetEndian::Big)).unwrap();
        let range = scan.next_read().unwrap();
        scan.accept(
            range,
            &[MemoryBlock {
                begin: 1,
                bytes: vec![0xaa; 6],
            }],
        );
        assert_eq!(
            scan.progress
                .hits
                .iter()
                .map(|hit| hit.address)
                .collect::<Vec<_>>(),
            [2, 4]
        );

        assert!(
            Pattern::parse(SearchKind::Pointer, "1", None, Some(TargetEndian::Little)).is_err()
        );
        assert!(Pattern::parse(SearchKind::Unsigned(16), "1", None, None).is_err());
        assert!(Pattern::parse(SearchKind::Unsigned(8), "255", None, None).is_ok());
        assert!(
            Pattern::parse(
                SearchKind::Unsigned(8),
                "256",
                None,
                Some(TargetEndian::Little)
            )
            .is_err()
        );
        assert!(
            Pattern::parse(
                SearchKind::Signed(8),
                "-129",
                None,
                Some(TargetEndian::Little)
            )
            .is_err()
        );
        assert!(Pattern::parse(SearchKind::Bytes, "?? ??", None, None).is_err());
        assert!(Pattern::parse(SearchKind::Bytes, "0g", None, None).is_err());
        assert!(Pattern::parse(SearchKind::Text, "", None, None).is_err());
    }
}
