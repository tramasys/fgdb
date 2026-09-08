use crate::{config::DebugSession, local_process::ProcessIdentity};

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

#[cfg(test)]
mod tests {
    use super::*;

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
