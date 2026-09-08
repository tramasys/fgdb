use super::*;
use crate::benchmarks::random_bytes;

fn compile(bytes: &[Option<u8>]) -> Pattern {
    let text = bytes
        .iter()
        .map(|byte| byte.map_or_else(|| String::from("??"), |byte| format!("{byte:02x}")))
        .collect::<Vec<_>>()
        .join(" ");

    Pattern::parse(SearchKind::Bytes, &text, None, None).unwrap()
}

#[test]
fn prefiltered_streams_match_naive_search_at_every_word_and_chunk_boundary() {
    for len in [1, 2, 3, 15, 16, 31, 32, 63, 64, 65, 127, 128, 129, 255, 256] {
        for wildcard in [None, Some(0), Some(3)] {
            let expected_pattern = random_bytes(len)
                .into_iter()
                .enumerate()
                .map(|(index, byte)| {
                    if len > 1 && wildcard.is_some_and(|phase| index % 7 == phase) {
                        None
                    } else {
                        Some(byte)
                    }
                })
                .collect::<Vec<_>>();

            let mut data = vec![0; 4096];

            for start in [31, 255, 997, 2039, 3500] {
                for (index, byte) in expected_pattern.iter().enumerate() {
                    data[start + index] = byte.unwrap_or(0x5a);
                }
            }

            let expected = data
                .windows(len)
                .enumerate()
                .filter(|(_, bytes)| {
                    expected_pattern
                        .iter()
                        .zip(*bytes)
                        .all(|(expected, actual)| {
                            expected.is_none_or(|expected| expected == *actual)
                        })
                })
                .map(|(start, bytes)| (start + len, bytes.to_vec()))
                .collect::<Vec<_>>();

            for chunk_size in [1, 2, 15, 16, 31, 32, 63, 64, 255, 256, 257, 4096] {
                let mut pattern = compile(&expected_pattern);
                let mut actual = Vec::new();

                for (index, chunk) in data.chunks(chunk_size).enumerate() {
                    let consumed = pattern.search(chunk, |end, pattern| {
                        actual.push((index * chunk_size + end, pattern.matched_bytes()));
                        true
                    });

                    assert_eq!(consumed, chunk.len());
                    assert_eq!(pattern.search(&[], |_, _| panic!("empty input matched")), 0);
                }

                assert_eq!(
                    actual, expected,
                    "length {len}, wildcard {wildcard:?}, chunk {chunk_size}"
                );
            }
        }
    }
}

#[test]
fn dense_random_and_sparse_inputs_agree_with_the_scalar_matcher() {
    for bytes in [
        vec![Some(0); 8],
        vec![Some(b'a'); 65],
        vec![Some(0x1f), None, Some(0x1f)],
    ] {
        for data in [vec![0; 8192], vec![b'a'; 8192], random_bytes(8192)] {
            let mut scalar = compile(&bytes);

            let mut expected = Vec::new();

            for (index, byte) in data.iter().enumerate() {
                if scalar.push(*byte) {
                    expected.push((index + 1, scalar.matched_bytes()));
                }
            }

            let mut filtered = compile(&bytes);
            let mut actual = Vec::new();

            filtered.search(&data, |end, pattern| {
                actual.push((end, pattern.matched_bytes()));
                true
            });

            assert_eq!(actual, expected);
        }
    }
}

#[test]
fn early_stop_can_resume_overlapping_matches_and_reset_discards_partial_state() {
    let mut pattern = compile(&[Some(b'a'), Some(b'b'), Some(b'a')]);
    let mut input = vec![0; 93];
    input.extend_from_slice(b"abababa");
    let mut consumed = 0;
    let mut matches = Vec::new();

    while consumed < input.len() {
        let count = pattern.search(&input[consumed..], |end, pattern| {
            assert_eq!(pattern.matched_bytes(), b"aba");
            matches.push(consumed + end);
            false
        });

        assert!(count > 0);
        consumed += count;
    }

    assert_eq!(matches, [96, 98, 100]);
    pattern.reset();
    pattern.search(b"ab", |_, _| panic!("incomplete match"));
    pattern.reset();
    pattern.search(b"a", |_, _| panic!("match bridged a reset"));
}

#[test]
fn dispatched_and_available_vector_backends_match_portable_byte_search() {
    let storage = random_bytes(2048);

    for offset in 0..64 {
        for len in (0..80).chain([127, 128, 255, 256, 1024]) {
            let bytes = &storage[offset..offset + len];
            let one = bytes.iter().position(|byte| *byte == b'\n');
            let count = bytes.iter().filter(|byte| **byte == b'\n').count();
            let two = bytes.iter().position(|byte| matches!(byte, b'"' | b'\\'));

            assert_eq!(memchr::memchr(b'\n', bytes), one);
            assert_eq!(memchr::memchr_iter(b'\n', bytes).count(), count);
            assert_eq!(memchr::memchr2(b'"', b'\\', bytes), two);
            assert_eq!(memchr::arch::all::memchr::One::new(b'\n').find(bytes), one);
            assert_eq!(
                memchr::arch::all::memchr::One::new(b'\n').count(bytes),
                count
            );

            assert_eq!(
                memchr::arch::all::memchr::Two::new(b'"', b'\\').find(bytes),
                two
            );

            #[cfg(target_arch = "x86_64")]
            {
                use memchr::arch::x86_64::{avx2, sse2};

                if let Some(searcher) = sse2::memchr::One::new(b'\n') {
                    assert_eq!(searcher.find(bytes), one);
                    assert_eq!(searcher.count(bytes), count);
                }

                if let Some(searcher) = avx2::memchr::One::new(b'\n') {
                    assert_eq!(searcher.find(bytes), one);
                    assert_eq!(searcher.count(bytes), count);
                }

                if let Some(searcher) = sse2::memchr::Two::new(b'"', b'\\') {
                    assert_eq!(searcher.find(bytes), two);
                }

                if let Some(searcher) = avx2::memchr::Two::new(b'"', b'\\') {
                    assert_eq!(searcher.find(bytes), two);
                }
            }
        }
    }
}
