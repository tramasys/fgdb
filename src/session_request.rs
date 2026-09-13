use crate::{config::DebugSession, local_process::ProcessIdentity};
use std::path::{Path, PathBuf};

/// Transient user intent. Process identities are not persisted in session files.
#[derive(Clone, Debug)]
pub(crate) struct SessionRequest {
    pub session: DebugSession,
    pub attach_identity: Option<ProcessIdentity>,
}

impl From<DebugSession> for SessionRequest {
    fn from(session: DebugSession) -> Self {
        Self {
            session,
            attach_identity: None,
        }
    }
}

impl SessionRequest {
    /// Filesystem validation runs on a worker before session submission.
    pub(crate) fn validate_paths(mut self) -> Result<Self, String> {
        match &mut self.session {
            DebugSession::Launch {
                executable,
                working_directory,
                ..
            } => {
                *working_directory = canonical_path(working_directory, "Working directory", true)?;

                if executable.is_relative() {
                    *executable = working_directory.join(&executable);
                }

                *executable = canonical_path(executable, "Executable", false)?;
            }
            DebugSession::CoreDump {
                executable,
                core_dump,
            } => {
                *executable = canonical_path(executable, "Executable", false)?;
                *core_dump = canonical_path(core_dump, "Core dump", false)?;
            }
            DebugSession::Attach { executable, .. } | DebugSession::Remote { executable, .. } => {
                if let Some(path) = executable {
                    *path = canonical_path(path, "Executable", false)?;
                }
            }
            DebugSession::RrReplay { trace_directory } => {
                *trace_directory = canonical_path(trace_directory, "rr trace directory", true)?;
            }
        }

        Ok(self)
    }

    pub(crate) fn validate_attach(
        &self,
        debugger_pid: Option<u32>,
    ) -> Result<Option<ProcessIdentity>, String> {
        let DebugSession::Attach { pid, .. } = self.session else {
            return Ok(None);
        };

        if Some(pid) == debugger_pid {
            return Err(String::from(
                "GDB cannot attach to itself. Select another process",
            ));
        }

        let current = crate::local_process::capture_identity(pid)?;

        if self
            .attach_identity
            .is_some_and(|expected| expected != current)
        {
            return Err(format!(
                "PID {pid} is no longer the selected process. Refresh the list and select it again"
            ));
        }

        Ok(Some(current))
    }
}

fn canonical_path(path: &Path, label: &str, directory: bool) -> Result<PathBuf, String> {
    let valid = if directory {
        path.is_dir()
    } else {
        path.is_file()
    };

    if !valid {
        let kind = if directory { "directory" } else { "file" };
        return Err(format!("{label} is not a {kind}: {}", path.display()));
    }

    path.canonicalize()
        .map_err(|error| format!("Cannot resolve {label}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_validation_resolves_launch_paths_and_preserves_attach_identity() {
        let root = gtk::glib::mkdtemp(std::env::temp_dir().join("fgdb-session-XXXXXX")).unwrap();
        let executable = root.join("program");
        std::fs::write(&executable, []).unwrap();

        let launch = SessionRequest::from(DebugSession::Launch {
            executable: PathBuf::from("program"),
            arguments: vec![String::from("two words")],
            environment: vec![(String::from("MODE"), String::from("debug build"))],
            working_directory: root.clone(),
        })
        .validate_paths()
        .unwrap();

        assert_eq!(launch.session.executable(), Some(executable.as_path()));
        assert_eq!(launch.session.working_directory(), Some(root.as_path()));

        let identity = ProcessIdentity {
            pid: 42,
            start_time: 10,
        };
        let attach = SessionRequest {
            session: DebugSession::Attach {
                pid: 42,
                executable: Some(executable.clone()),
            },
            attach_identity: Some(identity),
        }
        .validate_paths()
        .unwrap();

        assert_eq!(attach.attach_identity, Some(identity));
        let fifo = root.join("core");
        nix::unistd::mkfifo(&fifo, nix::sys::stat::Mode::S_IRUSR).unwrap();

        assert!(
            SessionRequest::from(DebugSession::CoreDump {
                executable,
                core_dump: fifo,
            })
            .validate_paths()
            .is_err()
        );

        assert!(
            SessionRequest::from(DebugSession::RrReplay {
                trace_directory: root.join("missing"),
            })
            .validate_paths()
            .is_err()
        );

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn selected_identity_is_preserved_and_not_rebound() {
        let request = SessionRequest {
            session: DebugSession::Attach {
                pid: 42,
                executable: None,
            },
            attach_identity: Some(ProcessIdentity {
                pid: 42,
                start_time: 10,
            }),
        };

        let restored = request.clone();
        assert_eq!(restored.attach_identity, request.attach_identity);
        assert!(
            restored
                .validate_attach(Some(42))
                .unwrap_err()
                .contains("itself")
        );
    }
}
