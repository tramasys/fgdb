use super::{Process, ProcessIdentity};
use std::{collections::HashMap, path::Path};

const STAT_LIMIT: usize = 16 * 1024;
const COMMAND_LIMIT: usize = 8 * 1024;

struct Stat {
    identity: ProcessIdentity,
    name: String,
    state: u8,
}

pub(crate) fn capture_identity(pid: u32) -> Result<ProcessIdentity, String> {
    if pid == 0 || pid > i32::MAX as u32 || pid == std::process::id() {
        return Err(String::from(
            "Choose a valid process other than fgdb itself",
        ));
    }

    read_stat(&Path::new("/proc").join(pid.to_string()), pid).map(|stat| stat.identity)
}

pub(crate) fn validate_identity(expected: ProcessIdentity) -> Result<(), String> {
    let current = capture_identity(expected.pid)?;
    compare_identity(expected, current)
}

fn compare_identity(expected: ProcessIdentity, current: ProcessIdentity) -> Result<(), String> {
    if expected == current {
        Ok(())
    } else {
        Err(format!(
            "PID {} is no longer the selected process. Refresh the list and select the process again",
            expected.pid
        ))
    }
}

fn read_stat(root: &Path, pid: u32) -> Result<Stat, String> {
    let bytes = crate::bounded::read_bytes(&root.join("stat"), STAT_LIMIT)
        .map_err(|error| format!("Cannot verify PID {pid}: {error}. The process may have exited or /proc access may be restricted"))?;

    let stat = parse_stat(&bytes, pid).ok_or_else(|| {
        format!("Cannot verify the identity of PID {pid}: invalid process metadata")
    })?;

    if matches!(stat.state, b'Z' | b'X' | b'x') {
        return Err(format!(
            "PID {pid} has exited. Refresh the list and select a live process"
        ));
    }

    Ok(stat)
}

fn parse_stat(bytes: &[u8], pid: u32) -> Option<Stat> {
    let open = bytes.iter().position(|byte| *byte == b'(')?;
    let close = bytes.iter().rposition(|byte| *byte == b')')?;

    if close <= open
        || std::str::from_utf8(&bytes[..open])
            .ok()?
            .trim()
            .parse::<u32>()
            .ok()?
            != pid
    {
        return None;
    }

    let mut fields = bytes[close + 1..]
        .split(u8::is_ascii_whitespace)
        .filter(|field| !field.is_empty());

    let state = fields.next()?;

    if state.len() != 1 {
        return None;
    }

    let start_time = std::str::from_utf8(fields.nth(18)?).ok()?.parse().ok()?;

    Some(Stat {
        identity: ProcessIdentity { pid, start_time },
        name: display_text(&String::from_utf8_lossy(&bytes[open + 1..close])),
        state: state[0],
    })
}

pub(super) fn read_process(
    root: &Path,
    pid: u32,
    owners: &HashMap<u32, String>,
) -> Result<Process, String> {
    let stat = read_stat(root, pid)?;
    let status_bytes = crate::bounded::read_prefix(&root.join("status"), 64 * 1024).ok();
    let status = status_bytes.as_deref().map(String::from_utf8_lossy);
    let uid = status.as_deref().and_then(|text| {
        text.lines().find_map(|line| {
            line.strip_prefix("Uid:")?
                .split_whitespace()
                .next()?
                .parse::<u32>()
                .ok()
        })
    });

    let owner = uid.map_or_else(
        || String::from("Unavailable"),
        |uid| {
            owners
                .get(&uid)
                .map_or_else(|| format!("UID {uid}"), |name| format!("{name} ({uid})"))
        },
    );

    let executable = std::fs::read_link(root.join("exe"))
        .ok()
        .map(|path| display_text(&path.to_string_lossy()));

    let mut notes = Vec::new();

    if executable.is_none() {
        notes.push("Executable path unavailable, showing process name");
    }

    if uid.is_none() {
        notes.push("Owner metadata unavailable");
    }

    let command = match crate::bounded::read_prefix(&root.join("cmdline"), COMMAND_LIMIT + 1) {
        Ok(bytes) if !bytes.is_empty() => {
            let truncated = bytes.len() > COMMAND_LIMIT;
            let text = format_command(&bytes[..bytes.len().min(COMMAND_LIMIT)]);

            if truncated {
                notes.push("Command line truncated at 8 KiB");
                format!("{text} …")
            } else {
                text
            }
        }
        _ => {
            notes.push("Command line unavailable");
            String::new()
        }
    };

    if status
        .as_deref()
        .and_then(|text| {
            text.lines()
                .find_map(|line| line.strip_prefix("TracerPid:")?.trim().parse::<u32>().ok())
        })
        .is_some_and(|tracer| tracer != 0)
    {
        notes.push("Already being traced");
    }

    compare_identity(stat.identity, read_stat(root, pid)?.identity)?;
    let executable = executable.unwrap_or(stat.name);
    let detail = notes.join(". ");
    let search = format!("{pid} {executable} {command} {owner}").to_lowercase();

    Ok(Process {
        identity: stat.identity,
        uid,
        executable,
        command,
        owner,
        detail,
        search,
    })
}

fn format_command(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len());
    let bytes = bytes.strip_suffix(b"\0").unwrap_or(bytes);

    for (index, arg) in bytes.split(|byte| *byte == 0).enumerate() {
        if index > 0 {
            output.push(' ');
        }

        output.push_str(&shell_words::quote(&display_text(
            &String::from_utf8_lossy(arg),
        )));
    }

    output
}

pub(super) fn display_text(text: &str) -> String {
    use std::fmt::Write as _;
    let mut output = String::with_capacity(text.len());

    for character in text.chars() {
        if character.is_control()
            || matches!(character, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        {
            let _ = write!(output, "{}", character.escape_default());
        } else {
            output.push(character);
        }
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exited_processes_and_mismatched_start_times_are_rejected() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();

        let identity = capture_identity(child.id()).unwrap();
        let live = validate_identity(identity);
        let replaced = validate_identity(ProcessIdentity {
            start_time: identity.start_time.wrapping_add(1),
            ..identity
        });

        child.kill().unwrap();
        child.wait().unwrap();
        assert!(live.is_ok());
        assert!(replaced.is_err());
        assert!(validate_identity(identity).is_err());
        assert!(capture_identity(0).is_err());
        assert!(capture_identity(std::process::id()).is_err());
    }

    #[test]
    fn identity_parser_handles_parentheses_and_whitespace_in_names() {
        let input =
            b"42 (a ) tricky\n(name)) S 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 12345 0";

        let stat = parse_stat(input, 42).unwrap();
        assert_eq!(stat.identity.start_time, 12345);
        assert_eq!(stat.name, "a ) tricky\\n(name)");
        assert!(parse_stat(input, 43).is_none());
        assert!(parse_stat(b"42 (short) S 1", 42).is_none());
    }

    #[test]
    fn reused_pids_are_rejected() {
        let identity = ProcessIdentity {
            pid: 42,
            start_time: 7,
        };

        assert!(compare_identity(identity, identity).is_ok());
        assert!(
            compare_identity(
                identity,
                ProcessIdentity {
                    start_time: 8,
                    ..identity
                }
            )
            .is_err()
        );
    }

    #[test]
    fn command_arguments_and_untrusted_text_remain_distinguishable() {
        assert_eq!(
            format_command(b"program\0two words\0\0"),
            "program 'two words' ''"
        );

        assert_eq!(display_text("line\n\t\u{202e}"), "line\\n\\t\\u{202e}");
    }

    #[test]
    fn missing_metadata_has_fallbacks_and_long_commands_are_explicitly_truncated() {
        let root =
            gtk::glib::mkdtemp(std::env::temp_dir().join("fgdb-process-test-XXXXXX")).unwrap();

        let stat = b"42 (fallback) S 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 12345 0";
        std::fs::write(root.join("stat"), stat).unwrap();
        let fallback = read_process(&root, 42, &HashMap::new()).unwrap();
        assert_eq!(fallback.executable, "fallback");
        assert_eq!(fallback.uid, None);
        assert!(fallback.command.is_empty());
        assert!(fallback.detail.contains("Owner metadata unavailable"));
        std::fs::write(
            root.join("status"),
            b"Name:\tbad\xffname\nUid:\t1234 1 2 3\nTracerPid:\t9\n",
        )
        .unwrap();

        std::fs::write(root.join("cmdline"), vec![b'a'; COMMAND_LIMIT + 100]).unwrap();
        let process = read_process(&root, 42, &HashMap::new()).unwrap();
        assert_eq!(process.uid, Some(1234));
        assert_eq!(process.owner, "UID 1234");
        assert!(process.command.ends_with(" …"));
        assert!(process.command.len() <= COMMAND_LIMIT + " …".len());
        assert!(process.detail.contains("truncated"));
        assert!(process.detail.contains("Already being traced"));
        let mut exited = stat.to_vec();
        exited[14] = b'Z';
        std::fs::write(root.join("stat"), exited).unwrap();
        assert!(read_process(&root, 42, &HashMap::new()).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
