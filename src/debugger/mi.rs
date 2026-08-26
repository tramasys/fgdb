use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, VecDeque},
    fs::File,
    io::{self, Read, Write},
    os::fd::{AsRawFd, OwnedFd},
    path::{Path, PathBuf},
    rc::{Rc, Weak},
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

struct ScopedMiRequest {
    token: u64,
    command: String,
    response: Option<MiRecord>,
    is_current: Box<dyn Fn() -> bool>,
    handler: ResponseHandler,
}

pub struct MiClient {
    master: RefCell<File>,
    _slave: OwnedFd,
    slave_path: PathBuf,
    incoming: RefCell<Vec<u8>>,
    next_token: Cell<u64>,
    ready: Cell<bool>,
    pending: RefCell<HashMap<u64, ResponseHandler>>,
    scoped_request: RefCell<Option<ScopedMiRequest>>,
    scoped_queue: RefCell<VecDeque<ScopedMiRequest>>,
    event_handler: EventHandler,
    source: RefCell<Option<glib::SourceId>>,
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
        });

        let weak_client = Rc::downgrade(&client);
        let source = glib_unix::unix_fd_add_local(
            master_fd,
            glib::IOCondition::IN | glib::IOCondition::HUP | glib::IOCondition::ERR,
            move |_, condition| Self::on_io_ready(&weak_client, condition),
        );
        client.source.replace(Some(source));

        Ok(client)
    }

    pub fn slave_path(&self) -> &Path {
        &self.slave_path
    }

    pub fn is_ready(&self) -> bool {
        self.ready.get()
    }

    pub fn send(&self, command: &str) -> io::Result<u64> {
        self.write_command(command)
    }

    pub fn request(
        &self,
        command: &str,
        handler: impl FnOnce(&MiClient, MiRecord) + 'static,
    ) -> io::Result<u64> {
        let token = self.allocate_token();
        self.pending.borrow_mut().insert(token, Box::new(handler));
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
        let token = self.allocate_token();
        let command = scoped_mi_command(command, elements);
        let request = ScopedMiRequest {
            token,
            command,
            response: None,
            is_current: Box::new(is_current),
            handler: Box::new(handler),
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
        let token = self.next_token.get();
        self.next_token.set(token.saturating_add(1));
        token
    }

    fn write_command(&self, command: &str) -> io::Result<u64> {
        let token = self.allocate_token();
        self.write_tokenized(token, command)?;
        Ok(token)
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

        if condition.intersects(glib::IOCondition::HUP | glib::IOCondition::ERR) {
            return client.disconnect();
        }

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
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => glib::ControlFlow::Continue,
            Err(_) => client.disconnect(),
        }
    }

    fn disconnect(&self) -> glib::ControlFlow {
        self.source.borrow_mut().take();
        self.pending.borrow_mut().clear();
        self.scoped_request.borrow_mut().take();
        self.scoped_queue.borrow_mut().clear();
        (self.event_handler)(self, MiEvent::Disconnected);
        glib::ControlFlow::Break
    }

    fn consume(&self, bytes: &[u8]) {
        let lines = {
            let mut incoming = self.incoming.borrow_mut();
            incoming.extend_from_slice(bytes);
            take_complete_lines(&mut incoming)
        };
        for line in lines {
            self.process_line(&line);
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
                    let request = self
                        .scoped_request
                        .borrow_mut()
                        .take()
                        .expect("scoped request checked above");
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
                if let Some(handler) = handler {
                    handler(self, record);
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

fn take_complete_lines(incoming: &mut Vec<u8>) -> Vec<String> {
    let mut lines = Vec::new();
    let mut start = 0;
    for newline in incoming
        .iter()
        .enumerate()
        .filter_map(|(index, byte)| (*byte == b'\n').then_some(index))
    {
        let end = if newline > start && incoming[newline - 1] == b'\r' {
            newline - 1
        } else {
            newline
        };
        lines.push(String::from_utf8_lossy(&incoming[start..end]).into_owned());
        start = newline + 1;
    }
    if start != 0 {
        incoming.drain(..start);
    }
    lines
}

impl Drop for MiClient {
    fn drop(&mut self) {
        if let Some(source) = self.source.borrow_mut().take() {
            source.remove();
        }
    }
}

pub fn parse_record(input: &str) -> Result<MiRecord, String> {
    Parser::new(input).record()
}

struct Parser<'a> {
    input: &'a [u8],
    position: usize,
}

impl<'a> Parser<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            input: input.as_bytes(),
            position: 0,
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
            std::str::from_utf8(&self.input[token_start..self.position])
                .ok()
                .and_then(|digits| digits.parse().ok())
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
        self.expect(b'[')?;
        let mut items = Vec::new();
        if !self.consume(b']') {
            loop {
                let saved = self.position;
                let item = if self.identifier().is_ok() && self.peek() == Some(b'=') {
                    self.position = saved;
                    MiListItem::Result(self.result()?)
                } else {
                    self.position = saved;
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
        Ok(String::from_utf8_lossy(&bytes).into_owned())
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
        scoped_mi_command, take_complete_lines,
    };

    #[test]
    fn extracts_complete_mi_lines_without_discarding_a_partial_record() {
        let mut incoming = b"1^done\r\n*stopped,reason=\"breakpoint-hit\"\n3^do".to_vec();
        assert_eq!(
            take_complete_lines(&mut incoming),
            ["1^done", "*stopped,reason=\"breakpoint-hit\""]
        );
        assert_eq!(incoming, b"3^do");

        incoming.extend_from_slice(b"ne\n");
        assert_eq!(take_complete_lines(&mut incoming), ["3^done"]);
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
