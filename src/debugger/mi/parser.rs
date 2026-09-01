use super::{MiListItem, MiRecord, MiResult, MiValue};

pub(super) const MAX_MI_RECORD_BYTES: usize = 8 * 1024 * 1024;
pub(super) const MAX_MI_NESTING: usize = 64;
const MAX_MI_ITEMS: usize = 100_000;

#[cfg(test)]
pub(super) fn parse_stream_output(input: &str) -> Result<String, String> {
    parse_stream_output_with_kinds(input, b"~")
}

pub(super) fn parse_any_stream_output(input: &str) -> Result<String, String> {
    parse_stream_output_with_kinds(input, b"~&@")
}

fn parse_stream_output_with_kinds(input: &str, kinds: &[u8]) -> Result<String, String> {
    let mut parser = Parser::new(input);
    if !parser.next().is_some_and(|kind| kinds.contains(&kind)) {
        return Err(String::from("not a supported stream record"));
    }
    let output = parser.c_string()?;
    if parser.position != parser.input.len() {
        return Err(String::from("trailing console stream data"));
    }
    Ok(output)
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
