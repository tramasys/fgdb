//! Conservative parsing of pointer metadata returned by GDB.

pub(super) fn is_pointer_type(type_name: &str) -> bool {
    let type_name = type_name.trim();

    if crate::language::is_fortran_array(type_name)
        || crate::language::uses_fortran_kind_star(type_name)
    {
        return false;
    }

    if type_name.starts_with(['&', '^', '*'])
        || type_name.starts_with("[^]")
        || type_name.starts_with("access ")
    {
        return true;
    }

    let mut generic_depth = 0_usize;
    let mut pointer = false;
    let mut parenthesized_pointer = false;

    for character in type_name.chars() {
        match character {
            '<' => generic_depth += 1,
            '>' => generic_depth = generic_depth.saturating_sub(1),
            '*' | '&' if generic_depth == 0 => pointer = true,
            ')' if generic_depth == 0 && pointer => parenthesized_pointer = true,
            // Distinguish arrays of pointers from pointers to arrays.
            '[' if generic_depth == 0 => return pointer && parenthesized_pointer,
            _ => {}
        }
    }

    pointer
}

pub(super) fn address(value: &str) -> Option<u64> {
    let mut value = value.trim();

    // GDB can prefix addresses with a cast, including function-pointer casts.
    while value.starts_with('(') {
        let mut depth = 0_usize;
        let end = value.char_indices().find_map(|(index, character)| {
            match character {
                '(' => depth += 1,
                ')' => depth = depth.saturating_sub(1),
                _ => {}
            }

            (depth == 0).then_some(index + character.len_utf8())
        })?;

        value = value[end..].trim_start();
    }

    let value = value.strip_prefix('@').unwrap_or(value);
    let digits = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))?;
    let end = digits
        .find(|character: char| !character.is_ascii_hexdigit())
        .unwrap_or(digits.len());
    let suffix = &digits[end..];

    if !suffix.is_empty()
        && !suffix.starts_with(|character: char| character.is_whitespace() || character == ':')
    {
        return None;
    }

    u64::from_str_radix(&digits[..end], 16).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_outer_pointer_types_are_pointers() {
        for name in [
            "Node *",
            "Node * const",
            "Node &",
            "Node &&",
            "&mut Node",
            "*const Node",
            "^Node",
            "[^]Node",
            "void (*)(int)",
            "int (*)[4]",
            "std::vector<Node *> *",
            "access long_long_integer",
            "access all fixture.node",
            "access array (1 .. 4) of integer",
        ] {
            assert!(is_pointer_type(name), "{name}");
        }

        for name in [
            "std::vector<Node *>",
            "Option<*mut Node>",
            "[&Node; 4]",
            "Node *[4]",
            "int (*[4])(void)",
            "character*24",
            "integer(kind=4) (-1:1,4:5)",
            "array (1 .. 4) of access fixture.node",
        ] {
            assert!(!is_pointer_type(name), "{name}");
        }
    }

    #[test]
    fn addresses_must_be_pointer_values_not_embedded_payloads() {
        for value in [
            "0x0",
            "(Node *) 0x0",
            "@0x0",
            "(void (*)(int)) 0x0",
            "0x0 <null>",
        ] {
            assert_eq!(address(value), Some(0), "{value}");
        }

        assert_eq!(address("0x1234 \"text\""), Some(0x1234));
        assert_eq!(address("@0x1234: {value = 7}"), Some(0x1234));

        for value in [
            "{value = 0x0}",
            "Rc(value = 0x0)",
            "Some(0x0)",
            "0xgarbage",
            "0x0garbage",
            "(Node *",
            "0x10000000000000000",
        ] {
            assert_eq!(address(value), None, "{value}");
        }
    }
}
