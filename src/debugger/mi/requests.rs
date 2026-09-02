use std::{
    io,
    time::{Duration, Instant},
};

use super::{MiClient, MiRecord, MiResult, MiValue, quote};
use crate::performance::{MI_BACKGROUND_BUDGET, MI_CONTROL_BUDGET, MI_INSPECTION_BUDGET};

pub(super) type ResponseHandler = Box<dyn FnOnce(&MiClient, MiRecord)>;
pub(super) type ScopedResponseHandler = Box<dyn FnOnce(&MiClient, MiRecord, String)>;

pub(super) const MAX_MI_COMMAND_BYTES: usize = 1024 * 1024;
pub(super) const MAX_PENDING_REQUESTS: usize = 512;
pub(super) const MAX_INSPECTION_REQUESTS: usize = 384;
pub(super) const MAX_NON_EXECUTION_REQUESTS: usize = 480;
pub(super) const MAX_SCOPED_REQUESTS: usize = 128;
pub(super) const MAX_BACKGROUND_SCOPED_REQUESTS: usize = 24;
pub(super) const MAX_CAPTURED_CONSOLE_BYTES: usize = 1024 * 1024;
pub(super) const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
pub(super) const MAX_REQUEST_LIFETIME: Duration = Duration::from_secs(5 * 60);
pub(super) const REQUEST_TIMEOUT_POLL: Duration = Duration::from_millis(250);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum CommandClass {
    Execution,
    #[default]
    Control,
    Inspection,
    Background,
}

impl CommandClass {
    pub(super) fn timeout(self) -> Duration {
        match self {
            Self::Execution | Self::Control => REQUEST_TIMEOUT,
            Self::Inspection => Duration::from_secs(15),
            Self::Background => Duration::from_secs(60),
        }
    }

    pub(super) fn maximum_lifetime(self) -> Duration {
        match self {
            Self::Execution | Self::Control => Duration::from_secs(60),
            Self::Inspection => Duration::from_secs(90),
            Self::Background => MAX_REQUEST_LIFETIME,
        }
    }

    pub(super) fn performance_budget(self) -> Duration {
        match self {
            Self::Execution | Self::Control => MI_CONTROL_BUDGET,
            Self::Inspection => MI_INSPECTION_BUDGET,
            Self::Background => MI_BACKGROUND_BUDGET,
        }
    }

    pub(super) fn queue_priority(self) -> u8 {
        match self {
            Self::Execution => 0,
            Self::Control => 1,
            Self::Inspection => 2,
            Self::Background => 3,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CommandOwner {
    Stop(u64),
    Session(u64),
}

pub(super) struct PendingRequest {
    pub(super) class: CommandClass,
    pub(super) owner: Option<CommandOwner>,
    pub(super) operation: String,
    pub(super) queued_at: Instant,
    pub(super) deadline: Instant,
    pub(super) hard_deadline: Instant,
    pub(super) is_current: Option<Box<dyn Fn() -> bool>>,
    pub(super) handler: ResponseHandler,
}

pub(super) struct ScopedMiRequest {
    pub(super) token: u64,
    pub(super) class: CommandClass,
    pub(super) owner: Option<CommandOwner>,
    pub(super) operation: String,
    pub(super) command: String,
    pub(super) response: Option<MiRecord>,
    pub(super) output: String,
    pub(super) expect_nested_mi: bool,
    pub(super) is_current: Box<dyn Fn() -> bool>,
    pub(super) handler: ScopedResponseHandler,
    pub(super) deadline: Instant,
    pub(super) hard_deadline: Instant,
    pub(super) queued_at: Instant,
    pub(super) started_at: Option<Instant>,
    pub(super) cancelled: bool,
    pub(super) output_truncated: bool,
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
