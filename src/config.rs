use std::{env, path::PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DebugSession {
    Launch {
        executable: PathBuf,
        arguments: Vec<String>,
        environment: Vec<(String, String)>,
        working_directory: PathBuf,
    },
    Attach {
        pid: u32,
        executable: Option<PathBuf>,
    },
    CoreDump {
        executable: PathBuf,
        core_dump: PathBuf,
    },
    Remote {
        endpoint: String,
        executable: Option<PathBuf>,
        extended: bool,
        remote_executable: Option<String>,
    },
}

impl DebugSession {
    pub fn executable(&self) -> Option<&std::path::Path> {
        match self {
            Self::Launch { executable, .. } | Self::CoreDump { executable, .. } => Some(executable),
            Self::Attach { executable, .. } | Self::Remote { executable, .. } => {
                executable.as_deref()
            }
        }
    }

    pub fn title(&self) -> String {
        match self {
            Self::Launch { executable, .. } => executable.to_string_lossy().into_owned(),
            Self::Attach { pid, executable } => executable.as_ref().map_or_else(
                || format!("PID {pid}"),
                |path| format!("{} · PID {pid}", path.to_string_lossy()),
            ),
            Self::Remote {
                endpoint,
                executable,
                ..
            } => executable.as_ref().map_or_else(
                || endpoint.clone(),
                |path| format!("{} · {endpoint}", path.to_string_lossy()),
            ),
            Self::CoreDump {
                executable,
                core_dump,
            } => format!(
                "{} · {}",
                executable.to_string_lossy(),
                core_dump.to_string_lossy()
            ),
        }
    }

    pub const fn kind_label(&self) -> &'static str {
        match self {
            Self::Launch { .. } => "Launch",
            Self::Attach { .. } => "Attached",
            Self::CoreDump { .. } => "Core dump",
            Self::Remote {
                extended: false, ..
            } => "Remote",
            Self::Remote { extended: true, .. } => "Extended remote",
        }
    }

    pub const fn supports_execution(&self) -> bool {
        !matches!(self, Self::CoreDump { .. })
    }

    pub const fn can_start(&self) -> bool {
        matches!(
            self,
            Self::Launch { .. }
                | Self::Remote {
                    extended: true,
                    remote_executable: Some(_),
                    ..
                }
        )
    }

    pub const fn supports_restart(&self) -> bool {
        self.can_start()
    }

    pub const fn supports_kill(&self) -> bool {
        !matches!(self, Self::CoreDump { .. })
    }

    pub const fn supports_detach(&self) -> bool {
        !matches!(self, Self::CoreDump { .. })
    }

    pub fn working_directory(&self) -> Option<&std::path::Path> {
        match self {
            Self::Launch {
                working_directory, ..
            } => Some(working_directory),
            Self::Attach { .. } | Self::CoreDump { .. } | Self::Remote { .. } => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct LaunchConfig {
    pub gdb_executable: String,
    pub gdb_startup_arguments: Vec<String>,
    pub target_arguments: Vec<String>,
    pub working_directory: PathBuf,
}

impl LaunchConfig {
    pub fn from_process() -> Result<Self, shell_words::ParseError> {
        let target_arguments = env::args().skip(1).collect();
        let gdb_executable = env::var("FGDB_GDB").unwrap_or_else(|_| String::from("gdb"));
        let gdb_startup_arguments = env::var("FGDB_GDB_ARGS").ok().map_or_else(
            || Ok(Vec::new()),
            |arguments| shell_words::split(&arguments),
        )?;
        let working_directory = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

        Ok(Self {
            gdb_executable,
            gdb_startup_arguments,
            target_arguments,
            working_directory,
        })
    }

    pub fn gdb_arguments(&self) -> Vec<String> {
        let mut arguments = vec![self.gdb_executable.clone(), String::from("--quiet")];
        arguments.extend(self.gdb_startup_arguments.iter().cloned());
        if !self.target_arguments.is_empty() {
            arguments.push(String::from("--args"));
            arguments.extend(self.target_arguments.iter().cloned());
        }
        arguments
    }

    pub fn target_name(&self) -> &str {
        self.target_arguments
            .first()
            .map_or("No target selected", String::as_str)
    }

    pub fn initial_session(&self) -> Option<DebugSession> {
        let (executable, arguments) = self.target_arguments.split_first()?;
        Some(DebugSession::Launch {
            executable: PathBuf::from(executable),
            arguments: arguments.to_vec(),
            environment: Vec::new(),
            working_directory: self.working_directory.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{DebugSession, LaunchConfig};
    use std::path::PathBuf;

    #[test]
    fn assembles_special_gef_startup_before_target() {
        let configuration = LaunchConfig {
            gdb_executable: String::from("/usr/bin/gdb"),
            gdb_startup_arguments: vec![String::from("-ex"), String::from("init-gef-special")],
            target_arguments: vec![String::from("/tmp/debug target"), String::from("arg")],
            working_directory: PathBuf::from("/tmp"),
        };

        assert_eq!(
            configuration.gdb_arguments(),
            [
                "/usr/bin/gdb",
                "--quiet",
                "-ex",
                "init-gef-special",
                "--args",
                "/tmp/debug target",
                "arg",
            ]
        );
    }

    #[test]
    fn derives_an_editable_launch_session_from_process_arguments() {
        let configuration = LaunchConfig {
            gdb_executable: String::from("gdb"),
            gdb_startup_arguments: Vec::new(),
            target_arguments: vec![
                String::from("/tmp/debug target"),
                String::from("first argument"),
            ],
            working_directory: PathBuf::from("/tmp/project"),
        };

        assert_eq!(
            configuration.initial_session(),
            Some(DebugSession::Launch {
                executable: PathBuf::from("/tmp/debug target"),
                arguments: vec![String::from("first argument")],
                environment: Vec::new(),
                working_directory: PathBuf::from("/tmp/project"),
            })
        );
    }
}
