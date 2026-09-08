use super::*;
use crate::benchmarks::{measure, random_bytes};
use std::hint::black_box;

#[test]
#[ignore = "manual release-mode throughput benchmark"]
fn benchmark_memory_scan() {
    const SIZE: usize = 8 * 1024 * 1024;
    let random = random_bytes(SIZE);
    let zeroes = vec![0; SIZE];

    for (name, kind, value, bytes, aligned) in [
        (
            "memory/text/random",
            SearchKind::Text,
            "fgdb-needle-42",
            &random,
            false,
        ),
        (
            "memory/text/zeroes",
            SearchKind::Text,
            "fgdb-needle-42",
            &zeroes,
            false,
        ),
        (
            "memory/pointer/random",
            SearchKind::Pointer,
            "0x1234567890",
            &random,
            true,
        ),
        (
            "memory/pointer/dense",
            SearchKind::Pointer,
            "0",
            &zeroes,
            true,
        ),
        (
            "memory/wildcard/random",
            SearchKind::Bytes,
            "66 ?? 64 62 ?? 34 32",
            &random,
            false,
        ),
    ] {
        let blocks = bytes
            .chunks(READ_BYTES as usize)
            .enumerate()
            .map(|(index, bytes)| MemoryBlock {
                begin: index as u64 * READ_BYTES,
                bytes: bytes.to_vec(),
            })
            .collect::<Vec<_>>();

        measure(name, || {
            let query = Query {
                kind,
                value: value.to_owned(),
                ranges: std::iter::once(0..SIZE as u64).collect(),
                aligned,
                max_results: MAX_RESULTS,
                max_bytes: SIZE as u64,
            };

            let mut scan = Scan::new(query, Some(64), Some(TargetEndian::Little)).unwrap();

            for block in black_box(&blocks) {
                if scan.limit_reached().is_some() {
                    break;
                }

                scan.accept(
                    block.begin..block.begin + block.bytes.len() as u64,
                    std::slice::from_ref(block),
                );
            }

            (scan.progress.searched, scan.progress.hits)
        });
    }
}
