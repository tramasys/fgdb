//! Bounded, bit-preserving vector snapshots. GDB register layouts belong here,
//! independently of the widgets and the target's source language.

use std::fmt::Write as _;

use super::StopContext;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct VectorValue {
    words: [u64; 8],
    bytes: usize,
}

pub(crate) fn register_bytes(name: &str) -> Option<usize> {
    [("xmm", 16), ("ymm", 32), ("zmm", 64)]
        .into_iter()
        .find_map(|(prefix, bytes)| {
            let index = name.strip_prefix(prefix)?;

            (!index.is_empty()
                && index.bytes().all(|byte| byte.is_ascii_digit())
                && index.parse::<u8>().is_ok_and(|index| index < 32))
            .then_some(bytes)
        })
}

impl VectorValue {
    pub(crate) fn parse(name: &str, value: &str) -> Option<Self> {
        let bytes = register_bytes(name)?;

        if value.len() > 64 * 1024 {
            return None;
        }

        let field = format!("v{}_int64", bytes / 8);
        let start = value.match_indices(&field).find_map(|(index, _)| {
            let before = value[..index].chars().next_back()?;
            let after = value[index + field.len()..].trim_start();

            (matches!(before, '{' | ',' | ' ' | '\n' | '\t') && after.starts_with('='))
                .then_some(after)
        })?;

        let body = start.strip_prefix('=')?.trim_start().strip_prefix('{')?;
        let body = body.split_once('}')?.0;
        let mut result = Self {
            words: [0; 8],
            bytes,
        };
        let mut count = 0;

        for part in body.split(',') {
            let part = part.trim();

            let part = if let Some(indexed) = part.strip_prefix('[') {
                let (index, lane) = indexed.split_once(']')?;

                if parse_word(index)? != count as u64 {
                    return None;
                }

                lane.trim_start().strip_prefix('=')?.trim()
            } else {
                part
            };

            let (value, repeats) = if let Some((value, repeats)) = part.split_once("<repeats ") {
                let repeats = repeats.strip_suffix(" times>")?.parse::<usize>().ok()?;
                (value.trim(), repeats)
            } else {
                (part, 1)
            };

            if repeats == 0 || repeats > bytes / 8 - count {
                return None;
            }

            result.words[count..count + repeats].fill(parse_word(value)?);
            count += repeats;
        }

        (count == bytes / 8).then_some(result)
    }

    pub(crate) fn bytes(&self) -> usize {
        self.bytes
    }

    pub(crate) fn lane(&self, index: usize, bytes: usize) -> Option<u64> {
        if !matches!(bytes, 1 | 2 | 4 | 8) || index >= self.bytes / bytes {
            return None;
        }

        let offset = index * bytes;
        let mask = u64::MAX >> (64 - bytes * 8);
        Some((self.words[offset / 8] >> ((offset % 8) * 8)) & mask)
    }

    pub(crate) fn set_lane(&mut self, index: usize, bytes: usize, value: u64) -> bool {
        if self.lane(index, bytes).is_none() {
            return false;
        }

        let offset = index * bytes;
        let shift = (offset % 8) * 8;
        let mask = u64::MAX >> (64 - bytes * 8);

        if value & !mask != 0 {
            return false;
        }

        self.words[offset / 8] = (self.words[offset / 8] & !(mask << shift)) | (value << shift);
        true
    }
}

fn parse_word(value: &str) -> Option<u64> {
    if let Some(hex) = value.strip_prefix("0x") {
        u64::from_str_radix(hex, 16).ok()
    } else if value.starts_with('-') {
        value.parse::<i64>().ok().map(|value| value as u64)
    } else {
        value.parse().ok()
    }
}

pub(crate) struct VectorWrite {
    pub(crate) context: StopContext,
    pub(crate) register: String,
    pub(crate) original: VectorValue,
    pub(crate) edited: VectorValue,
}

impl VectorWrite {
    pub(crate) fn expression(&self) -> Option<String> {
        let bytes = register_bytes(&self.register)?;

        if self.original.bytes != bytes
            || self.edited.bytes != bytes
            || self.original == self.edited
        {
            return None;
        }

        let field = format!("${}.v{}_int64", self.register, bytes / 8);
        let mut expression = String::with_capacity(1024);
        expression.push('(');

        for (index, value) in self.original.words[..bytes / 8].iter().enumerate() {
            if index != 0 {
                expression.push_str(" && ");
            }

            let _ = write!(expression, "{field}[{index}] == 0x{value:016x}ULL");
        }

        let _ = write!(expression, ") ? ({field} = {{");

        for (index, value) in self.edited.words[..bytes / 8].iter().enumerate() {
            if index != 0 {
                expression.push_str(", ");
            }

            let _ = write!(expression, "0x{value:016x}ULL");
        }

        // Check all original bits before one aggregate register assignment.
        // No user expression or target function call enters this command.
        expression.push_str("}, 1) : 0");
        Some(expression)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshots_are_exact_bounded_and_preserve_other_lanes() {
        let mut value = VectorValue::parse(
            "ymm0",
            "{v4_int64 = {[0x0] = 0x800000003f800000, [1] = 0 <repeats 3 times>}}",
        )
        .unwrap();
        assert_eq!(value.lane(0, 4), Some(0x3f800000));
        assert_eq!(value.lane(1, 4), Some(0x80000000));
        assert!(value.set_lane(1, 1, 0xff));
        assert_eq!(value.lane(0, 8), Some(0x800000003f80ff00));
        assert!(!value.set_lane(32, 1, 1));
        assert!(!value.set_lane(0, 1, 256));

        for text in [
            "{v4_int64 = {0 <repeats 1000000 times>}}",
            "{v4_int64 = {0, 0}}",
            "{v4_int64 = {[1] = 0 <repeats 4 times>}}",
            "{other_v4_int64 = {0,0,0,0}}",
            "<unavailable>",
        ] {
            assert!(VectorValue::parse("ymm0", text).is_none(), "{text}");
        }

        assert!(VectorValue::parse("xmm0;quit", "{v2_int64 = {0,0}}").is_none());

        let original = VectorValue::parse("xmm0", "{v2_int64 = {0, 0x8000000000000000}}").unwrap();
        let mut edited = original.clone();
        assert!(edited.set_lane(0, 4, 0x3fc00000));
        let mut write = VectorWrite {
            context: StopContext::new(1, 1, Some("i1".into()), "1".into(), 0).unwrap(),
            register: "xmm0".into(),
            original,
            edited,
        };

        assert_eq!(
            write.expression().unwrap(),
            "($xmm0.v2_int64[0] == 0x0000000000000000ULL && $xmm0.v2_int64[1] == 0x8000000000000000ULL) ? ($xmm0.v2_int64 = {0x000000003fc00000ULL, 0x8000000000000000ULL}, 1) : 0"
        );
        write.register = "ymm0".into();
        assert!(write.expression().is_none());
    }

    #[test]
    #[ignore = "requires GDB, an x86 host and the c-simd-target fixture"]
    fn live_vector_writes_preserve_bits_and_reject_conflicts() {
        let original = VectorValue::parse(
            "xmm0",
            "{v2_int64 = {0xc02000003f800000, 0x7fc0004280000000}}",
        )
        .unwrap();
        let mut edited = original.clone();
        assert!(edited.set_lane(0, 4, 0x3fc00000));
        let write = VectorWrite {
            context: StopContext::new(1, 1, Some("i1".into()), "1".into(), 0).unwrap(),
            register: "xmm0".into(),
            original,
            edited,
        };
        let expression = write.expression().unwrap();

        let output = std::process::Command::new("gdb")
            .args([
                "-q",
                "-nx",
                "-batch",
                "-iex",
                "set debuginfod enabled off",
                "target/debug-fixtures/c-simd-target",
            ])
            .args(["-ex", "break simd_sse_checkpoint", "-ex", "run"])
            .args([
                "-ex",
                &format!("p {expression}"),
                "-ex",
                &format!("p {expression}"),
            ])
            .args(["-ex", "p/x $xmm0.v2_int64", "-ex", "continue"])
            .output()
            .expect("start GDB");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "{stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(stdout.contains("$1 = 1"), "{stdout}");
        assert!(stdout.contains("$2 = 0"), "{stdout}");
        assert!(
            stdout.contains("c02000003fc00000  7fc0004280000000"),
            "{stdout}"
        );
    }
}
