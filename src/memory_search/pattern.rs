use super::MAX_PATTERN;
use crate::debugger::TargetEndian;

#[cfg(test)]
mod tests;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SearchKind {
    Text,
    Bytes,
    Pointer,
    Unsigned(u32),
    Signed(u32),
    Float(u32),
}

impl SearchKind {
    pub(crate) const ALL: [(Self, &'static str); 13] = [
        (Self::Text, "UTF-8 text"),
        (Self::Bytes, "Byte pattern"),
        (Self::Pointer, "Pointer"),
        (Self::Unsigned(8), "Unsigned 8-bit"),
        (Self::Unsigned(16), "Unsigned 16-bit"),
        (Self::Unsigned(32), "Unsigned 32-bit"),
        (Self::Unsigned(64), "Unsigned 64-bit"),
        (Self::Signed(8), "Signed 8-bit"),
        (Self::Signed(16), "Signed 16-bit"),
        (Self::Signed(32), "Signed 32-bit"),
        (Self::Signed(64), "Signed 64-bit"),
        (Self::Float(32), "Float 32-bit"),
        (Self::Float(64), "Float 64-bit"),
    ];
}

const WORDS: usize = MAX_PATTERN / u64::BITS as usize;
const MIN_PREFILTER_SKIP: usize = 16;

/// Shift-And supports wildcard bytes with bounded work per input byte.
/// State and a small ring buffer carry overlapping matches across read boundaries.
pub(crate) struct Pattern {
    first: Option<u8>,
    masks: Box<[[u64; WORDS]; 256]>,
    state: [u64; WORDS],
    tail: [u8; MAX_PATTERN],
    position: usize,
    len: usize,
    width: usize,
}

impl Pattern {
    pub(crate) fn parse(
        kind: SearchKind,
        input: &str,
        pointer_bits: Option<u32>,
        endian: Option<TargetEndian>,
    ) -> Result<Self, String> {
        if input.len() > MAX_PATTERN * 3 {
            return Err(String::from("Search patterns are limited to 256 bytes"));
        }

        let (bytes, width) = match kind {
            SearchKind::Text => (input.as_bytes().iter().copied().map(Some).collect(), 1),
            SearchKind::Bytes => {
                let mut bytes = Vec::new();

                for token in input.split_ascii_whitespace() {
                    let byte = if token == "??" {
                        None
                    } else if token.len() == 2 && token.bytes().all(|byte| byte.is_ascii_hexdigit())
                    {
                        Some(u8::from_str_radix(token, 16).map_err(|error| error.to_string())?)
                    } else {
                        return Err(String::from(
                            "Use hex byte pairs separated by spaces, with ?? for a wildcard",
                        ));
                    };

                    bytes.push(byte);
                }

                (bytes, 1)
            }
            _ => {
                let bits = match kind {
                    SearchKind::Pointer => pointer_bits
                        .filter(|bits| matches!(bits, 32 | 64))
                        .ok_or("Target pointer width is not known yet")?,
                    SearchKind::Unsigned(bits)
                    | SearchKind::Signed(bits)
                    | SearchKind::Float(bits) => bits,
                    _ => unreachable!(),
                };

                if !matches!(bits, 8 | 16 | 32 | 64) {
                    return Err(String::from("Unsupported numeric width"));
                }

                let endian = endian
                    .or_else(|| (bits == 8).then_some(TargetEndian::Little))
                    .ok_or("Target byte order is not known yet")?;

                let value = match kind {
                    SearchKind::Float(32) => u64::from(
                        input
                            .trim()
                            .parse::<f32>()
                            .map_err(|_| "Enter a 32-bit floating-point value")?
                            .to_bits(),
                    ),
                    SearchKind::Float(64) => input
                        .trim()
                        .parse::<f64>()
                        .map_err(|_| "Enter a 64-bit floating-point value")?
                        .to_bits(),
                    SearchKind::Float(_) => {
                        return Err(String::from("Unsupported floating-point width"));
                    }
                    SearchKind::Signed(_) => {
                        let input = input.trim();
                        let (negative, magnitude) = input
                            .strip_prefix('-')
                            .map_or((false, input), |magnitude| (true, magnitude));

                        let magnitude = i128::from(parse_address(magnitude)?);
                        let value = if negative { -magnitude } else { magnitude };
                        let bound = 1_i128 << (bits - 1);

                        if !(-bound..bound).contains(&value) {
                            return Err(String::from(
                                "Signed value does not fit the selected type",
                            ));
                        }

                        value as u64
                    }
                    _ => {
                        let value = parse_address(input)?;

                        if bits < 64 && value >= 1_u64 << bits {
                            return Err(String::from("Value does not fit the selected type"));
                        }

                        value
                    }
                };

                let width = bits as usize / 8;
                let bytes = match endian {
                    TargetEndian::Little => value.to_le_bytes()[..width]
                        .iter()
                        .copied()
                        .map(Some)
                        .collect(),
                    TargetEndian::Big => value.to_be_bytes()[8 - width..]
                        .iter()
                        .copied()
                        .map(Some)
                        .collect(),
                };

                (bytes, width)
            }
        };

        if bytes.is_empty() || bytes.len() > MAX_PATTERN {
            return Err(String::from("Enter a pattern containing 1 to 256 bytes"));
        }

        if bytes.iter().all(Option::is_none) {
            return Err(String::from(
                "The pattern must contain at least one exact byte",
            ));
        }

        let mut masks = Box::new([[0; WORDS]; 256]);

        for (index, byte) in bytes.iter().enumerate() {
            let word = index / 64;
            let bit = 1_u64 << (index % 64);

            if let Some(byte) = byte {
                masks[*byte as usize][word] |= bit;
            } else {
                for mask in masks.iter_mut() {
                    mask[word] |= bit;
                }
            }
        }

        Ok(Self {
            first: bytes[0],
            masks,
            state: [0; WORDS],
            tail: [0; MAX_PATTERN],
            position: 0,
            len: bytes.len(),
            width,
        })
    }

    pub(crate) fn len(&self) -> usize {
        self.len
    }

    pub(crate) fn width(&self) -> usize {
        self.width
    }

    pub(crate) fn reset(&mut self) {
        self.state.fill(0);
        self.position = 0;
    }

    /// Visit matches with their exclusive end offset in this input. Returning
    /// false stops after that match, and the result is the consumed byte count.
    pub(crate) fn search(
        &mut self,
        input: &[u8],
        mut matched: impl FnMut(usize, &Self) -> bool,
    ) -> usize {
        let Some(first) = self.first else {
            return self.search_scalar(input, 0, &mut matched);
        };

        if self.state != [0; WORDS] {
            return self.search_scalar(input, 0, &mut matched);
        }

        let mut offset = 0;
        let mut skipped = 0_usize;
        let mut probes = 0_usize;

        while offset < input.len() {
            // Skipping is valid only without a partial match. A later complete
            // match replaces the entire ring, so skipped bytes need no copying.
            if self.state == [0; WORDS] {
                let Some(skip) = memchr::memchr(first, &input[offset..]) else {
                    return input.len();
                };

                // Dense candidates do not benefit from a SIMD prefilter. Use
                // the original bounded matcher for the rest of this block.
                skipped += skip;
                probes += 1;

                if skipped < probes.saturating_mul(MIN_PREFILTER_SKIP) {
                    return self.search_scalar(input, offset, &mut matched);
                }

                offset += skip;
            }

            let found = self.push(input[offset]);
            offset += 1;

            if found && !matched(offset, self) {
                return offset;
            }
        }

        input.len()
    }

    #[inline]
    fn search_scalar(
        &mut self,
        input: &[u8],
        from: usize,
        matched: &mut impl FnMut(usize, &Self) -> bool,
    ) -> usize {
        for (offset, &byte) in input.iter().enumerate().skip(from) {
            if self.push(byte) && !matched(offset + 1, self) {
                return offset + 1;
            }
        }

        input.len()
    }

    #[inline]
    pub(crate) fn push(&mut self, byte: u8) -> bool {
        self.tail[self.position] = byte;
        self.position += 1;

        if self.position == self.len {
            self.position = 0;
        }

        if self.len <= u64::BITS as usize {
            self.state[0] = ((self.state[0] << 1) | 1) & self.masks[byte as usize][0];

            return self.state[0] & (1_u64 << (self.len - 1)) != 0;
        }

        let mut carry = 1;

        for (state, mask) in self.state[..self.len.div_ceil(64)]
            .iter_mut()
            .zip(&self.masks[byte as usize])
        {
            let next = *state >> 63;
            *state = ((*state << 1) | carry) & mask;
            carry = next;
        }

        self.state[(self.len - 1) / 64] & (1_u64 << ((self.len - 1) % 64)) != 0
    }

    pub(crate) fn matched_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(self.len);
        bytes.extend_from_slice(&self.tail[self.position..self.len]);
        bytes.extend_from_slice(&self.tail[..self.position]);
        bytes
    }
}

pub(crate) fn parse_address(text: &str) -> Result<u64, String> {
    let text = text.trim();
    let result = if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16)
    } else {
        text.parse()
    };

    result.map_err(|_| {
        String::from("Enter an unsigned decimal number or a hexadecimal value starting with 0x")
    })
}
