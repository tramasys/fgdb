use std::fmt::Write as _;

use super::{ALLOCATOR_PROBE_SPECS, AllocatorProbe, AllocatorProbeSymbol};

pub(crate) fn allocator_probe_script() -> String {
    let mut script = String::from("probes = [\n");

    for spec in ALLOCATOR_PROBE_SPECS {
        let _ = writeln!(script, "    {:?},", format!("(void *) {}", spec.expression));
    }

    script.push_str("]\n");
    script.push_str(include_str!("probe.py"));
    script
}

pub(crate) fn parse_allocator_probe(output: &str) -> Option<AllocatorProbe> {
    if output.len() > 32 * 1024 {
        return None;
    }

    let mut lines = output
        .lines()
        .skip_while(|line| *line != "FGDB_ALLOCATORS\t1");
    lines.next()?;
    let mut symbols = Vec::new();
    let mut seen = vec![false; ALLOCATOR_PROBE_SPECS.len()];

    for line in lines {
        let mut fields = line.split('\t');

        match fields.next()? {
            "S" => {
                let index = fields.next()?.parse::<usize>().ok()?;
                let spec = ALLOCATOR_PROBE_SPECS.get(index)?;
                let address = u64::from_str_radix(fields.next()?, 16).ok()?;

                let indirect = match fields.next()? {
                    "0" => false,
                    "1" => true,
                    _ => return None,
                };

                if fields.next().is_some() || seen[index] || address == 0 {
                    return None;
                }

                seen[index] = true;

                symbols.push(AllocatorProbeSymbol {
                    name: spec.name.to_owned(),
                    address,
                    indirect,
                });
            }
            "E" => {
                let attempted = fields.next()?.parse::<usize>().ok()?;

                if attempted > ALLOCATOR_PROBE_SPECS.len()
                    || seen.get(attempted..)?.iter().any(|seen| *seen)
                    || fields.next().is_some()
                {
                    return None;
                }

                return Some(AllocatorProbe {
                    complete: true,
                    dispatch_failures: ALLOCATOR_PROBE_SPECS.len() - attempted,
                    symbols,
                });
            }
            _ => return None,
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_incomplete_duplicate_and_invalid_probe_records() {
        let complete = format!(
            "FGDB_ALLOCATORS\t1\nS\t0\t1234\t0\nE\t{}\n",
            ALLOCATOR_PROBE_SPECS.len()
        );
        let probe = parse_allocator_probe(&complete).unwrap();
        assert_eq!(probe.symbols[0].name, "malloc");
        assert_eq!(probe.symbols[0].address, 0x1234);
        assert!(parse_allocator_probe("FGDB_ALLOCATORS\t1\nS\t0\t1234\t0\n").is_none());
        assert!(parse_allocator_probe(&complete.replace("1234", "0")).is_none());
        assert!(parse_allocator_probe(&complete.replace("S\t0", "S\t9999")).is_none());
        assert!(parse_allocator_probe(&complete.replace("E\t", "S\t0\t1234\t0\nE\t")).is_none());
    }
}
