use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

const MAX_RUSTC_OUTPUT_BYTES: usize = 64 * 1024;
const RUST_GDB_LOADER: &str = "gdb_load_rust_pretty_printers.py";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RustToolchain {
    sysroot: PathBuf,
    printer_directory: Option<PathBuf>,
    commit_hash: Option<String>,
}

impl RustToolchain {
    pub(crate) fn discover(working_directory: &Path, timeout: Duration) -> Option<Self> {
        let started = Instant::now();
        let output = rustc_output(working_directory, &["--print=sysroot"], timeout)?;
        let sysroot = parse_rustc_sysroot(std::str::from_utf8(&output).ok()?)?;

        if !sysroot.is_dir() {
            return None;
        }

        let commit_hash = timeout
            .checked_sub(started.elapsed())
            .filter(|remaining| !remaining.is_zero())
            .and_then(|remaining| rustc_output(working_directory, &["-vV"], remaining))
            .and_then(|output| {
                std::str::from_utf8(&output)
                    .ok()
                    .and_then(parse_rustc_commit_hash)
            });

        let printer_directory = sysroot.join("lib/rustlib/etc");

        let printer_directory = printer_directory
            .join(RUST_GDB_LOADER)
            .is_file()
            .then_some(printer_directory);

        Some(Self {
            sysroot,
            printer_directory,
            commit_hash,
        })
    }

    pub(crate) fn sysroot(&self) -> &Path {
        &self.sysroot
    }

    #[cfg(test)]
    pub(crate) fn with_printer_directory(
        sysroot: impl Into<PathBuf>,
        printer_directory: impl Into<PathBuf>,
    ) -> Self {
        Self {
            sysroot: sysroot.into(),
            printer_directory: Some(printer_directory.into()),
            commit_hash: None,
        }
    }

    /// Arguments used by rust-gdb to make the compiler-matched Python
    /// printers discoverable before GDB opens an executable. Keep the chosen
    /// debugger executable intact; these arguments also work with a custom
    /// Python-enabled GDB.
    pub(crate) fn gdb_printer_arguments(&self) -> Vec<String> {
        let Some(printer_directory) = self.printer_directory.as_deref() else {
            return Vec::new();
        };

        let Some(printer_directory) = printer_directory.to_str() else {
            return Vec::new();
        };

        let mut arguments = vec![
            format!("--directory={printer_directory}"),
            String::from("-iex"),
            format!(
                "add-auto-load-safe-path {}",
                gdb_cli_string(printer_directory)
            ),
        ];

        if let Some(commit_hash) = self.commit_hash.as_deref() {
            let rust_sources = self.sysroot.join("lib/rustlib/src/rust");

            if rust_sources.is_dir()
                && let Some(rust_sources) = rust_sources.to_str()
            {
                arguments.extend([
                    String::from("-iex"),
                    format!(
                        "set substitute-path {} {}",
                        gdb_cli_string(&format!("/rustc/{commit_hash}")),
                        gdb_cli_string(rust_sources)
                    ),
                ]);
            }
        }

        arguments
    }
}

fn rustc_output(
    working_directory: &Path,
    arguments: &[&str],
    timeout: Duration,
) -> Option<Vec<u8>> {
    let mut child = Command::new("rustc")
        .args(arguments)
        .current_dir(working_directory)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    let deadline = Instant::now().checked_add(timeout)?;

    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let output = child.wait_with_output().ok()?;

                return (status.success() && output.stdout.len() <= MAX_RUSTC_OUTPUT_BYTES)
                    .then_some(output.stdout);
            }
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(5)),
            Ok(None) | Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

fn parse_rustc_sysroot(output: &str) -> Option<PathBuf> {
    let sysroot = PathBuf::from(output.trim());

    (!sysroot.as_os_str().is_empty() && sysroot.is_absolute()).then_some(sysroot)
}

fn parse_rustc_commit_hash(output: &str) -> Option<String> {
    output
        .lines()
        .find_map(|line| line.trim().strip_prefix("commit-hash:"))
        .map(str::trim)
        .filter(|hash| !hash.is_empty() && hash.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .map(str::to_owned)
}

fn gdb_cli_string(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len().saturating_add(2));
    quoted.push('"');
    for character in value.chars() {
        match character {
            '\\' | '"' => {
                quoted.push('\\');
                quoted.push(character);
            }
            '\n' => quoted.push_str("\\n"),
            '\r' => quoted.push_str("\\r"),
            '\t' => quoted.push_str("\\t"),
            character => quoted.push(character),
        }
    }

    quoted.push('"');
    quoted
}

#[cfg(test)]
mod tests {
    use super::{RustToolchain, gdb_cli_string, parse_rustc_commit_hash, parse_rustc_sysroot};
    use std::{path::PathBuf, time::Duration};

    #[test]
    fn discovers_the_active_compiler_sysroot() {
        let toolchain = RustToolchain::discover(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")),
            Duration::from_secs(2),
        )
        .expect("the rustc compiling fgdb should report its sysroot");
        assert!(toolchain.sysroot().is_dir());
    }

    #[test]
    fn parses_verbose_rustc_output_and_sysroot() {
        assert_eq!(
            parse_rustc_sysroot("/opt/rust toolchain\n"),
            Some(PathBuf::from("/opt/rust toolchain"))
        );
        assert_eq!(
            parse_rustc_commit_hash("rustc 1.98.0\ncommit-hash: 88d9e12ae178fab0f\nhost: x86_64\n"),
            Some(String::from("88d9e12ae178fab0f"))
        );
    }

    #[test]
    fn rejects_non_hexadecimal_commit_hashes() {
        let output = "commit-hash: unknown\n/opt/rust\n";
        assert_eq!(parse_rustc_commit_hash(output), None);
    }

    #[test]
    fn quotes_paths_for_gdb_cli_commands() {
        assert_eq!(
            gdb_cli_string("/tmp/a \\\"quoted\\\" path"),
            "\"/tmp/a \\\\\\\"quoted\\\\\\\" path\""
        );
    }

    #[test]
    fn omits_printer_arguments_when_the_toolchain_has_no_loader() {
        let toolchain = RustToolchain {
            sysroot: PathBuf::from("/opt/rust"),
            printer_directory: None,
            commit_hash: None,
        };

        assert!(toolchain.gdb_printer_arguments().is_empty());
    }
}
