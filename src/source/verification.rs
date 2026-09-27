//! On-demand source checksums from a build-ID-validated debug object.

use crate::bounded::{FileIdentity, process::output_with_limit};
use std::{
    collections::BTreeMap,
    io::Read,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

pub(crate) struct Evidence {
    pub object: PathBuf,
    pub build_id: String,
    pub filename: PathBuf,
}

pub(crate) fn verify(evidence: Evidence, contents: &str) -> Result<bool, String> {
    let identity = FileIdentity::read(&evidence.object).map_err(|error| error.to_string())?;

    if identity.size > 512 * 1024 * 1024 || contents.len() > 16 * 1024 * 1024 {
        return Err("Source or debug object exceeds the verification size limit".into());
    }

    crate::debug_info::validate_debug_file(
        &evidence.object,
        Some(&evidence.build_id),
        None,
        &|| true,
    )?;

    let (mut file, _) =
        crate::bounded::open_regular_file(&evidence.object).map_err(|error| error.to_string())?;

    let mut header = [0; 6];
    file.read_exact(&mut header)
        .map_err(|error| error.to_string())?;

    let little = match header.get(5) {
        Some(1) => true,
        Some(2) => false,
        _ => return Err("Debug object byte order is unavailable".into()),
    };

    let output = output_with_limit(
        Command::new("readelf")
            .args([
                "--wide",
                "--debug-dump=rawline",
                "--debug-dump=no-follow-links",
                "--debug-dump=do-not-use-debuginfod",
                "--",
            ])
            .arg(&evidence.object)
            .env("LC_ALL", "C")
            .env("DEBUGINFOD_URLS", ""),
        Duration::from_secs(3),
        4 * 1024 * 1024,
    )
    .ok_or("readelf is unavailable, failed, or exceeded the 3-second / 4-MiB output limit")?;

    if FileIdentity::read(&evidence.object).ok().as_ref() != Some(&identity) {
        return Err("Debug object changed during verification".into());
    }

    let output =
        std::str::from_utf8(&output).map_err(|_| "Debug line-table paths are not UTF-8")?;

    let expected = checksum(output, &evidence.filename, little)?;
    let mut digest =
        gtk::glib::Checksum::new(gtk::glib::ChecksumType::Md5).ok_or("MD5 is unavailable")?;

    digest.update(contents.as_bytes());

    Ok(digest.digest() == expected)
}

fn table_name(text: &str) -> Option<&str> {
    if let Some((_, name)) = text.rsplit_once("): ") {
        Some(name)
    } else {
        text.strip_prefix("(string)").map(str::trim_start)
    }
}

fn checksum(output: &str, filename: &Path, little: bool) -> Result<[u8; 16], String> {
    let mut directories = BTreeMap::new();
    let mut directory_table = false;
    let mut file_table = false;
    let mut md5_column = false;
    let mut found = None;

    for line in output.lines() {
        let line = line.trim();

        if line.starts_with("The Directory Table") {
            directories.clear();
            directory_table = true;
            file_table = false;
        } else if line.starts_with("The File Name Table") {
            directory_table = false;
            file_table = true;
            md5_column = false;
        } else if line.starts_with("Line Number Statements:") || line.starts_with("Offset:") {
            directory_table = false;
            file_table = false;
        } else if directory_table {
            if let Some((index, rest)) = line.split_once(char::is_whitespace)
                && let Ok(index) = index.parse::<u64>()
                && let Some(name) = table_name(rest.trim())
            {
                directories.insert(index, PathBuf::from(name));
            }
        } else if file_table {
            if line.starts_with("Entry") {
                md5_column = line.split_whitespace().any(|column| column == "MD5");
                continue;
            }

            if !md5_column {
                continue;
            }

            let Some((prefix, rest)) = line.split_once("(data16)") else {
                continue;
            };
            let mut indices = prefix
                .split_whitespace()
                .filter_map(|part| part.parse::<u64>().ok());
            let (Some(_), Some(directory)) = (indices.next(), indices.next()) else {
                continue;
            };
            let Some((hash, name)) = rest.trim().split_once(char::is_whitespace) else {
                continue;
            };
            let Some(name) = table_name(name.trim()) else {
                continue;
            };
            let name = Path::new(name);
            let resolved = directories
                .get(&directory)
                .map(|directory| directory.join(name));

            if name != filename && resolved.as_deref() != Some(filename) {
                continue;
            }

            let hash = hash
                .strip_prefix("0x")
                .and_then(|hex| u128::from_str_radix(hex, 16).ok())
                .ok_or("Malformed source checksum")?;
            let hash = if little {
                hash.to_le_bytes()
            } else {
                hash.to_be_bytes()
            };

            if found.is_some_and(|previous| previous != hash) {
                return Err(
                    "Different compilation units contain conflicting source checksums".into(),
                );
            }

            found = Some(hash);
        }
    }

    found.ok_or_else(|| "No unambiguous DWARF source checksum is available for this file".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_tables_match_paths_endianness_and_reject_conflicts() {
        let table = "The Directory Table (offset 0x22):\n0 (line_strp) (offset: 0): /build dir\nThe File Name Table (offset 0x2e):\nEntry Dir MD5 Name\n0 (udata) 0 (data16) 0x010203 (line_strp) (offset: 0x20): main file.c\nLine Number Statements:\n";
        let little = checksum(table, Path::new("/build dir/main file.c"), true).unwrap();
        assert_eq!(&little[..3], &[3, 2, 1]);
        let big = checksum(table, Path::new("main file.c"), false).unwrap();
        assert_eq!(&big[13..], &[1, 2, 3]);
        assert!(checksum(table, Path::new("different.c"), true).is_err());
        assert!(
            checksum(
                &format!("{table}{}", table.replace("0x010203", "0x010204")),
                Path::new("main file.c"),
                true
            )
            .is_err()
        );
        assert!(checksum("No checksum", Path::new("main.c"), true).is_err());
    }
}
