use crate::debugger::{TargetArchitecture, ValueTypeKind, ValueTypeMetadata, Variable};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct IntegerFormat {
    signed: bool,
    bits: u32,
}

impl IntegerFormat {
    pub(super) const fn signed(bits: u32) -> Self {
        Self { signed: true, bits }
    }

    pub(super) const fn unsigned(bits: u32) -> Self {
        Self {
            signed: false,
            bits,
        }
    }

    const fn mask(self) -> u128 {
        if self.bits == 128 {
            u128::MAX
        } else {
            (1_u128 << self.bits) - 1
        }
    }

    const fn sign_bit(self) -> u128 {
        1_u128 << (self.bits - 1)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum IntegerRadix {
    Hexadecimal,
    Decimal,
    Binary,
    Octal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FloatRepresentation {
    Decimal,
    Scientific,
    HexFloat,
    RawBits,
}

impl FloatRepresentation {
    pub(super) const ALL: [Self; 4] = [
        Self::Decimal,
        Self::Scientific,
        Self::HexFloat,
        Self::RawBits,
    ];

    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::Decimal => "Decimal",
            Self::Scientific => "Scientific",
            Self::HexFloat => "Hex float",
            Self::RawBits => "Raw IEEE bits",
        }
    }

    pub(super) fn from_index(index: u32) -> Self {
        Self::ALL
            .get(index as usize)
            .copied()
            .unwrap_or(Self::Decimal)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct FloatEdit {
    pub(super) bits: u32,
    pub(super) raw_bytes: Vec<u8>,
}

pub(super) fn variable_float_edit(
    variable: &Variable,
    metadata: Option<&ValueTypeMetadata>,
) -> Option<FloatEdit> {
    let metadata = metadata.filter(|metadata| metadata.kind == ValueTypeKind::Float)?;
    let bits = metadata.bits.filter(|bits| matches!(bits, 32 | 64))?;
    let raw_bytes = metadata.raw_bytes.clone().or_else(|| {
        let value = variable.value.split_whitespace().next()?;
        parse_float_value(value, bits, FloatRepresentation::Decimal).ok()
    })?;
    (raw_bytes.len() * 8 == bits as usize).then_some(FloatEdit { bits, raw_bytes })
}

pub(super) fn format_float_value(
    raw_bytes: &[u8],
    bits: u32,
    representation: FloatRepresentation,
) -> String {
    if representation == FloatRepresentation::RawBits {
        return format!("0x{}", encode_hex(raw_bytes));
    }
    match bits {
        32 => {
            let raw = u32::from_be_bytes(raw_bytes.try_into().unwrap_or_default());
            let value = f32::from_bits(raw);
            format_float32(value, raw, representation)
        }
        64 => {
            let raw = u64::from_be_bytes(raw_bytes.try_into().unwrap_or_default());
            let value = f64::from_bits(raw);
            format_float64(value, raw, representation)
        }
        _ => String::new(),
    }
}

pub(super) fn parse_float_value(
    input: &str,
    bits: u32,
    representation: FloatRepresentation,
) -> Result<Vec<u8>, &'static str> {
    let input = input.trim();
    if input.is_empty() {
        return Err("Enter a floating-point value");
    }
    if representation == FloatRepresentation::RawBits {
        let digits = input
            .strip_prefix("0x")
            .or_else(|| input.strip_prefix("0X"))
            .unwrap_or(input)
            .replace(['_', '\''], "");
        let raw = u128::from_str_radix(&digits, 16)
            .map_err(|_| "Enter the raw bits as a hexadecimal integer")?;
        if bits < 128 && raw >= (1_u128 << bits) {
            return Err("The raw value does not fit the destination type");
        }
        let bytes = raw.to_be_bytes();
        return Ok(bytes[bytes.len() - bits.div_ceil(8) as usize..].to_vec());
    }
    match bits {
        32 => parse_float_number(input, representation)
            .map(|value| (value as f32).to_bits().to_be_bytes().to_vec()),
        64 => parse_float_number(input, representation)
            .map(|value| value.to_bits().to_be_bytes().to_vec()),
        _ => Err("This floating-point width is not editable as a scalar yet"),
    }
}

pub(super) fn canonical_gdb_float(raw_bytes: &[u8], bits: u32) -> String {
    let value = match bits {
        32 => f64::from(f32::from_bits(u32::from_be_bytes(
            raw_bytes.try_into().unwrap_or_default(),
        ))),
        64 => f64::from_bits(u64::from_be_bytes(raw_bytes.try_into().unwrap_or_default())),
        _ => return String::from("0.0"),
    };
    if value.is_nan() {
        String::from("(0.0/0.0)")
    } else if value == f64::INFINITY {
        String::from("(1.0/0.0)")
    } else if value == f64::NEG_INFINITY {
        String::from("(-1.0/0.0)")
    } else {
        value.to_string()
    }
}

fn parse_float_number(
    input: &str,
    representation: FloatRepresentation,
) -> Result<f64, &'static str> {
    let normalized = input.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "inf" | "+inf" | "infinity" | "+infinity" => return Ok(f64::INFINITY),
        "-inf" | "-infinity" => return Ok(f64::NEG_INFINITY),
        "nan" | "+nan" | "-nan" => return Ok(f64::NAN),
        _ => {}
    }
    if representation != FloatRepresentation::HexFloat {
        return normalized
            .replace('_', "")
            .parse()
            .map_err(|_| "Enter a number, inf, -inf, or nan");
    }
    parse_hex_float(&normalized)
}

fn parse_hex_float(input: &str) -> Result<f64, &'static str> {
    let (negative, input) = input
        .strip_prefix('-')
        .map_or((false, input), |input| (true, input));
    let input = input.strip_prefix('+').unwrap_or(input);
    let input = input
        .strip_prefix("0x")
        .ok_or("A hexadecimal float starts with 0x")?;
    let (significand, exponent) = input
        .split_once(['p', 'P'])
        .ok_or("A hexadecimal float needs a binary exponent such as p+0")?;
    let exponent = exponent
        .parse::<i32>()
        .map_err(|_| "The hexadecimal float exponent is invalid")?;
    let (whole, fraction) = significand.split_once('.').unwrap_or((significand, ""));
    let digits = format!("{whole}{fraction}").replace('_', "");
    if digits.is_empty() || digits.len() > 28 {
        return Err("The hexadecimal significand is invalid or too long");
    }
    let magnitude =
        u128::from_str_radix(&digits, 16).map_err(|_| "The hexadecimal significand is invalid")?;
    let binary_exponent = exponent
        .checked_sub(i32::try_from(fraction.len() * 4).map_err(|_| "Exponent is too large")?)
        .ok_or("Exponent is too large")?;
    let value = (magnitude as f64) * 2_f64.powi(binary_exponent);
    Ok(if negative { -value } else { value })
}

fn format_float32(value: f32, raw: u32, representation: FloatRepresentation) -> String {
    match representation {
        FloatRepresentation::Decimal => value.to_string(),
        FloatRepresentation::Scientific => format!("{value:e}"),
        FloatRepresentation::HexFloat => format_hex_float(
            raw >> 31 != 0,
            u64::from((raw >> 23) & 0xff),
            u64::from(raw & 0x7f_ffff),
            127,
            8,
            6,
            value.is_nan(),
            value.is_infinite(),
        ),
        FloatRepresentation::RawBits => unreachable!(),
    }
}

fn format_float64(value: f64, raw: u64, representation: FloatRepresentation) -> String {
    match representation {
        FloatRepresentation::Decimal => value.to_string(),
        FloatRepresentation::Scientific => format!("{value:e}"),
        FloatRepresentation::HexFloat => format_hex_float(
            raw >> 63 != 0,
            (raw >> 52) & 0x7ff,
            raw & 0x000f_ffff_ffff_ffff,
            1023,
            11,
            13,
            value.is_nan(),
            value.is_infinite(),
        ),
        FloatRepresentation::RawBits => unreachable!(),
    }
}

#[allow(clippy::too_many_arguments)]
fn format_hex_float(
    negative: bool,
    exponent: u64,
    fraction: u64,
    bias: i32,
    exponent_bits: u32,
    fraction_digits: usize,
    nan: bool,
    infinite: bool,
) -> String {
    let sign = if negative { "-" } else { "" };
    if nan {
        return format!("{sign}nan");
    }
    if infinite {
        return format!("{sign}inf");
    }
    if exponent == 0 && fraction == 0 {
        return format!("{sign}0x0p+0");
    }
    let max_exponent = (1_u64 << exponent_bits) - 1;
    debug_assert!(exponent < max_exponent);
    let (leading, power) = if exponent == 0 {
        ('0', 1 - bias)
    } else {
        ('1', exponent as i32 - bias)
    };
    let padded_fraction = if fraction_digits == 6 {
        fraction << 1
    } else {
        fraction
    };
    let mut fraction = format!("{padded_fraction:0fraction_digits$x}");
    while fraction.ends_with('0') && fraction.len() > 1 {
        fraction.pop();
    }
    format!("{sign}0x{leading}.{fraction}p{power:+}")
}

fn encode_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(
        String::with_capacity(bytes.len() * 2),
        |mut output, byte| {
            let _ = write!(output, "{byte:02x}");
            output
        },
    )
}

impl IntegerRadix {
    pub(super) const ALL: [Self; 4] = [Self::Hexadecimal, Self::Decimal, Self::Binary, Self::Octal];

    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::Hexadecimal => "Hexadecimal",
            Self::Decimal => "Decimal",
            Self::Binary => "Binary",
            Self::Octal => "Octal",
        }
    }

    pub(super) fn from_index(index: u32) -> Self {
        Self::ALL
            .get(index as usize)
            .copied()
            .unwrap_or(Self::Hexadecimal)
    }

    pub(super) const fn index(self) -> u32 {
        match self {
            Self::Hexadecimal => 0,
            Self::Decimal => 1,
            Self::Binary => 2,
            Self::Octal => 3,
        }
    }

    pub(super) fn detect(value: &str) -> Self {
        let value = value.trim().trim_start_matches(['+', '-']);
        if value.starts_with("0x") || value.starts_with("0X") {
            Self::Hexadecimal
        } else if value.starts_with("0b") || value.starts_with("0B") {
            Self::Binary
        } else if value.starts_with("0o") || value.starts_with("0O") {
            Self::Octal
        } else {
            Self::Decimal
        }
    }

    const fn radix(self) -> u32 {
        match self {
            Self::Hexadecimal => 16,
            Self::Decimal => 10,
            Self::Binary => 2,
            Self::Octal => 8,
        }
    }
}

pub(super) fn variable_integer_format(
    variable: &Variable,
    target_pointer_bits: u32,
    metadata: Option<&ValueTypeMetadata>,
) -> Option<IntegerFormat> {
    let type_name = variable.type_name.as_deref()?.trim().to_ascii_lowercase();
    if variable.is_pointer()
        || type_name.contains(['[', ']'])
        || matches!(type_name.as_str(), "bool" | "_bool")
        || ["float", "double", "struct ", "union ", "class ", "enum "]
            .iter()
            .any(|excluded| type_name.contains(excluded))
    {
        return None;
    }
    target_integer_format(metadata).or_else(|| integer_format(&type_name, target_pointer_bits))
}

pub(super) fn variable_boolean_value(
    variable: &Variable,
    metadata: Option<&ValueTypeMetadata>,
) -> Option<bool> {
    let type_name = variable.type_name.as_deref()?.trim().to_ascii_lowercase();
    let final_component = type_name.rsplit("::").next().unwrap_or(&type_name);
    let unqualified = final_component
        .split_whitespace()
        .filter(|word| !matches!(*word, "const" | "volatile" | "_atomic"))
        .collect::<Vec<_>>()
        .join(" ");
    if !matches!(unqualified.as_str(), "bool" | "_bool" | "c_bool")
        && !metadata.is_some_and(|metadata| metadata.kind == ValueTypeKind::Boolean)
    {
        return None;
    }

    let value = variable
        .value
        .split_whitespace()
        .next()?
        .to_ascii_lowercase();
    match value.as_str() {
        "false" => Some(false),
        "true" => Some(true),
        _ => parse_boolean_integer(&value).map(|value| value != 0),
    }
}

fn parse_boolean_integer(value: &str) -> Option<u128> {
    let (digits, radix) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
        .map(|digits| (digits, 16))
        .or_else(|| {
            value
                .strip_prefix("0b")
                .or_else(|| value.strip_prefix("0B"))
                .map(|digits| (digits, 2))
        })
        .unwrap_or((value, 10));
    u128::from_str_radix(digits, radix).ok()
}

pub(super) fn variable_character_format(
    variable: &Variable,
    target_pointer_bits: u32,
    rust_source: bool,
    metadata: Option<&ValueTypeMetadata>,
) -> Option<IntegerFormat> {
    let type_name = variable.type_name.as_deref()?.trim().to_ascii_lowercase();
    if variable.is_pointer() || type_name.contains(['[', ']']) {
        return None;
    }
    let final_component = type_name.rsplit("::").next().unwrap_or(&type_name);
    let unqualified = final_component
        .split_whitespace()
        .filter(|word| matches!(*word, "char" | "signed" | "unsigned" | "const" | "volatile"))
        .collect::<Vec<_>>()
        .join(" ");
    let rust_language = rust_source
        || metadata.is_some_and(|metadata| metadata.language.as_deref() == Some("rust"));
    if rust_language && unqualified == "char" {
        target_integer_format(metadata).or(Some(IntegerFormat::unsigned(32)))
    } else if matches!(
        unqualified.as_str(),
        "char" | "signed char" | "unsigned char" | "const char" | "volatile char"
    ) || matches!(
        final_component,
        "c_char" | "c_schar" | "c_uchar" | "char8_t" | "char16_t" | "char32_t" | "wchar_t"
    ) {
        target_integer_format(metadata).or_else(|| integer_format(&type_name, target_pointer_bits))
    } else if metadata.is_some_and(|metadata| metadata.kind == ValueTypeKind::Character) {
        target_integer_format(metadata)
    } else {
        None
    }
}

fn target_integer_format(metadata: Option<&ValueTypeMetadata>) -> Option<IntegerFormat> {
    let metadata = metadata?;
    let bits = metadata.bits.filter(|bits| (1..=128).contains(bits))?;
    metadata.signed.map(|signed| {
        if signed {
            IntegerFormat::signed(bits)
        } else {
            IntegerFormat::unsigned(bits)
        }
    })
}

pub(super) fn register_integer_format(
    register_expression: &str,
    target_pointer_bits: u32,
    architecture: TargetArchitecture,
) -> Option<IntegerFormat> {
    let name = register_expression.strip_prefix('$')?;
    if architecture.is_dedicated_address_register(name)
        || is_address_register_name(name)
        || super::formatting::vector_register_for_architecture(name, architecture)
        || super::formatting::floating_register_for_architecture(name, architecture)
    {
        return None;
    }
    let bits = architecture.scalar_register_bits(name, target_pointer_bits);
    Some(IntegerFormat::unsigned(bits))
}

pub(super) fn variable_is_address(variable: &Variable, architecture: TargetArchitecture) -> bool {
    variable.is_pointer()
        || variable.name.strip_prefix('$').is_some_and(|name| {
            architecture.is_dedicated_address_register(name) || is_address_register_name(name)
        })
}

fn is_address_register_name(name: &str) -> bool {
    matches!(
        name,
        "rip"
            | "eip"
            | "pc"
            | "nip"
            | "pswa"
            | "rsp"
            | "esp"
            | "sp"
            | "rbp"
            | "ebp"
            | "fp"
            | "lr"
            | "ra"
            | "gp"
            | "tp"
            | "fs_base"
            | "gs_base"
            | "tpidr_el0"
            | "tpidrro_el0"
            | "tpidruro"
            | "tpidrurw"
    )
}

pub(super) fn parse_integer_input(
    input: &str,
    format: IntegerFormat,
    selected_radix: IntegerRadix,
) -> Result<u128, &'static str> {
    let input = input.trim();
    if input.is_empty() {
        return Err("Enter a value");
    }
    let (negative, input) = input
        .strip_prefix('-')
        .map_or((false, input), |value| (true, value));
    let input = input.strip_prefix('+').unwrap_or(input);
    let (digits, radix, explicit_non_decimal) = if let Some(digits) = input
        .strip_prefix("0x")
        .or_else(|| input.strip_prefix("0X"))
    {
        (digits, 16, true)
    } else if let Some(digits) = input
        .strip_prefix("0b")
        .or_else(|| input.strip_prefix("0B"))
    {
        (digits, 2, true)
    } else if let Some(digits) = input
        .strip_prefix("0o")
        .or_else(|| input.strip_prefix("0O"))
    {
        (digits, 8, true)
    } else {
        (
            input,
            selected_radix.radix(),
            selected_radix != IntegerRadix::Decimal,
        )
    };
    let digits = digits.replace(['_', '\''], "");
    if digits.is_empty() {
        return Err("Enter digits after the base prefix");
    }
    let magnitude =
        u128::from_str_radix(&digits, radix).map_err(|_| "The value is not valid in this base")?;
    if magnitude > format.mask() {
        return Err("The value does not fit the destination type");
    }
    if negative {
        if !format.signed {
            return Err("Unsigned values cannot be negative");
        }
        if magnitude > format.sign_bit() {
            return Err("The negative value does not fit the destination type");
        }
        return Ok(((!magnitude).wrapping_add(1)) & format.mask());
    }
    if format.signed && !explicit_non_decimal && magnitude >= format.sign_bit() {
        return Err("The decimal value does not fit the signed destination type");
    }
    Ok(magnitude)
}

pub(super) fn format_integer_value(
    raw: u128,
    format: IntegerFormat,
    radix: IntegerRadix,
) -> String {
    let raw = raw & format.mask();
    match radix {
        IntegerRadix::Decimal if format.signed && raw & format.sign_bit() != 0 => {
            let magnitude = ((!raw).wrapping_add(1)) & format.mask();
            format!("-{magnitude}")
        }
        IntegerRadix::Decimal => raw.to_string(),
        IntegerRadix::Hexadecimal => {
            let width = format.bits.div_ceil(4) as usize;
            format!("0x{raw:0width$x}")
        }
        IntegerRadix::Binary => {
            let width = format.bits as usize;
            format!("0b{raw:0width$b}")
        }
        IntegerRadix::Octal => {
            let width = format.bits.div_ceil(3) as usize;
            format!("0o{raw:0width$o}")
        }
    }
}

pub(super) fn canonical_gdb_integer(
    raw: u128,
    format: IntegerFormat,
    radix: IntegerRadix,
) -> String {
    let formatted = format_integer_value(raw, format, radix);
    if radix == IntegerRadix::Octal {
        if let Some(digits) = formatted.strip_prefix("0o") {
            format!("0{digits}")
        } else {
            formatted
        }
    } else {
        formatted
    }
}

pub(super) fn parse_character_input(
    input: &str,
    format: IntegerFormat,
) -> Result<u128, &'static str> {
    let input = input.trim();
    let inner = input
        .strip_prefix('\'')
        .and_then(|value| value.strip_suffix('\''))
        .unwrap_or(input);
    let value = if let Some(escape) = inner.strip_prefix('\\') {
        match escape {
            "0" => 0,
            "a" => 7,
            "b" => 8,
            "t" => 9,
            "n" => 10,
            "v" => 11,
            "f" => 12,
            "r" => 13,
            "\\" => u32::from('\\'),
            "\"" => u32::from('"'),
            "'" => u32::from('\''),
            _ if escape.starts_with('x') => u32::from_str_radix(&escape[1..], 16)
                .map_err(|_| "Use a hexadecimal escape such as \\x41")?,
            _ if escape.starts_with("u{") && escape.ends_with('}') => {
                u32::from_str_radix(&escape[2..escape.len() - 1], 16)
                    .map_err(|_| "Use a Unicode escape such as \\u{41}")?
            }
            _ if escape.starts_with('u') && escape.len() == 5 => {
                u32::from_str_radix(&escape[1..], 16)
                    .map_err(|_| "Use a Unicode escape such as \\u0041")?
            }
            _ if escape.starts_with('U') && escape.len() == 9 => {
                u32::from_str_radix(&escape[1..], 16)
                    .map_err(|_| "Use a Unicode escape such as \\U00000041")?
            }
            _ if escape.len() <= 3
                && escape
                    .chars()
                    .all(|character| matches!(character, '0'..='7')) =>
            {
                u32::from_str_radix(escape, 8).map_err(|_| "Invalid octal character escape")?
            }
            _ => return Err("Enter one character or a supported escape sequence"),
        }
    } else {
        let mut characters = inner.chars();
        let character = characters.next().ok_or("Enter one character")?;
        if characters.next().is_some() {
            return Err("Enter exactly one character");
        }
        u32::from(character)
    };
    if u128::from(value) > format.mask() {
        return Err("The character does not fit the destination type");
    }
    Ok(u128::from(value))
}

pub(super) fn format_character_value(raw: u128, format: IntegerFormat) -> String {
    let value = (raw & format.mask()) as u32;
    match value {
        0 => String::from("'\\0'"),
        7 => String::from("'\\a'"),
        8 => String::from("'\\b'"),
        9 => String::from("'\\t'"),
        10 => String::from("'\\n'"),
        11 => String::from("'\\v'"),
        12 => String::from("'\\f'"),
        13 => String::from("'\\r'"),
        39 => String::from("'\\\''"),
        92 => String::from("'\\\\'"),
        _ => char::from_u32(value)
            .filter(|character| !character.is_control())
            .map_or_else(
                || {
                    if format.bits <= 8 {
                        format!("'\\x{value:02x}'")
                    } else {
                        format!("'\\u{{{value:x}}}'")
                    }
                },
                |character| format!("'{character}'"),
            ),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StringAssignmentKind {
    Buffer,
    CppString,
    RustString,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum StringStorage {
    Buffer { capacity: usize, pointer: bool },
    CppString,
    RustString { length: usize },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct StringEdit {
    pub(super) bytes: Vec<u8>,
    pub(super) storage: StringStorage,
}

impl StringEdit {
    pub(super) const fn assignment_kind(&self) -> StringAssignmentKind {
        match self.storage {
            StringStorage::Buffer { .. } => StringAssignmentKind::Buffer,
            StringStorage::CppString => StringAssignmentKind::CppString,
            StringStorage::RustString { .. } => StringAssignmentKind::RustString,
        }
    }
}

pub(super) fn string_edit(variable: &Variable) -> Option<StringEdit> {
    let type_name = variable.type_name.as_deref()?.trim().to_ascii_lowercase();
    let compact = ["const", "volatile", "__restrict", "restrict"]
        .into_iter()
        .fold(type_name, |name, qualifier| name.replace(qualifier, ""))
        .replace(' ', "");
    let owned_type = compact.trim_end_matches(['&', '*']);
    let bytes = decode_gdb_string(&variable.value)?;
    if is_rust_string_type_name(owned_type) {
        return Some(StringEdit {
            storage: StringStorage::RustString {
                length: bytes.len(),
            },
            bytes,
        });
    }
    if owned_type == "std::string"
        || owned_type.starts_with("std::__cxx11::basic_string<char,")
        || owned_type.starts_with("std::basic_string<char,")
    {
        return Some(StringEdit {
            bytes,
            storage: StringStorage::CppString,
        });
    }
    let base = compact.split(['*', '[']).next().unwrap_or(compact.as_str());
    let narrow_char = matches!(
        base,
        "char" | "signedchar" | "unsignedchar" | "c_char" | "c_schar" | "c_uchar"
    );
    if !narrow_char {
        return None;
    }
    if variable.is_pointer() {
        let capacity = bytes.len();
        return Some(StringEdit {
            bytes,
            storage: StringStorage::Buffer {
                capacity,
                pointer: true,
            },
        });
    }
    let length = compact
        .rsplit_once('[')
        .and_then(|(_, length)| length.strip_suffix(']'))
        .and_then(|length| length.parse::<usize>().ok())?;
    let capacity = length.checked_sub(1)?;
    Some(StringEdit {
        bytes,
        storage: StringStorage::Buffer {
            capacity,
            pointer: false,
        },
    })
}

pub(super) fn is_rust_string(variable: &Variable) -> bool {
    variable.type_name.as_deref().is_some_and(|type_name| {
        let compact = type_name
            .trim()
            .to_ascii_lowercase()
            .replace([' ', '&', '*'], "");
        is_rust_string_type_name(&compact)
    })
}

fn is_rust_string_type_name(type_name: &str) -> bool {
    matches!(type_name, "alloc::string::string" | "std::string::string")
}

pub(super) fn parse_string_input(input: &str) -> Result<Vec<u8>, &'static str> {
    let mut bytes = Vec::with_capacity(input.len());
    let mut characters = input.chars().peekable();
    while let Some(character) = characters.next() {
        if character != '\\' {
            let mut encoded = [0_u8; 4];
            bytes.extend_from_slice(character.encode_utf8(&mut encoded).as_bytes());
            continue;
        }
        let escape = characters
            .next()
            .ok_or("A trailing backslash needs an escape")?;
        match escape {
            '0'..='7' => {
                let mut digits = String::from(escape);
                while digits.len() < 3
                    && characters
                        .peek()
                        .is_some_and(|character| matches!(character, '0'..='7'))
                {
                    if let Some(character) = characters.next() {
                        digits.push(character);
                    }
                }
                bytes.push(
                    u8::from_str_radix(&digits, 8)
                        .map_err(|_| "An octal escape must fit in one byte")?,
                );
            }
            'a' => bytes.push(7),
            'b' => bytes.push(8),
            't' => bytes.push(9),
            'n' => bytes.push(10),
            'v' => bytes.push(11),
            'f' => bytes.push(12),
            'r' => bytes.push(13),
            '\\' => bytes.push(b'\\'),
            '"' => bytes.push(b'"'),
            '\'' => bytes.push(b'\''),
            'x' => {
                let high = characters
                    .next()
                    .ok_or("\\x needs exactly two hexadecimal digits")?;
                let low = characters
                    .next()
                    .ok_or("\\x needs exactly two hexadecimal digits")?;
                let digits = [high, low].iter().collect::<String>();
                bytes.push(
                    u8::from_str_radix(&digits, 16)
                        .map_err(|_| "\\x needs exactly two hexadecimal digits")?,
                );
            }
            _ => {
                return Err("Supported escapes include \\n, \\t, \\0, \\123, \\xNN, and \\\\");
            }
        }
    }
    Ok(bytes)
}

pub(super) fn format_string_bytes(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len());
    for byte in bytes {
        match *byte {
            b'\\' => output.push_str("\\\\"),
            b'\n' => output.push_str("\\n"),
            b'\r' => output.push_str("\\r"),
            b'\t' => output.push_str("\\t"),
            0 => output.push_str("\\0"),
            0x20..=0x7e => output.push(char::from(*byte)),
            _ => output.push_str(&format!("\\x{byte:02x}")),
        }
    }
    output
}

fn decode_gdb_string(value: &str) -> Option<Vec<u8>> {
    let quoted = value.get(value.find('"')?..)?;
    let mut escaped = false;
    let mut content = String::new();
    for character in quoted[1..].chars() {
        if !escaped && character == '"' {
            return parse_string_input(&content).ok();
        }
        content.push(character);
        if character == '\\' {
            escaped = !escaped;
        } else {
            escaped = false;
        }
    }
    None
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
    let bits = if fast && bits != 8 {
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
        "sigatomict" | "pidt" => Some(IntegerFormat::signed(32)),
        "clockt" => Some(IntegerFormat::signed(target_pointer_bits)),
        "wintt" | "uidt" | "gidt" | "modet" | "socklent" => Some(IntegerFormat::unsigned(32)),
        // time_t/off_t/dev_t/ino_t/nlink_t vary across Linux time64, LFS and
        // libc ABIs. GDB type metadata is authoritative; do not guess here.
        _ => None,
    }
}

#[cfg(test)]
mod float_tests {
    use super::*;

    #[test]
    fn converts_float_representations_without_changing_the_bits() {
        let raw = (-13.25_f64).to_bits().to_be_bytes();
        for representation in FloatRepresentation::ALL {
            let formatted = format_float_value(&raw, 64, representation);
            assert_eq!(
                parse_float_value(&formatted, 64, representation).unwrap(),
                raw
            );
        }
    }

    #[test]
    fn parses_special_values_and_exact_raw_float_bits() {
        assert!(
            f64::from_bits(u64::from_be_bytes(
                parse_float_value("nan", 64, FloatRepresentation::Decimal)
                    .unwrap()
                    .try_into()
                    .unwrap()
            ))
            .is_nan()
        );
        assert_eq!(
            parse_float_value("0x7ff8000000000042", 64, FloatRepresentation::RawBits).unwrap(),
            0x7ff8_0000_0000_0042_u64.to_be_bytes()
        );
        assert_eq!(
            parse_float_value("0x1.8p+1", 64, FloatRepresentation::HexFloat).unwrap(),
            3.0_f64.to_bits().to_be_bytes()
        );
    }

    #[test]
    fn target_metadata_overrides_host_abi_integer_guesses() {
        let variable = Variable {
            name: String::from("platform_long"),
            value: String::from("1"),
            type_name: Some(String::from("long")),
            argument: false,
            varobj: None,
            num_children: 0,
            has_more: false,
        };
        let metadata = ValueTypeMetadata {
            kind: ValueTypeKind::Integer,
            bits: Some(32),
            signed: Some(true),
            language: Some(String::from("c")),
            raw_bytes: None,
            enum_variants: Vec::new(),
        };
        assert_eq!(
            variable_integer_format(&variable, 64, Some(&metadata)),
            Some(IntegerFormat::signed(32))
        );
    }
}
