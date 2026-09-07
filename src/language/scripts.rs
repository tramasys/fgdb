//! User printers use GDB's normal source/register protocol. A validated script
//! path is shared by startup configuration and interactive loading.

use std::path::{Path, PathBuf};

#[derive(Debug)]
pub(crate) struct PrinterScript {
    path: PathBuf,
    command: String,
}

impl PrinterScript {
    /// Performs filesystem I/O. Interactive callers must use a worker.
    pub(crate) fn resolve(path: &Path) -> Result<Self, String> {
        if path.as_os_str().is_empty() {
            return Err(String::from("Choose a pretty-printer script to load"));
        }

        let canonical = path.canonicalize().map_err(|error| error.to_string())?;

        if !canonical
            .metadata()
            .map_err(|error| error.to_string())?
            .is_file()
        {
            return Err(String::from("the path is not a regular file"));
        }

        let command = source_command(&canonical)?;

        Ok(Self {
            path: canonical,
            command,
        })
    }

    pub(crate) fn command(&self) -> &str {
        &self.command
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn into_path(self) -> PathBuf {
        self.path
    }
}

pub(crate) fn source_command(path: &Path) -> Result<String, &'static str> {
    let text = path
        .to_str()
        .ok_or("the path is not valid UTF-8 for this GDB session")?;

    if !path.is_absolute() {
        return Err("the script path must be absolute");
    }

    if text
        .bytes()
        .any(|byte| matches!(byte, b'\0' | b'\n' | b'\r'))
    {
        return Err("the path contains unsupported characters");
    }

    // Unlike most CLI commands, source consumes the entire filename literally.
    // Quotes become part of the filename. An absolute path cannot be an option.
    Ok(format!("source {text}"))
}
