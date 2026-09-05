use std::{
    io,
    rc::Rc,
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
pub(super) const MAX_ACTIVE_CONTROL_REQUESTS: usize = 8;
pub(super) const MAX_ACTIVE_INSPECTION_REQUESTS: usize = 4;
pub(super) const MAX_ACTIVE_BACKGROUND_REQUESTS: usize = 1;
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
    const ALL: [Self; 4] = [
        Self::Execution,
        Self::Control,
        Self::Inspection,
        Self::Background,
    ];

    fn active_limit(self) -> usize {
        match self {
            Self::Execution => usize::MAX,
            Self::Control => MAX_ACTIVE_CONTROL_REQUESTS,
            Self::Inspection => MAX_ACTIVE_INSPECTION_REQUESTS,
            Self::Background => MAX_ACTIVE_BACKGROUND_REQUESTS,
        }
    }

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

    pub(super) fn queue_timeout(self) -> Duration {
        match self {
            Self::Execution | Self::Control => Duration::from_secs(15),
            Self::Inspection => Duration::from_secs(30),
            Self::Background => Duration::from_secs(60),
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
    pub(super) command: Option<String>,
    pub(super) queued_at: Instant,
    pub(super) started_at: Option<Instant>,
    pub(super) deadline: Instant,
    pub(super) hard_deadline: Instant,
    pub(super) is_current: Option<Rc<dyn Fn() -> bool>>,
    pub(super) handler: Option<ResponseHandler>,
}

impl PendingRequest {
    pub(super) fn complete(self, client: &MiClient, record: MiRecord) {
        if let Some(handler) = self.handler {
            let record = if self.is_current.is_some_and(|is_current| !is_current()) {
                synthetic_error_record("superseded", "request superseded")
            } else {
                record
            };

            handler(client, record);
        }
    }
}

#[derive(Default)]
struct PendingClass {
    count: usize,
    active: usize,
    next: Option<u64>,
}

/// Read-only scheduling facts collected in one pass over pending requests.
/// Rebuild after dispatch or callbacks instead of maintaining parallel counters
/// across cancellation, completion, reconnect, and reentrant handlers.
#[derive(Default)]
pub(super) struct RequestSchedule {
    classes: [PendingClass; 4],
    pub(super) queued_bytes: usize,
}

impl RequestSchedule {
    pub(super) fn from_pending<'a>(
        pending: impl IntoIterator<Item = (&'a u64, &'a PendingRequest)>,
    ) -> Self {
        let mut schedule = Self::default();

        for (&token, request) in pending {
            let class = &mut schedule.classes[request.class.queue_priority() as usize];
            class.count += 1;

            if request.started_at.is_some() {
                class.active += 1;
            } else if request.command.is_some() {
                class.next = Some(class.next.map_or(token, |next| next.min(token)));
            }

            if let Some(command) = &request.command {
                schedule.queued_bytes = schedule.queued_bytes.saturating_add(command.len());
            }
        }

        schedule
    }

    pub(super) fn pending_count(&self, class: CommandClass) -> usize {
        self.classes[class.queue_priority() as usize].count
    }

    pub(super) fn has_capacity(&self, class: CommandClass) -> bool {
        self.classes[class.queue_priority() as usize].active < class.active_limit()
    }

    pub(super) fn next(
        &self,
        capture_active: bool,
        capture_priority: Option<(u8, u64)>,
    ) -> Option<u64> {
        CommandClass::ALL.into_iter().find_map(|class| {
            let priority = class.queue_priority();
            let token = self.classes[priority as usize].next?;
            let capture_allows = class == CommandClass::Execution
                || (!capture_active
                    && capture_priority.is_none_or(|next| (priority, token) < next));

            (capture_allows && self.has_capacity(class)).then_some(token)
        })
    }
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
    pub(super) is_current: Rc<dyn Fn() -> bool>,
    pub(super) handler: ScopedResponseHandler,
    pub(super) deadline: Instant,
    pub(super) hard_deadline: Instant,
    pub(super) queued_at: Instant,
    pub(super) started_at: Option<Instant>,
    pub(super) cancelled: bool,
    pub(super) output_truncated: bool,
}

impl ScopedMiRequest {
    pub(super) fn complete(self, client: &MiClient, record: MiRecord) {
        let record = if record.class != "superseded" && (self.cancelled || !(self.is_current)()) {
            synthetic_error_record("superseded", "request superseded")
        } else {
            record
        };

        (self.handler)(client, record, self.output);
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn scheduling_snapshot_matches_scan_based_selection() {
        let now = Instant::now();
        let mut random = 123_u32;

        for length in 0..=64 {
            let pending = (0..length)
                .map(|index| {
                    random = random.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    let class = CommandClass::ALL[((random >> 16) % 4) as usize];
                    let started = random >> 20 & 3 == 0;
                    let command = (random >> 24 & 3 != 0).then(|| String::from("-thread-info"));

                    (
                        (index * 67) % 257 + 1,
                        PendingRequest {
                            class,
                            owner: None,
                            operation: String::new(),
                            command,
                            queued_at: now,
                            started_at: started.then_some(now),
                            deadline: now,
                            hard_deadline: now,
                            is_current: None,
                            handler: None,
                        },
                    )
                })
                .collect::<HashMap<_, _>>();
            let schedule = RequestSchedule::from_pending(&pending);
            assert_eq!(
                schedule.queued_bytes,
                pending
                    .values()
                    .filter_map(|request| request.command.as_ref())
                    .map(String::len)
                    .sum()
            );

            for class in CommandClass::ALL {
                assert_eq!(
                    schedule.pending_count(class),
                    pending
                        .values()
                        .filter(|request| request.class == class)
                        .count()
                );
            }

            for capture_active in [false, true] {
                for capture_priority in [
                    None,
                    Some((0, 128)),
                    Some((1, 128)),
                    Some((2, 128)),
                    Some((3, 128)),
                ] {
                    let expected = pending
                        .iter()
                        .filter(|(token, request)| {
                            let active = pending
                                .values()
                                .filter(|other| {
                                    other.started_at.is_some() && other.class == request.class
                                })
                                .count();
                            let limit = match request.class {
                                CommandClass::Execution => usize::MAX,
                                CommandClass::Control => MAX_ACTIVE_CONTROL_REQUESTS,
                                CommandClass::Inspection => MAX_ACTIVE_INSPECTION_REQUESTS,
                                CommandClass::Background => MAX_ACTIVE_BACKGROUND_REQUESTS,
                            };

                            request.started_at.is_none()
                                && request.command.is_some()
                                && (request.class == CommandClass::Execution
                                    || (!capture_active
                                        && capture_priority.is_none_or(|priority| {
                                            (request.class.queue_priority(), **token) < priority
                                        })))
                                && active < limit
                        })
                        .min_by_key(|(token, request)| (request.class.queue_priority(), **token))
                        .map(|(token, _)| *token);

                    assert_eq!(schedule.next(capture_active, capture_priority), expected);
                }
            }
        }
    }
}
