use super::*;
use crate::benchmarks::measure;
use std::hint::black_box;

#[test]
#[ignore = "manual release-mode throughput benchmark"]
fn benchmark_mi_decoding() {
    for (name, record) in [
        ("mi/small", String::from("1^done,value=\"42\"")),
        (
            "mi/memory",
            format!(
                "1^done,memory=[{{begin=\"0x1000\",offset=\"0x0\",end=\"0x11000\",contents=\"{}\"}}]",
                "0123456789abcdef".repeat(8192)
            ),
        ),
        (
            "mi/variables",
            format!(
                "1^done,variables=[{}]",
                (0..512)
                    .map(|i| format!("{{name=\"value_{i}\",value=\"42\",type=\"unsigned int\"}}"))
                    .collect::<Vec<_>>()
                    .join(",")
            ),
        ),
    ] {
        assert!(parse_record(&record).is_ok());
        measure(name, || parse_record(black_box(&record)).unwrap());
    }

    let console = format!("~\"{}\"", "source value = \\\"Hello λ\\\"\\n".repeat(2048));
    assert!(parse_any_stream_output(&console).is_ok());
    measure("mi/escaped-stream", || {
        parse_any_stream_output(black_box(&console)).unwrap()
    });
}
