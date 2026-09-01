use std::{
    io,
    time::{Duration, Instant},
};

use super::{MiClient, MiRecord, MiResult, MiValue, quote};

pub(super) type ResponseHandler = Box<dyn FnOnce(&MiClient, MiRecord)>;
pub(super) type ScopedResponseHandler = Box<dyn FnOnce(&MiClient, MiRecord, String)>;

pub(super) const MAX_MI_COMMAND_BYTES: usize = 1024 * 1024;
pub(super) const MAX_PENDING_REQUESTS: usize = 4096;
pub(super) const MAX_SCOPED_REQUESTS: usize = 1024;
pub(super) const MAX_CAPTURED_CONSOLE_BYTES: usize = 1024 * 1024;
pub(super) const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
pub(super) const MAX_REQUEST_LIFETIME: Duration = Duration::from_secs(5 * 60);
pub(super) const REQUEST_TIMEOUT_POLL: Duration = Duration::from_millis(250);

pub(super) struct PendingRequest {
    pub(super) deadline: Instant,
    pub(super) hard_deadline: Instant,
    pub(super) progress_generation: u64,
    pub(super) is_current: Option<Box<dyn Fn() -> bool>>,
    pub(super) handler: ResponseHandler,
}

pub(super) struct ScopedMiRequest {
    pub(super) token: u64,
    pub(super) command: String,
    pub(super) response: Option<MiRecord>,
    pub(super) output: String,
    pub(super) expect_nested_mi: bool,
    pub(super) is_current: Box<dyn Fn() -> bool>,
    pub(super) handler: ScopedResponseHandler,
    pub(super) deadline: Instant,
    pub(super) hard_deadline: Instant,
}

pub(super) fn error_record(message: &str) -> MiRecord {
    synthetic_error_record("error", message)
}

pub(super) fn synthetic_error_record(class: &str, message: &str) -> MiRecord {
    MiRecord {
        token: None,
        kind: '^',
        class: class.to_owned(),
        results: vec![MiResult {
            name: String::from("msg"),
            value: MiValue::Const(message.to_owned()),
        }],
    }
}

pub(super) fn scoped_mi_command(command: &str, elements: usize) -> String {
    let console_command = format!(
        "with print elements {elements} -- interpreter-exec mi {}",
        quote(command)
    );
    format!("-interpreter-exec console {}", quote(&console_command))
}

pub(super) fn validate_console_command(command: &str) -> io::Result<()> {
    if command.trim().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "GDB console command cannot be empty",
        ));
    }
    if command.len() > MAX_MI_COMMAND_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "GDB console command exceeds the 1 MiB limit",
        ));
    }
    if command
        .bytes()
        .any(|byte| matches!(byte, b'\0' | b'\n' | b'\r'))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "GDB console command contains NUL or a line break",
        ));
    }
    Ok(())
}

pub(super) fn validate_mi_command(command: &str) -> io::Result<()> {
    if command.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "GDB/MI command cannot be empty",
        ));
    }
    if command.len() > MAX_MI_COMMAND_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "GDB/MI command exceeds the 1 MiB limit",
        ));
    }
    if command.bytes().any(|byte| matches!(byte, b'\n' | b'\r')) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "GDB/MI command contains a line break",
        ));
    }
    Ok(())
}
