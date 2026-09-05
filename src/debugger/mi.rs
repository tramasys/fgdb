use std::{
    cell::{Cell, RefCell},
    collections::{BTreeSet, HashMap, HashSet, VecDeque},
    io::{self, Read},
    os::fd::AsRawFd,
    path::PathBuf,
    rc::{Rc, Weak},
    time::{Duration, Instant},
};

use gtk::glib;

mod input;
mod parser;
mod protocol;
#[cfg(test)]
mod regression_tests;
mod requests;
mod transport;

pub use parser::{parse_record, quote};
pub use protocol::{
    GdbCapabilities, MiEvent, MiListItem, MiRecord, MiResult, MiValue, result_field,
};

use crate::performance::{
    BudgetOutcome, MI_SCOPED_QUEUE_BUDGET, PerformanceNotice, duration_notice,
};
#[cfg(test)]
use parser::{MAX_MI_NESTING, parse_stream_output};
use parser::{MAX_MI_RECORD_BYTES, parse_any_stream_output};
#[cfg(test)]
use requests::MAX_MI_COMMAND_BYTES;
use requests::{
    CommandClass, CommandOwner, MAX_ACTIVE_BACKGROUND_REQUESTS, MAX_ACTIVE_CONTROL_REQUESTS,
    MAX_ACTIVE_INSPECTION_REQUESTS, MAX_BACKGROUND_SCOPED_REQUESTS, MAX_CAPTURED_CONSOLE_BYTES,
    MAX_INSPECTION_REQUESTS, MAX_NON_EXECUTION_REQUESTS, MAX_PENDING_REQUESTS, MAX_SCOPED_REQUESTS,
    PendingRequest, REQUEST_TIMEOUT, REQUEST_TIMEOUT_POLL, ResponseHandler, ScopedMiRequest,
    error_record, scoped_mi_command, synthetic_error_record, validate_console_command,
    validate_mi_command,
};
#[cfg(test)]
use transport::test_transport;
use transport::{
    IoSource, MAX_MI_WRITE_BATCH_BYTES, MAX_QUEUED_MI_BYTES, MiTransport, OutgoingQueue,
    complete_input_end, drain_outgoing, open_transport,
};

type TransportFactory = Rc<dyn Fn() -> io::Result<MiTransport>>;
use input::{DecodedLine, PendingInput};
type EventHandler = Box<dyn Fn(&MiClient, MiEvent)>;
const MAX_RETAINED_MI_INPUT_BYTES: usize = 256 * 1024;
const MI_READ_CHUNK_BYTES: usize = 16 * 1024;
const MAX_MI_READ_BATCH_BYTES: usize = 256 * 1024;
const MAX_MI_READ_BATCH_TIME: Duration = Duration::from_millis(4);
const RUST_PRINTER_PROBE: &str = "python import gdb; next(printer for holder in [*gdb.objfiles(), gdb.current_progspace()] for printer in getattr(holder, \"pretty_printers\", []) if getattr(printer, \"name\", \"\") == \"rust\")";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct MiRecordHeader {
    token: Option<u64>,
    kind: Option<u8>,
}

pub struct MiClient {
    transport_factory: TransportFactory,
    transport: RefCell<MiTransport>,
    incoming: RefCell<Vec<u8>>,
    pending_input: RefCell<Option<PendingInput>>,
    input_source: RefCell<Option<glib::SourceId>>,
    next_token: Cell<u64>,
    printer_probe_generation: Cell<u64>,
    ready: Cell<bool>,
    initializing: Cell<bool>,
    capabilities: RefCell<GdbCapabilities>,
    pending: RefCell<HashMap<u64, PendingRequest>>,
    deferred_variable_deletions: RefCell<BTreeSet<String>>,
    flushing_variable_deletions: Cell<bool>,
    scoped_request: RefCell<Option<ScopedMiRequest>>,
    scoped_queue: RefCell<VecDeque<ScopedMiRequest>>,
    outgoing: RefCell<OutgoingQueue>,
    event_handler: EventHandler,
    self_weak: Weak<Self>,
    connected: Cell<bool>,
    quarantined: Cell<bool>,
    transport_epoch: Cell<u64>,
    read_source: RefCell<Option<glib::SourceId>>,
    write_source: RefCell<Option<glib::SourceId>>,
    write_callback_active: Cell<bool>,
    timeout_source: RefCell<Option<glib::SourceId>>,
    discarding_oversized_line: Cell<bool>,
    oversized_record_header: Cell<Option<MiRecordHeader>>,
    thread_exit_since_prompt: Cell<bool>,
    unusable_reported: Cell<bool>,
}

impl MiClient {
    pub fn open(event_handler: impl Fn(&MiClient, MiEvent) + 'static) -> io::Result<Rc<Self>> {
        let factory: TransportFactory = Rc::new(open_transport);
        let transport = factory()?;

        Ok(Self::from_transport(transport, factory, event_handler))
    }

    fn from_transport(
        transport: MiTransport,
        transport_factory: TransportFactory,
        event_handler: impl Fn(&MiClient, MiEvent) + 'static,
    ) -> Rc<Self> {
        let client = Rc::new_cyclic(|self_weak| Self {
            transport_factory,
            transport: RefCell::new(transport),
            incoming: RefCell::new(Vec::new()),
            pending_input: RefCell::new(None),
            input_source: RefCell::new(None),
            next_token: Cell::new(1),
            printer_probe_generation: Cell::new(0),
            ready: Cell::new(false),
            initializing: Cell::new(false),
            capabilities: RefCell::new(GdbCapabilities::default()),
            pending: RefCell::new(HashMap::new()),
            deferred_variable_deletions: RefCell::new(BTreeSet::new()),
            flushing_variable_deletions: Cell::new(false),
            scoped_request: RefCell::new(None),
            scoped_queue: RefCell::new(VecDeque::new()),
            outgoing: RefCell::new(OutgoingQueue::default()),
            event_handler: Box::new(event_handler),
            self_weak: self_weak.clone(),
            connected: Cell::new(true),
            quarantined: Cell::new(false),
            transport_epoch: Cell::new(1),
            read_source: RefCell::new(None),
            write_source: RefCell::new(None),
            write_callback_active: Cell::new(false),
            timeout_source: RefCell::new(None),
            discarding_oversized_line: Cell::new(false),
            oversized_record_header: Cell::new(None),
            thread_exit_since_prompt: Cell::new(false),
            unusable_reported: Cell::new(false),
        });

        client.install_sources();

        client
    }

    #[cfg(test)]
    fn open_with_injected_transport(
        event_handler: impl Fn(&MiClient, MiEvent) + 'static,
    ) -> io::Result<(Rc<Self>, std::os::unix::net::UnixStream)> {
        let (transport, peer) = test_transport()?;

        let factory: TransportFactory =
            Rc::new(|| test_transport().map(|(transport, _)| transport));

        Ok((
            Self::from_transport(transport, factory, event_handler),
            peer,
        ))
    }

    pub fn slave_path(&self) -> PathBuf {
        self.transport.borrow().slave_path.clone()
    }

    pub fn is_ready(&self) -> bool {
        self.ready.get() && !self.quarantined.get()
    }

    pub(crate) fn weak(&self) -> Weak<Self> {
        self.self_weak.clone()
    }

    pub(crate) fn transport_epoch(&self) -> u64 {
        self.transport_epoch.get()
    }

    pub(crate) fn quarantine(&self, message: impl Into<String>) {
        self.report_unusable(message.into());
    }

    fn advance_transport_epoch(&self) -> u64 {
        let epoch = self.transport_epoch.get().wrapping_add(1);
        self.transport_epoch.set(epoch);

        epoch
    }

    pub fn capabilities(&self) -> GdbCapabilities {
        self.capabilities.borrow().clone()
    }

    pub fn reconnect(&self) -> io::Result<PathBuf> {
        self.advance_transport_epoch();
        self.remove_sources();

        self.printer_probe_generation
            .set(self.printer_probe_generation.get().wrapping_add(1));

        if self.connected.replace(false) {
            self.ready.set(false);
            self.initializing.set(false);

            self.outgoing.borrow_mut().clear();
            self.fail_pending_requests("GDB/MI connection replaced");
        }

        let transport = (self.transport_factory)()?;
        let slave_path = transport.slave_path.clone();
        self.transport.replace(transport);
        self.incoming.borrow_mut().clear();
        self.outgoing.borrow_mut().clear();
        self.discarding_oversized_line.set(false);
        self.oversized_record_header.set(None);
        self.thread_exit_since_prompt.set(false);
        self.unusable_reported.set(false);
        self.quarantined.set(false);
        self.ready.set(false);
        self.initializing.set(false);
        self.capabilities.replace(GdbCapabilities::default());
        self.connected.set(true);
        self.install_sources();

        Ok(slave_path)
    }

    fn install_sources(&self) {
        self.install_read_source();
        let weak_client = self.self_weak.clone();

        let timeout_source = glib::timeout_add_local(REQUEST_TIMEOUT_POLL, move || {
            let Some(client) = weak_client.upgrade() else {
                return glib::ControlFlow::Break;
            };

            client.expire_requests();
            glib::ControlFlow::Continue
        });
        self.timeout_source.replace(Some(timeout_source));
    }

    fn install_read_source(&self) {
        let master_fd = self.transport.borrow().master.as_raw_fd();
        let weak_client = self.self_weak.clone();

        let source = glib_unix::unix_fd_add_local(
            master_fd,
            glib::IOCondition::IN | glib::IOCondition::HUP | glib::IOCondition::ERR,
            move |_, condition| Self::on_io_ready(&weak_client, condition),
        );

        self.read_source.replace(Some(source));
    }

    fn remove_sources(&self) {
        for slot in [
            &self.read_source,
            &self.write_source,
            &self.timeout_source,
            &self.input_source,
        ] {
            if let Some(source) = slot.borrow_mut().take() {
                source.remove();
            }
        }
        self.pending_input.borrow_mut().take();
    }

    pub fn send(&self, command: &str) -> io::Result<u64> {
        self.request_inner(command, CommandClass::Execution, None, None, Box::new(|client, record| {
            if record.is_success() {
                return;
            }

            let message = record
                .error_message()
                .unwrap_or("GDB rejected the command")
                .to_owned();

            match record.class.as_str() {
                "timeout" => client.report_unusable(format!(
                    "GDB did not answer an execution command within {} seconds. Its target state is unknown.",
                    REQUEST_TIMEOUT.as_secs()
                )),
                "superseded" | "unavailable" => {}
                _ => (client.event_handler)(client, MiEvent::Error(message)),
            }
        }))
    }

    /// Submit a command. All request methods return Err without invoking the
    /// new handler, or Ok with exactly one terminal callback. Cancellation may
    /// complete synchronously with a synthetic `superseded` record.
    pub fn request(
        &self,
        command: &str,
        handler: impl FnOnce(&MiClient, MiRecord) + 'static,
    ) -> io::Result<u64> {
        self.request_inner(
            command,
            CommandClass::Control,
            None,
            None,
            Box::new(handler),
        )
    }

    pub fn request_when(
        &self,
        command: &str,
        is_current: impl Fn() -> bool + 'static,
        handler: impl FnOnce(&MiClient, MiRecord) + 'static,
    ) -> io::Result<u64> {
        self.request_inner(
            command,
            CommandClass::Inspection,
            None,
            Some(Box::new(is_current)),
            Box::new(handler),
        )
    }

    pub(crate) fn request_for_stop(
        &self,
        command: &str,
        generation: u64,
        is_current: impl Fn() -> bool + 'static,
        handler: impl FnOnce(&MiClient, MiRecord) + 'static,
    ) -> io::Result<u64> {
        self.request_inner(
            command,
            CommandClass::Inspection,
            Some(CommandOwner::Stop(generation)),
            Some(Box::new(is_current)),
            Box::new(handler),
        )
    }

    pub(crate) fn request_control_for_stop(
        &self,
        command: &str,
        generation: u64,
        is_current: impl Fn() -> bool + 'static,
        handler: impl FnOnce(&MiClient, MiRecord) + 'static,
    ) -> io::Result<u64> {
        self.request_inner(
            command,
            CommandClass::Control,
            Some(CommandOwner::Stop(generation)),
            Some(Box::new(is_current)),
            Box::new(handler),
        )
    }

    pub(crate) fn request_for_session(
        &self,
        command: &str,
        generation: u64,
        is_current: impl Fn() -> bool + 'static,
        handler: impl FnOnce(&MiClient, MiRecord) + 'static,
    ) -> io::Result<u64> {
        self.request_inner(
            command,
            CommandClass::Control,
            Some(CommandOwner::Session(generation)),
            Some(Box::new(is_current)),
            Box::new(handler),
        )
    }

    /// Release stale callbacks, but retain sent requests and their deadlines
    /// until GDB acknowledges them. Cancellation does not release wire credits.
    pub(crate) fn cancel_stale_stop_requests(&self, current_generation: u64) {
        let stale_tokens = self
            .pending
            .borrow()
            .iter()
            .filter_map(|(token, request)| {
                matches!(request.owner, Some(CommandOwner::Stop(generation)) if generation != current_generation)
                    .then_some(*token)
            })
            .collect::<Vec<_>>();

        for token in stale_tokens {
            self.cancel_pending_request(token);
        }

        let mut cancelled = Vec::new();

        {
            let mut queue = self.scoped_queue.borrow_mut();
            let mut retained = VecDeque::with_capacity(queue.len());

            while let Some(request) = queue.pop_front() {
                if matches!(request.owner, Some(CommandOwner::Stop(generation)) if generation != current_generation)
                {
                    cancelled.push(request);
                } else {
                    retained.push_back(request);
                }
            }

            *queue = retained;
        }

        for request in cancelled {
            (request.handler)(
                self,
                synthetic_error_record("superseded", "request superseded by a newer stop"),
                request.output,
            );
        }

        let active = self.scoped_request.borrow().as_ref().and_then(|request| {
            matches!(request.owner, Some(CommandOwner::Stop(generation)) if generation != current_generation)
                .then_some(request.token)
        });

        if let Some(token) = active {
            if self.outgoing.borrow_mut().cancel_unstarted(token) {
                let request = { self.scoped_request.borrow_mut().take() };

                if let Some(request) = request {
                    (request.handler)(
                        self,
                        synthetic_error_record("superseded", "request superseded by a newer stop"),
                        request.output,
                    );
                }

                self.start_next_scoped_request();
            } else if let Some(request) = self.scoped_request.borrow_mut().as_mut() {
                request.cancelled = true;
            }
        }

        self.dispatch_pending_requests();
        self.stop_write_source_if_idle();
    }

    fn request_inner(
        &self,
        command: &str,
        class: CommandClass,
        owner: Option<CommandOwner>,
        is_current: Option<Box<dyn Fn() -> bool>>,
        handler: ResponseHandler,
    ) -> io::Result<u64> {
        validate_mi_command(command)?;
        self.cancel_invalid_pending_requests();

        if is_current.as_ref().is_some_and(|is_current| !is_current()) {
            let token = self.allocate_token();
            handler(
                self,
                synthetic_error_record("superseded", "request superseded"),
            );
            return Ok(token);
        }

        if !self.connected.get() || self.quarantined.get() {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "GDB/MI connection is unavailable",
            ));
        }

        let pending_count = self.pending.borrow().len();

        let inspection_count = self
            .pending
            .borrow()
            .values()
            .filter(|request| request.class == CommandClass::Inspection)
            .count();

        if class == CommandClass::Inspection && inspection_count >= MAX_INSPECTION_REQUESTS {
            let message = format!(
                "inspection queue reached its {MAX_INSPECTION_REQUESTS}-request budget. Retry after the current refresh"
            );

            self.report_performance(PerformanceNotice {
                outcome: BudgetOutcome::Rejected,
                operation: command_operation(command),
                detail: message.clone(),
            });

            return Err(io::Error::new(io::ErrorKind::WouldBlock, message));
        }

        if class != CommandClass::Execution && pending_count >= MAX_NON_EXECUTION_REQUESTS {
            let message = format!(
                "non-execution queue reached its {MAX_NON_EXECUTION_REQUESTS}-request budget. Capacity is reserved for debugger execution controls"
            );

            self.report_performance(PerformanceNotice {
                outcome: BudgetOutcome::Rejected,
                operation: command_operation(command),
                detail: message.clone(),
            });

            return Err(io::Error::new(io::ErrorKind::WouldBlock, message));
        }

        if pending_count >= MAX_PENDING_REQUESTS {
            let message = format!(
                "MI queue reached its {MAX_PENDING_REQUESTS}-request hard budget. Retry after pending commands complete"
            );

            self.report_performance(PerformanceNotice {
                outcome: BudgetOutcome::Rejected,
                operation: command_operation(command),
                detail: message.clone(),
            });

            return Err(io::Error::new(io::ErrorKind::WouldBlock, message));
        }

        let queued_bytes = self
            .pending
            .borrow()
            .values()
            .filter_map(|request| request.command.as_ref())
            .fold(self.outgoing.borrow().remaining_bytes, |total, command| {
                total.saturating_add(command.len())
            });

        if queued_bytes.saturating_add(command.len()) > MAX_QUEUED_MI_BYTES {
            let message = String::from("queued GDB commands exceed the 8 MiB memory budget");

            self.report_performance(PerformanceNotice {
                outcome: BudgetOutcome::Rejected,
                operation: command_operation(command),
                detail: message.clone(),
            });

            return Err(io::Error::new(io::ErrorKind::WouldBlock, message));
        }

        let token = self.allocate_token();
        let now = Instant::now();

        self.pending.borrow_mut().insert(
            token,
            PendingRequest {
                class,
                owner,
                operation: command_operation(command),
                command: Some(command.to_owned()),
                queued_at: now,
                started_at: None,
                deadline: now + class.queue_timeout(),
                hard_deadline: now + class.maximum_lifetime(),
                is_current: is_current.map(Rc::from),
                handler: Some(handler),
            },
        );

        if self.can_dispatch(class, token)
            && let Err(error) = self.start_pending_request(token)
        {
            self.pending.borrow_mut().remove(&token);
            return Err(error);
        }

        self.dispatch_pending_requests();

        Ok(token)
    }

    fn can_dispatch(&self, class: CommandClass, token: u64) -> bool {
        if class == CommandClass::Execution {
            return true;
        }

        // Console streams have no token. A captured command must drain all
        // predecessors and exclude ordinary successors until its outer result.
        // Priority still applies between captures, so diagnostics cannot hold
        // a backlog of controls behind them.
        if self.scoped_request.borrow().is_some()
            || self.scoped_queue.borrow().front().is_some_and(|request| {
                (request.class.queue_priority(), request.token) < (class.queue_priority(), token)
            })
        {
            return false;
        }

        let pending = self.pending.borrow();

        let active = pending
            .values()
            .filter(|request| request.started_at.is_some() && request.class == class)
            .count();

        active
            < match class {
                CommandClass::Execution => usize::MAX,
                CommandClass::Control => MAX_ACTIVE_CONTROL_REQUESTS,
                CommandClass::Inspection => MAX_ACTIVE_INSPECTION_REQUESTS,
                CommandClass::Background => MAX_ACTIVE_BACKGROUND_REQUESTS,
            }
    }

    fn start_pending_request(&self, token: u64) -> io::Result<()> {
        let guard = self
            .pending
            .borrow()
            .get(&token)
            .and_then(|request| request.is_current.clone());

        if guard.is_some_and(|is_current| !is_current()) {
            self.cancel_pending_request(token);
            return Ok(());
        }

        let (class, command) = {
            let mut pending = self.pending.borrow_mut();

            let request = pending.get_mut(&token).ok_or_else(|| {
                io::Error::new(io::ErrorKind::NotFound, "queued GDB request disappeared")
            })?;

            let command = request.command.take().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "GDB request already dispatched",
                )
            })?;

            (request.class, command)
        };

        if let Err(error) = self.write_tokenized(token, class, &command) {
            if let Some(request) = self.pending.borrow_mut().get_mut(&token) {
                request.command = Some(command);
            }

            return Err(error);
        }

        // Interrupts must remain available during a captured query. Discard
        // its captured result only after execution is accepted by the queue.
        if class == CommandClass::Execution
            && let Some(request) = self.scoped_request.borrow_mut().as_mut()
        {
            request.cancelled = true;
        }

        let now = Instant::now();

        let notice = if let Some(request) = self.pending.borrow_mut().get_mut(&token) {
            request.started_at = Some(now);
            request.deadline = now + class.timeout();
            request.hard_deadline = now + class.maximum_lifetime();

            duration_notice(
                format!("{} queue wait", request.operation),
                now.saturating_duration_since(request.queued_at),
                MI_SCOPED_QUEUE_BUDGET,
            )
        } else {
            None
        };

        if let Some(notice) = notice {
            self.report_performance(notice);
        }

        Ok(())
    }

    fn dispatch_pending_requests(&self) {
        loop {
            self.start_next_scoped_request();
            let capture_active = self.scoped_request.borrow().is_some();
            let capture_priority = self
                .scoped_queue
                .borrow()
                .front()
                .map(|request| (request.class.queue_priority(), request.token));
            let next = {
                let pending = self.pending.borrow();

                let active_control = pending
                    .values()
                    .filter(|request| {
                        request.started_at.is_some() && request.class == CommandClass::Control
                    })
                    .count();

                let active_inspection = pending
                    .values()
                    .filter(|request| {
                        request.started_at.is_some() && request.class == CommandClass::Inspection
                    })
                    .count();

                let active_background = pending
                    .values()
                    .filter(|request| {
                        request.started_at.is_some() && request.class == CommandClass::Background
                    })
                    .count();

                pending
                    .iter()
                    .filter(|(token, request)| {
                        request.started_at.is_none()
                            && request.command.is_some()
                            && (request.class == CommandClass::Execution
                                || (!capture_active
                                    && capture_priority.is_none_or(|priority| {
                                        (request.class.queue_priority(), **token) < priority
                                    })))
                            && match request.class {
                                CommandClass::Execution => true,
                                CommandClass::Control => {
                                    active_control < MAX_ACTIVE_CONTROL_REQUESTS
                                }
                                CommandClass::Inspection => {
                                    active_inspection < MAX_ACTIVE_INSPECTION_REQUESTS
                                }
                                CommandClass::Background => {
                                    active_background < MAX_ACTIVE_BACKGROUND_REQUESTS
                                }
                            }
                    })
                    .min_by_key(|(token, request)| (request.class.queue_priority(), **token))
                    .map(|(token, _)| *token)
            };

            let Some(token) = next else {
                break;
            };

            if let Err(error) = self.start_pending_request(token) {
                let request = self.pending.borrow_mut().remove(&token);

                if let Some(request) = request {
                    request.complete(
                        self,
                        synthetic_error_record("unavailable", &error.to_string()),
                    );
                }

                if matches!(error.kind(), io::ErrorKind::BrokenPipe) {
                    break;
                }
            }
        }

        self.flush_variable_deletions();
    }

    pub(crate) fn delete_variable_object(&self, name: String) {
        if !self.connected.get() || self.quarantined.get() {
            return;
        }

        self.deferred_variable_deletions.borrow_mut().insert(name);
        self.flush_variable_deletions();
    }

    fn flush_variable_deletions(&self) {
        if !self.connected.get()
            || self.quarantined.get()
            || self.flushing_variable_deletions.replace(true)
        {
            return;
        }

        // Cleanup keeps ownership while ordinary admission is backpressured.
        // Use the background window so it cannot fill the execution reserve.
        while self.pending.borrow().len() < MAX_NON_EXECUTION_REQUESTS
            && self.can_dispatch(CommandClass::Background, self.next_token.get())
        {
            let Some(name) = self.deferred_variable_deletions.borrow_mut().pop_first() else {
                break;
            };
            let retry = name.clone();
            let epoch = self.transport_epoch.get();
            let command = format!("-var-delete {}", quote(&name));

            if self
                .request_inner(
                    &command,
                    CommandClass::Background,
                    None,
                    None,
                    Box::new(move |client, record| {
                        // GDB's own errors usually mean a parent already removed this
                        // child. Transport rejection before execution needs a retry.
                        if matches!(record.class.as_str(), "unavailable" | "timeout")
                            && client.transport_epoch.get() == epoch
                            && client.connected.get()
                            && !client.quarantined.get()
                        {
                            client
                                .deferred_variable_deletions
                                .borrow_mut()
                                .insert(retry);
                        }
                    }),
                )
                .is_err()
            {
                if self.transport_epoch.get() == epoch
                    && self.connected.get()
                    && !self.quarantined.get()
                {
                    self.deferred_variable_deletions.borrow_mut().insert(name);
                }
                break;
            }
        }

        self.flushing_variable_deletions.set(false);
    }

    fn cancel_invalid_pending_requests(&self) {
        loop {
            let guards = self
                .pending
                .borrow()
                .iter()
                .filter_map(|(token, request)| {
                    request.is_current.clone().map(|guard| (*token, guard))
                })
                .collect::<Vec<_>>();
            let mut cancelled = false;

            // Neither guards nor callbacks run under a pending-map borrow.
            for (token, is_current) in guards {
                if !is_current() {
                    self.cancel_pending_request(token);
                    cancelled = true;
                }
            }

            if !cancelled {
                return;
            }

            // A cancellation callback can invalidate a previously checked
            // request. Revalidate before returning to the write boundary.
        }
    }

    fn cancel_pending_request(&self, token: u64) {
        let unwritten = self.outgoing.borrow_mut().cancel_unstarted(token);
        let handler = {
            let mut pending = self.pending.borrow_mut();
            let Some(request) = pending.get_mut(&token) else {
                return;
            };
            let remove = unwritten || request.started_at.is_none();
            request.is_current = None;
            let handler = request.handler.take();

            if remove {
                pending.remove(&token);
            }

            handler
        };

        if let Some(handler) = handler {
            handler(
                self,
                synthetic_error_record("superseded", "request superseded"),
            );
        }
    }

    pub(crate) fn request_with_print_limit_for_stop(
        &self,
        command: &str,
        elements: usize,
        generation: u64,
        is_current: impl Fn() -> bool + 'static,
        handler: impl FnOnce(&MiClient, MiRecord) + 'static,
    ) -> io::Result<u64> {
        self.request_with_print_limit_for_owner(
            command,
            elements,
            Some(CommandOwner::Stop(generation)),
            is_current,
            handler,
        )
    }

    fn request_with_print_limit_for_owner(
        &self,
        command: &str,
        elements: usize,
        owner: Option<CommandOwner>,
        is_current: impl Fn() -> bool + 'static,
        handler: impl FnOnce(&MiClient, MiRecord) + 'static,
    ) -> io::Result<u64> {
        validate_mi_command(command)?;
        let operation = command_operation(command);
        let command = scoped_mi_command(command, elements);
        validate_mi_command(&command)?;
        let token = self.allocate_token();
        let class = CommandClass::Inspection;
        let now = Instant::now();

        let request = ScopedMiRequest {
            token,
            class,
            owner,
            operation,
            command,
            response: None,
            output: String::new(),
            expect_nested_mi: true,
            is_current: Rc::new(is_current),
            handler: Box::new(move |client, record, _| handler(client, record)),
            deadline: now + class.timeout(),
            hard_deadline: now + class.maximum_lifetime(),
            queued_at: now,
            started_at: None,
            cancelled: false,
            output_truncated: false,
        };

        self.queue_scoped_request(request)?;

        Ok(token)
    }

    /// Run a CLI command without allowing its un-tokened stream records to
    /// interleave with another captured command. The completion record remains
    /// authoritative; `output` is diagnostic text only.
    pub fn request_console(
        &self,
        command: &str,
        handler: impl FnOnce(&MiClient, MiRecord, String) + 'static,
    ) -> io::Result<u64> {
        self.request_console_when(command, || true, handler)
    }

    pub fn request_console_when(
        &self,
        command: &str,
        is_current: impl Fn() -> bool + 'static,
        handler: impl FnOnce(&MiClient, MiRecord, String) + 'static,
    ) -> io::Result<u64> {
        validate_console_command(command)?;
        let operation = console_operation(command);
        let command = format!("-interpreter-exec console {}", quote(command));
        validate_mi_command(&command)?;
        let token = self.allocate_token();
        let now = Instant::now();

        self.queue_scoped_request(ScopedMiRequest {
            token,
            class: CommandClass::Background,
            owner: None,
            operation,
            command,
            response: None,
            output: String::new(),
            expect_nested_mi: false,
            is_current: Rc::new(is_current),
            handler: Box::new(handler),
            deadline: now + CommandClass::Background.timeout(),
            hard_deadline: now + CommandClass::Background.maximum_lifetime(),
            queued_at: now,
            started_at: None,
            cancelled: false,
            output_truncated: false,
        })?;

        Ok(token)
    }

    /// Capture a fully encoded, explicitly thread/frame-scoped console command.
    pub(crate) fn request_console_for_stop(
        &self,
        command: &str,
        generation: u64,
        is_current: impl Fn() -> bool + 'static,
        handler: impl FnOnce(&MiClient, MiRecord, String) + 'static,
    ) -> io::Result<u64> {
        validate_mi_command(command)?;
        let token = self.allocate_token();
        let class = CommandClass::Inspection;
        let now = Instant::now();

        self.queue_scoped_request(ScopedMiRequest {
            token,
            class,
            owner: Some(CommandOwner::Stop(generation)),
            operation: command_operation(command),
            command: command.to_owned(),
            response: None,
            output: String::new(),
            expect_nested_mi: false,
            is_current: Rc::new(is_current),
            handler: Box::new(handler),
            deadline: now + class.timeout(),
            hard_deadline: now + class.maximum_lifetime(),
            queued_at: now,
            started_at: None,
            cancelled: false,
            output_truncated: false,
        })?;

        Ok(token)
    }

    fn queue_scoped_request(&self, request: ScopedMiRequest) -> io::Result<()> {
        self.cancel_invalid_scoped_queue();

        if !self.connected.get() || self.quarantined.get() {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "GDB/MI connection is unavailable",
            ));
        }

        let queued_requests = usize::from(self.scoped_request.borrow().is_some())
            .saturating_add(self.scoped_queue.borrow().len());

        if queued_requests >= MAX_SCOPED_REQUESTS {
            let message =
                format!("scoped MI queue reached its {MAX_SCOPED_REQUESTS}-request hard budget");

            self.report_performance(PerformanceNotice {
                outcome: BudgetOutcome::Rejected,
                operation: request.operation.clone(),
                detail: message.clone(),
            });

            return Err(io::Error::new(io::ErrorKind::WouldBlock, message));
        }

        let background_requests = usize::from(
            self.scoped_request
                .borrow()
                .as_ref()
                .is_some_and(|active| active.class == CommandClass::Background),
        ) + self
            .scoped_queue
            .borrow()
            .iter()
            .filter(|queued| queued.class == CommandClass::Background)
            .count();

        if request.class == CommandClass::Background
            && background_requests >= MAX_BACKGROUND_SCOPED_REQUESTS
        {
            let message = format!(
                "background queue reached its {MAX_BACKGROUND_SCOPED_REQUESTS}-request budget. Retry after queued diagnostics complete"
            );

            self.report_performance(PerformanceNotice {
                outcome: BudgetOutcome::Rejected,
                operation: request.operation.clone(),
                detail: message.clone(),
            });

            return Err(io::Error::new(io::ErrorKind::WouldBlock, message));
        }

        let queued_bytes = self
            .scoped_queue
            .borrow()
            .iter()
            .fold(0_usize, |total, queued| {
                total.saturating_add(queued.command.len())
            });

        if queued_bytes.saturating_add(request.command.len()) > MAX_QUEUED_MI_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "queued scoped GDB commands exceed the 8 MiB limit",
            ));
        }

        if !(request.is_current)() {
            (request.handler)(
                self,
                synthetic_error_record("superseded", "request superseded"),
                String::new(),
            );

            return Ok(());
        }

        {
            let position =
                self.scoped_queue.borrow().iter().position(|queued| {
                    queued.class.queue_priority() > request.class.queue_priority()
                });

            let mut queue = self.scoped_queue.borrow_mut();

            if let Some(position) = position {
                queue.insert(position, request);
            } else {
                queue.push_back(request);
            }
        }

        self.dispatch_pending_requests();

        Ok(())
    }

    fn cancel_invalid_scoped_queue(&self) {
        let guards = self
            .scoped_queue
            .borrow()
            .iter()
            .map(|request| (request.token, Rc::clone(&request.is_current)))
            .collect::<Vec<_>>();
        let stale_tokens = guards
            .into_iter()
            .filter_map(|(token, is_current)| (!is_current()).then_some(token))
            .collect::<HashSet<_>>();
        let mut stale = Vec::new();

        {
            let mut queue = self.scoped_queue.borrow_mut();
            let mut retained = VecDeque::with_capacity(queue.len());

            while let Some(request) = queue.pop_front() {
                if stale_tokens.contains(&request.token) {
                    stale.push(request);
                } else {
                    retained.push_back(request);
                }
            }

            *queue = retained;
        }

        for request in stale {
            (request.handler)(
                self,
                synthetic_error_record("superseded", "request superseded"),
                request.output,
            );
        }
    }

    fn start_scoped_request(
        &self,
        mut request: ScopedMiRequest,
    ) -> Result<(), Box<(io::Error, ScopedMiRequest)>> {
        // Queueing latency and GDB response latency are separate concerns. A
        // request that waited behind another scoped query still receives the
        // full response window once it reaches the command channel.
        let now = Instant::now();
        request.deadline = now + request.class.timeout();
        request.hard_deadline = now + request.class.maximum_lifetime();
        request.started_at = Some(now);

        let queue_notice = duration_notice(
            format!("{} queue wait", request.operation),
            now.saturating_duration_since(request.queued_at),
            MI_SCOPED_QUEUE_BUDGET,
        );

        if let Err(error) = self.write_tokenized(request.token, request.class, &request.command) {
            return Err(Box::new((error, request)));
        }

        // The encoded command is now owned by the output queue. Do not retain
        // a duplicate, potentially large allocation while waiting for GDB.
        request.command = String::new();
        self.scoped_request.replace(Some(request));

        if let Some(notice) = queue_notice {
            self.report_performance(notice);
        }

        Ok(())
    }

    fn start_next_scoped_request(&self) {
        loop {
            if self.scoped_request.borrow().is_some()
                || self
                    .pending
                    .borrow()
                    .values()
                    .any(|request| request.started_at.is_some())
            {
                return;
            }

            let priority = self
                .scoped_queue
                .borrow()
                .front()
                .map(|request| (request.class.queue_priority(), request.token));

            if priority.is_some_and(|priority| {
                self.pending
                    .borrow()
                    .iter()
                    .any(|(token, request)| (request.class.queue_priority(), *token) < priority)
            }) {
                return;
            }

            let request = { self.scoped_queue.borrow_mut().pop_front() };

            let Some(request) = request else {
                return;
            };

            if !(request.is_current)() {
                (request.handler)(
                    self,
                    synthetic_error_record("superseded", "request superseded"),
                    String::new(),
                );

                continue;
            }

            if let Err(failure) = self.start_scoped_request(request) {
                let (error, request) = *failure;

                (request.handler)(
                    self,
                    synthetic_error_record("unavailable", &error.to_string()),
                    String::new(),
                );
            } else {
                return;
            }
        }
    }

    fn allocate_token(&self) -> u64 {
        let mut token = self.next_token.get().max(1);

        loop {
            let in_use = self.pending.borrow().contains_key(&token)
                || self
                    .scoped_request
                    .borrow()
                    .as_ref()
                    .is_some_and(|request| request.token == token)
                || self
                    .scoped_queue
                    .borrow()
                    .iter()
                    .any(|request| request.token == token);

            if !in_use {
                self.next_token.set(token.wrapping_add(1).max(1));
                return token;
            }

            token = token.wrapping_add(1).max(1);
        }
    }

    fn write_tokenized(&self, token: u64, class: CommandClass, command: &str) -> io::Result<()> {
        if !self.connected.get() || self.quarantined.get() {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                if self.quarantined.get() {
                    "GDB/MI connection is quarantined"
                } else {
                    "GDB/MI connection is closed"
                },
            ));
        }

        self.outgoing
            .borrow_mut()
            .enqueue(token, class.queue_priority(), command)?;

        self.ensure_write_source();

        Ok(())
    }

    fn ensure_write_source(&self) {
        if self.write_callback_active.get()
            || self.write_source.borrow().is_some()
            || self.pending_input.borrow().is_some()
        {
            return;
        }

        let weak_client = self.self_weak.clone();
        let master_fd = self.transport.borrow().master.as_raw_fd();

        let source = glib_unix::unix_fd_add_local(
            master_fd,
            glib::IOCondition::OUT | glib::IOCondition::HUP | glib::IOCondition::ERR,
            move |_, condition| Self::on_write_ready(&weak_client, condition),
        );

        self.write_source.replace(Some(source));
    }

    fn stop_write_source_if_idle(&self) {
        if !self.write_callback_active.get()
            && self.outgoing.borrow().is_empty()
            && let Some(source) = self.write_source.borrow_mut().take()
        {
            source.remove();
        }
    }

    fn on_write_ready(weak_client: &Weak<Self>, condition: glib::IOCondition) -> glib::ControlFlow {
        let Some(client) = weak_client.upgrade() else {
            return glib::ControlFlow::Break;
        };

        // Request callbacks may cancel and enqueue writes synchronously. Keep
        // this source's ownership until its callback has finished, so a new
        // source cannot be mistaken for the one returning Break.
        client.write_callback_active.set(true);
        let result = Self::drain_write_ready(Rc::clone(&client), condition);
        client.write_callback_active.set(false);

        if client.connected.get()
            && !client.quarantined.get()
            && !client.outgoing.borrow().is_empty()
        {
            client.ensure_write_source();
        }
        result
    }

    fn drain_write_ready(client: Rc<Self>, condition: glib::IOCondition) -> glib::ControlFlow {
        let epoch = client.transport_epoch.get();

        loop {
            client.cancel_invalid_pending_requests();
            client.dispatch_pending_requests();

            if client.transport_epoch.get() != epoch {
                return glib::ControlFlow::Break;
            }

            let scoped_guard = client.scoped_request.borrow().as_ref().map(|request| {
                (
                    request.token,
                    request.cancelled,
                    Rc::clone(&request.is_current),
                )
            });

            if let Some((token, cancelled, is_current)) = scoped_guard
                && (cancelled || !is_current())
            {
                let unwritten = client.outgoing.borrow_mut().cancel_unstarted(token);

                if unwritten {
                    let request = client.scoped_request.borrow_mut().take();

                    if let Some(request) = request {
                        (request.handler)(
                            &client,
                            synthetic_error_record("superseded", "request superseded"),
                            request.output,
                        );
                    }

                    continue;
                }

                if let Some(request) = client.scoped_request.borrow_mut().as_mut() {
                    request.cancelled = true;
                }
            }

            break;
        }

        let result = {
            let mut transport = client.transport.borrow_mut();
            let mut outgoing = client.outgoing.borrow_mut();

            drain_outgoing(
                &mut transport.master,
                &mut outgoing,
                MAX_MI_WRITE_BATCH_BYTES,
            )
        };

        match result {
            Ok(true) => {
                client.write_source.borrow_mut().take();

                glib::ControlFlow::Break
            }
            Ok(false) if condition.intersects(glib::IOCondition::HUP | glib::IOCondition::ERR) => {
                client.write_source.borrow_mut().take();

                (client.event_handler)(
                    &client,
                    MiEvent::Error(String::from("GDB closed the MI command channel")),
                );

                if client.transport_epoch.get() == epoch {
                    client.disconnect(IoSource::Write)
                } else {
                    glib::ControlFlow::Break
                }
            }
            Ok(false) => glib::ControlFlow::Continue,
            Err(error) => {
                client.write_source.borrow_mut().take();

                (client.event_handler)(
                    &client,
                    MiEvent::Error(format!("Could not write a GDB/MI command: {error}")),
                );

                if client.transport_epoch.get() == epoch {
                    client.disconnect(IoSource::Write)
                } else {
                    glib::ControlFlow::Break
                }
            }
        }
    }

    fn on_io_ready(weak_client: &Weak<Self>, condition: glib::IOCondition) -> glib::ControlFlow {
        let Some(client) = weak_client.upgrade() else {
            return glib::ControlFlow::Break;
        };

        let epoch = client.transport_epoch.get();
        let started = Instant::now();
        let mut total = 0_usize;
        let mut bytes = [0_u8; MI_READ_CHUNK_BYTES];

        while total < MAX_MI_READ_BATCH_BYTES && started.elapsed() < MAX_MI_READ_BATCH_TIME {
            let remaining = MAX_MI_READ_BATCH_BYTES - total;
            let read_length = remaining.min(bytes.len());
            let read_result = {
                let mut transport = client.transport.borrow_mut();

                transport.master.read(&mut bytes[..read_length])
            };

            match read_result {
                Ok(0) => return client.disconnect(IoSource::Read),
                Ok(length) => {
                    total += length;
                    client.consume(&bytes[..length]);

                    // Event handlers may reconnect while consuming a record.
                    // The replacement transport owns a new readiness source.
                    if client.transport_epoch.get() != epoch
                        || !client.connected.get()
                        || client.pending_input.borrow().is_some()
                    {
                        return glib::ControlFlow::Break;
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    return if condition.intersects(glib::IOCondition::HUP | glib::IOCondition::ERR)
                    {
                        client.disconnect(IoSource::Read)
                    } else {
                        glib::ControlFlow::Continue
                    };
                }
                Err(_) => return client.disconnect(IoSource::Read),
            }
        }

        glib::ControlFlow::Continue
    }

    fn disconnect(&self, origin: IoSource) -> glib::ControlFlow {
        if !self.connected.replace(false) {
            return glib::ControlFlow::Break;
        }

        let epoch = self.advance_transport_epoch();
        self.ready.set(false);

        if let Some(source) = self.read_source.borrow_mut().take()
            && origin != IoSource::Read
        {
            source.remove();
        }

        if let Some(source) = self.write_source.borrow_mut().take()
            && origin != IoSource::Write
        {
            source.remove();
        }

        if let Some(source) = self.timeout_source.borrow_mut().take() {
            source.remove();
        }

        if let Some(source) = self.input_source.borrow_mut().take() {
            source.remove();
        }
        self.pending_input.borrow_mut().take();

        self.outgoing.borrow_mut().clear();
        self.fail_pending_requests("GDB/MI connection closed");

        // A pending callback is allowed to replace the transport. Do not let
        // the old channel publish Disconnected into that fresh connection.
        if self.transport_epoch.get() == epoch {
            (self.event_handler)(self, MiEvent::Disconnected);
        }

        glib::ControlFlow::Break
    }

    fn fail_pending_requests(&self, reason: &str) {
        self.deferred_variable_deletions.borrow_mut().clear();
        let pending = std::mem::take(&mut *self.pending.borrow_mut());
        let scoped = self.scoped_request.borrow_mut().take();
        let queued = self.scoped_queue.borrow_mut().drain(..).collect::<Vec<_>>();

        for request in pending.into_values() {
            request.complete(self, synthetic_error_record("unavailable", reason));
        }

        if let Some(request) = scoped {
            (request.handler)(
                self,
                synthetic_error_record("unavailable", reason),
                request.output,
            );
        }

        for request in queued {
            (request.handler)(
                self,
                synthetic_error_record("unavailable", reason),
                request.output,
            );
        }
    }

    fn consume(&self, bytes: &[u8]) {
        let epoch = self.transport_epoch.get();

        let bytes = if self.discarding_oversized_line.get() {
            let Some(newline) = bytes.iter().position(|byte| *byte == b'\n') else {
                return;
            };

            self.discarding_oversized_line.set(false);

            let header = self
                .oversized_record_header
                .replace(None)
                .unwrap_or_default();

            self.handle_oversized_record(header);

            if self.transport_epoch.get() != epoch {
                return;
            }

            &bytes[newline + 1..]
        } else {
            bytes
        };

        if bytes.is_empty() {
            return;
        }

        let complete = {
            let mut incoming = self.incoming.borrow_mut();
            let previous_len = incoming.len();
            incoming.extend_from_slice(bytes);

            // `incoming` retains only the unterminated suffix after each
            // consume pass, so a new record terminator can only occur in the
            // bytes just appended. Searching that suffix keeps assembly of a
            // large, chunked MI record linear instead of rescanning the full
            // accumulated record after every PTY read.
            let complete_end = complete_input_end(bytes).map(|end| previous_len + end);

            if complete_end.is_none() && incoming.len() > MAX_MI_RECORD_BYTES {
                self.oversized_record_header
                    .set(Some(mi_record_header(&incoming)));

                incoming.clear();
                self.discarding_oversized_line.set(true);
            }

            complete_end.map(|complete_end| {
                let remaining = incoming.split_off(complete_end);

                std::mem::replace(&mut *incoming, remaining)
            })
        };

        let Some(complete) = complete else {
            return;
        };

        if complete.len() > input::INLINE_INPUT_BYTES {
            self.defer_input(complete);
            return;
        }

        for line in complete.split(|byte| *byte == b'\n') {
            if self.transport_epoch.get() != epoch {
                break;
            }

            let line = line.strip_suffix(b"\r").unwrap_or(line);

            if line.is_empty() {
                continue;
            }

            if line.len() > MAX_MI_RECORD_BYTES {
                self.handle_oversized_record(mi_record_header(line));
                continue;
            }

            self.process_line(&String::from_utf8_lossy(line));
        }

        let mut incoming = self.incoming.borrow_mut();

        if incoming.capacity() > MAX_RETAINED_MI_INPUT_BYTES
            && incoming.len() < MAX_RETAINED_MI_INPUT_BYTES
        {
            incoming.shrink_to(MAX_RETAINED_MI_INPUT_BYTES);
        }
    }

    fn handle_oversized_record(&self, header: MiRecordHeader) {
        let detail = format!(
            "GDB/MI record exceeded the {} MiB parsing budget",
            MAX_MI_RECORD_BYTES / (1024 * 1024)
        );

        match (header.kind, header.token) {
            (Some(b'^'), Some(token)) => {
                self.reject_tokenized_result(token, &detail);
            }
            (Some(b'~' | b'&'), _) => {
                if let Some(request) = self.scoped_request.borrow_mut().as_mut() {
                    request.output_truncated = true;
                }

                self.report_performance(PerformanceNotice {
                    outcome: BudgetOutcome::Partial,
                    operation: String::from("GDB console output"),
                    detail,
                });
            }
            (Some(b'@'), _) => self.report_performance(PerformanceNotice {
                outcome: BudgetOutcome::Partial,
                operation: String::from("GDB target output"),
                detail,
            }),
            _ => self.report_unusable(format!(
                "{detail}. It was an asynchronous state record, so debugger synchronization can no longer be trusted."
            )),
        }
    }

    fn reject_tokenized_result(&self, token: u64, detail: &str) {
        self.outgoing.borrow_mut().cancel_unstarted(token);

        let scoped = self
            .scoped_request
            .borrow()
            .as_ref()
            .is_some_and(|request| request.token == token);

        if scoped {
            let request = { self.scoped_request.borrow_mut().take() };

            if let Some(request) = request {
                self.report_command_duration(
                    request.class,
                    &request.operation,
                    request.started_at.unwrap_or(request.queued_at),
                );

                self.report_performance(PerformanceNotice {
                    outcome: BudgetOutcome::Rejected,
                    operation: request.operation.clone(),
                    detail: detail.to_owned(),
                });

                (request.handler)(
                    self,
                    synthetic_error_record("resource-limit", detail),
                    request.output,
                );

                self.dispatch_pending_requests();
            }

            self.stop_write_source_if_idle();
            return;
        }

        let request = { self.pending.borrow_mut().remove(&token) };

        if let Some(request) = request {
            self.report_command_duration(
                request.class,
                &request.operation,
                request.started_at.unwrap_or(request.queued_at),
            );

            self.report_performance(PerformanceNotice {
                outcome: BudgetOutcome::Rejected,
                operation: request.operation.clone(),
                detail: detail.to_owned(),
            });

            request.complete(self, synthetic_error_record("resource-limit", detail));
            self.dispatch_pending_requests();
        } else {
            self.report_performance(PerformanceNotice {
                outcome: BudgetOutcome::Rejected,
                operation: String::from("late GDB/MI result"),
                detail: detail.to_owned(),
            });
        }

        self.stop_write_source_if_idle();
    }

    fn report_unusable(&self, message: String) {
        if !self.unusable_reported.replace(true) {
            let epoch = self.advance_transport_epoch();
            self.quarantined.set(true);
            self.ready.set(false);
            self.initializing.set(false);
            // Quarantine also retires callbacks that will return Break after
            // observing the new epoch. Clear their IDs before GLib destroys
            // them, otherwise a later restart tries to remove a dead source.
            self.remove_sources();
            self.outgoing.borrow_mut().clear();
            self.fail_pending_requests("GDB/MI connection quarantined");

            if self.transport_epoch.get() == epoch {
                (self.event_handler)(self, MiEvent::DebuggerUnusable(message));
            }
        }
    }

    fn report_performance(&self, notice: PerformanceNotice) {
        (self.event_handler)(self, MiEvent::Performance(notice));
    }

    fn report_command_duration(&self, class: CommandClass, operation: &str, started_at: Instant) {
        if let Some(notice) = duration_notice(
            operation,
            Instant::now().saturating_duration_since(started_at),
            class.performance_budget(),
        ) {
            self.report_performance(notice);
        }
    }

    fn expire_requests(&self) {
        let epoch = self.transport_epoch.get();
        let now = Instant::now();
        self.cancel_invalid_pending_requests();

        if self.transport_epoch.get() != epoch {
            return;
        }

        let expired = {
            let pending = self.pending.borrow();

            pending
                .iter()
                .filter_map(|(token, request)| {
                    (request.deadline <= now || request.hard_deadline <= now).then_some(*token)
                })
                .collect::<Vec<_>>()
        };

        let expired_tokens = expired.iter().copied().collect::<HashSet<_>>();

        let cancelled_before_write = self
            .outgoing
            .borrow_mut()
            .cancel_unstarted_many(&expired_tokens);

        for token in expired {
            let cancelled_before_write = cancelled_before_write.contains(&token);
            let request = { self.pending.borrow_mut().remove(&token) };

            if let Some(request) = request {
                let safe_to_forget = request.started_at.is_none() || cancelled_before_write;
                let command_class = request.class;

                request.complete(
                    self,
                    synthetic_error_record("timeout", "GDB request timed out"),
                );

                if self.transport_epoch.get() != epoch {
                    return;
                }

                if !safe_to_forget {
                    self.report_unusable(format!(
                        "GDB did not complete a {:?} command within its {}-second response budget. The command stream can no longer be synchronized safely.",
                        command_class,
                        command_class.timeout().as_secs()
                    ));

                    return;
                }
            }
        }

        self.dispatch_pending_requests();

        if self.transport_epoch.get() != epoch {
            return;
        }

        let scoped_state = self.scoped_request.borrow().as_ref().map(|request| {
            (
                request.token,
                request.cancelled,
                request.deadline <= now,
                request.hard_deadline <= now,
                Rc::clone(&request.is_current),
            )
        });

        if let Some((token, cancelled, idle_timed_out, lifetime_timed_out, is_current)) =
            scoped_state
        {
            let stale = cancelled || !is_current();
            let timed_out = idle_timed_out || lifetime_timed_out;

            // A scoped request wraps nested MI in `interpreter-exec`. Once any
            // part of it has reached GDB, its un-tokened console response must
            // be drained before another scoped request starts. Removing a sent
            // stale request here could otherwise attach its late nested result
            // to the next request.
            let cancelled_before_write =
                (stale || timed_out) && self.outgoing.borrow_mut().cancel_unstarted(token);

            if timed_out || (stale && cancelled_before_write) {
                let request = { self.scoped_request.borrow_mut().take() };

                let Some(request) = request else {
                    return;
                };

                let command_class = request.class;

                let (class, reason) = if lifetime_timed_out {
                    ("timeout", "GDB request exceeded its maximum lifetime")
                } else if idle_timed_out {
                    ("timeout", "GDB request stopped making progress")
                } else {
                    ("superseded", "request superseded")
                };

                (request.handler)(self, synthetic_error_record(class, reason), request.output);

                if self.transport_epoch.get() != epoch {
                    return;
                }

                if timed_out && !cancelled_before_write {
                    let message = if lifetime_timed_out {
                        format!(
                            "GDB retained one {:?} command for more than {} seconds. The MI command stream can no longer be synchronized safely.",
                            command_class,
                            command_class.maximum_lifetime().as_secs()
                        )
                    } else {
                        format!(
                            "GDB stopped making command progress for {} seconds. The MI command stream can no longer be synchronized safely.",
                            command_class.timeout().as_secs()
                        )
                    };

                    self.report_unusable(message);
                    return;
                }

                self.dispatch_pending_requests();
            }
        }

        self.stop_write_source_if_idle();
    }

    fn begin_initialization(&self) {
        self.capabilities.replace(GdbCapabilities::default());
        let weak_client = self.self_weak.clone();

        if self
            .request("-list-features", move |client, record| {
                if record.is_done() {
                    let mut capabilities = client.capabilities.borrow_mut();
                    capabilities.features_known = true;
                    capabilities.features = listed_features(&record);
                }

                client.detect_gdb_version();
            })
            .is_err()
            && let Some(client) = weak_client.upgrade()
        {
            client.detect_gdb_version();
        }
    }

    fn detect_gdb_version(&self) {
        let weak_client = self.self_weak.clone();

        if self
            .request(
                "-data-evaluate-expression \"$_gdb_major\"",
                move |client, record| {
                    if let Some(major) = record
                        .is_done()
                        .then(|| record.field("value"))
                        .flatten()
                        .and_then(MiValue::as_const)
                    {
                        client
                            .capabilities
                            .borrow_mut()
                            .set_version_component(major, false);
                    }

                    client.detect_gdb_minor_version();
                },
            )
            .is_err()
            && let Some(client) = weak_client.upgrade()
        {
            client.detect_gdb_minor_version();
        }
    }

    fn detect_gdb_minor_version(&self) {
        let weak_client = self.self_weak.clone();

        if self
            .request(
                "-data-evaluate-expression \"$_gdb_minor\"",
                move |client, record| {
                    if let Some(minor) = record
                        .is_done()
                        .then(|| record.field("value"))
                        .flatten()
                        .and_then(MiValue::as_const)
                    {
                        client
                            .capabilities
                            .borrow_mut()
                            .set_version_component(minor, true);
                    }

                    if client.capabilities.borrow().version.is_some() {
                        client.configure_mi_async();
                    } else {
                        client.detect_gdb_version_from_banner();
                    }
                },
            )
            .is_err()
            && let Some(client) = weak_client.upgrade()
        {
            client.detect_gdb_version_from_banner();
        }
    }

    fn detect_gdb_version_from_banner(&self) {
        let weak_client = self.self_weak.clone();

        if self
            .request_console("show version", move |client, record, output| {
                if record.is_done()
                    && let Some(version) = gdb_version_from_banner(&output)
                {
                    client.capabilities.borrow_mut().version = Some(version);
                }

                client.configure_mi_async();
            })
            .is_err()
            && let Some(client) = weak_client.upgrade()
        {
            client.configure_mi_async();
        }
    }

    fn configure_mi_async(&self) {
        let weak_client = self.self_weak.clone();

        if self
            .request("-gdb-set mi-async on", move |client, record| {
                if record.is_success() {
                    client.capabilities.borrow_mut().mi_async = true;
                    client.configure_pretty_printing();
                } else {
                    client.configure_legacy_target_async();
                }
            })
            .is_err()
            && let Some(client) = weak_client.upgrade()
        {
            client.configure_legacy_target_async();
        }
    }

    fn configure_legacy_target_async(&self) {
        let weak_client = self.self_weak.clone();

        if self
            .request("-gdb-set target-async on", move |client, record| {
                client.capabilities.borrow_mut().mi_async = record.is_success();
                client.configure_pretty_printing();
            })
            .is_err()
            && let Some(client) = weak_client.upgrade()
        {
            client.configure_pretty_printing();
        }
    }

    fn configure_pretty_printing(&self) {
        let weak_client = self.self_weak.clone();

        if self
            .request("-enable-pretty-printing", move |client, record| {
                let enabled = record.is_success();
                client.capabilities.borrow_mut().pretty_printing = enabled;

                if enabled {
                    client.probe_rust_pretty_printing(true);
                } else {
                    client.finish_initialization();
                }
            })
            .is_err()
            && let Some(client) = weak_client.upgrade()
        {
            client.finish_initialization();
        }
    }

    pub fn refresh_pretty_printer_capabilities(&self) {
        if self.is_ready() && self.capabilities.borrow().pretty_printing {
            self.probe_rust_pretty_printing(false);
        }
    }

    pub fn set_pretty_printing(
        &self,
        enabled: bool,
        handler: impl FnOnce(&MiClient, MiRecord) + 'static,
    ) -> io::Result<u64> {
        let command = if enabled {
            "-enable-pretty-printing"
        } else {
            "-disable-pretty-printing"
        };

        self.request(command, move |client, record| {
            if record.is_success() {
                {
                    let mut capabilities = client.capabilities.borrow_mut();
                    capabilities.pretty_printing = enabled;

                    if !enabled {
                        capabilities.rust_pretty_printing = false;
                    }
                }

                (client.event_handler)(client, MiEvent::CapabilitiesChanged(client.capabilities()));

                if enabled {
                    client.probe_rust_pretty_printing(false);
                }
            }

            handler(client, record);
        })
    }

    fn probe_rust_pretty_printing(&self, initializing: bool) {
        let generation = self.printer_probe_generation.get().wrapping_add(1);
        self.printer_probe_generation.set(generation);
        let command = format!("-interpreter-exec console {}", quote(RUST_PRINTER_PROBE));
        let weak_client = self.self_weak.clone();

        if self
            .request(&command, move |client, record| {
                if client.printer_probe_generation.get() != generation {
                    return;
                }

                client.finish_rust_printer_probe(generation, record.is_success(), initializing);
            })
            .is_err()
            && let Some(client) = weak_client.upgrade()
        {
            client.finish_rust_printer_probe(generation, false, initializing);
        }
    }

    fn finish_rust_printer_probe(&self, generation: u64, loaded: bool, initializing: bool) {
        if self.printer_probe_generation.get() != generation {
            return;
        }

        let changed = {
            let mut capabilities = self.capabilities.borrow_mut();
            let changed = capabilities.rust_pretty_printing != loaded;
            capabilities.rust_pretty_printing = loaded;

            changed
        };

        if initializing {
            self.finish_initialization();
        } else if changed {
            (self.event_handler)(self, MiEvent::CapabilitiesChanged(self.capabilities()));
        }
    }

    fn finish_initialization(&self) {
        if !self.connected.get() || self.quarantined.get() || self.ready.replace(true) {
            return;
        }

        self.initializing.set(false);
        (self.event_handler)(self, MiEvent::Ready(self.capabilities()));
    }

    fn process_line(&self, line: &str) {
        self.process_decoded_line(DecodedLine::parse(line.as_bytes()));
    }

    fn process_decoded_line(&self, line: DecodedLine) {
        if self.quarantined.get() {
            return;
        }

        let record = match line {
            DecodedLine::Prompt => {
                if !self.ready.get() && !self.initializing.replace(true) {
                    self.begin_initialization();
                } else if self.ready.get() && self.thread_exit_since_prompt.replace(false) {
                    (self.event_handler)(self, MiEvent::ThreadExitPrompt);
                }
                return;
            }
            DecodedLine::Stream {
                kind,
                output,
                nested,
            } => {
                if let Some(request) = self.scoped_request.borrow_mut().as_mut()
                    && !request.cancelled
                {
                    let command_output = matches!(kind, b'~' | b'&');
                    if command_output {
                        let now = Instant::now();
                        request.deadline =
                            (now + request.class.timeout()).min(request.hard_deadline);
                    }

                    if request.expect_nested_mi {
                        if let Some(response) = nested {
                            request.response = Some(response);
                        }
                    } else if command_output {
                        let remaining =
                            MAX_CAPTURED_CONSOLE_BYTES.saturating_sub(request.output.len());
                        let accepted = output.floor_char_boundary(remaining);
                        request.output.push_str(&output[..accepted]);
                        request.output_truncated |= accepted < output.len();
                    }
                }
                return;
            }
            DecodedLine::Invalid {
                header,
                error,
                state,
            } => {
                if header.kind == Some(b'^')
                    && let Some(token) = header.token
                {
                    self.reject_tokenized_result(
                        token,
                        &format!("GDB/MI result exceeded its structural budget: {error}"),
                    );
                } else if state {
                    self.report_unusable(String::from("GDB emitted a malformed MI state record. Debugger synchronization can no longer be trusted."));
                }
                return;
            }
            DecodedLine::Oversized(header) => {
                self.handle_oversized_record(header);
                return;
            }
            DecodedLine::Ignored => return,
            DecodedLine::Record(record) => record,
        };

        match record.kind {
            '^' => {
                let scoped = self
                    .scoped_request
                    .borrow()
                    .as_ref()
                    .is_some_and(|request| record.token == Some(request.token));

                if scoped {
                    let request = { self.scoped_request.borrow_mut().take() };

                    let Some(request) = request else {
                        return;
                    };

                    let mut response = if request.cancelled || !(request.is_current)() {
                        synthetic_error_record("superseded", "request superseded")
                    } else if request.expect_nested_mi {
                        request.response.unwrap_or_else(|| {
                            if record.is_done() {
                                error_record("scoped MI command returned no result")
                            } else {
                                record
                            }
                        })
                    } else {
                        record
                    };

                    self.report_command_duration(
                        request.class,
                        &request.operation,
                        request.started_at.unwrap_or(request.queued_at),
                    );

                    if request.output_truncated {
                        response.results.push(MiResult {
                            name: String::from("fgdb-output-truncated"),
                            value: MiValue::Const(MAX_CAPTURED_CONSOLE_BYTES.to_string()),
                        });

                        self.report_performance(PerformanceNotice {
                            outcome: BudgetOutcome::Partial,
                            operation: request.operation.clone(),
                            detail: format!(
                                "console output was capped at {} KiB. The activity view and parser received a partial result",
                                MAX_CAPTURED_CONSOLE_BYTES / 1024
                            ),
                        });
                    }

                    (request.handler)(self, response, request.output);
                    self.dispatch_pending_requests();
                    return;
                }

                let handler = record
                    .token
                    .and_then(|token| self.pending.borrow_mut().remove(&token));

                if let Some(request) = handler {
                    self.report_command_duration(
                        request.class,
                        &request.operation,
                        request.started_at.unwrap_or(request.queued_at),
                    );

                    request.complete(self, record);
                    self.dispatch_pending_requests();
                }
            }
            '*' if record.class == "running" => {
                let thread_id = record
                    .field("thread-id")
                    .and_then(MiValue::as_const)
                    .map(str::to_owned);

                (self.event_handler)(self, MiEvent::Running { thread_id });
            }
            '*' if record.class == "stopped" => {
                let reason = record
                    .field("reason")
                    .and_then(MiValue::as_const)
                    .map(str::to_owned);

                let signal_name = record
                    .field("signal-name")
                    .and_then(MiValue::as_const)
                    .map(str::to_owned);

                let signal_meaning = record
                    .field("signal-meaning")
                    .and_then(MiValue::as_const)
                    .map(str::to_owned);

                let address = record
                    .field("frame")
                    .and_then(MiValue::as_tuple)
                    .and_then(|frame| result_field(frame, "addr"))
                    .and_then(MiValue::as_const)
                    .map(str::to_owned);

                let frame_level = record
                    .field("frame")
                    .and_then(MiValue::as_tuple)
                    .and_then(|frame| result_field(frame, "level"))
                    .and_then(MiValue::as_const)
                    .and_then(|level| level.parse().ok());

                let thread_id = record
                    .field("thread-id")
                    .and_then(MiValue::as_const)
                    .map(str::to_owned);

                let group_id = record
                    .field("thread-group")
                    .or_else(|| record.field("thread-group-id"))
                    .and_then(MiValue::as_const)
                    .map(str::to_owned);

                let fork_pid = record
                    .field("newpid")
                    .and_then(MiValue::as_const)
                    .and_then(|pid| pid.parse().ok());

                let all_stopped =
                    record.field("stopped-threads").and_then(MiValue::as_const) == Some("all");

                (self.event_handler)(
                    self,
                    MiEvent::Stopped {
                        reason,
                        signal_name,
                        signal_meaning,
                        address,
                        thread_id,
                        group_id,
                        frame_level,
                        fork_pid,
                        all_stopped,
                    },
                );
            }
            '=' if record.class.starts_with("breakpoint-") => {
                (self.event_handler)(self, MiEvent::BreakpointsChanged);
            }
            '=' if record.class == "thread-created" => {
                let id = record
                    .field("id")
                    .and_then(MiValue::as_const)
                    .map(str::to_owned);

                let group_id = record
                    .field("group-id")
                    .and_then(MiValue::as_const)
                    .map(str::to_owned);

                (self.event_handler)(self, MiEvent::ThreadsChanged { id, group_id });
            }
            '=' if record.class == "thread-exited" => {
                let Some(id) = record
                    .field("id")
                    .and_then(MiValue::as_const)
                    .map(str::to_owned)
                else {
                    return;
                };

                let group_id = record
                    .field("group-id")
                    .and_then(MiValue::as_const)
                    .map(str::to_owned);

                self.thread_exit_since_prompt.set(true);
                (self.event_handler)(self, MiEvent::ThreadExited { id, group_id });
            }
            '=' if record.class == "thread-group-started" => {
                let Some(id) = record
                    .field("id")
                    .and_then(MiValue::as_const)
                    .map(str::to_owned)
                else {
                    return;
                };

                let pid = record
                    .field("pid")
                    .and_then(MiValue::as_const)
                    .and_then(|pid| pid.parse().ok());

                (self.event_handler)(self, MiEvent::InferiorStarted { id, pid });
            }
            '=' if record.class == "thread-group-exited" => {
                let Some(id) = record
                    .field("id")
                    .and_then(MiValue::as_const)
                    .map(str::to_owned)
                else {
                    return;
                };

                let exit_code = record
                    .field("exit-code")
                    .and_then(MiValue::as_const)
                    .map(str::to_owned);

                (self.event_handler)(self, MiEvent::InferiorExited { id, exit_code });
            }

            '=' if matches!(
                record.class.as_str(),
                "thread-group-added" | "thread-group-removed"
            ) =>
            {
                (self.event_handler)(self, MiEvent::InferiorsChanged);
            }
            '=' if matches!(record.class.as_str(), "library-loaded" | "library-unloaded") => {
                let group_id = record
                    .field("thread-group")
                    .and_then(MiValue::as_const)
                    .map(str::to_owned);

                (self.event_handler)(self, MiEvent::LibrariesChanged { group_id });
            }
            '=' if record.class == "thread-selected" => {
                let thread_id = record
                    .field("id")
                    .and_then(MiValue::as_const)
                    .map(str::to_owned);

                let frame_level = record
                    .field("frame")
                    .and_then(MiValue::as_tuple)
                    .and_then(|frame| result_field(frame, "level"))
                    .and_then(MiValue::as_const)
                    .and_then(|level| level.parse().ok());

                (self.event_handler)(
                    self,
                    MiEvent::SelectionChanged {
                        thread_id,
                        group_id: None,
                        frame_level,
                    },
                );
            }
            '=' if record.class == "thread-group-selected" => {
                let group_id = record
                    .field("id")
                    .and_then(MiValue::as_const)
                    .map(str::to_owned);

                (self.event_handler)(
                    self,
                    MiEvent::SelectionChanged {
                        thread_id: None,
                        group_id,
                        frame_level: None,
                    },
                );
            }
            '=' if record.class == "cmd-param-changed" => {
                let Some(parameter) = record
                    .field("param")
                    .and_then(MiValue::as_const)
                    .map(str::to_owned)
                else {
                    return;
                };

                let value = record
                    .field("value")
                    .and_then(MiValue::as_const)
                    .map(str::to_owned);

                (self.event_handler)(self, MiEvent::CommandParameterChanged { parameter, value });
            }
            _ => {}
        }
    }
}

fn listed_features(record: &MiRecord) -> Vec<String> {
    let mut features = record
        .field("features")
        .and_then(MiValue::as_list)
        .into_iter()
        .flatten()
        .filter_map(|item| match item {
            MiListItem::Value(MiValue::Const(feature)) => Some(feature.clone()),
            MiListItem::Value(MiValue::Tuple(_) | MiValue::List(_)) | MiListItem::Result(_) => None,
        })
        .collect::<Vec<_>>();

    features.sort_unstable();
    features.dedup();

    features
}

fn gdb_version_from_banner(output: &str) -> Option<String> {
    output.lines().find_map(|line| {
        if !line.to_ascii_lowercase().contains("gdb") {
            return None;
        }

        line.split_whitespace().rev().find_map(|word| {
            let version = word
                .trim_matches(|character: char| !character.is_ascii_alphanumeric())
                .chars()
                .take_while(|character| character.is_ascii_digit() || *character == '.')
                .collect::<String>();

            let version = version.trim_end_matches('.');

            (!version.is_empty()
                && version.split('.').all(|component| {
                    !component.is_empty() && component.bytes().all(|b| b.is_ascii_digit())
                }))
            .then(|| version.to_owned())
        })
    })
}

fn command_operation(command: &str) -> String {
    command
        .split_ascii_whitespace()
        .next()
        .filter(|operation| operation.starts_with('-'))
        .unwrap_or("GDB/MI command")
        .to_owned()
}

fn console_operation(command: &str) -> String {
    let operation = command
        .split_ascii_whitespace()
        .next()
        .filter(|operation| !operation.is_empty())
        .unwrap_or("command");

    format!("GDB console `{operation}`")
}

fn mi_record_header(line: &[u8]) -> MiRecordHeader {
    let token_end = line
        .iter()
        .position(|byte| !byte.is_ascii_digit())
        .unwrap_or(line.len());

    let token = (token_end > 0)
        .then(|| std::str::from_utf8(&line[..token_end]).ok()?.parse().ok())
        .flatten();

    MiRecordHeader {
        token,
        kind: line.get(token_end).copied(),
    }
}

fn looks_like_mi_record(line: &str) -> bool {
    let marker = line.bytes().find(|byte| !byte.is_ascii_digit());

    matches!(marker, Some(b'^' | b'*' | b'+' | b'='))
}

impl Drop for MiClient {
    fn drop(&mut self) {
        self.remove_sources();
    }
}

#[cfg(test)]
mod tests {
    use super::{
        GdbCapabilities, MiListItem, MiValue, OutgoingQueue, complete_input_end, drain_outgoing,
        gdb_version_from_banner, listed_features, parse_record, parse_stream_output, quote,
        result_field, scoped_mi_command, validate_mi_command,
    };
    use std::sync::Mutex;

    pub(super) static MI_CLIENT_TEST_LOCK: Mutex<()> = Mutex::new(());
    struct BackpressuredWriter;

    impl std::io::Write for BackpressuredWriter {
        fn write(&mut self, _buffer: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::from(std::io::ErrorKind::WouldBlock))
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn parses_and_sorts_gdb_feature_negotiation() {
        let record =
            parse_record(r#"2^done,features=["thread-info","pending-breakpoints","thread-info"]"#)
                .unwrap();

        assert_eq!(
            listed_features(&record),
            ["pending-breakpoints", "thread-info"]
        );

        let capabilities = GdbCapabilities {
            version: Some(String::from("17.2")),
            features_known: true,
            features: listed_features(&record),
            mi_async: true,
            pretty_printing: true,
            rust_pretty_printing: true,
        };

        assert!(capabilities.supports("thread-info"));
        assert!(!capabilities.supports("data-read-memory-bytes"));

        assert_eq!(
            capabilities.compatibility_summary(),
            "GDB 17.2 · MI async · pretty printers · Rust printers · feature list"
        );

        assert!(GdbCapabilities::default().supports("future-mi-command"));
    }

    #[test]
    fn extracts_versions_from_standard_and_packaged_gdb_banners() {
        assert_eq!(
            gdb_version_from_banner("GNU gdb (GDB) 17.2\nCopyright (C) 2025"),
            Some(String::from("17.2"))
        );

        assert_eq!(
            gdb_version_from_banner(
                "GNU gdb (Ubuntu 15.0.50.20240403-0ubuntu1) 15.0.50.20240403-git"
            ),
            Some(String::from("15.0.50.20240403"))
        );

        assert_eq!(gdb_version_from_banner("unrecognized debugger"), None);
    }

    #[test]
    fn replaces_the_transport_without_replacing_the_client() {
        let _guard = MI_CLIENT_TEST_LOCK.lock().unwrap();
        let context = gtk::glib::MainContext::new();

        context
            .with_thread_default(|| {
                let client = super::MiClient::open(|_, _| {}).unwrap();
                let first = client.slave_path();
                let second = client.reconnect().unwrap();
                assert_ne!(first, second);
                assert_eq!(client.slave_path(), second);
            })
            .unwrap();
    }

    #[test]
    fn injected_transport_runs_a_deterministic_request_transcript() {
        use std::{
            cell::RefCell,
            io::{Read, Write},
            rc::Rc,
        };

        let _guard = MI_CLIENT_TEST_LOCK.lock().unwrap();
        let context = gtk::glib::MainContext::new();

        context
            .with_thread_default(|| {
                let result = Rc::new(RefCell::new(None));
                let result_for_request = Rc::clone(&result);

                let (client, mut peer) =
                    super::MiClient::open_with_injected_transport(|_, _| {}).unwrap();

                let token = client
                    .request("-thread-info", move |_, record| {
                        result_for_request.replace(Some(record.class));
                    })
                    .unwrap();

                super::MiClient::on_write_ready(&client.weak(), gtk::glib::IOCondition::OUT);
                let mut command = [0_u8; 128];
                let count = peer.read(&mut command).unwrap();

                assert_eq!(
                    std::str::from_utf8(&command[..count]).unwrap(),
                    format!("{token}-thread-info\n")
                );

                peer.write_all(format!("{token}^done\n").as_bytes())
                    .unwrap();

                super::MiClient::on_io_ready(&client.weak(), gtk::glib::IOCondition::IN);
                assert_eq!(result.borrow().as_deref(), Some("done"));
            })
            .unwrap();
    }

    #[test]
    fn one_readiness_callback_drains_records_across_chunk_boundaries() {
        use std::{cell::RefCell, io::Write, rc::Rc};

        let _guard = MI_CLIENT_TEST_LOCK.lock().unwrap();
        let context = gtk::glib::MainContext::new();

        context
            .with_thread_default(|| {
                let result = Rc::new(RefCell::new(None));
                let result_for_request = Rc::clone(&result);
                let (client, mut peer) =
                    super::MiClient::open_with_injected_transport(|_, _| {}).unwrap();

                let token = client
                    .request("-thread-info", move |_, record| {
                        result_for_request.replace(Some(record.class));
                    })
                    .unwrap();

                let padding = "x".repeat(super::MI_READ_CHUNK_BYTES + 512);
                let transcript = format!("~\"{padding}\"\n{token}^done\n");
                peer.write_all(transcript.as_bytes()).unwrap();

                super::MiClient::on_io_ready(&client.weak(), gtk::glib::IOCondition::IN);

                assert_eq!(result.borrow().as_deref(), Some("done"));
                assert!(client.incoming.borrow().is_empty());
            })
            .unwrap();
    }

    #[test]
    fn reconnect_clears_timeout_quarantine_before_accepting_new_work() {
        let _guard = MI_CLIENT_TEST_LOCK.lock().unwrap();
        let context = gtk::glib::MainContext::new();

        context
            .with_thread_default(|| {
                let client = super::MiClient::open(|_, _| {}).unwrap();
                client.ready.set(true);
                client.quarantine("simulated timed-out command");
                assert!(!client.is_ready());
                assert!(client.request("-thread-info", |_, _| {}).is_err());
                client.reconnect().unwrap();
                assert!(!client.quarantined.get());
                assert!(client.connected.get());
                assert!(!client.is_ready());
                client.ready.set(true);
                assert!(client.request("-thread-info", |_, _| {}).is_ok());
            })
            .unwrap();
    }

    #[test]
    fn reconnect_does_not_publish_ready_from_an_old_printer_probe() {
        use std::{cell::RefCell, rc::Rc};

        let _guard = MI_CLIENT_TEST_LOCK.lock().unwrap();
        let context = gtk::glib::MainContext::new();

        context
            .with_thread_default(|| {
                let events = Rc::new(RefCell::new(Vec::new()));
                let events_for_client = Rc::clone(&events);

                let client = super::MiClient::open(move |_, event| {
                    events_for_client.borrow_mut().push(event);
                })
                .unwrap();
                client.process_line("(gdb)");
                client.process_line(r#"1^done,features=[]"#);
                client.process_line(r#"2^done,value="17""#);
                client.process_line(r#"3^done,value="2""#);
                client.process_line("4^done");
                client.process_line("5^done");
                assert!(events.borrow().is_empty());
                client.reconnect().unwrap();
                assert!(events.borrow().is_empty());
                assert!(!client.is_ready());
            })
            .unwrap();
    }

    #[test]
    fn publishes_ready_only_after_capability_negotiation() {
        use std::{cell::RefCell, rc::Rc};

        let _guard = MI_CLIENT_TEST_LOCK.lock().unwrap();
        let context = gtk::glib::MainContext::new();

        context
            .with_thread_default(|| {
                let events = Rc::new(RefCell::new(Vec::new()));
                let events_for_client = Rc::clone(&events);

                let client = super::MiClient::open(move |_, event| {
                    events_for_client.borrow_mut().push(event);
                })
                .unwrap();
                client.process_line("(gdb)");
                assert!(events.borrow().is_empty());

                client.process_line(
                    r#"1^done,features=["pending-breakpoints","data-read-memory-bytes"]"#,
                );

                client.process_line(r#"2^done,value="17""#);
                client.process_line(r#"3^done,value="2""#);
                client.process_line("4^done");
                client.process_line("5^done");
                client.process_line("6^done");
                let events = events.borrow();

                let [super::MiEvent::Ready(capabilities)] = events.as_slice() else {
                    panic!("expected one negotiated ready event, got {events:?}");
                };

                assert_eq!(capabilities.version.as_deref(), Some("17.2"));
                assert!(capabilities.mi_async);
                assert!(capabilities.pretty_printing);
                assert!(capabilities.rust_pretty_printing);
                assert!(capabilities.supports("pending-breakpoints"));
                assert!(!capabilities.supports("thread-info"));
            })
            .unwrap();
    }

    #[test]
    fn remains_ready_when_rust_printer_probing_is_unavailable() {
        use std::{cell::RefCell, rc::Rc};

        let _guard = MI_CLIENT_TEST_LOCK.lock().unwrap();
        let context = gtk::glib::MainContext::new();

        context
            .with_thread_default(|| {
                let events = Rc::new(RefCell::new(Vec::new()));
                let events_for_client = Rc::clone(&events);

                let client = super::MiClient::open(move |_, event| {
                    events_for_client.borrow_mut().push(event);
                })
                .unwrap();
                client.process_line("(gdb)");
                client.process_line(r#"1^done,features=[]"#);
                client.process_line(r#"2^done,value="17""#);
                client.process_line(r#"3^done,value="2""#);
                client.process_line("4^done");
                client.process_line("5^done");
                client.process_line(r#"6^error,msg="Python is unavailable""#);
                let events = events.borrow();

                let [super::MiEvent::Ready(capabilities)] = events.as_slice() else {
                    panic!("expected a negotiated ready event, got {events:?}");
                };

                assert!(capabilities.pretty_printing);
                assert!(!capabilities.rust_pretty_printing);
            })
            .unwrap();
    }

    #[test]
    fn publishes_when_rust_printer_availability_changes() {
        use std::{cell::RefCell, rc::Rc};

        let _guard = MI_CLIENT_TEST_LOCK.lock().unwrap();
        let context = gtk::glib::MainContext::new();

        context
            .with_thread_default(|| {
                let events = Rc::new(RefCell::new(Vec::new()));
                let events_for_client = Rc::clone(&events);

                let client = super::MiClient::open(move |_, event| {
                    events_for_client.borrow_mut().push(event);
                })
                .unwrap();
                client.ready.set(true);
                client.capabilities.borrow_mut().pretty_printing = true;
                client.refresh_pretty_printer_capabilities();
                client.process_line("1^done");
                let events = events.borrow();

                let [super::MiEvent::CapabilitiesChanged(capabilities)] = events.as_slice() else {
                    panic!("expected one capability update, got {events:?}");
                };

                assert!(capabilities.rust_pretty_printing);
            })
            .unwrap();
    }

    #[test]
    fn publishes_process_scoped_async_events() {
        use std::{cell::RefCell, rc::Rc};

        let _guard = MI_CLIENT_TEST_LOCK.lock().unwrap();
        let context = gtk::glib::MainContext::new();

        context
            .with_thread_default(|| {
                let events = Rc::new(RefCell::new(Vec::new()));
                let events_for_client = Rc::clone(&events);

                let client = super::MiClient::open(move |_, event| {
                    events_for_client.borrow_mut().push(event);
                })
                .unwrap();
                client.ready.set(true);
                client.process_line(r#"=thread-group-started,id="i2",pid="4312""#);
                client.process_line(r#"=thread-created,id="3",group-id="i2""#);
                client.process_line(r#"=thread-exited,id="4",group-id="i2""#);
                client.process_line(r#"=library-loaded,id="libc",thread-group="i2""#);

                client.process_line(
                    r#"=thread-selected,id="3",frame={level="2",addr="0x401004"}"#,
                );

                client.process_line(r#"=thread-group-selected,id="i2""#);
                client.process_line(r#"*running,thread-id="all""#);

                client.process_line(
                    r#"*stopped,reason="fork",newpid="4313",thread-id="3",thread-group="i2",stopped-threads="all",frame={level="0",addr="0x401000"}"#,
                );

                client.process_line(r#"=thread-group-exited,id="i2",exit-code="0""#);
                client.process_line("(gdb)");

                assert_eq!(
                    events.borrow().as_slice(),
                    [
                        super::MiEvent::InferiorStarted {
                            id: String::from("i2"),
                            pid: Some(4312),
                        },
                        super::MiEvent::ThreadsChanged {
                            id: Some(String::from("3")),
                            group_id: Some(String::from("i2")),
                        },
                        super::MiEvent::ThreadExited {
                            id: String::from("4"),
                            group_id: Some(String::from("i2")),
                        },
                        super::MiEvent::LibrariesChanged {
                            group_id: Some(String::from("i2")),
                        },
                        super::MiEvent::SelectionChanged {
                            thread_id: Some(String::from("3")),
                            group_id: None,
                            frame_level: Some(2),
                        },
                        super::MiEvent::SelectionChanged {
                            thread_id: None,
                            group_id: Some(String::from("i2")),
                            frame_level: None,
                        },
                        super::MiEvent::Running {
                            thread_id: Some(String::from("all")),
                        },
                        super::MiEvent::Stopped {
                            reason: Some(String::from("fork")),
                            signal_name: None,
                            signal_meaning: None,
                            address: Some(String::from("0x401000")),
                            thread_id: Some(String::from("3")),
                            group_id: Some(String::from("i2")),
                            frame_level: Some(0),
                            fork_pid: Some(4313),
                            all_stopped: true,
                        },
                        super::MiEvent::InferiorExited {
                            id: String::from("i2"),
                            exit_code: Some(String::from("0")),
                        },
                        super::MiEvent::ThreadExitPrompt,
                    ]
                );
            })
            .unwrap();
    }

    #[test]
    fn publishes_structured_command_parameter_changes() {
        use std::{cell::RefCell, rc::Rc};

        let _guard = MI_CLIENT_TEST_LOCK.lock().unwrap();
        let context = gtk::glib::MainContext::new();

        context
            .with_thread_default(|| {
                let events = Rc::new(RefCell::new(Vec::new()));
                let events_for_client = Rc::clone(&events);

                let client = super::MiClient::open(move |_, event| {
                    events_for_client.borrow_mut().push(event);
                })
                .unwrap();
                client.ready.set(true);
                client.process_line(r#"=cmd-param-changed,param="scheduler-locking",value="step""#);

                assert_eq!(
                    events.borrow().as_slice(),
                    [super::MiEvent::CommandParameterChanged {
                        parameter: String::from("scheduler-locking"),
                        value: Some(String::from("step")),
                    }]
                );
            })
            .unwrap();
    }

    #[test]
    fn console_prose_does_not_drive_debugger_state() {
        use std::{cell::RefCell, rc::Rc};

        let _guard = MI_CLIENT_TEST_LOCK.lock().unwrap();
        let context = gtk::glib::MainContext::new();

        context
            .with_thread_default(|| {
                let events = Rc::new(RefCell::new(Vec::new()));
                let events_for_client = Rc::clone(&events);

                let client = super::MiClient::open(move |_, event| {
                    events_for_client.borrow_mut().push(event);
                })
                .unwrap();
                client.ready.set(true);
                let warning = r#"~"Cannot remove breakpoints because program is no longer writable.\nFurther execution is probably impossible.\n""#;
                client.process_line(warning);
                client.process_line(warning);
                client.process_line("(gdb)");
                assert!(events.borrow().is_empty());
                assert!(client.is_ready());
                assert!(client.request("-thread-info", |_, _| {}).is_ok());
            })
            .unwrap();
    }

    #[test]
    fn quarantine_fails_callbacks_before_publishing_the_terminal_event() {
        use std::{cell::RefCell, rc::Rc};

        let _guard = MI_CLIENT_TEST_LOCK.lock().unwrap();
        let context = gtk::glib::MainContext::new();

        context
            .with_thread_default(|| {
                let order = Rc::new(RefCell::new(Vec::new()));
                let order_for_events = Rc::clone(&order);

                let client = super::MiClient::open(move |_, event| {
                    if matches!(event, super::MiEvent::DebuggerUnusable(_)) {
                        order_for_events.borrow_mut().push("event");
                    }
                })
                .unwrap();
                let order_for_request = Rc::clone(&order);

                client
                    .request("-thread-info", move |_, record| {
                        assert_eq!(record.class, "unavailable");
                        order_for_request.borrow_mut().push("callback");
                    })
                    .unwrap();

                client.quarantine("test quarantine");
                assert_eq!(order.borrow().as_slice(), ["callback", "event"]);
                assert!(!client.is_ready());
                assert!(client.request("-thread-info", |_, _| {}).is_err());
            })
            .unwrap();
    }

    #[test]
    fn an_unwritten_timed_out_request_is_cancelled_without_quarantining_gdb() {
        use std::{cell::RefCell, rc::Rc, time::Instant};

        let _guard = MI_CLIENT_TEST_LOCK.lock().unwrap();
        let context = gtk::glib::MainContext::new();

        context
            .with_thread_default(|| {
                let events = Rc::new(RefCell::new(Vec::new()));
                let events_for_client = Rc::clone(&events);

                let client = super::MiClient::open(move |_, event| {
                    events_for_client.borrow_mut().push(event);
                })
                .unwrap();
                client.ready.set(true);
                let response_class = Rc::new(RefCell::new(None));
                let response_class_for_request = Rc::clone(&response_class);

                let token = client
                    .request("-thread-info", move |_, record| {
                        response_class_for_request.replace(Some(record.class));
                    })
                    .unwrap();

                client
                    .pending
                    .borrow_mut()
                    .get_mut(&token)
                    .unwrap()
                    .deadline = Instant::now();

                client.expire_requests();
                assert_eq!(response_class.borrow().as_deref(), Some("timeout"));
                assert!(client.is_ready());
                assert!(events.borrow().is_empty());
            })
            .unwrap();
    }

    #[test]
    fn a_sent_timed_out_request_quarantines_the_command_stream() {
        use std::{cell::RefCell, rc::Rc, time::Instant};

        let _guard = MI_CLIENT_TEST_LOCK.lock().unwrap();
        let context = gtk::glib::MainContext::new();

        context
            .with_thread_default(|| {
                let events = Rc::new(RefCell::new(Vec::new()));
                let events_for_client = Rc::clone(&events);

                let client = super::MiClient::open(move |_, event| {
                    events_for_client.borrow_mut().push(event);
                })
                .unwrap();
                client.ready.set(true);
                let token = client.request("-thread-info", |_, _| {}).unwrap();
                client.outgoing.borrow_mut().clear();

                client
                    .pending
                    .borrow_mut()
                    .get_mut(&token)
                    .unwrap()
                    .deadline = Instant::now();

                client.expire_requests();
                assert!(!client.is_ready());

                assert!(matches!(
                    events.borrow().as_slice(),
                    [super::MiEvent::DebuggerUnusable(_)]
                ));
            })
            .unwrap();
    }

    #[test]
    fn unrelated_response_progress_does_not_extend_an_expired_request() {
        use std::{cell::RefCell, rc::Rc, time::Instant};

        let _guard = MI_CLIENT_TEST_LOCK.lock().unwrap();
        let context = gtk::glib::MainContext::new();

        context
            .with_thread_default(|| {
                let client = super::MiClient::open(|_, _| {}).unwrap();
                client.ready.set(true);
                let first = client.request("-thread-info", |_, _| {}).unwrap();
                let second_result = Rc::new(RefCell::new(None));
                let second_result_for_request = Rc::clone(&second_result);

                let second = client
                    .request("-stack-list-frames", move |_, record| {
                        second_result_for_request.replace(Some(record.class));
                    })
                    .unwrap();

                client.process_line(&format!("{first}^done"));

                client
                    .pending
                    .borrow_mut()
                    .get_mut(&second)
                    .unwrap()
                    .deadline = Instant::now();

                client.expire_requests();
                assert_eq!(second_result.borrow().as_deref(), Some("timeout"));
                assert!(!client.pending.borrow().contains_key(&second));
                assert!(client.is_ready());
            })
            .unwrap();
    }

    #[test]
    fn newer_stop_cancels_owned_inspection_without_touching_current_work() {
        use std::{cell::RefCell, rc::Rc};

        let _guard = MI_CLIENT_TEST_LOCK.lock().unwrap();
        let context = gtk::glib::MainContext::new();

        context
            .with_thread_default(|| {
                let client = super::MiClient::open(|_, _| {}).unwrap();
                client.ready.set(true);
                let results = Rc::new(RefCell::new(Vec::new()));
                let results_for_old = Rc::clone(&results);

                let old = client
                    .request_for_stop(
                        "-stack-list-frames --thread 1",
                        1,
                        || true,
                        move |_, record| results_for_old.borrow_mut().push(record.class),
                    )
                    .unwrap();

                let results_for_scoped = Rc::clone(&results);

                let old_scoped = client
                    .request_with_print_limit_for_stop(
                        "-stack-list-variables --thread 1 --frame 0 --simple-values",
                        32,
                        1,
                        || true,
                        move |_, record| results_for_scoped.borrow_mut().push(record.class),
                    )
                    .unwrap();

                let current = client
                    .request_for_stop("-thread-info", 2, || true, |_, _| {})
                    .unwrap();

                client.cancel_stale_stop_requests(2);
                assert_eq!(results.borrow().as_slice(), ["superseded", "superseded"]);
                assert!(!client.pending.borrow().contains_key(&old));
                assert!(client.pending.borrow().contains_key(&current));

                assert_ne!(
                    client
                        .scoped_request
                        .borrow()
                        .as_ref()
                        .map(|request| request.token),
                    Some(old_scoped)
                );

                assert!(client.is_ready());
            })
            .unwrap();
    }

    #[test]
    fn a_sent_superseded_scoped_request_is_drained_before_the_next_one_starts() {
        use std::{cell::Cell, cell::RefCell, rc::Rc};

        let _guard = MI_CLIENT_TEST_LOCK.lock().unwrap();
        let context = gtk::glib::MainContext::new();

        context
            .with_thread_default(|| {
                let client = super::MiClient::open(|_, _| {}).unwrap();
                client.ready.set(true);
                let first_is_current = Rc::new(Cell::new(true));
                let first_result = Rc::new(RefCell::new(None));
                let first_is_current_for_request = Rc::clone(&first_is_current);
                let first_result_for_request = Rc::clone(&first_result);

                let first = client
                    .request_with_print_limit_for_owner(
                        "-data-evaluate-expression value",
                        32,
                        None,
                        move || first_is_current_for_request.get(),
                        move |_, record| {
                            first_result_for_request.replace(Some(record.class));
                        },
                    )
                    .unwrap();

                // Model a command that has left fgdb without needing a live
                // GDB process in this transport-level regression test.
                client.outgoing.borrow_mut().clear();
                first_is_current.set(false);

                let second = client
                    .request_with_print_limit_for_owner(
                        "-data-evaluate-expression other",
                        32,
                        None,
                        || true,
                        |_, _| {},
                    )
                    .unwrap();

                client.expire_requests();
                assert!(first_result.borrow().is_none());

                assert_eq!(
                    client
                        .scoped_request
                        .borrow()
                        .as_ref()
                        .map(|request| request.token),
                    Some(first)
                );

                assert_eq!(client.scoped_queue.borrow().len(), 1);
                client.process_line(&format!("{first}^done"));
                assert_eq!(first_result.borrow().as_deref(), Some("superseded"));

                assert_eq!(
                    client
                        .scoped_request
                        .borrow()
                        .as_ref()
                        .map(|request| request.token),
                    Some(second)
                );

                assert!(client.scoped_queue.borrow().is_empty());
            })
            .unwrap();
    }

    #[test]
    fn serializes_console_commands_and_returns_their_stream_output() {
        use std::{cell::RefCell, rc::Rc, time::Instant};

        let _guard = MI_CLIENT_TEST_LOCK.lock().unwrap();
        let context = gtk::glib::MainContext::new();

        context
            .with_thread_default(|| {
                let client = super::MiClient::open(|_, _| {}).unwrap();
                client.ready.set(true);
                let result = Rc::new(RefCell::new(None));
                let result_for_request = Rc::clone(&result);

                let token = client
                    .request_console("show directories", move |_, record, output| {
                        result_for_request.replace(Some((record.class, output)));
                    })
                    .unwrap();

                let expired = Instant::now();

                client
                    .scoped_request
                    .borrow_mut()
                    .as_mut()
                    .unwrap()
                    .deadline = expired;

                client.process_line(r#"~"Source directories: /src:$cwd\n""#);
                client.process_line(r#"&"warning from GDB\n""#);
                client.process_line(r#"@"inferior output\n""#);
                assert!(client.scoped_request.borrow().as_ref().unwrap().deadline > expired);
                assert!(result.borrow().is_none());
                client.process_line(&format!("{token}^done"));

                assert_eq!(
                    result.borrow().as_ref(),
                    Some(&(
                        String::from("done"),
                        String::from("Source directories: /src:$cwd\nwarning from GDB\n")
                    ))
                );
            })
            .unwrap();
    }

    #[test]
    fn scoped_inspection_overtakes_queued_background_work() {
        let _guard = MI_CLIENT_TEST_LOCK.lock().unwrap();
        let context = gtk::glib::MainContext::new();

        context
            .with_thread_default(|| {
                let client = super::MiClient::open(|_, _| {}).unwrap();
                client.ready.set(true);

                client
                    .request_console("show version", |_, _, _| {})
                    .unwrap();

                client
                    .request_console("show directories", |_, _, _| {})
                    .unwrap();

                client
                    .request_with_print_limit_for_owner(
                        "-stack-list-variables --simple-values",
                        32,
                        None,
                        || true,
                        |_, _| {},
                    )
                    .unwrap();

                let queue = client.scoped_queue.borrow();
                assert_eq!(queue.len(), 2);
                assert_eq!(queue[0].class, super::CommandClass::Inspection);
                assert_eq!(queue[1].class, super::CommandClass::Background);
            })
            .unwrap();
    }

    #[test]
    fn request_admission_preserves_control_and_execution_capacity() {
        use std::{cell::RefCell, rc::Rc};

        let _guard = MI_CLIENT_TEST_LOCK.lock().unwrap();
        let context = gtk::glib::MainContext::new();

        context
            .with_thread_default(|| {
                let events = Rc::new(RefCell::new(Vec::new()));
                let events_for_client = Rc::clone(&events);

                let client = super::MiClient::open(move |_, event| {
                    events_for_client.borrow_mut().push(event);
                })
                .unwrap();
                client.ready.set(true);

                for _ in 0..super::MAX_INSPECTION_REQUESTS {
                    client
                        .request_when("-thread-info", || true, |_, _| {})
                        .unwrap();
                }

                let rejected = client.request_when("-thread-info", || true, |_, _| {});
                assert_eq!(rejected.unwrap_err().kind(), std::io::ErrorKind::WouldBlock);
                assert!(client.request("-break-insert main", |_, _| {}).is_ok());

                let additional_controls =
                    super::MAX_NON_EXECUTION_REQUESTS.saturating_sub(client.pending.borrow().len());

                for _ in 0..additional_controls {
                    client.request("-gdb-show language", |_, _| {}).unwrap();
                }

                assert_eq!(
                    client
                        .request("-gdb-show language", |_, _| {})
                        .unwrap_err()
                        .kind(),
                    std::io::ErrorKind::WouldBlock
                );

                assert!(client.send("-exec-next").is_ok());

                assert!(events.borrow().iter().any(|event| matches!(
                    event,
                    super::MiEvent::Performance(notice)
                        if notice.outcome == crate::performance::BudgetOutcome::Rejected
                )));
            })
            .unwrap();
    }

    #[test]
    fn inspection_window_reserves_a_priority_lane_for_execution() {
        let _guard = MI_CLIENT_TEST_LOCK.lock().unwrap();
        let context = gtk::glib::MainContext::new();

        context
            .with_thread_default(|| {
                let client = super::MiClient::open(|_, _| {}).unwrap();
                client.ready.set(true);

                let tokens = (0..=super::MAX_ACTIVE_INSPECTION_REQUESTS)
                    .map(|_| {
                        client
                            .request_when("-thread-info", || true, |_, _| {})
                            .unwrap()
                    })
                    .collect::<Vec<_>>();

                assert_eq!(
                    client
                        .pending
                        .borrow()
                        .values()
                        .filter(|request| request.started_at.is_some())
                        .count(),
                    super::MAX_ACTIVE_INSPECTION_REQUESTS
                );

                assert!(
                    client
                        .pending
                        .borrow()
                        .get(tokens.last().unwrap())
                        .unwrap()
                        .started_at
                        .is_none()
                );

                let execution = client.send("-exec-next").unwrap();

                assert_eq!(
                    client
                        .outgoing
                        .borrow()
                        .commands
                        .front()
                        .map(|command| command.token),
                    Some(execution)
                );

                client.process_line(&format!("{}^done", tokens[0]));

                assert!(
                    client
                        .pending
                        .borrow()
                        .get(tokens.last().unwrap())
                        .unwrap()
                        .started_at
                        .is_some()
                );
            })
            .unwrap();
    }

    #[test]
    fn queued_request_timeout_is_safe_and_does_not_quarantine_gdb() {
        use std::{cell::RefCell, rc::Rc, time::Instant};

        let _guard = MI_CLIENT_TEST_LOCK.lock().unwrap();
        let context = gtk::glib::MainContext::new();

        context
            .with_thread_default(|| {
                let client = super::MiClient::open(|_, _| {}).unwrap();
                client.ready.set(true);

                for _ in 0..super::MAX_ACTIVE_INSPECTION_REQUESTS {
                    client
                        .request_when("-thread-info", || true, |_, _| {})
                        .unwrap();
                }

                let result = Rc::new(RefCell::new(None));
                let result_for_handler = Rc::clone(&result);

                let queued = client
                    .request_when(
                        "-stack-list-frames",
                        || true,
                        move |_, record| {
                            result_for_handler.replace(Some(record.class));
                        },
                    )
                    .unwrap();

                {
                    let mut pending = client.pending.borrow_mut();
                    let request = pending.get_mut(&queued).unwrap();
                    assert!(request.started_at.is_none());
                    request.deadline = Instant::now();
                }

                client.expire_requests();
                assert_eq!(result.borrow().as_deref(), Some("timeout"));
                assert!(client.is_ready());
            })
            .unwrap();
    }

    #[test]
    fn oversized_tokenized_result_fails_only_its_request() {
        use std::{cell::RefCell, rc::Rc};

        let _guard = MI_CLIENT_TEST_LOCK.lock().unwrap();
        let context = gtk::glib::MainContext::new();

        context
            .with_thread_default(|| {
                let events = Rc::new(RefCell::new(Vec::new()));
                let events_for_client = Rc::clone(&events);

                let client = super::MiClient::open(move |_, event| {
                    events_for_client.borrow_mut().push(event);
                })
                .unwrap();
                client.ready.set(true);
                let result = Rc::new(RefCell::new(None));
                let result_for_request = Rc::clone(&result);

                let token = client
                    .request("-thread-info", move |_, record| {
                        result_for_request.replace(Some(record.class));
                    })
                    .unwrap();

                let mut oversized = format!("{token}^done,value=\"").into_bytes();
                oversized.resize(super::MAX_MI_RECORD_BYTES + 1, b'x');
                client.consume(&oversized);
                assert!(result.borrow().is_none());
                assert!(client.discarding_oversized_line.get());
                client.consume(b"discarded tail\n*running,thread-id=\"all\"\n");
                assert_eq!(result.borrow().as_deref(), Some("resource-limit"));
                assert!(!client.pending.borrow().contains_key(&token));
                assert!(!client.discarding_oversized_line.get());
                assert!(client.is_ready());

                assert!(
                    !events
                        .borrow()
                        .iter()
                        .any(|event| matches!(event, super::MiEvent::DebuggerUnusable(_)))
                );

                assert!(events.borrow().iter().any(|event| matches!(
                    event,
                    super::MiEvent::Running { thread_id }
                        if thread_id.as_deref() == Some("all")
                )));
            })
            .unwrap();
    }

    #[test]
    fn oversized_console_capture_returns_an_explicit_partial_result() {
        use std::{cell::RefCell, rc::Rc};

        let _guard = MI_CLIENT_TEST_LOCK.lock().unwrap();
        let context = gtk::glib::MainContext::new();

        context
            .with_thread_default(|| {
                let client = super::MiClient::open(|_, _| {}).unwrap();
                client.ready.set(true);
                let result = Rc::new(RefCell::new(None));
                let result_for_request = Rc::clone(&result);

                let token = client
                    .request_console("show directories", move |_, record, output| {
                        result_for_request
                            .replace(Some((record.output_was_truncated(), output.len())));
                    })
                    .unwrap();

                let output = "x".repeat(super::MAX_CAPTURED_CONSOLE_BYTES + 257);
                client.process_line(&format!("~\"{output}\""));
                client.process_line(&format!("{token}^done"));

                assert_eq!(
                    *result.borrow(),
                    Some((true, super::MAX_CAPTURED_CONSOLE_BYTES))
                );

                assert!(client.is_ready());
            })
            .unwrap();
    }

    #[test]
    fn oversized_asynchronous_state_requires_recovery() {
        use std::{cell::RefCell, rc::Rc};

        let _guard = MI_CLIENT_TEST_LOCK.lock().unwrap();
        let context = gtk::glib::MainContext::new();

        context
            .with_thread_default(|| {
                let events = Rc::new(RefCell::new(Vec::new()));
                let events_for_client = Rc::clone(&events);

                let client = super::MiClient::open(move |_, event| {
                    events_for_client.borrow_mut().push(event);
                })
                .unwrap();
                client.ready.set(true);

                client.handle_oversized_record(super::MiRecordHeader {
                    token: None,
                    kind: Some(b'*'),
                });

                assert!(!client.is_ready());

                assert!(matches!(
                    events.borrow().as_slice(),
                    [super::MiEvent::DebuggerUnusable(_)]
                ));
            })
            .unwrap();
    }

    #[test]
    fn malformed_state_records_require_recovery_but_late_errors_do_not() {
        use std::{cell::RefCell, rc::Rc};

        let _guard = MI_CLIENT_TEST_LOCK.lock().unwrap();
        let context = gtk::glib::MainContext::new();

        context
            .with_thread_default(|| {
                let events = Rc::new(RefCell::new(Vec::new()));
                let events_for_client = Rc::clone(&events);

                let client = super::MiClient::open(move |_, event| {
                    events_for_client.borrow_mut().push(event);
                })
                .unwrap();
                client.ready.set(true);
                client.process_line(r#"91^error,msg="late response""#);
                assert!(events.borrow().is_empty());
                client.process_line("*stopped,reason");

                assert!(matches!(
                    events.borrow().as_slice(),
                    [super::MiEvent::DebuggerUnusable(_)]
                ));
            })
            .unwrap();
    }

    #[test]
    fn transport_can_be_replaced_from_inside_an_input_callback() {
        use std::{cell::Cell, rc::Rc};

        let _guard = MI_CLIENT_TEST_LOCK.lock().unwrap();
        let context = gtk::glib::MainContext::new();

        context
            .with_thread_default(|| {
                let replaced = Rc::new(Cell::new(false));
                let replaced_for_client = Rc::clone(&replaced);
                let stale_running_seen = Rc::new(Cell::new(false));
                let stale_running_seen_for_client = Rc::clone(&stale_running_seen);

                let client = super::MiClient::open(move |client, event| {
                    if event == super::MiEvent::InferiorsChanged {
                        client.reconnect().unwrap();
                        replaced_for_client.set(true);
                    } else if matches!(event, super::MiEvent::Running { .. }) {
                        stale_running_seen_for_client.set(true);
                    }
                })
                .unwrap();
                client.ready.set(true);
                client.consume(b"=thread-group-added,id=\"i2\"\n*running,thread-id=\"all\"\n");
                assert!(replaced.get());
                assert!(!stale_running_seen.get());
            })
            .unwrap();
    }

    #[test]
    fn accepts_all_successful_mi_result_classes_for_session_commands() {
        for class in ["done", "connected", "running"] {
            assert!(parse_record(&format!("1^{class}")).unwrap().is_success());
        }

        assert!(
            !parse_record(r#"1^error,msg="failed""#)
                .unwrap()
                .is_success()
        );

        assert!(!super::synthetic_error_record("timeout", "timed out").is_success());
    }

    #[test]
    fn drains_outgoing_commands_in_fifo_order_and_bounded_batches() {
        let mut outgoing = OutgoingQueue::default();
        outgoing.enqueue(7, 0, "-exec-next").unwrap();
        outgoing.enqueue(8, 0, "-exec-step").unwrap();
        let total = outgoing.remaining_bytes;
        let mut written = Vec::new();
        assert!(!drain_outgoing(&mut written, &mut outgoing, 5).unwrap());
        assert_eq!(written, b"7-exe");
        assert_eq!(outgoing.remaining_bytes, total - 5);
        assert!(drain_outgoing(&mut written, &mut outgoing, 1024).unwrap());
        assert_eq!(written, b"7-exec-next\n8-exec-step\n");
        assert_eq!(outgoing.remaining_bytes, 0);
    }

    #[test]
    fn preserves_queued_output_when_the_pty_applies_backpressure() {
        let mut outgoing = OutgoingQueue::default();
        outgoing.enqueue(11, 0, "-exec-continue").unwrap();
        let remaining = outgoing.remaining_bytes;
        assert!(!drain_outgoing(&mut BackpressuredWriter, &mut outgoing, 1024).unwrap());
        assert_eq!(outgoing.remaining_bytes, remaining);
        assert_eq!(outgoing.commands.front().unwrap().written, 0);
    }

    #[test]
    fn priority_commands_overtake_only_wholly_unwritten_output() {
        let mut outgoing = OutgoingQueue::default();
        outgoing.enqueue(1, 2, "-inspection-one").unwrap();
        outgoing.enqueue(2, 2, "-inspection-two").unwrap();
        outgoing.enqueue(3, 0, "-exec-next").unwrap();

        assert_eq!(
            outgoing
                .commands
                .iter()
                .map(|command| command.token)
                .collect::<Vec<_>>(),
            [3, 1, 2]
        );

        outgoing.advance(1);
        outgoing.enqueue(4, 0, "-exec-step").unwrap();

        assert_eq!(
            outgoing
                .commands
                .iter()
                .map(|command| command.token)
                .collect::<Vec<_>>(),
            [3, 4, 1, 2]
        );
    }

    #[test]
    fn only_cancels_commands_that_have_not_started_writing() {
        let mut outgoing = OutgoingQueue::default();
        outgoing.enqueue(1, 0, "-first").unwrap();
        outgoing.enqueue(2, 0, "-second").unwrap();
        outgoing.advance(1);
        assert!(!outgoing.cancel_unstarted(1));
        assert!(outgoing.cancel_unstarted(2));
        assert_eq!(outgoing.commands.len(), 1);
        assert_eq!(outgoing.commands.front().unwrap().token, 1);
    }

    #[test]
    fn cancels_expired_unstarted_commands_in_one_queue_pass() {
        let mut outgoing = OutgoingQueue::default();
        outgoing.enqueue(1, 0, "-partially-written").unwrap();
        outgoing.enqueue(2, 0, "-expired-two").unwrap();
        outgoing.enqueue(3, 0, "-retained").unwrap();
        outgoing.enqueue(4, 0, "-expired-four").unwrap();
        outgoing.advance(3);
        let before = outgoing.remaining_bytes;
        let removed = outgoing.commands[1].bytes.len() + outgoing.commands[3].bytes.len();

        let cancelled = outgoing.cancel_unstarted_many(&std::collections::HashSet::from([
            1_u64, 2_u64, 4_u64, 99_u64,
        ]));

        assert_eq!(cancelled, std::collections::HashSet::from([2_u64, 4_u64]));
        assert_eq!(outgoing.remaining_bytes, before - removed);

        assert_eq!(
            outgoing
                .commands
                .iter()
                .map(|command| (command.token, command.written))
                .collect::<Vec<_>>(),
            [(1, 3), (3, 0)]
        );
    }

    #[test]
    fn locates_complete_mi_lines_without_claiming_a_partial_record() {
        let incoming = b"1^done\r\n*stopped,reason=\"breakpoint-hit\"\n3^do";

        assert_eq!(
            complete_input_end(incoming),
            Some(b"1^done\r\n*stopped,reason=\"breakpoint-hit\"\n".len())
        );

        assert_eq!(complete_input_end(b"3^do"), None);
        assert_eq!(complete_input_end(b"3^done\n"), Some(7));
    }

    #[test]
    fn locates_a_terminator_relative_to_an_existing_partial_record() {
        let previous = b"17^done,value=\"partial";
        let appended = b" value\"\n*stopped\nnext";
        let complete = complete_input_end(appended).map(|end| previous.len() + end);

        assert_eq!(
            complete,
            Some(previous.len() + b" value\"\n*stopped\n".len())
        );
    }

    #[test]
    fn parses_nested_stack_frames() {
        let record = parse_record(
            r#"17^done,stack=[frame={level="0",addr="0x1",func="main",fullname="/tmp/a.c",line="8"},frame={level="1",addr="0x2",func="_start"}]"#,
        )
        .expect("valid record");
        assert_eq!(record.token, Some(17));
        assert_eq!(record.class, "done");
        let stack = record.field("stack").and_then(MiValue::as_list).unwrap();

        let MiListItem::Result(frame) = &stack[0] else {
            panic!("frame result expected");
        };

        let tuple = frame.value.as_tuple().unwrap();

        assert_eq!(
            result_field(tuple, "fullname").and_then(MiValue::as_const),
            Some("/tmp/a.c")
        );
    }

    #[test]
    fn parses_value_lists_and_escaped_strings() {
        let record = parse_record(r#"4^done,register-names=["rax","r\"bx","line\n"]"#)
            .expect("valid record");

        let names = record
            .field("register-names")
            .and_then(MiValue::as_list)
            .unwrap();

        assert_eq!(
            names,
            [
                MiListItem::Value(MiValue::Const(String::from("rax"))),
                MiListItem::Value(MiValue::Const(String::from("r\"bx"))),
                MiListItem::Value(MiValue::Const(String::from("line\n"))),
            ]
        );
    }

    #[test]
    fn parses_unescaped_and_escaped_strings_through_the_same_model() {
        let plain = parse_record(r#"1^done,value="plain ASCII value""#).unwrap();
        let escaped = parse_record(r#"2^done,value="plain\040ASCII\040value""#).unwrap();
        assert_eq!(plain.field("value"), escaped.field("value"));
    }

    #[test]
    fn replaces_invalid_bytes_only_when_an_mi_escape_requires_it() {
        let record = parse_record(r#"1^done,value="\xff""#).expect("valid record");
        assert_eq!(record.field("value").and_then(MiValue::as_const), Some("�"));
    }

    #[test]
    fn rejects_excessively_nested_values() {
        let nested = format!(
            "1^done,value={}\"x\"{}",
            "[".repeat(super::MAX_MI_NESTING + 1),
            "]".repeat(super::MAX_MI_NESTING + 1)
        );

        assert!(parse_record(&nested).unwrap_err().contains("nesting limit"));
    }

    #[test]
    fn rejects_tokens_outside_the_supported_range() {
        assert!(
            parse_record("18446744073709551616^done")
                .unwrap_err()
                .contains("token")
        );
    }

    #[test]
    fn rejects_unsafe_or_unreasonably_large_commands() {
        assert!(validate_mi_command("").is_err());
        assert!(validate_mi_command("-exec-next\n99-gdb-exit").is_err());
        assert!(validate_mi_command(&"x".repeat(super::MAX_MI_COMMAND_BYTES + 1)).is_err());
        assert!(super::validate_console_command("show directories\ngdb-exit").is_err());
    }

    #[test]
    fn quotes_mi_arguments() {
        assert_eq!(quote("src/a file.c:12"), r#""src/a file.c:12""#);
        assert_eq!(quote("a\"b\\c"), r#""a\"b\\c""#);
    }

    #[test]
    fn wraps_mi_commands_in_scoped_print_limits() {
        assert_eq!(
            scoped_mi_command("-stack-list-variables --simple-values", 128),
            r#"-interpreter-exec console "with print elements 128 -- interpreter-exec mi \"-stack-list-variables --simple-values\"""#
        );

        let nested = parse_stream_output(r#"~"^done,value=\"bounded\"\n""#).unwrap();
        let record = parse_record(nested.trim()).unwrap();

        assert_eq!(
            record.field("value").and_then(MiValue::as_const),
            Some("bounded")
        );
    }

    #[test]
    fn decodes_gdb_console_escape_extensions_as_terminal_escapes() {
        let output = parse_stream_output(r#"~"\e[1m\e[31m[!]\e[0m Heap not initialized\n""#)
            .expect("valid GDB console stream");

        assert_eq!(
            output,
            "\u{1b}[1m\u{1b}[31m[!]\u{1b}[0m Heap not initialized\n"
        );
    }
}
