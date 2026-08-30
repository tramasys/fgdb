use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, VecDeque},
    fs::File,
    io::{self, Read, Write},
    os::fd::{AsRawFd, OwnedFd},
    path::PathBuf,
    rc::{Rc, Weak},
    time::{Duration, Instant},
};

use gtk::glib;
use nix::{
    fcntl::{FcntlArg, OFlag, fcntl},
    pty::openpty,
    sys::termios::{SetArg, cfmakeraw, tcgetattr, tcsetattr},
    unistd::ttyname,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MiEvent {
    Ready(GdbCapabilities),
    InferiorsChanged,
    InferiorStarted {
        id: String,
        pid: Option<u32>,
    },
    InferiorExited {
        id: String,
        exit_code: Option<String>,
    },
    Running {
        thread_id: Option<String>,
    },
    Stopped {
        reason: Option<String>,
        signal_name: Option<String>,
        signal_meaning: Option<String>,
        address: Option<String>,
        thread_id: Option<String>,
        fork_pid: Option<u32>,
        all_stopped: bool,
    },
    BreakpointsChanged,
    ThreadsChanged {
        group_id: Option<String>,
    },
    LibrariesChanged {
        group_id: Option<String>,
    },
    SelectionChanged,
    Error(String),
    Disconnected,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GdbCapabilities {
    pub version: Option<String>,
    pub features_known: bool,
    pub features: Vec<String>,
    pub mi_async: bool,
    pub pretty_printing: bool,
}

impl GdbCapabilities {
    pub fn supports(&self, feature: &str) -> bool {
        !self.features_known || self.features.iter().any(|available| available == feature)
    }

    pub fn compatibility_summary(&self) -> String {
        let mut available = Vec::with_capacity(3);
        if self.mi_async {
            available.push("MI async");
        }
        if self.pretty_printing {
            available.push("pretty printers");
        }
        if self.features_known {
            available.push("feature list");
        }
        let support = if available.is_empty() {
            String::from("compatibility mode")
        } else {
            available.join(" · ")
        };
        self.version.as_ref().map_or(support.clone(), |version| {
            format!("GDB {version} · {support}")
        })
    }

    fn set_version_component(&mut self, component: &str, minor: bool) {
        if component.is_empty() || !component.bytes().all(|byte| byte.is_ascii_digit()) {
            return;
        }
        if minor {
            if let Some(version) = self.version.as_mut() {
                version.push('.');
                version.push_str(component);
            }
        } else {
            self.version = Some(component.to_owned());
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MiRecord {
    pub token: Option<u64>,
    pub kind: char,
    pub class: String,
    pub results: Vec<MiResult>,
}

impl MiRecord {
    pub fn field(&self, name: &str) -> Option<&MiValue> {
        result_field(&self.results, name)
    }

    pub fn is_done(&self) -> bool {
        self.class == "done"
    }

    pub fn is_success(&self) -> bool {
        self.kind == '^' && self.class != "error" && self.class != "exit"
    }

    pub fn error_message(&self) -> Option<&str> {
        self.field("msg").and_then(MiValue::as_const)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MiResult {
    pub name: String,
    pub value: MiValue,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MiValue {
    Const(String),
    Tuple(Vec<MiResult>),
    List(Vec<MiListItem>),
}

impl MiValue {
    pub fn as_const(&self) -> Option<&str> {
        match self {
            Self::Const(value) => Some(value),
            Self::Tuple(_) | Self::List(_) => None,
        }
    }

    pub fn as_tuple(&self) -> Option<&[MiResult]> {
        match self {
            Self::Tuple(results) => Some(results),
            Self::Const(_) | Self::List(_) => None,
        }
    }

    pub fn as_list(&self) -> Option<&[MiListItem]> {
        match self {
            Self::List(items) => Some(items),
            Self::Const(_) | Self::Tuple(_) => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MiListItem {
    Value(MiValue),
    Result(MiResult),
}

pub fn result_field<'a>(results: &'a [MiResult], name: &str) -> Option<&'a MiValue> {
    results
        .iter()
        .find(|result| result.name == name)
        .map(|result| &result.value)
}

type ResponseHandler = Box<dyn FnOnce(&MiClient, MiRecord)>;
type EventHandler = Box<dyn Fn(&MiClient, MiEvent)>;

const MAX_MI_RECORD_BYTES: usize = 8 * 1024 * 1024;
const MAX_RETAINED_MI_INPUT_BYTES: usize = 256 * 1024;
const MAX_MI_NESTING: usize = 64;
const MAX_MI_ITEMS: usize = 100_000;
const MAX_MI_COMMAND_BYTES: usize = 1024 * 1024;
const MAX_QUEUED_MI_BYTES: usize = 8 * 1024 * 1024;
const MAX_MI_WRITE_BATCH_BYTES: usize = 256 * 1024;
const MAX_PENDING_REQUESTS: usize = 4096;
const MAX_SCOPED_REQUESTS: usize = 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const REQUEST_TIMEOUT_POLL: Duration = Duration::from_millis(250);

struct PendingRequest {
    deadline: Instant,
    is_current: Option<Box<dyn Fn() -> bool>>,
    handler: ResponseHandler,
}

struct ScopedMiRequest {
    token: u64,
    command: String,
    response: Option<MiRecord>,
    is_current: Box<dyn Fn() -> bool>,
    handler: ResponseHandler,
    deadline: Instant,
}

struct OutgoingCommand {
    token: u64,
    bytes: Vec<u8>,
    written: usize,
}

#[derive(Default)]
struct OutgoingQueue {
    commands: VecDeque<OutgoingCommand>,
    remaining_bytes: usize,
}

impl OutgoingQueue {
    fn enqueue(&mut self, token: u64, command: &str) -> io::Result<()> {
        let capacity = command
            .len()
            .checked_add(21)
            .ok_or_else(|| io::Error::other("GDB/MI command size overflow"))?;
        let mut bytes = Vec::with_capacity(capacity);
        writeln!(&mut bytes, "{token}{command}")?;
        let new_size = self
            .remaining_bytes
            .checked_add(bytes.len())
            .ok_or_else(|| io::Error::other("GDB/MI output queue size overflow"))?;
        if new_size > MAX_QUEUED_MI_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "GDB/MI output queue exceeds the 8 MiB limit",
            ));
        }
        self.remaining_bytes = new_size;
        self.commands.push_back(OutgoingCommand {
            token,
            bytes,
            written: 0,
        });
        Ok(())
    }

    fn advance(&mut self, count: usize) {
        let Some(command) = self.commands.front_mut() else {
            return;
        };
        let count = count.min(command.bytes.len().saturating_sub(command.written));
        command.written += count;
        self.remaining_bytes = self.remaining_bytes.saturating_sub(count);
        if command.written == command.bytes.len() {
            self.commands.pop_front();
        }
    }

    fn cancel_unstarted(&mut self, token: u64) -> bool {
        let Some(index) = self
            .commands
            .iter()
            .position(|command| command.token == token && command.written == 0)
        else {
            return false;
        };
        if let Some(command) = self.commands.remove(index) {
            self.remaining_bytes = self.remaining_bytes.saturating_sub(command.bytes.len());
        }
        true
    }

    fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }

    fn clear(&mut self) {
        self.commands.clear();
        self.remaining_bytes = 0;
    }
}

fn drain_outgoing(
    writer: &mut impl Write,
    outgoing: &mut OutgoingQueue,
    byte_budget: usize,
) -> io::Result<bool> {
    let mut written_this_batch = 0_usize;
    while written_this_batch < byte_budget {
        let write_result = {
            let Some(command) = outgoing.commands.front() else {
                return Ok(true);
            };
            let remaining_budget = byte_budget - written_this_batch;
            let remaining = &command.bytes[command.written..];
            writer.write(&remaining[..remaining.len().min(remaining_budget)])
        };
        match write_result {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "could not write a GDB/MI command",
                ));
            }
            Ok(count) => {
                outgoing.advance(count);
                written_this_batch += count;
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(false),
            Err(error) => return Err(error),
        }
    }
    Ok(outgoing.is_empty())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum IoSource {
    Read,
    Write,
}

pub struct MiClient {
    transport: RefCell<MiTransport>,
    incoming: RefCell<Vec<u8>>,
    next_token: Cell<u64>,
    ready: Cell<bool>,
    initializing: Cell<bool>,
    capabilities: RefCell<GdbCapabilities>,
    pending: RefCell<HashMap<u64, PendingRequest>>,
    scoped_request: RefCell<Option<ScopedMiRequest>>,
    scoped_queue: RefCell<VecDeque<ScopedMiRequest>>,
    outgoing: RefCell<OutgoingQueue>,
    event_handler: EventHandler,
    self_weak: Weak<Self>,
    connected: Cell<bool>,
    read_source: RefCell<Option<glib::SourceId>>,
    write_source: RefCell<Option<glib::SourceId>>,
    timeout_source: RefCell<Option<glib::SourceId>>,
    discarding_oversized_line: Cell<bool>,
}

struct MiTransport {
    master: File,
    _slave: OwnedFd,
    slave_path: PathBuf,
}

fn open_transport() -> io::Result<MiTransport> {
    let pty = openpty(None, None).map_err(io::Error::other)?;
    let slave_path = ttyname(&pty.slave).map_err(io::Error::other)?;

    let mut terminal_settings = tcgetattr(&pty.slave).map_err(io::Error::other)?;
    cfmakeraw(&mut terminal_settings);
    tcsetattr(&pty.slave, SetArg::TCSANOW, &terminal_settings).map_err(io::Error::other)?;

    let master = File::from(pty.master);
    let flags = fcntl(&master, FcntlArg::F_GETFL).map_err(io::Error::other)?;
    let flags = OFlag::from_bits_truncate(flags) | OFlag::O_NONBLOCK;
    fcntl(&master, FcntlArg::F_SETFL(flags)).map_err(io::Error::other)?;
    Ok(MiTransport {
        master,
        _slave: pty.slave,
        slave_path,
    })
}

impl MiClient {
    pub fn open(event_handler: impl Fn(&MiClient, MiEvent) + 'static) -> io::Result<Rc<Self>> {
        let transport = open_transport()?;
        let client = Rc::new_cyclic(|self_weak| Self {
            transport: RefCell::new(transport),
            incoming: RefCell::new(Vec::new()),
            next_token: Cell::new(1),
            ready: Cell::new(false),
            initializing: Cell::new(false),
            capabilities: RefCell::new(GdbCapabilities::default()),
            pending: RefCell::new(HashMap::new()),
            scoped_request: RefCell::new(None),
            scoped_queue: RefCell::new(VecDeque::new()),
            outgoing: RefCell::new(OutgoingQueue::default()),
            event_handler: Box::new(event_handler),
            self_weak: self_weak.clone(),
            connected: Cell::new(true),
            read_source: RefCell::new(None),
            write_source: RefCell::new(None),
            timeout_source: RefCell::new(None),
            discarding_oversized_line: Cell::new(false),
        });
        client.install_sources();
        Ok(client)
    }

    pub fn slave_path(&self) -> PathBuf {
        self.transport.borrow().slave_path.clone()
    }

    pub fn is_ready(&self) -> bool {
        self.ready.get()
    }

    pub fn capabilities(&self) -> GdbCapabilities {
        self.capabilities.borrow().clone()
    }

    pub fn reconnect(&self) -> io::Result<PathBuf> {
        if self.connected.replace(false) {
            self.ready.set(false);
            self.initializing.set(false);
            if let Some(source) = self.read_source.borrow_mut().take() {
                source.remove();
            }
            if let Some(source) = self.write_source.borrow_mut().take() {
                source.remove();
            }
            if let Some(source) = self.timeout_source.borrow_mut().take() {
                source.remove();
            }
            self.outgoing.borrow_mut().clear();
            self.fail_pending_requests("GDB/MI connection replaced");
        }
        let transport = open_transport()?;
        let slave_path = transport.slave_path.clone();
        self.transport.replace(transport);
        self.incoming.borrow_mut().clear();
        self.outgoing.borrow_mut().clear();
        self.discarding_oversized_line.set(false);
        self.ready.set(false);
        self.initializing.set(false);
        self.capabilities.replace(GdbCapabilities::default());
        self.connected.set(true);
        self.install_sources();
        Ok(slave_path)
    }

    fn install_sources(&self) {
        let master_fd = self.transport.borrow().master.as_raw_fd();
        let weak_client = self.self_weak.clone();
        let source = glib_unix::unix_fd_add_local(
            master_fd,
            glib::IOCondition::IN | glib::IOCondition::HUP | glib::IOCondition::ERR,
            move |_, condition| Self::on_io_ready(&weak_client, condition),
        );
        self.read_source.replace(Some(source));
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

    pub fn send(&self, command: &str) -> io::Result<u64> {
        self.request(command, |client, record| {
            if !record.is_success() {
                let message = record
                    .error_message()
                    .unwrap_or("GDB rejected the command")
                    .to_owned();
                (client.event_handler)(client, MiEvent::Error(message));
            }
        })
    }

    pub fn request(
        &self,
        command: &str,
        handler: impl FnOnce(&MiClient, MiRecord) + 'static,
    ) -> io::Result<u64> {
        self.request_inner(command, None, Box::new(handler))
    }

    pub fn request_when(
        &self,
        command: &str,
        is_current: impl Fn() -> bool + 'static,
        handler: impl FnOnce(&MiClient, MiRecord) + 'static,
    ) -> io::Result<u64> {
        self.request_inner(command, Some(Box::new(is_current)), Box::new(handler))
    }

    fn request_inner(
        &self,
        command: &str,
        is_current: Option<Box<dyn Fn() -> bool>>,
        handler: ResponseHandler,
    ) -> io::Result<u64> {
        validate_mi_command(command)?;
        if self.pending.borrow().len() >= MAX_PENDING_REQUESTS {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "too many pending GDB/MI requests",
            ));
        }
        let token = self.allocate_token();
        self.pending.borrow_mut().insert(
            token,
            PendingRequest {
                deadline: Instant::now() + REQUEST_TIMEOUT,
                is_current,
                handler,
            },
        );
        if let Err(error) = self.write_tokenized(token, command) {
            self.pending.borrow_mut().remove(&token);
            return Err(error);
        }
        Ok(token)
    }

    pub fn request_with_print_limit_when(
        &self,
        command: &str,
        elements: usize,
        is_current: impl Fn() -> bool + 'static,
        handler: impl FnOnce(&MiClient, MiRecord) + 'static,
    ) -> io::Result<u64> {
        validate_mi_command(command)?;
        let command = scoped_mi_command(command, elements);
        validate_mi_command(&command)?;
        let token = self.allocate_token();
        let request = ScopedMiRequest {
            token,
            command,
            response: None,
            is_current: Box::new(is_current),
            handler: Box::new(handler),
            deadline: Instant::now() + REQUEST_TIMEOUT,
        };
        self.queue_scoped_request(request)?;
        Ok(token)
    }

    fn queue_scoped_request(&self, request: ScopedMiRequest) -> io::Result<()> {
        let queued_requests = usize::from(self.scoped_request.borrow().is_some())
            .saturating_add(self.scoped_queue.borrow().len());
        if queued_requests >= MAX_SCOPED_REQUESTS {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "too many queued scoped GDB requests",
            ));
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
            (request.handler)(self, error_record("request superseded"));
            return Ok(());
        }
        if self.scoped_request.borrow().is_some() || !self.scoped_queue.borrow().is_empty() {
            self.scoped_queue.borrow_mut().push_back(request);
        } else if let Err(failure) = self.start_scoped_request(request) {
            return Err(failure.0);
        }
        Ok(())
    }

    fn start_scoped_request(
        &self,
        mut request: ScopedMiRequest,
    ) -> Result<(), Box<(io::Error, ScopedMiRequest)>> {
        if let Err(error) = self.write_tokenized(request.token, &request.command) {
            return Err(Box::new((error, request)));
        }
        // The encoded command is now owned by the output queue. Do not retain
        // a duplicate, potentially large allocation while waiting for GDB.
        request.command = String::new();
        self.scoped_request.replace(Some(request));
        Ok(())
    }

    fn start_next_scoped_request(&self) {
        loop {
            if self.scoped_request.borrow().is_some() {
                return;
            }
            let request = { self.scoped_queue.borrow_mut().pop_front() };
            let Some(request) = request else {
                return;
            };
            if request.deadline <= Instant::now() {
                (request.handler)(self, error_record("GDB request timed out"));
                continue;
            }
            if !(request.is_current)() {
                (request.handler)(self, error_record("request superseded"));
                continue;
            }
            if let Err(failure) = self.start_scoped_request(request) {
                let (error, request) = *failure;
                (request.handler)(self, error_record(&error.to_string()));
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

    fn write_tokenized(&self, token: u64, command: &str) -> io::Result<()> {
        if !self.connected.get() {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "GDB/MI connection is closed",
            ));
        }
        self.outgoing.borrow_mut().enqueue(token, command)?;
        self.ensure_write_source();
        Ok(())
    }

    fn ensure_write_source(&self) {
        if self.write_source.borrow().is_some() {
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
        if self.outgoing.borrow().is_empty()
            && let Some(source) = self.write_source.borrow_mut().take()
        {
            source.remove();
        }
    }

    fn on_write_ready(weak_client: &Weak<Self>, condition: glib::IOCondition) -> glib::ControlFlow {
        let Some(client) = weak_client.upgrade() else {
            return glib::ControlFlow::Break;
        };
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
                client.disconnect(IoSource::Write)
            }
            Ok(false) => glib::ControlFlow::Continue,
            Err(error) => {
                client.write_source.borrow_mut().take();
                (client.event_handler)(
                    &client,
                    MiEvent::Error(format!("Could not write a GDB/MI command: {error}")),
                );
                client.disconnect(IoSource::Write)
            }
        }
    }

    fn on_io_ready(weak_client: &Weak<Self>, condition: glib::IOCondition) -> glib::ControlFlow {
        let Some(client) = weak_client.upgrade() else {
            return glib::ControlFlow::Break;
        };

        let mut bytes = [0_u8; 16 * 1024];
        let read_result = {
            let mut transport = client.transport.borrow_mut();
            transport.master.read(&mut bytes)
        };

        match read_result {
            Ok(0) => client.disconnect(IoSource::Read),
            Ok(length) => {
                client.consume(&bytes[..length]);
                glib::ControlFlow::Continue
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                if condition.intersects(glib::IOCondition::HUP | glib::IOCondition::ERR) {
                    client.disconnect(IoSource::Read)
                } else {
                    glib::ControlFlow::Continue
                }
            }
            Err(_) => client.disconnect(IoSource::Read),
        }
    }

    fn disconnect(&self, origin: IoSource) -> glib::ControlFlow {
        if !self.connected.replace(false) {
            return glib::ControlFlow::Break;
        }
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
        self.outgoing.borrow_mut().clear();
        self.fail_pending_requests("GDB/MI connection closed");
        (self.event_handler)(self, MiEvent::Disconnected);
        glib::ControlFlow::Break
    }

    fn fail_pending_requests(&self, reason: &str) {
        let pending = std::mem::take(&mut *self.pending.borrow_mut());
        let scoped = self.scoped_request.borrow_mut().take();
        let queued = self.scoped_queue.borrow_mut().drain(..).collect::<Vec<_>>();
        for request in pending.into_values() {
            (request.handler)(self, error_record(reason));
        }
        if let Some(request) = scoped {
            (request.handler)(self, error_record(reason));
        }
        for request in queued {
            (request.handler)(self, error_record(reason));
        }
    }

    fn consume(&self, bytes: &[u8]) {
        let bytes = if self.discarding_oversized_line.get() {
            let Some(newline) = bytes.iter().position(|byte| *byte == b'\n') else {
                return;
            };
            self.discarding_oversized_line.set(false);
            &bytes[newline + 1..]
        } else {
            bytes
        };
        if bytes.is_empty() {
            return;
        }
        let complete_end = {
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
                incoming.clear();
                self.discarding_oversized_line.set(true);
            }
            complete_end
        };
        let Some(complete_end) = complete_end else {
            if self.discarding_oversized_line.get() {
                (self.event_handler)(
                    self,
                    MiEvent::Error(format!(
                        "GDB emitted an MI record larger than {} MiB. The record was discarded",
                        MAX_MI_RECORD_BYTES / (1024 * 1024)
                    )),
                );
            }
            return;
        };
        {
            let incoming = self.incoming.borrow();
            for line in incoming[..complete_end].split(|byte| *byte == b'\n') {
                let line = line.strip_suffix(b"\r").unwrap_or(line);
                if line.is_empty() {
                    continue;
                }
                if line.len() > MAX_MI_RECORD_BYTES {
                    (self.event_handler)(
                        self,
                        MiEvent::Error(String::from("Oversized GDB/MI record discarded")),
                    );
                    continue;
                }
                self.process_line(&String::from_utf8_lossy(line));
            }
        }
        let mut incoming = self.incoming.borrow_mut();
        let remaining = incoming.len().saturating_sub(complete_end);
        incoming.copy_within(complete_end.., 0);
        incoming.truncate(remaining);
        if incoming.capacity() > MAX_RETAINED_MI_INPUT_BYTES
            && incoming.len() < MAX_RETAINED_MI_INPUT_BYTES
        {
            incoming.shrink_to(MAX_RETAINED_MI_INPUT_BYTES);
        }
    }

    fn expire_requests(&self) {
        let now = Instant::now();
        let expired = {
            let pending = self.pending.borrow();
            pending
                .iter()
                .filter_map(|(token, request)| {
                    let stale = request
                        .is_current
                        .as_ref()
                        .is_some_and(|is_current| !is_current());
                    (stale || request.deadline <= now).then_some((*token, stale))
                })
                .collect::<Vec<_>>()
        };
        for (token, stale) in expired {
            self.outgoing.borrow_mut().cancel_unstarted(token);
            let request = { self.pending.borrow_mut().remove(&token) };
            if let Some(request) = request {
                (request.handler)(
                    self,
                    error_record(if stale {
                        "request superseded"
                    } else {
                        "GDB request timed out"
                    }),
                );
            }
        }

        let scoped_reason = self.scoped_request.borrow().as_ref().and_then(|request| {
            if !(request.is_current)() {
                Some("request superseded")
            } else if request.deadline <= now {
                Some("GDB request timed out")
            } else {
                None
            }
        });
        let expired_scoped = scoped_reason.and_then(|reason| {
            self.scoped_request
                .borrow_mut()
                .take()
                .map(|request| (reason, request))
        });
        if let Some((reason, request)) = expired_scoped {
            self.outgoing.borrow_mut().cancel_unstarted(request.token);
            (request.handler)(self, error_record(reason));
            self.start_next_scoped_request();
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
                    client.configure_mi_async();
                },
            )
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
                client.capabilities.borrow_mut().pretty_printing = record.is_success();
                client.finish_initialization();
            })
            .is_err()
            && let Some(client) = weak_client.upgrade()
        {
            client.finish_initialization();
        }
    }

    fn finish_initialization(&self) {
        if !self.connected.get() || self.ready.replace(true) {
            return;
        }
        self.initializing.set(false);
        (self.event_handler)(self, MiEvent::Ready(self.capabilities()));
    }

    fn process_line(&self, line: &str) {
        let line = line.trim();
        if line == "(gdb)" {
            if !self.ready.get() && !self.initializing.replace(true) {
                self.begin_initialization();
            }
            return;
        }

        if line.starts_with('~') {
            // Console stream records matter on this private MI channel only
            // while a scoped `interpreter-exec mi` request is waiting for its
            // nested result. Avoid decoding ignored console output, and avoid
            // constructing a parse error for every ordinary MI record.
            if self.scoped_request.borrow().is_some()
                && let Ok(output) = parse_stream_output(line)
                && let Ok(response) = parse_record(output.trim())
                && response.kind == '^'
                && let Some(request) = self.scoped_request.borrow_mut().as_mut()
            {
                request.response = Some(response);
            }
            return;
        }

        let Ok(record) = parse_record(line) else {
            return;
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
                    let response = request.response.unwrap_or_else(|| {
                        if record.is_done() {
                            error_record("scoped MI command returned no result")
                        } else {
                            record
                        }
                    });
                    (request.handler)(self, response);
                    self.start_next_scoped_request();
                    return;
                }
                let handler = record
                    .token
                    .and_then(|token| self.pending.borrow_mut().remove(&token));
                if let Some(request) = handler {
                    (request.handler)(self, record);
                } else if record.class == "error" {
                    let message = record
                        .error_message()
                        .unwrap_or("GDB command failed")
                        .to_owned();
                    (self.event_handler)(self, MiEvent::Error(message));
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
                let thread_id = record
                    .field("thread-id")
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
                        fork_pid,
                        all_stopped,
                    },
                );
            }
            '=' if record.class.starts_with("breakpoint-") => {
                (self.event_handler)(self, MiEvent::BreakpointsChanged);
            }
            '=' if matches!(record.class.as_str(), "thread-created" | "thread-exited") => {
                let group_id = record
                    .field("group-id")
                    .and_then(MiValue::as_const)
                    .map(str::to_owned);
                (self.event_handler)(self, MiEvent::ThreadsChanged { group_id });
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
            '=' if matches!(
                record.class.as_str(),
                "thread-selected" | "thread-group-selected"
            ) =>
            {
                (self.event_handler)(self, MiEvent::SelectionChanged);
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

fn parse_stream_output(input: &str) -> Result<String, String> {
    let mut parser = Parser::new(input);
    if parser.next() != Some(b'~') {
        return Err(String::from("not a console stream record"));
    }
    let output = parser.c_string()?;
    if parser.position != parser.input.len() {
        return Err(String::from("trailing console stream data"));
    }
    Ok(output)
}

fn error_record(message: &str) -> MiRecord {
    MiRecord {
        token: None,
        kind: '^',
        class: String::from("error"),
        results: vec![MiResult {
            name: String::from("msg"),
            value: MiValue::Const(message.to_owned()),
        }],
    }
}

fn scoped_mi_command(command: &str, elements: usize) -> String {
    let console_command = format!(
        "with print elements {elements} -- interpreter-exec mi {}",
        quote(command)
    );
    format!("-interpreter-exec console {}", quote(&console_command))
}

fn validate_mi_command(command: &str) -> io::Result<()> {
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

fn complete_input_end(incoming: &[u8]) -> Option<usize> {
    incoming
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map(|end| end + 1)
}

impl Drop for MiClient {
    fn drop(&mut self) {
        if let Some(source) = self.read_source.borrow_mut().take() {
            source.remove();
        }
        if let Some(source) = self.write_source.borrow_mut().take() {
            source.remove();
        }
        if let Some(source) = self.timeout_source.borrow_mut().take() {
            source.remove();
        }
    }
}

pub fn parse_record(input: &str) -> Result<MiRecord, String> {
    if input.len() > MAX_MI_RECORD_BYTES {
        return Err(String::from("MI record exceeds the parser byte limit"));
    }
    Parser::new(input).record()
}

struct Parser<'a> {
    input: &'a [u8],
    position: usize,
    depth: usize,
    items: usize,
}

impl<'a> Parser<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            input: input.as_bytes(),
            position: 0,
            depth: 0,
            items: 0,
        }
    }

    fn record(mut self) -> Result<MiRecord, String> {
        let token_start = self.position;
        while self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
            self.position += 1;
        }
        let token = if self.position == token_start {
            None
        } else {
            let digits = std::str::from_utf8(&self.input[token_start..self.position])
                .map_err(|error| error.to_string())?;
            Some(
                digits
                    .parse()
                    .map_err(|_| String::from("MI token exceeds the supported integer range"))?,
            )
        };

        let kind = self.next().ok_or_else(|| String::from("empty MI record"))? as char;
        if !matches!(kind, '^' | '*' | '+' | '=') {
            return Err(format!("unsupported MI record kind {kind:?}"));
        }
        let class = self.identifier()?;
        let mut results = Vec::new();
        while self.consume(b',') {
            results.push(self.result()?);
        }
        if self.position != self.input.len() {
            return Err(String::from("trailing data in MI record"));
        }
        Ok(MiRecord {
            token,
            kind,
            class,
            results,
        })
    }

    fn result(&mut self) -> Result<MiResult, String> {
        self.bump_item()?;
        let name = self.identifier()?;
        self.expect(b'=')?;
        let value = self.value()?;
        Ok(MiResult { name, value })
    }

    fn value(&mut self) -> Result<MiValue, String> {
        match self.peek() {
            Some(b'"') => self.c_string().map(MiValue::Const),
            Some(b'{') => self.tuple(),
            Some(b'[') => self.list(),
            other => Err(format!("invalid MI value start {other:?}")),
        }
    }

    fn tuple(&mut self) -> Result<MiValue, String> {
        self.enter_container()?;
        let result = self.tuple_inner();
        self.depth -= 1;
        result
    }

    fn tuple_inner(&mut self) -> Result<MiValue, String> {
        self.expect(b'{')?;
        // GDB renders breakpoint command lists as `script={"silent",...}`.
        // That is a braced value list rather than the result tuple required by
        // the published MI grammar, but frontends still need to accept GDB's
        // own output. Keep ordinary `{name=value}` tuples unchanged.
        if self.peek() == Some(b'"') {
            let mut items = Vec::new();
            loop {
                self.bump_item()?;
                items.push(MiListItem::Value(self.value()?));
                if self.consume(b'}') {
                    break;
                }
                self.expect(b',')?;
            }
            return Ok(MiValue::List(items));
        }
        let mut results = Vec::new();
        if !self.consume(b'}') {
            loop {
                results.push(self.result()?);
                if self.consume(b'}') {
                    break;
                }
                self.expect(b',')?;
            }
        }
        Ok(MiValue::Tuple(results))
    }

    fn list(&mut self) -> Result<MiValue, String> {
        self.enter_container()?;
        let result = self.list_inner();
        self.depth -= 1;
        result
    }

    fn list_inner(&mut self) -> Result<MiValue, String> {
        self.expect(b'[')?;
        let mut items = Vec::new();
        if !self.consume(b']') {
            loop {
                let item = if self.next_item_is_result() {
                    MiListItem::Result(self.result()?)
                } else {
                    self.bump_item()?;
                    MiListItem::Value(self.value()?)
                };
                items.push(item);
                if self.consume(b']') {
                    break;
                }
                self.expect(b',')?;
            }
        }
        Ok(MiValue::List(items))
    }

    fn enter_container(&mut self) -> Result<(), String> {
        if self.depth >= MAX_MI_NESTING {
            return Err(String::from("MI value exceeds the nesting limit"));
        }
        self.depth += 1;
        Ok(())
    }

    fn bump_item(&mut self) -> Result<(), String> {
        self.items = self.items.saturating_add(1);
        if self.items > MAX_MI_ITEMS {
            Err(String::from("MI record exceeds the item limit"))
        } else {
            Ok(())
        }
    }

    fn next_item_is_result(&self) -> bool {
        let mut position = self.position;
        while self
            .input
            .get(position)
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            position += 1;
        }
        position > self.position && self.input.get(position) == Some(&b'=')
    }

    fn c_string(&mut self) -> Result<String, String> {
        self.expect(b'"')?;
        let start = self.position;
        while let Some(byte) = self.peek() {
            match byte {
                b'"' => {
                    let end = self.position;
                    self.position += 1;
                    return Ok(std::str::from_utf8(&self.input[start..end])
                        .expect("MI parser input originates from a Rust string")
                        .to_owned());
                }
                b'\\' => break,
                _ => self.position += 1,
            }
        }
        if self.peek().is_none() {
            return Err(String::from("unterminated MI string"));
        }
        let mut bytes = Vec::with_capacity(self.position.saturating_sub(start).saturating_add(16));
        bytes.extend_from_slice(&self.input[start..self.position]);
        loop {
            match self.next() {
                Some(b'"') => break,
                Some(b'\\') => self.escape(&mut bytes)?,
                Some(byte) => bytes.push(byte),
                None => return Err(String::from("unterminated MI string")),
            }
        }
        Ok(String::from_utf8(bytes)
            .unwrap_or_else(|error| String::from_utf8_lossy(error.as_bytes()).into_owned()))
    }

    fn escape(&mut self, output: &mut Vec<u8>) -> Result<(), String> {
        let escaped = self
            .next()
            .ok_or_else(|| String::from("unterminated MI escape"))?;
        match escaped {
            b'n' => output.push(b'\n'),
            b'r' => output.push(b'\r'),
            b't' => output.push(b'\t'),
            b'b' => output.push(8),
            b'f' => output.push(12),
            b'v' => output.push(11),
            b'a' => output.push(7),
            // GDB console streams can use the common `\e` extension for the
            // ESC byte even though it is not part of ISO C. Preserve it as an
            // actual terminal escape so downstream ANSI sanitizers can remove
            // the complete control sequence instead of exposing `e[31m`.
            b'e' => output.push(0x1b),
            b'"' => output.push(b'"'),
            b'\\' => output.push(b'\\'),
            b'x' => {
                let start = self.position;
                while self.peek().is_some_and(|byte| byte.is_ascii_hexdigit()) {
                    self.position += 1;
                }
                if start == self.position {
                    return Err(String::from("empty hexadecimal MI escape"));
                }
                let digits = std::str::from_utf8(&self.input[start..self.position])
                    .map_err(|error| error.to_string())?;
                let value = u32::from_str_radix(digits, 16).map_err(|error| error.to_string())?;
                output.push(value as u8);
            }
            b'0'..=b'7' => {
                let mut value = u32::from(escaped - b'0');
                for _ in 0..2 {
                    let Some(next) = self.peek().filter(|byte| matches!(byte, b'0'..=b'7')) else {
                        break;
                    };
                    self.position += 1;
                    value = value * 8 + u32::from(next - b'0');
                }
                output.push(value as u8);
            }
            other => output.push(other),
        }
        Ok(())
    }

    fn identifier(&mut self) -> Result<String, String> {
        let start = self.position;
        while self
            .peek()
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            self.position += 1;
        }
        if start == self.position {
            return Err(String::from("expected MI identifier"));
        }
        Ok(String::from_utf8_lossy(&self.input[start..self.position]).into_owned())
    }

    fn expect(&mut self, expected: u8) -> Result<(), String> {
        if self.consume(expected) {
            Ok(())
        } else {
            Err(format!("expected {:?}", expected as char))
        }
    }

    fn consume(&mut self, expected: u8) -> bool {
        if self.peek() == Some(expected) {
            self.position += 1;
            true
        } else {
            false
        }
    }

    fn peek(&self) -> Option<u8> {
        self.input.get(self.position).copied()
    }

    fn next(&mut self) -> Option<u8> {
        let byte = self.peek()?;
        self.position += 1;
        Some(byte)
    }
}

pub fn quote(argument: &str) -> String {
    let mut quoted = String::with_capacity(argument.len() + 2);
    quoted.push('"');
    for character in argument.chars() {
        match character {
            '\\' => quoted.push_str("\\\\"),
            '"' => quoted.push_str("\\\""),
            '\n' => quoted.push_str("\\n"),
            '\r' => quoted.push_str("\\r"),
            '\t' => quoted.push_str("\\t"),
            other => quoted.push(other),
        }
    }
    quoted.push('"');
    quoted
}

#[cfg(test)]
mod tests {
    use super::{
        GdbCapabilities, MiListItem, MiValue, OutgoingQueue, complete_input_end, drain_outgoing,
        listed_features, parse_record, parse_stream_output, quote, result_field, scoped_mi_command,
        validate_mi_command,
    };

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
        };
        assert!(capabilities.supports("thread-info"));
        assert!(!capabilities.supports("data-read-memory-bytes"));
        assert_eq!(
            capabilities.compatibility_summary(),
            "GDB 17.2 · MI async · pretty printers · feature list"
        );
        assert!(GdbCapabilities::default().supports("future-mi-command"));
    }

    #[test]
    fn replaces_the_transport_without_replacing_the_client() {
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
    fn publishes_ready_only_after_capability_negotiation() {
        use std::{cell::RefCell, rc::Rc};

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
                let events = events.borrow();
                let [super::MiEvent::Ready(capabilities)] = events.as_slice() else {
                    panic!("expected one negotiated ready event, got {events:?}");
                };
                assert_eq!(capabilities.version.as_deref(), Some("17.2"));
                assert!(capabilities.mi_async);
                assert!(capabilities.pretty_printing);
                assert!(capabilities.supports("pending-breakpoints"));
                assert!(!capabilities.supports("thread-info"));
            })
            .unwrap();
    }

    #[test]
    fn publishes_process_scoped_async_events() {
        use std::{cell::RefCell, rc::Rc};

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
                client.process_line(r#"=library-loaded,id="libc",thread-group="i2""#);
                client.process_line(r#"*running,thread-id="all""#);
                client.process_line(
                    r#"*stopped,reason="fork",newpid="4313",thread-id="3",stopped-threads="all",frame={addr="0x401000"}"#,
                );
                client.process_line(r#"=thread-group-exited,id="i2",exit-code="0""#);

                assert_eq!(
                    events.borrow().as_slice(),
                    [
                        super::MiEvent::InferiorStarted {
                            id: String::from("i2"),
                            pid: Some(4312),
                        },
                        super::MiEvent::ThreadsChanged {
                            group_id: Some(String::from("i2")),
                        },
                        super::MiEvent::LibrariesChanged {
                            group_id: Some(String::from("i2")),
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
                            fork_pid: Some(4313),
                            all_stopped: true,
                        },
                        super::MiEvent::InferiorExited {
                            id: String::from("i2"),
                            exit_code: Some(String::from("0")),
                        },
                    ]
                );
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
    }

    #[test]
    fn drains_outgoing_commands_in_fifo_order_and_bounded_batches() {
        let mut outgoing = OutgoingQueue::default();
        outgoing.enqueue(7, "-exec-next").unwrap();
        outgoing.enqueue(8, "-exec-step").unwrap();
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
        outgoing.enqueue(11, "-exec-continue").unwrap();
        let remaining = outgoing.remaining_bytes;

        assert!(!drain_outgoing(&mut BackpressuredWriter, &mut outgoing, 1024).unwrap());
        assert_eq!(outgoing.remaining_bytes, remaining);
        assert_eq!(outgoing.commands.front().unwrap().written, 0);
    }

    #[test]
    fn only_cancels_commands_that_have_not_started_writing() {
        let mut outgoing = OutgoingQueue::default();
        outgoing.enqueue(1, "-first").unwrap();
        outgoing.enqueue(2, "-second").unwrap();
        outgoing.advance(1);

        assert!(!outgoing.cancel_unstarted(1));
        assert!(outgoing.cancel_unstarted(2));
        assert_eq!(outgoing.commands.len(), 1);
        assert_eq!(outgoing.commands.front().unwrap().token, 1);
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
