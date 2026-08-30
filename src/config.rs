use std::{
    env, fs,
    io::Write,
    path::{Path, PathBuf},
};

const MAX_CONFIG_BYTES: usize = 64 * 1024;
const DEFAULT_CONFIG: &str = "# fgdb configuration\n# Environment variables override these values for one launch.\ngdb=gdb\ngdb_args=\nsource_path=\ngef_context=hide\n";

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
    pub gef_context_visible: bool,
    pub source_paths: Vec<PathBuf>,
    pub target_arguments: Vec<String>,
    pub working_directory: PathBuf,
}

impl LaunchConfig {
    pub fn from_process() -> Result<Self, shell_words::ParseError> {
        let target_arguments = env::args().skip(1).collect();
        let file_config = read_user_config();
        let gdb_executable = env::var("FGDB_GDB")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .or(file_config.gdb_executable)
            .unwrap_or_else(|| String::from("gdb"));
        let gdb_startup_arguments = env::var("FGDB_GDB_ARGS")
            .ok()
            .or(file_config.gdb_startup_arguments)
            .map_or_else(
                || Ok(Vec::new()),
                |arguments| shell_words::split(&arguments),
            )?;
        let gef_context_visible = env::var("FGDB_GEF_CONTEXT")
            .ok()
            .and_then(|value| parse_gef_context(&value))
            .or(file_config.gef_context_visible)
            .unwrap_or(false);
        let source_paths = env::var_os("FGDB_SOURCE_PATH")
            .map(|paths| env::split_paths(&paths).collect())
            .or_else(|| {
                file_config
                    .source_path
                    .as_deref()
                    .map(|paths| env::split_paths(paths).collect())
            })
            .unwrap_or_default();
        let working_directory = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

        Ok(Self {
            gdb_executable,
            gdb_startup_arguments,
            gef_context_visible,
            source_paths,
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

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct FileConfig {
    gdb_executable: Option<String>,
    gdb_startup_arguments: Option<String>,
    gef_context_visible: Option<bool>,
    source_path: Option<String>,
}

fn config_path() -> PathBuf {
    gtk::glib::user_config_dir().join("fgdb/config.conf")
}

fn read_user_config() -> FileConfig {
    let path = config_path();
    match crate::bounded::read_string(&path, MAX_CONFIG_BYTES) {
        Ok(contents) => parse_user_config(&contents),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let _ = create_default_config(&path);
            FileConfig::default()
        }
        Err(_) => FileConfig::default(),
    }
}

fn create_default_config(path: &Path) -> std::io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "the configuration path has no parent directory",
        )
    })?;
    fs::create_dir_all(parent)?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(DEFAULT_CONFIG.as_bytes())
}

fn parse_user_config(contents: &str) -> FileConfig {
    let mut config = FileConfig::default();
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        match key {
            "gdb" | "gdb_executable" if !value.is_empty() => {
                config.gdb_executable = Some(unquote_config_value(value).to_owned());
            }
            "gdb_args" | "gdb_arguments" => {
                config.gdb_startup_arguments = Some(value.to_owned());
            }
            "gef_context" | "gef.context" => {
                if let Some(visible) = parse_gef_context(value) {
                    config.gef_context_visible = Some(visible);
                }
            }
            "source_path" | "source_paths" => {
                config.source_path = Some(unquote_config_value(value).to_owned());
            }
            _ => {}
        }
    }
    config
}

fn unquote_config_value(value: &str) -> &str {
    let value = value.trim();
    if value.len() >= 2
        && ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')))
    {
        &value[1..value.len() - 1]
    } else {
        value
    }
}

fn parse_gef_context(value: &str) -> Option<bool> {
    match value
        .trim()
        .trim_matches(['\'', '"'])
        .to_ascii_lowercase()
        .as_str()
    {
        "show" | "visible" | "on" | "true" | "yes" | "1" => Some(true),
        "hide" | "hidden" | "off" | "false" | "no" | "0" => Some(false),
        _ => None,
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
            gef_context_visible: false,
            source_paths: Vec::new(),
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
            gef_context_visible: false,
            source_paths: Vec::new(),
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

    #[test]
    fn gef_context_defaults_can_be_explicitly_made_visible() {
        for value in ["show", "ON", " true ", "yes", "1"] {
            assert_eq!(super::parse_gef_context(value), Some(true), "{value}");
        }
        for value in ["hide", "off", "false", "no", "0"] {
            assert_eq!(super::parse_gef_context(value), Some(false), "{value}");
        }
        assert_eq!(super::parse_gef_context("unexpected"), None);
    }

    #[test]
    fn reads_gef_context_from_the_user_config_format() {
        assert_eq!(
            super::parse_user_config(
                "# fgdb\ngdb=/usr/bin/gdb\ngdb_args=-ex init-gef-special\ngef_context=show\nsource_path='/src/one:/src/two'\n"
            ),
            super::FileConfig {
                gdb_executable: Some(String::from("/usr/bin/gdb")),
                gdb_startup_arguments: Some(String::from("-ex init-gef-special")),
                gef_context_visible: Some(true),
                source_path: Some(String::from("/src/one:/src/two")),
            }
        );
        assert_eq!(
            super::parse_user_config("gef.context='hide'\nunknown=value\n"),
            super::FileConfig {
                gef_context_visible: Some(false),
                ..super::FileConfig::default()
            }
        );
    }
}
