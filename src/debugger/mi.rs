use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, VecDeque},
    fs::File,
    io::{self, Read, Write},
    os::fd::{AsRawFd, OwnedFd},
    path::{Path, PathBuf},
    rc::{Rc, Weak},
    time::{Duration, Instant},
};

use gtk::glib;
use nix::{
    pty::openpty,
    sys::termios::{SetArg, cfmakeraw, tcgetattr, tcsetattr},
    unistd::ttyname,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MiEvent {
    Ready,
    InferiorStarted,
    Running,
    Stopped {
        reason: Option<String>,
        signal_name: Option<String>,
        signal_meaning: Option<String>,
    },
    BreakpointsChanged,
    ThreadsChanged,
    LibrariesChanged,
    SelectionChanged,
    Error(String),
    Disconnected,
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
const MAX_MI_NESTING: usize = 64;
const MAX_MI_ITEMS: usize = 100_000;
const MAX_MI_COMMAND_BYTES: usize = 1024 * 1024;
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

pub struct MiClient {
    master: RefCell<File>,
    _slave: OwnedFd,
    slave_path: PathBuf,
    incoming: RefCell<Vec<u8>>,
    next_token: Cell<u64>,
    ready: Cell<bool>,
    pending: RefCell<HashMap<u64, PendingRequest>>,
    scoped_request: RefCell<Option<ScopedMiRequest>>,
    scoped_queue: RefCell<VecDeque<ScopedMiRequest>>,
    event_handler: EventHandler,
    source: RefCell<Option<glib::SourceId>>,
    timeout_source: RefCell<Option<glib::SourceId>>,
    discarding_oversized_line: Cell<bool>,
}

impl MiClient {
    pub fn open(event_handler: impl Fn(&MiClient, MiEvent) + 'static) -> io::Result<Rc<Self>> {
        let pty = openpty(None, None).map_err(io::Error::other)?;
        let slave_path = ttyname(&pty.slave).map_err(io::Error::other)?;

        let mut terminal_settings = tcgetattr(&pty.slave).map_err(io::Error::other)?;
        cfmakeraw(&mut terminal_settings);
        tcsetattr(&pty.slave, SetArg::TCSANOW, &terminal_settings).map_err(io::Error::other)?;

        let master = File::from(pty.master);
        let master_fd = master.as_raw_fd();
        let client = Rc::new(Self {
            master: RefCell::new(master),
            _slave: pty.slave,
            slave_path,
            incoming: RefCell::new(Vec::new()),
            next_token: Cell::new(1),
            ready: Cell::new(false),
            pending: RefCell::new(HashMap::new()),
            scoped_request: RefCell::new(None),
            scoped_queue: RefCell::new(VecDeque::new()),
            event_handler: Box::new(event_handler),
            source: RefCell::new(None),
            timeout_source: RefCell::new(None),
            discarding_oversized_line: Cell::new(false),
        });

        let weak_client = Rc::downgrade(&client);
        let source = glib_unix::unix_fd_add_local(
            master_fd,
            glib::IOCondition::IN | glib::IOCondition::HUP | glib::IOCondition::ERR,
            move |_, condition| Self::on_io_ready(&weak_client, condition),
        );
        client.source.replace(Some(source));
        let weak_client = Rc::downgrade(&client);
        let timeout_source = glib::timeout_add_local(REQUEST_TIMEOUT_POLL, move || {
            let Some(client) = weak_client.upgrade() else {
                return glib::ControlFlow::Break;
            };
            client.expire_requests();
            glib::ControlFlow::Continue
        });
        client.timeout_source.replace(Some(timeout_source));

        Ok(client)
    }

    pub fn slave_path(&self) -> &Path {
        &self.slave_path
    }

    pub fn is_ready(&self) -> bool {
        self.ready.get()
    }

    pub fn send(&self, command: &str) -> io::Result<u64> {
        self.request(command, |client, record| {
            if !record.is_done() {
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
        let queued_requests = usize::from(self.scoped_request.borrow().is_some())
            .saturating_add(self.scoped_queue.borrow().len());
        if queued_requests >= MAX_SCOPED_REQUESTS {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "too many queued scoped GDB/MI requests",
            ));
        }
        let token = self.allocate_token();
        let request = ScopedMiRequest {
            token,
            command,
            response: None,
            is_current: Box::new(is_current),
            handler: Box::new(handler),
            deadline: Instant::now() + REQUEST_TIMEOUT,
        };
        if !(request.is_current)() {
            (request.handler)(self, error_record("request superseded"));
            return Ok(token);
        }
        if self.scoped_request.borrow().is_some() || !self.scoped_queue.borrow().is_empty() {
            self.scoped_queue.borrow_mut().push_back(request);
        } else if let Err(failure) = self.start_scoped_request(request) {
            return Err(failure.0);
        }
        Ok(token)
    }

    fn start_scoped_request(
        &self,
        request: ScopedMiRequest,
    ) -> Result<(), Box<(io::Error, ScopedMiRequest)>> {
        if let Err(error) = self.write_tokenized(request.token, &request.command) {
            return Err(Box::new((error, request)));
        }
        self.scoped_request.replace(Some(request));
        Ok(())
    }

    fn start_next_scoped_request(&self) {
        while self.scoped_request.borrow().is_none() {
            let Some(request) = self.scoped_queue.borrow_mut().pop_front() else {
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
        let mut master = self.master.borrow_mut();
        writeln!(master, "{token}{command}")?;
        master.flush()
    }

    fn on_io_ready(weak_client: &Weak<Self>, condition: glib::IOCondition) -> glib::ControlFlow {
        let Some(client) = weak_client.upgrade() else {
            return glib::ControlFlow::Break;
        };

        let mut bytes = [0_u8; 16 * 1024];
        let read_result = {
            let mut master = client.master.borrow_mut();
            master.read(&mut bytes)
        };

        match read_result {
            Ok(0) => client.disconnect(),
            Ok(length) => {
                client.consume(&bytes[..length]);
                glib::ControlFlow::Continue
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                if condition.intersects(glib::IOCondition::HUP | glib::IOCondition::ERR) {
                    client.disconnect()
                } else {
                    glib::ControlFlow::Continue
                }
            }
            Err(_) => client.disconnect(),
        }
    }

    fn disconnect(&self) -> glib::ControlFlow {
        self.ready.set(false);
        self.source.borrow_mut().take();
        if let Some(source) = self.timeout_source.borrow_mut().take() {
            source.remove();
        }
        self.pending.borrow_mut().clear();
        self.scoped_request.borrow_mut().take();
        self.scoped_queue.borrow_mut().clear();
        (self.event_handler)(self, MiEvent::Disconnected);
        glib::ControlFlow::Break
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
        let complete = {
            let mut incoming = self.incoming.borrow_mut();
            incoming.extend_from_slice(bytes);
            let complete = take_complete_input(&mut incoming);
            if complete.is_none() && incoming.len() > MAX_MI_RECORD_BYTES {
                incoming.clear();
                self.discarding_oversized_line.set(true);
            }
            complete
        };
        let Some(complete) = complete else {
            if self.discarding_oversized_line.get() {
                (self.event_handler)(
                    self,
                    MiEvent::Error(format!(
                        "GDB emitted an MI record larger than {} MiB; the record was discarded",
                        MAX_MI_RECORD_BYTES / (1024 * 1024)
                    )),
                );
            }
            return;
        };
        for line in complete.split(|byte| *byte == b'\n') {
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
            if let Some(request) = self.pending.borrow_mut().remove(&token) {
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
        if let Some(reason) = scoped_reason
            && let Some(request) = self.scoped_request.borrow_mut().take()
        {
            (request.handler)(self, error_record(reason));
            self.start_next_scoped_request();
        }
    }

    fn process_line(&self, line: &str) {
        if line.trim() == "(gdb)" {
            if !self.ready.replace(true) {
                let _ = self.request("-gdb-set mi-async on", |_, _| {});
                (self.event_handler)(self, MiEvent::Ready);
            }
            return;
        }

        if let Ok(output) = parse_stream_output(line.trim()) {
            if let Ok(response) = parse_record(output.trim())
                && response.kind == '^'
                && let Some(request) = self.scoped_request.borrow_mut().as_mut()
            {
                request.response = Some(response);
            }
            return;
        }

        let Ok(record) = parse_record(line.trim()) else {
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
                    let Some(request) = self.scoped_request.borrow_mut().take() else {
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
                (self.event_handler)(self, MiEvent::Running);
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
                (self.event_handler)(
                    self,
                    MiEvent::Stopped {
                        reason,
                        signal_name,
                        signal_meaning,
                    },
                );
            }
            '=' if record.class.starts_with("breakpoint-") => {
                (self.event_handler)(self, MiEvent::BreakpointsChanged);
            }
            '=' if matches!(record.class.as_str(), "thread-created" | "thread-exited") => {
                (self.event_handler)(self, MiEvent::ThreadsChanged);
            }
            '=' if record.class == "thread-group-started" => {
                (self.event_handler)(self, MiEvent::InferiorStarted);
            }
            '=' if matches!(record.class.as_str(), "library-loaded" | "library-unloaded") => {
                (self.event_handler)(self, MiEvent::LibrariesChanged);
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

fn take_complete_input(incoming: &mut Vec<u8>) -> Option<Vec<u8>> {
    let end = incoming.iter().rposition(|byte| *byte == b'\n')? + 1;
    let remainder = incoming.split_off(end);
    Some(std::mem::replace(incoming, remainder))
}

impl Drop for MiClient {
    fn drop(&mut self) {
        if let Some(source) = self.source.borrow_mut().take() {
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
        let mut bytes = Vec::new();
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
        MiListItem, MiValue, parse_record, parse_stream_output, quote, result_field,
        scoped_mi_command, take_complete_input, validate_mi_command,
    };

    #[test]
    fn extracts_complete_mi_lines_without_discarding_a_partial_record() {
        let mut incoming = b"1^done\r\n*stopped,reason=\"breakpoint-hit\"\n3^do".to_vec();
        assert_eq!(
            take_complete_input(&mut incoming),
            Some(b"1^done\r\n*stopped,reason=\"breakpoint-hit\"\n".to_vec())
        );
        assert_eq!(incoming, b"3^do");

        incoming.extend_from_slice(b"ne\n");
        assert_eq!(
            take_complete_input(&mut incoming),
            Some(b"3^done\n".to_vec())
        );
        assert!(incoming.is_empty());
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
}
