use crate::debugger::Variable;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct IntegerFormat {
    signed: bool,
    bits: u32,
}

impl IntegerFormat {
    const fn signed(bits: u32) -> Self {
        Self { signed: true, bits }
    }

    const fn unsigned(bits: u32) -> Self {
        Self {
            signed: false,
            bits,
        }
    }
}

pub(super) fn integer_decimal_value(
    variable: &Variable,
    value: &str,
    target_pointer_bits: u32,
) -> Option<String> {
    let type_name = variable.type_name.as_deref()?.trim().to_ascii_lowercase();
    if variable.is_pointer()
        || type_name.contains(['[', ']'])
        || ["float", "double", "struct ", "union ", "class "]
            .iter()
            .any(|excluded| type_name.contains(excluded))
    {
        return None;
    }
    let format = integer_format(&type_name, target_pointer_bits)?;

    let value = value.trim();
    let (negative, magnitude) = value
        .strip_prefix('-')
        .map_or((false, value), |value| (true, value));
    let (digits, radix) = magnitude
        .strip_prefix("0x")
        .or_else(|| magnitude.strip_prefix("0X"))
        .map(|digits| (digits, 16))
        .or_else(|| {
            magnitude
                .strip_prefix("0b")
                .or_else(|| magnitude.strip_prefix("0B"))
                .map(|digits| (digits, 2))
        })
        .or_else(|| {
            magnitude
                .strip_prefix("0o")
                .or_else(|| magnitude.strip_prefix("0O"))
                .map(|digits| (digits, 8))
        })?;
    let digits = digits.replace(['_', '\''], "");
    let magnitude = u128::from_str_radix(&digits, radix).ok()?;
    if negative {
        return Some(format!("-{magnitude}"));
    }

    let encoded_bits = match radix {
        2 => digits.len(),
        8 => digits.len().saturating_mul(3),
        16 => digits.len().saturating_mul(4),
        _ => return None,
    };
    if format.signed && encoded_bits >= format.bits as usize {
        let raw = if format.bits == 128 {
            magnitude
        } else {
            magnitude & ((1_u128 << format.bits) - 1)
        };
        if raw & (1_u128 << (format.bits - 1)) != 0 {
            let signed = if format.bits == 128 {
                (raw as i128).to_string()
            } else {
                format!("-{}", (1_u128 << format.bits) - raw)
            };
            return Some(signed);
        }
    }

    Some(magnitude.to_string())
}

fn integer_format(type_name: &str, target_pointer_bits: u32) -> Option<IntegerFormat> {
    let target_pointer_bits = match target_pointer_bits {
        16 | 32 | 64 | 128 => target_pointer_bits,
        _ => 64,
    };
    let final_component = type_name.rsplit("::").next().unwrap_or(type_name);
    let unqualified = final_component
        .split_whitespace()
        .filter(|word| {
            !matches!(
                *word,
                "const" | "volatile" | "restrict" | "__restrict" | "__restrict__" | "_atomic"
            )
        })
        .collect::<Vec<_>>()
        .join(" ");
    let compact = unqualified.replace([' ', '_'], "");

    let primitive = match unqualified.as_str() {
        "i8" => Some(IntegerFormat::signed(8)),
        "u8" => Some(IntegerFormat::unsigned(8)),
        "i16" => Some(IntegerFormat::signed(16)),
        "u16" => Some(IntegerFormat::unsigned(16)),
        "i32" => Some(IntegerFormat::signed(32)),
        "u32" => Some(IntegerFormat::unsigned(32)),
        "i64" => Some(IntegerFormat::signed(64)),
        "u64" => Some(IntegerFormat::unsigned(64)),
        "i128" => Some(IntegerFormat::signed(128)),
        "u128" => Some(IntegerFormat::unsigned(128)),
        "isize" => Some(IntegerFormat::signed(target_pointer_bits)),
        "usize" => Some(IntegerFormat::unsigned(target_pointer_bits)),
        "bool" | "_bool" => Some(IntegerFormat::unsigned(8)),
        "char8_t" => Some(IntegerFormat::unsigned(8)),
        "char16_t" => Some(IntegerFormat::unsigned(16)),
        "char32_t" => Some(IntegerFormat::unsigned(32)),
        "wchar_t" => Some(IntegerFormat::signed(32)),
        "signed char" | "char" | "c_schar" | "c_char" => Some(IntegerFormat::signed(8)),
        "unsigned char" | "c_uchar" => Some(IntegerFormat::unsigned(8)),
        "short" | "short int" | "signed short" | "signed short int" | "c_short" => {
            Some(IntegerFormat::signed(16))
        }
        "unsigned short" | "unsigned short int" | "c_ushort" => Some(IntegerFormat::unsigned(16)),
        "int" | "signed" | "signed int" | "c_int" => Some(IntegerFormat::signed(32)),
        "unsigned" | "unsigned int" | "c_uint" => Some(IntegerFormat::unsigned(32)),
        "long" | "long int" | "signed long" | "signed long int" | "c_long" => {
            Some(IntegerFormat::signed(target_pointer_bits))
        }
        "unsigned long" | "unsigned long int" | "c_ulong" => {
            Some(IntegerFormat::unsigned(target_pointer_bits))
        }
        "long long"
        | "long long int"
        | "signed long long"
        | "signed long long int"
        | "c_longlong" => Some(IntegerFormat::signed(64)),
        "unsigned long long" | "unsigned long long int" | "c_ulonglong" => {
            Some(IntegerFormat::unsigned(64))
        }
        _ => None,
    };
    primitive
        .or_else(|| c_builtin_integer_format(&unqualified, target_pointer_bits))
        .or_else(|| fixed_width_integer_format(&compact, target_pointer_bits))
        .or_else(|| c_bit_int_format(type_name))
        .or_else(|| c_integer_typedef_format(&compact, target_pointer_bits))
        .or_else(|| {
            if compact.contains("unsignedint128") {
                Some(IntegerFormat::unsigned(128))
            } else if compact.contains("int128") {
                Some(IntegerFormat::signed(128))
            } else {
                None
            }
        })
}

fn fixed_width_integer_format(name: &str, target_pointer_bits: u32) -> Option<IntegerFormat> {
    let name = name.trim_start_matches('_');
    let (forced_signedness, name) = if let Some(name) = name.strip_prefix("unsigned") {
        (Some(false), name)
    } else if let Some(name) = name.strip_prefix("signed") {
        (Some(true), name)
    } else {
        (None, name)
    };
    let (signed, width) = if let Some(width) = name.strip_prefix("uint") {
        (false, width)
    } else {
        (true, name.strip_prefix("int")?)
    };
    let (fast, width) = width
        .strip_prefix("fast")
        .map_or((false, width), |width| (true, width));
    let width = width.strip_prefix("least").unwrap_or(width);
    let width = width.strip_suffix('t').unwrap_or(width);
    let bits = width.parse::<u32>().ok()?;
    if !matches!(bits, 8 | 16 | 32 | 64 | 128) {
        return None;
    }
    let bits = if fast {
        bits.max(target_pointer_bits)
    } else {
        bits
    };
    let signed = forced_signedness.unwrap_or(signed);
    Some(if signed {
        IntegerFormat::signed(bits)
    } else {
        IntegerFormat::unsigned(bits)
    })
}

fn c_builtin_integer_format(name: &str, target_pointer_bits: u32) -> Option<IntegerFormat> {
    let words = name.split_whitespace().collect::<Vec<_>>();
    if words.is_empty()
        || words.iter().any(|word| {
            !matches!(
                *word,
                "signed" | "unsigned" | "char" | "short" | "int" | "long"
            )
        })
        || (words.contains(&"signed") && words.contains(&"unsigned"))
        || (words.contains(&"short") && words.contains(&"long"))
    {
        return None;
    }
    let bits = if words.contains(&"char") {
        8
    } else if words.contains(&"short") {
        16
    } else if words.iter().filter(|word| **word == "long").count() >= 2 {
        64
    } else if words.contains(&"long") {
        target_pointer_bits
    } else {
        32
    };
    Some(if words.contains(&"unsigned") {
        IntegerFormat::unsigned(bits)
    } else {
        IntegerFormat::signed(bits)
    })
}

fn c_bit_int_format(type_name: &str) -> Option<IntegerFormat> {
    let marker = type_name.find("_bitint")?;
    let suffix = &type_name[marker + "_bitint".len()..];
    let open = suffix.find('(')?;
    let close = suffix[open + 1..].find(')')? + open + 1;
    let bits = suffix[open + 1..close].trim().parse::<u32>().ok()?;
    if bits == 0 || bits > 128 {
        return None;
    }
    Some(if type_name.contains("unsigned") {
        IntegerFormat::unsigned(bits)
    } else {
        IntegerFormat::signed(bits)
    })
}

fn c_integer_typedef_format(name: &str, target_pointer_bits: u32) -> Option<IntegerFormat> {
    match name.trim_start_matches('_') {
        "sizet" | "rsizet" | "uintptrt" => Some(IntegerFormat::unsigned(target_pointer_bits)),
        "ssizet" | "ptrdifft" | "intptrt" => Some(IntegerFormat::signed(target_pointer_bits)),
        "intmaxt" => Some(IntegerFormat::signed(64)),
        "uintmaxt" => Some(IntegerFormat::unsigned(64)),
        "sigatomict" | "pidt" | "clockt" | "timet" | "offt" => {
            Some(IntegerFormat::signed(target_pointer_bits))
        }
        "wintt" | "uidt" | "gidt" | "modet" | "devt" | "inot" | "nlinkt" | "socklent" => {
            Some(IntegerFormat::unsigned(target_pointer_bits))
        }
        _ => None,
    }
}

pub(super) fn architecture_pointer_bits(architecture: &str) -> Option<u32> {
    let architecture = architecture.to_ascii_lowercase();
    if architecture.contains("64")
        || architecture.contains("aarch64")
        || architecture.contains("s390x")
    {
        Some(64)
    } else if architecture.contains("32")
        || architecture.contains("i386")
        || architecture.contains("i486")
        || architecture.contains("i586")
        || architecture.contains("i686")
        || architecture.starts_with("arm")
    {
        Some(32)
    } else {
        None
    }
}
