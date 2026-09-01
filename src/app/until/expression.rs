const MAX_UNTIL_EXPRESSION_BYTES: usize = 4096;

pub(super) fn validate(expression: &str) -> Result<(), &'static str> {
    let expression = expression.trim();
    if expression.is_empty() {
        return Err("Enter a GDB expression to evaluate after each instruction.");
    }
    if expression.len() > MAX_UNTIL_EXPRESSION_BYTES {
        return Err("The Until expression is too large.");
    }
    if contains_assignment(expression) {
        return Err(
            "Assignments are not allowed in an Until expression. Use == to compare values.",
        );
    }
    Ok(())
}

fn contains_assignment(expression: &str) -> bool {
    #[derive(Clone, Copy)]
    enum LexicalState {
        Normal,
        SingleQuoted,
        DoubleQuoted,
    }

    let bytes = expression.as_bytes();
    let mut state = LexicalState::Normal;
    let mut index = 0;
    while index < bytes.len() {
        match (state, bytes[index]) {
            (LexicalState::Normal, b'\'') => state = LexicalState::SingleQuoted,
            (LexicalState::Normal, b'"') => state = LexicalState::DoubleQuoted,
            (LexicalState::SingleQuoted, b'\\') | (LexicalState::DoubleQuoted, b'\\') => {
                index = index.saturating_add(1);
            }
            (LexicalState::SingleQuoted, b'\'') | (LexicalState::DoubleQuoted, b'"') => {
                state = LexicalState::Normal
            }
            (LexicalState::Normal, b'=') => {
                let previous = index.checked_sub(1).and_then(|index| bytes.get(index));
                let previous_previous = index.checked_sub(2).and_then(|index| bytes.get(index));
                let next = bytes.get(index + 1);
                let comparison = next == Some(&b'=')
                    || matches!(previous, Some(b'=' | b'!'))
                    || (previous == Some(&b'<') && previous_previous != Some(&b'<'))
                    || (previous == Some(&b'>') && previous_previous != Some(&b'>'));
                if !comparison {
                    return true;
                }
            }
            _ => {}
        }
        index = index.saturating_add(1);
    }
    false
}

pub(super) fn parse_value(value: &str) -> Option<bool> {
    let value = value.trim();
    if value.eq_ignore_ascii_case("true") {
        return Some(true);
    }
    if value.eq_ignore_ascii_case("false") {
        return Some(false);
    }
    let value = value.split_whitespace().next()?.trim_matches(['(', ')']);
    let (negative, digits) = value
        .strip_prefix('-')
        .map_or((false, value), |digits| (true, digits));
    let number = digits
        .strip_prefix("0x")
        .or_else(|| digits.strip_prefix("0X"))
        .map_or_else(
            || digits.parse::<u128>().ok(),
            |digits| u128::from_str_radix(digits, 16).ok(),
        )?;
    Some(negative || number != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_comparisons_and_quoted_equals_without_allowing_assignments() {
        for expression in [
            "$rax == 0",
            "*(int*)$rbx != 4",
            "left <= right",
            "left >= right",
            "left <=> right",
            "c == '='",
            "strcmp(s, \"=\") == 0",
            "foo(\"a=b\") == 1",
            "foo(\"a=\\\"b\") == 1",
        ] {
            assert!(validate(expression).is_ok(), "rejected {expression:?}");
        }
        for expression in [
            "$rax = 0",
            "value += 1",
            "value -= 1",
            "value *= 2",
            "value /= 2",
            "value %= 2",
            "value <<= 1",
            "value >>= 1",
            "value &= mask",
            "value |= mask",
            "value ^= mask",
        ] {
            assert!(validate(expression).is_err(), "accepted {expression:?}");
        }
    }
}
