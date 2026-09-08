use std::fmt::Write as _;

use super::HeapBackend;
use crate::misc::{HeapInspectionRow, HeapInspectionSnapshot};

const MAX_OUTPUT_BYTES: usize = 512 * 1024;
const MAX_ROWS: usize = 1024;
const MAX_CELL_BYTES: usize = 2048;

/// Scripts only read typed metadata. They never call an inferior function or
/// invoke a pretty printer, and never infer offsets for an unknown layout.
pub(crate) fn allocator_inspection_script(backend: HeapBackend) -> Option<String> {
    let adapter = match backend {
        HeapBackend::Glibc => return None,
        HeapBackend::Musl => include_str!("musl.py"),
        HeapBackend::Jemalloc => include_str!("jemalloc.py"),
        HeapBackend::Tcmalloc => include_str!("tcmalloc.py"),
        HeapBackend::Mimalloc => include_str!("mimalloc.py"),
    };

    let common = include_str!("reader.py");
    let mut script = String::with_capacity(common.len() + adapter.len() + 64);
    script.push_str(common);
    script.push_str(adapter);
    let _ = writeln!(script, "\nrun_inspector({:?}, inspect)", backend.key());
    Some(script)
}

pub(crate) fn parse_allocator_inspection(
    backend: HeapBackend,
    output: &str,
) -> Result<HeapInspectionSnapshot, String> {
    if output.len() > MAX_OUTPUT_BYTES {
        return Err(String::from(
            "Allocator metadata exceeded the output budget",
        ));
    }

    let expected_header = format!("FGDB_HEAP\t1\t{}", backend.key());
    let mut lines = output.lines().skip_while(|line| *line != expected_header);

    if lines.next().is_none() {
        return Err(String::from(
            "GDB did not return a supported allocator snapshot",
        ));
    }

    let mut snapshot = HeapInspectionSnapshot {
        command: format!("{} state", backend.name()),
        ..Default::default()
    };

    for line in lines {
        let mut cells = line.split('\t');

        match cells.next() {
            Some("R") if snapshot.rows.len() < MAX_ROWS => {
                let row = HeapInspectionRow {
                    kind: decode_cell(cells.next())?,
                    location: decode_cell(cells.next())?,
                    metric: decode_cell(cells.next())?,
                    state: decode_cell(cells.next())?,
                    details: decode_cell(cells.next())?,
                    inspect_address: None,
                };

                if cells.next().is_some() {
                    break;
                }

                snapshot.rows.push(row);
            }
            Some("E") => {
                let status = cells.next();
                let truncated = cells.next();
                let count = cells.next().and_then(|value| value.parse::<usize>().ok());
                let summary = decode_cell(cells.next())?;

                if !matches!(status, Some("ok" | "partial" | "unavailable"))
                    || !matches!(truncated, Some("0" | "1"))
                    || count != Some(snapshot.rows.len())
                    || cells.next().is_some()
                {
                    break;
                }

                snapshot.truncated = truncated == Some("1") || status == Some("partial");
                snapshot.summary = summary;

                if status == Some("unavailable") {
                    snapshot.diagnostic = Some(snapshot.summary.clone());
                }

                return Ok(snapshot);
            }
            _ => break,
        }
    }

    Err(String::from(
        "Allocator metadata was incomplete or malformed. The previous snapshot was retained",
    ))
}

fn decode_cell(value: Option<&str>) -> Result<String, String> {
    let invalid = || String::from("Invalid allocator metadata field");
    let value = value.ok_or_else(invalid)?;

    if value.len() > MAX_CELL_BYTES * 2 || !value.len().is_multiple_of(2) {
        return Err(invalid());
    }

    let mut decoded = Vec::with_capacity(value.len() / 2);

    for pair in value.as_bytes().as_chunks::<2>().0 {
        let high = (pair[0] as char).to_digit(16).ok_or_else(invalid)?;
        let low = (pair[1] as char).to_digit(16).ok_or_else(invalid)?;
        decoded.push((high * 16 + low) as u8);
    }

    let value = String::from_utf8(decoded).map_err(|_| invalid())?;

    if value.chars().any(|character| character.is_control()) {
        return Err(invalid());
    }

    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requires_matching_backend_and_complete_bounded_metadata() {
        let valid = "FGDB_HEAP\t1\tjemalloc\nR\t4172656e61\t30\t31\t\t\nE\tok\t0\t1\t4f4b\n";
        let snapshot = parse_allocator_inspection(HeapBackend::Jemalloc, valid).unwrap();
        assert_eq!(snapshot.rows[0].kind, "Arena");
        assert_eq!(snapshot.rows[0].inspect_address, None);
        assert!(parse_allocator_inspection(HeapBackend::Musl, valid).is_err());

        for invalid in [
            valid.replace("E\tok", "E\tunknown"),
            valid.replace("\t0\t1\t", "\t0\t2\t"),
            valid.replace("4f4b", "0a"),
            valid.replace("4f4b", "z0"),
            valid.replace("4f4b", &"61".repeat(MAX_CELL_BYTES + 1)),
            valid.lines().take(2).collect::<Vec<_>>().join("\n"),
        ] {
            assert!(parse_allocator_inspection(HeapBackend::Jemalloc, &invalid).is_err());
        }
    }
}
