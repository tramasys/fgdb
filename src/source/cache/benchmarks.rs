use super::*;
use crate::benchmarks::measure;
use std::hint::black_box;

#[test]
#[ignore = "manual release-mode throughput benchmark"]
fn benchmark_source_index() {
    for (name, contents) in [
        (
            "source/index/typical",
            include_str!("../../../examples/cpp_variable_viewer_target.cpp").repeat(64),
        ),
        (
            "source/index/long-lines",
            format!("{}\r\n", "x".repeat(4096)).repeat(1024),
        ),
        ("source/index/short-lines", "x\n".repeat(512 * 1024)),
    ] {
        assert!(source_line_ranges(&contents).is_some());
        measure(name, || source_line_ranges(black_box(&contents)).unwrap());
    }
}
