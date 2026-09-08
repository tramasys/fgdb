use std::{fmt::Write as _, path::PathBuf};

use super::ModuleKey;
use crate::{
    debug_info::DebugFileSearch,
    debugger::{MiListItem, MiRecord, MiValue, parse_record},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Configuration {
    pub debuginfod: String,
    pub urls: String,
    pub auto_solib: bool,
    pub search: DebugFileSearch,
    pub cache_override: Option<PathBuf>,
}

#[derive(Clone, Debug)]
pub(super) struct Snapshot {
    pub configuration: Configuration,
    pub space: u64,
    pub pid: u32,
    pub object: u64,
    pub loaded: bool,
    pub build_id: Option<String>,
    pub filename: PathBuf,
    pub debug_files: Vec<PathBuf>,
}

pub(super) enum Action<'a> {
    Configuration,
    Inspect,
    Load,
    Attach {
        path: &'a std::path::Path,
        stamp: &'a str,
    },
}

pub(super) fn exact_regex(value: &str) -> Result<String, String> {
    if value.is_empty() || value.len() > 16 * 1024 || value.contains(['\0', '\n', '\r']) {
        return Err(String::from("Invalid module path for symbol loading"));
    }

    let mut pattern = String::with_capacity(value.len() + 2);
    pattern.push('^');

    // MI uses a basic regex while CLI commands can use extended syntax.
    // Bracket literals avoid turning a literal '+' into the BRE '\+' operator.
    for character in value.chars() {
        match character {
            '[' => pattern.push_str("[[]"),
            ']' => pattern.push_str("[]]"),
            '^' => pattern.push_str("\\^"),
            '\\' => pattern.push_str("[\\\\]"),
            '.' | '$' | '*' | '+' | '?' | '(' | ')' | '{' | '}' | '|' => {
                pattern.push('[');
                pattern.push(character);
                pattern.push(']');
            }

            _ => pattern.push(character),
        }
    }

    pattern.push('$');
    Ok(pattern)
}

fn python_string(value: &str) -> String {
    let mut literal = String::with_capacity(value.len() * 2 + 28);
    literal.push_str("bytes.fromhex('");

    for byte in value.bytes() {
        let _ = write!(literal, "{byte:02x}");
    }

    literal.push_str("').decode('utf-8')");
    literal
}

pub(super) fn command(
    key: Option<&ModuleKey>,
    action: Action<'_>,
    expected: Option<&Snapshot>,
    directories: &[PathBuf],
) -> Result<String, String> {
    let mut script = String::new();
    let (name, path, stamp) = match action {
        Action::Configuration => ("configuration", "", ""),
        Action::Inspect => ("inspect", "", ""),
        Action::Load => ("load", "", ""),
        Action::Attach { path, stamp } => (
            "attach",
            path.to_str().ok_or("The debug-file path is not UTF-8")?,
            stamp,
        ),
    };

    let _ = writeln!(script, "action = '{name}'");
    let _ = writeln!(
        script,
        "extra_directories = [{}]",
        directories
            .iter()
            .map(|path| path
                .to_str()
                .map(python_string)
                .ok_or("The debug-directory path is not UTF-8"))
            .collect::<Result<Vec<_>, _>>()?
            .join(",")
    );

    if let Some(key) = key {
        let pattern = exact_regex(&key.target)?;

        for (name, value) in [
            ("inferior_id", key.inferior.as_str()),
            ("target", &key.target),
            ("mapping_from", key.from.as_deref().unwrap_or("")),
            ("mapping_to", key.to.as_deref().unwrap_or("")),
            ("pattern", &pattern),
            ("debug_file", path),
            ("debug_stamp", stamp),
            (
                "expected_build_id",
                expected
                    .and_then(|snapshot| snapshot.build_id.as_deref())
                    .unwrap_or(""),
            ),
        ] {
            let _ = writeln!(script, "{name} = {}", python_string(value));
        }

        let _ = writeln!(
            script,
            "expected_space = {}\nexpected_pid = {}\nexpected_object = {}",
            expected.map_or(0, |snapshot| snapshot.space),
            expected.map_or(0, |snapshot| snapshot.pid),
            expected.map_or(0, |snapshot| snapshot.object)
        );
    }

    script.push_str(include_str!("backend.py"));
    Ok(format!("python exec({}, {{}})", python_string(&script)))
}

fn record(output: &str) -> Result<MiRecord, String> {
    if output.len() > 128 * 1024 {
        return Err(String::from("Symbol metadata exceeded its output budget"));
    }

    let mut records = output
        .lines()
        .filter_map(|line| line.strip_prefix("FGDB_SYMBOLS"));

    let record = records
        .next()
        .and_then(|record| parse_record(record).ok())
        .filter(MiRecord::is_done)
        .ok_or("GDB did not return a complete symbol snapshot")?;

    if records.next().is_some() {
        return Err(String::from("GDB returned ambiguous symbol metadata"));
    }

    Ok(record)
}

fn text<'a>(record: &'a MiRecord, key: &str) -> Result<&'a str, String> {
    record
        .field(key)
        .and_then(MiValue::as_const)
        .ok_or_else(|| format!("Missing symbol metadata field '{key}'"))
}

fn flag(record: &MiRecord, key: &str) -> Result<bool, String> {
    match text(record, key)? {
        "0" => Ok(false),
        "1" => Ok(true),
        _ => Err(format!("Invalid symbol metadata flag '{key}'")),
    }
}

fn paths(record: &MiRecord, key: &str, limit: usize) -> Result<Vec<PathBuf>, String> {
    let values = record
        .field(key)
        .and_then(MiValue::as_list)
        .filter(|values| values.len() <= limit)
        .ok_or_else(|| format!("Invalid symbol path list '{key}'"))?;

    values
        .iter()
        .map(|item| match item {
            MiListItem::Value(MiValue::Const(value)) if !value.contains(['\0', '\n', '\r']) => {
                Ok(PathBuf::from(value))
            }

            _ => Err(String::from("Invalid debug-file path")),
        })
        .collect()
}

fn configuration_from(record: &MiRecord) -> Result<Configuration, String> {
    let cache = text(record, "cache-override")?;

    Ok(Configuration {
        debuginfod: text(record, "debuginfod")?.to_owned(),
        urls: text(record, "urls")?.to_owned(),
        auto_solib: flag(record, "auto-solib")?,
        search: DebugFileSearch {
            directories: paths(record, "directories", 64)?,
            caches: paths(record, "caches", 4)?,
        },
        cache_override: (!cache.is_empty()).then(|| PathBuf::from(cache)),
    })
}

pub(super) fn parse_configuration(output: &str) -> Result<Configuration, String> {
    configuration_from(&record(output)?)
}

pub(super) fn parse(output: &str) -> Result<Snapshot, String> {
    let record = record(output)?;
    let number = |key| {
        text(&record, key)?
            .parse::<u64>()
            .map_err(|_| format!("Invalid symbol metadata '{key}'"))
    };

    let build_id = text(&record, "build-id")?;

    if !build_id.is_empty() && !valid_build_id(build_id) {
        return Err(String::from("GDB reported an invalid build ID"));
    }

    Ok(Snapshot {
        configuration: configuration_from(&record)?,
        space: number("programspace")?,
        pid: u32::try_from(number("pid")?).map_err(|_| "Invalid inferior PID")?,
        object: number("object")?,
        loaded: flag(&record, "loaded")?,
        build_id: (!build_id.is_empty()).then(|| build_id.to_owned()),
        filename: PathBuf::from(text(&record, "filename")?),
        debug_files: paths(&record, "debug-files", 32)?,
    })
}

pub(super) fn valid_build_id(value: &str) -> bool {
    (4..=256).contains(&value.len())
        && value.len().is_multiple_of(2)
        && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_module_patterns_are_literal_in_both_gdb_regex_dialects() {
        assert_eq!(
            exact_regex("/lib/a [x]+(v)?{2}|$.so").unwrap(),
            "/lib/a [[]x[]][+][(]v[)][?][{]2[}][|][$][.]so$".replacen('/', "^/", 1)
        );

        assert_eq!(
            exact_regex("/lib/quote\"é.so").unwrap(),
            "^/lib/quote\"é[.]so$"
        );

        assert_eq!(
            exact_regex("/lib/back\\slash^x.so").unwrap(),
            "^/lib/back[\\\\]slash\\^x[.]so$"
        );

        for invalid in ["", "/lib/foo\nsharedlibrary", "/lib/foo\r", "/lib/foo\0"] {
            assert!(exact_regex(invalid).is_err());
        }
    }

    #[test]
    fn only_complete_bounded_symbol_snapshots_are_accepted() {
        let output = "FGDB_SYMBOLS^done,debuginfod=\"ask\",urls=\"\",auto-solib=\"0\",directories=[\"/debug\"],caches=[],cache-override=\"\",programspace=\"123\",pid=\"42\",object=\"321\",loaded=\"1\",build-id=\"1234abcd\",filename=\"/lib/quote\\\"é.so\",debug-files=[\"/debug/a.debug\"]";
        let snapshot = parse(output).unwrap();
        assert!(snapshot.loaded);
        assert_eq!(snapshot.filename, PathBuf::from("/lib/quote\"é.so"));
        assert_eq!(
            snapshot.configuration.search.directories,
            [PathBuf::from("/debug")]
        );

        assert_eq!(snapshot.debug_files, [PathBuf::from("/debug/a.debug")]);
        assert!(parse("^done").is_err());
        assert!(parse(&format!("{output}\n{output}")).is_err());
        assert!(parse(&output.replace("1234abcd", "../invalid")).is_err());
        assert!(parse(&output.replace("loaded=\"1\"", "loaded=\"maybe\"")).is_err());
        assert!(parse(&"x".repeat(128 * 1024 + 1)).is_err());
    }
}
