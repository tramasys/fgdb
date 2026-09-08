//! Opt-in release benchmarks. Keep input generation outside the timed closures.
//! Run with --ignored --nocapture --test-threads=1 and filter on benchmark_.

use std::{
    hint::black_box,
    time::{Duration, Instant},
};

#[expect(
    clippy::assertions_on_constants,
    reason = "ignored benchmarks must compile in debug mode but refuse debug timing"
)]
pub(crate) fn measure<T>(name: &str, mut run: impl FnMut() -> T) {
    assert!(
        !cfg!(debug_assertions),
        "run throughput benchmarks with --release"
    );

    let warmup = Instant::now();
    let mut warmup_iterations = 0_u64;

    while warmup.elapsed() < Duration::from_millis(50) {
        black_box(run());
        warmup_iterations += 1;
    }

    let estimate = warmup.elapsed().as_nanos() / u128::from(warmup_iterations);
    let batch = (100_000 / estimate.max(1)).clamp(1, 1024) as u64;
    let mut samples = [0.0_f64; 7];

    for sample in &mut samples {
        let started = Instant::now();
        let mut iterations = 0_u64;

        loop {
            for _ in 0..batch {
                black_box(run());
            }

            iterations += batch;

            if started.elapsed() >= Duration::from_millis(100) {
                break;
            }
        }

        *sample = started.elapsed().as_secs_f64() * 1e9 / iterations as f64;
    }

    samples.sort_unstable_by(f64::total_cmp);
    println!(
        "BENCH {name}: {:.1} ns/op (range {:.1}..{:.1})",
        samples[3], samples[0], samples[6]
    );
}

pub(crate) fn random_bytes(len: usize) -> Vec<u8> {
    let mut state = 0x9e3779b97f4a7c15_u64;

    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        })
        .collect()
}
