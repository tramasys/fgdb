//! Versioned, bounded debugging workspaces. Only reusable definitions are stored.

use crate::{
    config::DebugSession,
    debugger::{Breakpoint, MemoryFormat},
};
use gtk::glib;
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

mod io;
pub(crate) use io::{read, save_revision, write};

const MAX_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_ITEMS: usize = 256;

pub(crate) type CaptureResult = Box<dyn FnOnce(Result<Vec<Breakpoint>, String>)>;
pub(crate) type RestoreResult = Box<dyn FnOnce(Result<Vec<String>, String>)>;

#[derive(Clone, Debug)]
pub(crate) struct Investigation {
    pub session: DebugSession,
    pub breakpoints: Vec<SavedBreakpoint>,
    pub watches: Vec<String>,
    pub memory: Vec<SavedMemory>,
    pub sources: Vec<(PathBuf, u32)>,
    pub notes: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SavedBreakpoint {
    pub location: String,
    pub hardware: bool,
    pub enabled: bool,
    pub temporary: bool,
    pub condition: String,
    pub ignore_count: u64,
    pub commands: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SavedMemory {
    pub expression: String,
    pub bytes: usize,
    pub format: MemoryFormat,
}

impl MemoryFormat {
    const ALL: [Self; 7] = [
        Self::Bytes,
        Self::U16,
        Self::U32,
        Self::U64,
        Self::F32,
        Self::F64,
        Self::Pointers,
    ];

    pub fn key(self) -> &'static str {
        match self {
            Self::Bytes => "bytes",
            Self::U16 => "u16",
            Self::U32 => "u32",
            Self::U64 => "u64",
            Self::F32 => "f32",
            Self::F64 => "f64",
            Self::Pointers => "pointers",
        }
    }

    fn parse(value: &str) -> Result<Self, String> {
        Self::ALL
            .into_iter()
            .find(|format| format.key() == value)
            .ok_or_else(|| format!("Unknown saved memory format '{value}'"))
    }
}

impl Investigation {
    pub fn capture_breakpoints(&mut self, breakpoints: &[Breakpoint]) {
        self.breakpoints.clear();
        self.notes.clear();
        let disabled_locations: HashSet<_> = breakpoints
            .iter()
            .filter(|breakpoint| breakpoint.is_location() && !breakpoint.enabled)
            .map(Breakpoint::command_number)
            .collect();

        for breakpoint in breakpoints
            .iter()
            .filter(|breakpoint| !breakpoint.is_location())
        {
            let reason = if !matches!(
                breakpoint.kind.as_str(),
                "breakpoint" | "hw breakpoint" | "hardware breakpoint"
            ) {
                Some("this stop-point kind cannot yet be restored portably")
            } else if !matches!(
                breakpoint.disposition.as_deref(),
                None | Some("keep" | "del")
            ) {
                Some("this breakpoint disposition cannot yet be restored portably")
            } else if breakpoint.enabled && disabled_locations.contains(breakpoint.command_number())
            {
                Some("individual disabled locations cannot be identified safely after rebuilding")
            } else if breakpoint.thread.is_some() || breakpoint.inferior.is_some() {
                Some("thread and inferior identities do not survive a new session")
            } else {
                None
            };

            let location = breakpoint.original_location.clone().or_else(|| {
                Some(format!(
                    "{}:{}",
                    breakpoint.source_path()?,
                    breakpoint.line?
                ))
            });

            let reason = reason.or_else(|| match location.as_deref() {
                None => Some("no reusable source or function location was reported"),
                Some(location) if !reusable_location(location) => {
                    Some("addresses and relative locations do not survive relocation or rebuilding")
                }
                _ => None,
            });

            if let Some(reason) = reason {
                self.notes.push(format!(
                    "Breakpoint #{} was not saved: {reason}",
                    breakpoint.number
                ));
                continue;
            }

            self.breakpoints.push(SavedBreakpoint {
                location: location.unwrap_or_default(),
                hardware: breakpoint.is_hardware_breakpoint(),
                enabled: breakpoint.enabled,
                temporary: breakpoint.disposition.as_deref() == Some("del"),
                condition: breakpoint.condition.clone().unwrap_or_default(),
                ignore_count: breakpoint.ignore_count,
                commands: breakpoint.commands.clone(),
            });
        }
    }

    fn validate(&self) -> Result<(), String> {
        if [
            self.breakpoints.len(),
            self.watches.len(),
            self.memory.len(),
            self.sources.len(),
            self.notes.len(),
        ]
        .into_iter()
        .any(|count| count > MAX_ITEMS)
            || self
                .breakpoints
                .iter()
                .any(|breakpoint| breakpoint.commands.len() > MAX_ITEMS)
        {
            return Err(format!(
                "A workspace supports at most {MAX_ITEMS} entries per collection"
            ));
        }

        let mut budget = InputBudget::default();
        budget.list(&self.watches)?;
        budget.list(&self.notes)?;

        if self
            .watches
            .iter()
            .any(|expression| expression.trim().is_empty())
        {
            return Err(String::from("Saved watch expressions must not be empty"));
        }

        for breakpoint in &self.breakpoints {
            budget.text(&breakpoint.location)?;
            budget.text(&breakpoint.condition)?;
            budget.list(&breakpoint.commands)?;

            if !reusable_location(&breakpoint.location) || breakpoint.ignore_count == u64::MAX {
                return Err(String::from(
                    "Invalid saved breakpoint location or ignore count",
                ));
            }
        }

        for memory in &self.memory {
            budget.text(&memory.expression)?;

            if memory.expression.trim().is_empty() || !(1..=65_536).contains(&memory.bytes) {
                return Err(String::from("Invalid saved memory expression or range"));
            }
        }

        for (path, line) in &self.sources {
            budget.path(path)?;

            if !(1..=250_000).contains(line) {
                return Err(String::from("Invalid saved source line"));
            }
        }

        match &self.session {
            DebugSession::Launch {
                executable,
                arguments,
                environment,
                working_directory,
            } => {
                budget.path(executable)?;
                budget.path(working_directory)?;
                budget.list(arguments)?;

                if environment.len() > MAX_ITEMS {
                    return Err(String::from("Too many saved environment overrides"));
                }

                let mut names = HashSet::new();

                for (name, value) in environment {
                    budget.text(name)?;
                    budget.text(value)?;

                    if name.is_empty()
                        || name.contains('=')
                        || !names.insert(name)
                        || name.len() + value.len() + 1 > 16_384
                    {
                        return Err(String::from("Invalid or duplicate saved environment name"));
                    }
                }
            }
            DebugSession::Attach { executable, .. } => {
                if let Some(executable) = executable {
                    budget.path(executable)?;
                }
            }
            DebugSession::CoreDump {
                executable,
                core_dump,
            } => {
                budget.path(executable)?;
                budget.path(core_dump)?;
            }
            DebugSession::Remote {
                endpoint,
                executable,
                remote_executable,
                ..
            } => {
                budget.text(endpoint)?;

                if endpoint.trim().is_empty() {
                    return Err(String::from("Saved remote endpoint cannot be empty"));
                }

                if let Some(executable) = executable {
                    budget.path(executable)?;
                }

                if let Some(executable) = remote_executable {
                    budget.text(executable)?;

                    if executable.trim().is_empty() {
                        return Err(String::from("Saved remote executable must not be empty"));
                    }
                }
            }
            DebugSession::RrReplay { trace_directory } => budget.path(trace_directory)?,
        }

        Ok(())
    }

    pub fn encode(&self) -> Result<String, String> {
        // Reject oversized values before serialization allocates copies of them.
        self.validate()?;
        let file = glib::KeyFile::new();
        file.set_integer("workspace", "version", 1);
        write_session(&file, &self.session)?;
        write_list(&file, "watches", &self.watches);
        write_list(&file, "notes", &self.notes);
        file.set_integer("workspace", "breakpoints", self.breakpoints.len() as i32);
        file.set_integer("workspace", "memory", self.memory.len() as i32);
        file.set_integer("workspace", "sources", self.sources.len() as i32);

        for (index, breakpoint) in self.breakpoints.iter().enumerate() {
            let group = format!("breakpoint {index}");
            file.set_string(&group, "location", &breakpoint.location);
            file.set_boolean(&group, "hardware", breakpoint.hardware);
            file.set_boolean(&group, "enabled", breakpoint.enabled);
            file.set_boolean(&group, "temporary", breakpoint.temporary);
            file.set_string(&group, "condition", &breakpoint.condition);
            file.set_uint64(&group, "ignore", breakpoint.ignore_count);
            write_list(&file, &format!("commands {index}"), &breakpoint.commands);
        }

        for (index, memory) in self.memory.iter().enumerate() {
            let group = format!("memory {index}");
            file.set_string(&group, "expression", &memory.expression);
            file.set_uint64(&group, "bytes", memory.bytes as u64);
            file.set_string(&group, "format", memory.format.key());
        }

        for (index, (path, line)) in self.sources.iter().enumerate() {
            let group = format!("source {index}");
            file.set_string(&group, "path", &path_text(path)?);
            file.set_uint64(&group, "line", u64::from(*line));
        }

        let text = file.to_data().to_string();

        if text.len() > MAX_BYTES {
            return Err(String::from("Workspace exceeds the 1 MiB limit"));
        }

        Ok(text)
    }

    pub fn decode(text: &str) -> Result<Self, String> {
        if text.len() > MAX_BYTES || text.contains('\0') {
            return Err(String::from(
                "Workspace must be valid text of at most 1 MiB",
            ));
        }

        let file = glib::KeyFile::new();
        file.load_from_data(text, glib::KeyFileFlags::NONE)
            .map_err(message)?;

        if file.integer("workspace", "version").map_err(message)? != 1 {
            return Err(String::from("Unsupported workspace version"));
        }

        let session = read_session(&file)?;
        let watches = read_list(&file, "watches")?;
        let notes = read_list(&file, "notes")?;
        let mut breakpoints = Vec::new();

        for index in 0..count(&file, "workspace", "breakpoints")? {
            let group = format!("breakpoint {index}");
            let location = text_value(&file, &group, "location")?;

            let ignore_count = file.uint64(&group, "ignore").map_err(message)?;

            breakpoints.push(SavedBreakpoint {
                location,
                hardware: file.boolean(&group, "hardware").map_err(message)?,
                enabled: file.boolean(&group, "enabled").map_err(message)?,
                temporary: file.boolean(&group, "temporary").map_err(message)?,
                condition: text_value(&file, &group, "condition")?,
                ignore_count,
                commands: read_list(&file, &format!("commands {index}"))?,
            });
        }

        let mut memory = Vec::new();
        for index in 0..count(&file, "workspace", "memory")? {
            let group = format!("memory {index}");
            let bytes = file.uint64(&group, "bytes").map_err(message)?;
            let format = MemoryFormat::parse(&text_value(&file, &group, "format")?)?;
            let expression = text_value(&file, &group, "expression")?;

            memory.push(SavedMemory {
                expression,
                bytes: usize::try_from(bytes).map_err(message)?,
                format,
            });
        }

        let mut sources = Vec::new();
        for index in 0..count(&file, "workspace", "sources")? {
            let group = format!("source {index}");
            let line = file.uint64(&group, "line").map_err(message)?;

            sources.push((
                absolute_path(&file, &group, "path")?,
                u32::try_from(line).map_err(message)?,
            ));
        }

        let workspace = Self {
            session,
            breakpoints,
            watches,
            memory,
            sources,
            notes,
        };

        workspace.validate()?;
        Ok(workspace)
    }
}

fn reusable_location(location: &str) -> bool {
    let location = location.trim();
    !location.is_empty()
        && !location.starts_with(['*', '+', '-'])
        && !location.starts_with("0x")
        && !location.starts_with("0X")
        && !location.bytes().all(|byte| byte.is_ascii_digit())
}

#[derive(Default)]
struct InputBudget(usize);

impl InputBudget {
    fn text(&mut self, text: &str) -> Result<(), String> {
        if text.len() > 16_384 || text.contains(['\0', '\n', '\r']) {
            return Err(String::from("Invalid or oversized workspace text value"));
        }

        self.0 = self.0.saturating_add(text.len());

        if self.0 > MAX_BYTES {
            return Err(String::from("Workspace exceeds the 1 MiB limit"));
        }

        Ok(())
    }

    fn path(&mut self, path: &Path) -> Result<(), String> {
        let text = path.to_str().ok_or("Workspace paths must be valid UTF-8")?;

        if text.is_empty() {
            return Err(String::from("Workspace paths must not be empty"));
        }

        self.text(text)
    }

    fn list(&mut self, items: &[String]) -> Result<(), String> {
        if items.len() > MAX_ITEMS {
            return Err(format!(
                "A workspace supports at most {MAX_ITEMS} entries per collection"
            ));
        }

        items.iter().try_for_each(|item| self.text(item))
    }
}

fn message(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn path_text(path: &Path) -> Result<String, String> {
    let text = std::path::absolute(path)
        .map_err(message)?
        .into_os_string()
        .into_string()
        .map_err(|_| String::from("Workspace paths must be valid UTF-8"))?;

    InputBudget::default().text(&text)?;
    Ok(text)
}

fn text_value(file: &glib::KeyFile, group: &str, key: &str) -> Result<String, String> {
    let value = file.string(group, key).map_err(message)?.to_string();

    if value.len() > 16_384 || value.contains(['\0', '\n', '\r']) {
        return Err(format!("Invalid workspace value {group}/{key}"));
    }

    Ok(value)
}

fn absolute_path(file: &glib::KeyFile, group: &str, key: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(text_value(file, group, key)?);

    if !path.is_absolute() {
        return Err(format!("Workspace path {group}/{key} must be absolute"));
    }

    Ok(path)
}

fn count(file: &glib::KeyFile, group: &str, key: &str) -> Result<usize, String> {
    let count = file.integer(group, key).map_err(message)?;

    if !(0..=MAX_ITEMS as i32).contains(&count) {
        return Err(format!(
            "Workspace collection {group}/{key} exceeds {MAX_ITEMS} items"
        ));
    }

    Ok(count as usize)
}

fn write_list(file: &glib::KeyFile, group: &str, items: &[String]) {
    file.set_integer(group, "count", items.len() as i32);

    for (index, item) in items.iter().enumerate() {
        file.set_string(group, &index.to_string(), item);
    }
}

fn read_list(file: &glib::KeyFile, group: &str) -> Result<Vec<String>, String> {
    (0..count(file, group, "count")?)
        .map(|index| text_value(file, group, &index.to_string()))
        .collect()
}

fn write_session(file: &glib::KeyFile, session: &DebugSession) -> Result<(), String> {
    let kind = match session {
        DebugSession::Launch {
            executable,
            arguments,
            environment,
            working_directory,
        } => {
            file.set_string(
                "session",
                "executable",
                &path_text(&working_directory.join(executable))?,
            );

            file.set_string("session", "directory", &path_text(working_directory)?);
            write_list(file, "arguments", arguments);
            write_list(
                file,
                "environment",
                &environment
                    .iter()
                    .map(|(name, value)| format!("{name}={value}"))
                    .collect::<Vec<_>>(),
            );
            "launch"
        }
        DebugSession::Attach { executable, .. } => {
            // A saved attach session is a template. PID reuse must never attach to an unrelated process.
            file.set_string(
                "session",
                "executable",
                &executable
                    .as_deref()
                    .map(path_text)
                    .transpose()?
                    .unwrap_or_default(),
            );
            "attach"
        }
        DebugSession::CoreDump {
            executable,
            core_dump,
        } => {
            file.set_string("session", "executable", &path_text(executable)?);
            file.set_string("session", "core", &path_text(core_dump)?);
            "core"
        }
        DebugSession::Remote {
            endpoint,
            executable,
            extended,
            remote_executable,
        } => {
            file.set_string("session", "endpoint", endpoint);
            file.set_string(
                "session",
                "executable",
                &executable
                    .as_deref()
                    .map(path_text)
                    .transpose()?
                    .unwrap_or_default(),
            );

            file.set_boolean("session", "extended", *extended);
            file.set_string(
                "session",
                "remote_executable",
                remote_executable.as_deref().unwrap_or(""),
            );
            "remote"
        }
        DebugSession::RrReplay { trace_directory } => {
            file.set_string("session", "trace", &path_text(trace_directory)?);
            "rr"
        }
    };

    file.set_string("session", "kind", kind);
    Ok(())
}

fn read_session(file: &glib::KeyFile) -> Result<DebugSession, String> {
    let optional_executable = || -> Result<Option<PathBuf>, String> {
        let path = PathBuf::from(text_value(file, "session", "executable")?);

        if path.as_os_str().is_empty() {
            Ok(None)
        } else if !path.is_absolute() {
            Err(String::from("Saved executable must be an absolute path"))
        } else {
            Ok(Some(path))
        }
    };

    Ok(match text_value(file, "session", "kind")?.as_str() {
        "launch" => {
            let environment = read_list(file, "environment")?
                .into_iter()
                .map(|entry| {
                    let (name, value) = entry
                        .split_once('=')
                        .ok_or_else(|| String::from("Invalid saved environment entry"))?;

                    if name.is_empty() {
                        return Err(String::from("Saved environment names cannot be empty"));
                    }

                    Ok((name.to_owned(), value.to_owned()))
                })
                .collect::<Result<Vec<_>, String>>()?;

            DebugSession::Launch {
                executable: absolute_path(file, "session", "executable")?,
                arguments: read_list(file, "arguments")?,
                environment,
                working_directory: absolute_path(file, "session", "directory")?,
            }
        }
        "attach" => DebugSession::Attach {
            pid: 0,
            executable: optional_executable()?,
        },
        "core" => DebugSession::CoreDump {
            executable: absolute_path(file, "session", "executable")?,
            core_dump: absolute_path(file, "session", "core")?,
        },
        "remote" => {
            let remote = text_value(file, "session", "remote_executable")?;
            let endpoint = text_value(file, "session", "endpoint")?;

            if endpoint.trim().is_empty() {
                return Err(String::from("Saved remote endpoint cannot be empty"));
            }

            DebugSession::Remote {
                endpoint,
                executable: optional_executable()?,
                extended: file.boolean("session", "extended").map_err(message)?,
                remote_executable: (!remote.is_empty()).then_some(remote),
            }
        }
        "rr" => DebugSession::RrReplay {
            trace_directory: absolute_path(file, "session", "trace")?,
        },
        _ => return Err(String::from("Unknown workspace session kind")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn workspace() -> Investigation {
        Investigation {
            session: DebugSession::Launch {
                executable: PathBuf::from("bin/program"),
                working_directory: PathBuf::from("/tmp/project with spaces"),
                arguments: vec!["--name".into(), "a b='c'".into()],
                environment: vec![("APP_MODE".into(), "some=value".into())],
            },
            breakpoints: Vec::new(),
            watches: vec!["values[index]".into()],
            memory: vec![SavedMemory {
                expression: "&values".into(),
                bytes: 128,
                format: MemoryFormat::U32,
            }],
            sources: vec![(PathBuf::from("/tmp/source.c"), 7)],
            notes: Vec::new(),
        }
    }

    #[test]
    fn workspace_roundtrip_preserves_definitions_and_rejects_invalid_input() {
        let mut workspace = workspace();
        let record = crate::debugger::parse_record(r#"^done,BreakpointTable={body=[bkpt={number="9",type="breakpoint",enabled="n",original-location="worker",cond="count > 3",ignore="2",script=["silent","printf \"count=%d\\n\", count","continue"]},bkpt={number="10",type="breakpoint",original-location="*0x1234"},bkpt={number="11",type="breakpoint",original-location="worker",thread="2"}]}"#).unwrap();
        workspace.capture_breakpoints(&crate::debugger::breakpoints(&record));
        let text = workspace.encode().unwrap();
        let restored = Investigation::decode(&text).unwrap();
        assert_eq!(restored.breakpoints, workspace.breakpoints);
        assert_eq!(restored.breakpoints.len(), 1);
        assert_eq!(restored.notes.len(), 2);
        assert_eq!(restored.watches, workspace.watches);
        assert_eq!(restored.memory, workspace.memory);
        workspace.capture_breakpoints(&crate::debugger::breakpoints(&record));
        assert_eq!(
            workspace.notes.len(),
            2,
            "Recapture must not accumulate omission notes"
        );
        assert_eq!(
            restored.session.executable().unwrap(),
            Path::new("/tmp/project with spaces/bin/program")
        );
        assert!(Investigation::decode(&text.replace("version=1", "version=999")).is_err());
        assert!(
            Investigation::decode(&text.replace("breakpoints=1", "breakpoints=999999")).is_err()
        );
        assert!(
            Investigation::decode(&text.replace("location=worker", "location=*0x1234")).is_err()
        );

        for location in ["  *0X1234", "+12", "218", "0X1234", " "] {
            assert!(
                Investigation::decode(
                    &text.replace("location=worker", &format!("location={location}"))
                )
                .is_err()
            );
        }

        let mut oversized = workspace.clone();
        oversized.breakpoints[0].commands = vec!["x".repeat(16_384); 65];
        assert!(oversized.encode().is_err());

        let unusual = crate::debugger::parse_record(r#"^done,BreakpointTable={body=[bkpt={number="1",type="tracepoint",enabled="y",original-location="worker"},bkpt={number="2",type="breakpoint",enabled="y",disp="dis",original-location="worker"},bkpt={number="3",type="breakpoint",enabled="y",original-location="worker",locations=[{number="3.1",enabled="n",addr="0x1234"}]}]}"#).unwrap();
        let mut omitted = workspace.clone();
        omitted.capture_breakpoints(&crate::debugger::breakpoints(&unusual));
        assert!(omitted.breakpoints.is_empty());
        assert_eq!(omitted.notes.len(), 3);

        for session in [
            DebugSession::Attach {
                pid: 1234,
                executable: Some(PathBuf::from("/tmp/program")),
            },
            DebugSession::CoreDump {
                executable: PathBuf::from("/tmp/program"),
                core_dump: PathBuf::from("/tmp/core"),
            },
            DebugSession::Remote {
                endpoint: "localhost:1234".into(),
                executable: None,
                extended: true,
                remote_executable: Some("/target/program".into()),
            },
            DebugSession::RrReplay {
                trace_directory: PathBuf::from("/tmp/trace"),
            },
        ] {
            workspace.session = session.clone();
            let saved = Investigation::decode(&workspace.encode().unwrap()).unwrap();

            if let DebugSession::Attach { pid, .. } = saved.session {
                assert_eq!(pid, 0)
            } else {
                assert_eq!(saved.session, session)
            }
        }
    }
}
