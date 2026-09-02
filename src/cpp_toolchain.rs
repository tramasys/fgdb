use std::{
    collections::BTreeSet,
    env,
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

const MAX_COMPILER_OUTPUT_BYTES: usize = 64 * 1024;
const MAX_AUTO_LOAD_SCRIPT_BYTES: usize = 256 * 1024;
const LIBSTDCXX_PRINTER: &str = "libstdcxx/v6/printers.py";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GccPrettyPrinter {
    python_directory: PathBuf,
}

impl GccPrettyPrinter {
    pub(crate) fn discover(working_directory: &Path, timeout: Duration) -> Option<Self> {
        let compiler = compiler_command();
        let started = Instant::now();
        let reported_library = [
            "-print-file-name=libstdc++.so.6",
            "-print-file-name=libstdc++.so",
        ]
        .into_iter()
        .find_map(|argument| {
            let remaining = timeout.checked_sub(started.elapsed())?;

            if remaining.is_zero() {
                return None;
            }

            let output = compiler_output(
                &compiler,
                working_directory,
                OsStr::new(argument),
                remaining,
            )?;

            parse_compiler_path(std::str::from_utf8(&output).ok()?)
        })?;

        let resolved_library = reported_library
            .canonicalize()
            .unwrap_or_else(|_| reported_library.clone());

        discover_python_directory(&reported_library, &resolved_library)
            .map(|python_directory| Self { python_directory })
    }

    pub(crate) fn python_directory(&self) -> &Path {
        &self.python_directory
    }

    /// Register the compiler-matched libstdc++ printers globally. GDB's
    /// object-file auto-loader can still register the same printer against the
    /// exact shared object later, which takes precedence for that object.
    pub(crate) fn gdb_arguments(&self) -> Vec<String> {
        let Some(directory) = self.python_directory.to_str() else {
            return Vec::new();
        };

        let directory = python_string(directory);
        let command = format!(
            "python import gdb, sys; sys.path.insert(0, {directory}) if {directory} not in sys.path else None; from libstdcxx.v6 import register_libstdcxx_printers; register_libstdcxx_printers(None) if not any(getattr(printer, 'name', None) == 'libstdc++-v6' for printer in gdb.pretty_printers) else None"
        );

        vec![String::from("-iex"), command]
    }
}

fn compiler_output(
    compiler: &[OsString],
    working_directory: &Path,
    argument: &OsStr,
    timeout: Duration,
) -> Option<Vec<u8>> {
    let (executable, prefix_arguments) = compiler.split_first()?;
    let mut child = Command::new(executable)
        .args(prefix_arguments)
        .arg(argument)
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

                return (status.success() && output.stdout.len() <= MAX_COMPILER_OUTPUT_BYTES)
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

fn compiler_command() -> Vec<OsString> {
    let Some(value) = env::var_os("CXX").filter(|value| !value.is_empty()) else {
        return vec![OsString::from("g++")];
    };

    if Path::new(&value).is_file() {
        return vec![value];
    }

    value
        .to_str()
        .and_then(|value| shell_words::split(value).ok())
        .filter(|arguments| !arguments.is_empty())
        .map(|arguments| arguments.into_iter().map(OsString::from).collect())
        .unwrap_or_else(|| vec![value])
}

fn parse_compiler_path(output: &str) -> Option<PathBuf> {
    let path = PathBuf::from(output.trim());

    (!path.as_os_str().is_empty() && path.is_absolute() && path.exists()).then_some(path)
}

fn discover_python_directory(reported_library: &Path, resolved_library: &Path) -> Option<PathBuf> {
    let mut candidates = BTreeSet::new();

    for hook in auto_load_hook_candidates(resolved_library) {
        if let Some(directory) = python_directory_from_hook(&hook) {
            candidates.insert(directory);
        }
    }

    let prefixes = installation_prefixes(reported_library, resolved_library);
    let version_components = reported_library
        .components()
        .filter_map(|component| component.as_os_str().to_str())
        .filter(|component| {
            component.split('.').next().is_some_and(|major| {
                !major.is_empty() && major.bytes().all(|byte| byte.is_ascii_digit())
            })
        })
        .collect::<BTreeSet<_>>();

    for prefix in prefixes {
        candidates.insert(prefix.join("share/gcc/python"));

        for version in &version_components {
            let major = version.split('.').next().unwrap_or(version);
            candidates.insert(prefix.join(format!("share/gcc-{major}/python")));
            candidates.insert(prefix.join(format!("share/gcc-{version}/python")));
        }

        if let Some(gcc_suffix) = gcc_install_suffix(reported_library) {
            candidates.insert(prefix.join("share/gcc").join(gcc_suffix).join("python"));
        }
    }

    candidates.into_iter().find_map(|directory| {
        directory
            .join(LIBSTDCXX_PRINTER)
            .is_file()
            .then(|| directory.canonicalize().unwrap_or(directory))
    })
}

fn auto_load_hook_candidates(library: &Path) -> Vec<PathBuf> {
    let relative = library.strip_prefix(Path::new("/")).unwrap_or(library);
    let mut candidates = vec![append_suffix(library, "-gdb.py")];

    for root in ["/usr/share/gdb/auto-load", "/usr/local/share/gdb/auto-load"] {
        candidates.push(append_suffix(&Path::new(root).join(relative), "-gdb.py"));
    }

    candidates
}

fn append_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);

    PathBuf::from(value)
}

fn python_directory_from_hook(path: &Path) -> Option<PathBuf> {
    let contents = crate::bounded::read_string(path, MAX_AUTO_LOAD_SCRIPT_BYTES).ok()?;

    contents.lines().find_map(|line| {
        let value = line.trim().strip_prefix("pythondir")?.trim();
        let value = value.strip_prefix('=')?.trim();
        let value = quoted_python_string(value)?;
        let directory = PathBuf::from(value);

        (directory.is_absolute() && directory.join(LIBSTDCXX_PRINTER).is_file())
            .then_some(directory)
    })
}

fn quoted_python_string(value: &str) -> Option<&str> {
    let quote = value.as_bytes().first().copied()?;

    if !matches!(quote, b'\'' | b'"') || value.as_bytes().last().copied() != Some(quote) {
        return None;
    }

    let value = &value[1..value.len().saturating_sub(1)];
    (!value.contains('\\')).then_some(value)
}

fn installation_prefixes(reported_library: &Path, resolved_library: &Path) -> BTreeSet<PathBuf> {
    let mut prefixes = BTreeSet::new();

    for library in [reported_library, resolved_library] {
        for ancestor in library.ancestors() {
            if ancestor
                .file_name()
                .and_then(OsStr::to_str)
                .is_some_and(|name| matches!(name, "lib" | "lib32" | "lib64"))
                && let Some(prefix) = ancestor.parent()
            {
                prefixes.insert(prefix.to_path_buf());
            }
        }
    }

    prefixes
}

fn gcc_install_suffix(path: &Path) -> Option<PathBuf> {
    let components = path.components().collect::<Vec<_>>();
    let gcc = components
        .iter()
        .position(|component| component.as_os_str() == "gcc")?;
    let end = gcc.saturating_add(3).min(components.len());

    (end > gcc + 1).then(|| components[gcc + 1..end].iter().collect())
}

fn python_string(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len().saturating_add(2));
    quoted.push('\'');

    for character in value.chars() {
        match character {
            '\\' | '\'' => {
                quoted.push('\\');
                quoted.push(character);
            }
            '\n' => quoted.push_str("\\n"),
            '\r' => quoted.push_str("\\r"),
            '\t' => quoted.push_str("\\t"),
            character => quoted.push(character),
        }
    }

    quoted.push('\'');
    quoted
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_directory(name: &str) -> PathBuf {
        env::temp_dir().join(format!("fgdb-cpp-toolchain-{}-{name}", std::process::id()))
    }

    #[test]
    fn finds_versioned_gcc_printer_directories_from_the_library_layout() {
        let root = test_directory("versioned-discovery");
        let library = root.join("lib/gcc/test-target/14/libstdc++.so.6");
        let python = root.join("share/gcc-14/python");
        let printer = python.join(LIBSTDCXX_PRINTER);
        std::fs::create_dir_all(library.parent().unwrap()).unwrap();
        std::fs::create_dir_all(printer.parent().unwrap()).unwrap();
        std::fs::write(&library, []).unwrap();
        std::fs::write(&printer, []).unwrap();

        assert_eq!(
            discover_python_directory(&library, &library),
            Some(python.canonicalize().unwrap())
        );

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn derives_gcc_install_suffixes() {
        assert_eq!(
            gcc_install_suffix(Path::new("/usr/lib/gcc/x86_64-linux-gnu/14/libstdc++.so")),
            Some(PathBuf::from("x86_64-linux-gnu/14"))
        );
    }

    #[test]
    fn quotes_python_paths_without_executable_boundaries() {
        assert_eq!(
            python_string("/tmp/user's \\printer"),
            "'/tmp/user\\'s \\\\printer'"
        );
    }

    #[test]
    fn rejects_compiler_output_that_is_not_an_existing_absolute_path() {
        assert_eq!(parse_compiler_path("libstdc++.so.6\n"), None);
        assert_eq!(parse_compiler_path("\n"), None);
    }
}
