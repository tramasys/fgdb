use std::path::Path;

const MAX_VECTOR_BYTES: usize = 2 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProcessArgument {
    pub index: usize,
    pub address: Option<u64>,
    pub byte_len: usize,
    pub value: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProcessEnvironment {
    pub index: usize,
    pub address: Option<u64>,
    pub byte_len: usize,
    pub name: String,
    pub value: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ProcessStartupSnapshot {
    pub pid: u32,
    pub argument_range: Option<(u64, u64)>,
    pub environment_range: Option<(u64, u64)>,
    pub arguments: Vec<ProcessArgument>,
    pub environment: Vec<ProcessEnvironment>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct StartupRanges {
    identity: u64,
    arguments: Option<(u64, u64)>,
    environment: Option<(u64, u64)>,
}

pub(crate) fn read_process_startup(
    pid: u32,
    debugger_pid: u32,
) -> Result<ProcessStartupSnapshot, String> {
    let root = super::verified_proc_root(pid, debugger_pid)?;
    let before = read_startup_ranges(&root.join("stat"))
        .ok_or_else(|| format!("Cannot read argument boundaries from /proc/{pid}/stat"))?;
    let mut snapshot = ProcessStartupSnapshot {
        pid,
        argument_range: before.arguments,
        environment_range: before.environment,
        ..ProcessStartupSnapshot::default()
    };

    match crate::bounded::read_bytes(&root.join("cmdline"), MAX_VECTOR_BYTES) {
        Ok(bytes) => {
            snapshot.arguments = nul_entries(&bytes)
                .enumerate()
                .map(|(index, (offset, value))| ProcessArgument {
                    index,
                    address: entry_address(before.arguments, offset, value.len()),
                    byte_len: value.len(),
                    value: display_bytes(value),
                })
                .collect();
        }
        Err(error) => snapshot
            .warnings
            .push(format!("Cannot read /proc/{pid}/cmdline: {error}")),
    }

    match crate::bounded::read_bytes(&root.join("environ"), MAX_VECTOR_BYTES) {
        Ok(bytes) => {
            snapshot.environment = nul_entries(&bytes)
                .enumerate()
                .map(|(index, (offset, entry))| {
                    let (name, value) = entry
                        .iter()
                        .position(|byte| *byte == b'=')
                        .map_or((entry, &[][..]), |separator| {
                            (&entry[..separator], &entry[separator + 1..])
                        });
                    ProcessEnvironment {
                        index,
                        address: entry_address(before.environment, offset, entry.len()),
                        byte_len: entry.len(),
                        name: display_bytes(name),
                        value: display_bytes(value),
                    }
                })
                .collect();
        }
        Err(error) => snapshot
            .warnings
            .push(format!("Cannot read /proc/{pid}/environ: {error}")),
    }

    let after = read_startup_ranges(&root.join("stat"))
        .ok_or_else(|| format!("Process {pid} disappeared while its startup data was read"))?;
    super::verified_proc_root(pid, debugger_pid)?;
    if before.identity != after.identity {
        return Err(format!(
            "Process {pid} changed while its startup data was being read"
        ));
    }
    Ok(snapshot)
}

fn read_startup_ranges(path: &Path) -> Option<StartupRanges> {
    parse_startup_ranges(&crate::bounded::read_string(path, 64 * 1024).ok()?)
}

fn parse_startup_ranges(stat: &str) -> Option<StartupRanges> {
    // Fields are indexed from `state` (proc stat field 3) after removing the
    // parenthesized command, which may itself contain spaces or parentheses.
    let mut identity = None;
    let mut argument_start = None;
    let mut argument_end = None;
    let mut environment_start = None;
    let mut environment_end = None;
    for (index, value) in stat.rsplit_once(") ")?.1.split_whitespace().enumerate() {
        let target = match index {
            19 => &mut identity,
            45 => &mut argument_start,
            46 => &mut argument_end,
            47 => &mut environment_start,
            48 => &mut environment_end,
            _ => continue,
        };
        *target = value.parse::<u64>().ok();
        if index == 48 {
            break;
        }
    }
    Some(StartupRanges {
        identity: identity?,
        arguments: argument_start
            .zip(argument_end)
            .and_then(|(start, end)| valid_range(start, end)),
        environment: environment_start
            .zip(environment_end)
            .and_then(|(start, end)| valid_range(start, end)),
    })
}

fn valid_range(start: u64, end: u64) -> Option<(u64, u64)> {
    (start != 0 && end >= start).then_some((start, end))
}

fn nul_entries(bytes: &[u8]) -> impl Iterator<Item = (usize, &[u8])> {
    let mut offset = 0;
    bytes.split_inclusive(|byte| *byte == 0).map(move |entry| {
        let start = offset;
        offset += entry.len();
        let value = if entry.last() == Some(&0) {
            &entry[..entry.len() - 1]
        } else {
            entry
        };
        (start, value)
    })
}

fn entry_address(range: Option<(u64, u64)>, offset: usize, byte_len: usize) -> Option<u64> {
    let (start, end) = range?;
    let offset = u64::try_from(offset).ok()?;
    let byte_len = u64::try_from(byte_len).ok()?;
    let address = start.checked_add(offset)?;
    (address.checked_add(byte_len)? < end).then_some(address)
}

fn display_bytes(bytes: &[u8]) -> String {
    if let Ok(text) = std::str::from_utf8(bytes) {
        let mut display = String::with_capacity(text.len());
        for character in text.chars() {
            match character {
                '\n' => display.push_str("\\n"),
                '\r' => display.push_str("\\r"),
                '\t' => display.push_str("\\t"),
                character if character.is_control() => {
                    use std::fmt::Write as _;
                    let _ = write!(display, "\\u{{{:x}}}", u32::from(character));
                }
                character => display.push(character),
            }
        }
        return display;
    }

    let mut display = String::with_capacity(bytes.len());
    for byte in bytes {
        if byte.is_ascii_graphic() || *byte == b' ' {
            display.push(char::from(*byte));
        } else {
            use std::fmt::Write as _;
            let _ = write!(display, "\\x{byte:02x}");
        }
    }
    display
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_empty_vector_entries_and_offsets() {
        assert_eq!(
            nul_entries(b"fgdb\0\0--flag\0").collect::<Vec<_>>(),
            vec![(0, &b"fgdb"[..]), (5, &b""[..]), (6, &b"--flag"[..])]
        );
        assert!(nul_entries(b"").next().is_none());
    }

    #[test]
    fn decodes_utf8_and_losslessly_escapes_binary_values() {
        assert_eq!(display_bytes("Grüezi\n".as_bytes()), "Grüezi\\n");
        assert_eq!(display_bytes(b"a\xffb"), "a\\xffb");
    }

    #[test]
    fn parses_linux_argument_and_environment_boundaries() {
        let mut fields = vec!["0"; 49];
        fields[0] = "S";
        fields[19] = "12345";
        fields[45] = "4096";
        fields[46] = "4128";
        fields[47] = "8192";
        fields[48] = "8256";
        let stat = format!("77 (name with ) parens) {}", fields.join(" "));
        assert_eq!(
            parse_startup_ranges(&stat),
            Some(StartupRanges {
                identity: 12345,
                arguments: Some((4096, 4128)),
                environment: Some((8192, 8256)),
            })
        );
    }

    #[test]
    fn refuses_addresses_outside_the_kernel_reported_block() {
        assert_eq!(entry_address(Some((0x1000, 0x1010)), 4, 3), Some(0x1004));
        assert_eq!(entry_address(Some((0x1000, 0x1010)), 15, 1), None);
        assert_eq!(entry_address(None, 0, 1), None);
    }
}
