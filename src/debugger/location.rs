//! Storage facts, independent of value formatting and language-specific printers.

pub(crate) const BATCH_LIMIT: usize = 16;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ValueLocation {
    Memory { address: u64, referenced: bool },
    NonAddressable,
    OptimizedOut,
    Unavailable,
    Unknown(String),
}

impl ValueLocation {
    pub(crate) fn address(&self) -> Option<u64> {
        match self {
            Self::Memory { address, .. } => Some(*address),
            _ => None,
        }
    }
}

/// None is cancellation, not a negative result to cache at the next stop.
pub(crate) type LocationReply = Box<dyn FnOnce(Option<Vec<ValueLocation>>)>;

pub(crate) fn parse_locations(output: &str, count: usize) -> Option<Vec<ValueLocation>> {
    if count > BATCH_LIMIT || output.len() > 32 * 1024 {
        return None;
    }

    let mut locations = Vec::with_capacity(count);

    for line in output
        .lines()
        .filter_map(|line| line.strip_prefix("FGDB_LOCATION:"))
    {
        let mut fields = line.split('\t');

        if fields.next()? != "1" || fields.next()?.parse::<usize>().ok()? != locations.len() {
            return None;
        }

        let kind = fields.next()?;
        let address = fields.next()?;
        let detail = fields.next()?;

        if fields.next().is_some() || locations.len() >= count || detail.len() > 2048 {
            return None;
        }

        let location = match kind {
            "memory" | "reference" if detail.is_empty() => {
                let digits = address.strip_prefix("0x")?;

                if digits.is_empty()
                    || digits.len() > 16
                    || !digits.bytes().all(|byte| byte.is_ascii_hexdigit())
                {
                    return None;
                }

                ValueLocation::Memory {
                    address: u64::from_str_radix(digits, 16).ok()?,
                    referenced: kind == "reference",
                }
            }
            "no-address" if address.is_empty() && detail.is_empty() => {
                ValueLocation::NonAddressable
            }
            "optimized" if address.is_empty() && detail.is_empty() => ValueLocation::OptimizedOut,
            "unavailable" if address.is_empty() && detail.is_empty() => ValueLocation::Unavailable,
            "unknown" if address.is_empty() => {
                let (bytes, remainder) = detail.as_bytes().as_chunks::<2>();

                if !remainder.is_empty() {
                    return None;
                }

                let decoded = bytes
                    .iter()
                    .map(|pair| {
                        let high = (pair[0] as char).to_digit(16)?;
                        let low = (pair[1] as char).to_digit(16)?;
                        Some((high * 16 + low) as u8)
                    })
                    .collect::<Option<Vec<_>>>()?;

                ValueLocation::Unknown(String::from_utf8(decoded).ok()?)
            }
            _ => return None,
        };

        locations.push(location);
    }

    (locations.len() == count).then_some(locations)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_explicit_storage_states_and_rejects_incomplete_or_ambiguous_records() {
        let output = "noise\nFGDB_LOCATION:1\t0\tmemory\t0x0\t\nFGDB_LOCATION:1\t1\treference\t0x1234\t\nFGDB_LOCATION:1\t2\tno-address\t\t\nFGDB_LOCATION:1\t3\toptimized\t\t\nFGDB_LOCATION:1\t4\tunavailable\t\t\nFGDB_LOCATION:1\t5\tunknown\t\t6572726f72\n";
        let locations = parse_locations(output, 6).unwrap();
        assert_eq!(locations[0].address(), Some(0));
        assert_eq!(
            locations[1],
            ValueLocation::Memory {
                address: 0x1234,
                referenced: true
            }
        );

        assert_eq!(locations[2], ValueLocation::NonAddressable);
        assert_eq!(locations[3], ValueLocation::OptimizedOut);
        assert_eq!(locations[4], ValueLocation::Unavailable);
        assert_eq!(locations[5], ValueLocation::Unknown("error".into()));
        assert!(parse_locations(output, 7).is_none());
        assert!(parse_locations(output, 5).is_none());

        for invalid in [
            "FGDB_LOCATION:2\t0\tmemory\t0x1\t",
            "FGDB_LOCATION:1\t1\tmemory\t0x1\t",
            "FGDB_LOCATION:1\t0\tmemory\t0x10000000000000000\t",
            "FGDB_LOCATION:1\t0\tmemory\t0x00000000000000001\t",
            "FGDB_LOCATION:1\t0\tmemory\t0x+1\t",
            "FGDB_LOCATION:1\t0\tmemory\t0x\t",
            "FGDB_LOCATION:1\t0\tno-address\t0x1\t",
            "FGDB_LOCATION:1\t0\tregister\t\t",
            "FGDB_LOCATION:1\t0\tunknown\t\tff",
            "FGDB_LOCATION:1\t0\tunknown\t\tf",
            "FGDB_LOCATION:1\t0\tunknown\t\tzz",
        ] {
            assert!(parse_locations(invalid, 1).is_none(), "{invalid}");
        }

        assert!(parse_locations(&"x".repeat(32769), 0).is_none());
        assert!(parse_locations("", BATCH_LIMIT + 1).is_none());
    }
}
