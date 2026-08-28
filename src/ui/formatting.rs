use super::*;

pub(super) fn populate_register_group<'a>(
    group: &RegisterGroupView,
    registers: impl IntoIterator<Item = &'a Register>,
    previous: &HashMap<String, String>,
    ring: Option<u64>,
    architecture: TargetArchitecture,
    endian: Option<TargetEndian>,
    pointer_bits: u32,
) {
    let rows = registers
        .into_iter()
        .map(|register| RegisterRowData {
            register: register.clone(),
            changed: register_changed(register, previous),
            ring,
            architecture,
            endian,
            pointer_bits,
        })
        .collect::<Vec<_>>();
    let count = rows.len() as i32;
    replace_boxed_store(&group.store, rows);
    group.panel.set_visible(count != 0);
    if count == 0 {
        return;
    }
    group.view.set_size_request(-1, 24 + count * 26);
}

pub(super) fn register_in_group(
    group: RegisterGroupKind,
    name: &str,
    architecture: TargetArchitecture,
) -> bool {
    match group {
        RegisterGroupKind::General => {
            architecture.is_core_register(name)
                && !architecture.is_status_register(name)
                && !architecture.is_thread_pointer(name)
                && !(matches!(
                    architecture,
                    TargetArchitecture::X86 | TargetArchitecture::X86_64
                ) && matches!(name, "cs" | "ss" | "ds" | "es" | "fs" | "gs"))
        }
        RegisterGroupKind::Bases => architecture.is_thread_pointer(name),
        RegisterGroupKind::Flags => architecture.is_status_register(name),
        RegisterGroupKind::Segments => {
            matches!(
                architecture,
                TargetArchitecture::X86 | TargetArchitecture::X86_64
            ) && matches!(name, "cs" | "ss" | "ds" | "es" | "fs" | "gs")
        }
        RegisterGroupKind::Vector => vector_register_for_architecture(name, architecture),
        RegisterGroupKind::FloatingPoint => floating_register_for_architecture(name, architecture),
        RegisterGroupKind::Other => true,
    }
}

pub(super) fn vector_register_for_architecture(
    name: &str,
    architecture: TargetArchitecture,
) -> bool {
    architecture.is_vector_register(name)
}

pub(super) fn floating_register_for_architecture(
    name: &str,
    architecture: TargetArchitecture,
) -> bool {
    architecture.is_floating_register(name)
}

pub(super) fn register_changed(register: &Register, previous: &HashMap<String, String>) -> bool {
    previous
        .get(&register.name)
        .is_some_and(|value| value != &register.value)
}

pub(super) fn same_register_values(left: &[Register], right: &[Register]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| left.name == right.name && left.value == right.value)
}

pub(super) fn register_value_css(
    register: &Register,
    architecture: TargetArchitecture,
    endian: Option<TargetEndian>,
    pointer_bits: u32,
) -> &'static str {
    if architecture.is_program_counter(&register.name) {
        "memory-code"
    } else if architecture.is_stack_or_frame_pointer(&register.name) {
        "memory-stack"
    } else if register.pointer_chain.iter().skip(1).any(|value| {
        value.contains('"')
            || hex_value(value).is_some_and(|value| {
                endian.is_some_and(|endian| {
                    ascii_annotation(value, endian, pointer_bits)
                        .is_some_and(|annotation| !annotation.starts_with('('))
                })
            })
    }) {
        "memory-string"
    } else if matches!(register.name.as_str(), "fs_base" | "gs_base")
        || register
            .pointer_chain
            .first()
            .is_some_and(|value| value.contains('<'))
    {
        "memory-writable"
    } else if hex_value(&register.value) == Some(0)
        || vector_lane_values(&register.name, &register.value)
            .is_some_and(|lanes| lanes.iter().all(|lane| lane == "0x0000000000000000"))
    {
        "register-zero"
    } else {
        "memory-none"
    }
}

pub(super) fn register_text(
    register: &Register,
    architecture: TargetArchitecture,
    endian: Option<TargetEndian>,
    pointer_bits: u32,
) -> String {
    let values = if register.pointer_chain.is_empty() {
        std::slice::from_ref(&register.value)
    } else {
        register.pointer_chain.as_slice()
    };
    values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            if index == 0 {
                format_register_value_for_target(
                    &register.name,
                    value,
                    false,
                    architecture,
                    endian,
                    pointer_bits,
                )
            } else {
                format_target_pointer_word(value, endian, pointer_bits)
            }
        })
        .collect::<Vec<_>>()
        .join("  →  ")
}

pub(super) fn register_primary_value(
    register: &Register,
    architecture: TargetArchitecture,
) -> String {
    let value = register.pointer_chain.first().unwrap_or(&register.value);
    format_register_value_for_architecture(&register.name, value, false, architecture)
}

pub(super) fn register_details(
    register: &Register,
    _architecture: TargetArchitecture,
    endian: Option<TargetEndian>,
    pointer_bits: u32,
) -> String {
    register
        .pointer_chain
        .iter()
        .skip(1)
        .map(|value| format_target_pointer_word(value, endian, pointer_bits))
        .collect::<Vec<_>>()
        .join("  →  ")
}

fn format_target_pointer_word(
    value: &str,
    endian: Option<TargetEndian>,
    pointer_bits: u32,
) -> String {
    // Dereference chains contain pointer-sized memory words, even when their
    // source register is wider (x32, AArch64 ILP32, or MIPS n32).
    format_register_value_for_target(
        "pointer",
        value,
        true,
        TargetArchitecture::Unknown,
        endian,
        pointer_bits,
    )
}

pub(super) fn is_flags_register(name: &str) -> bool {
    matches!(name, "eflags" | "rflags" | "cpsr" | "pstate" | "nzcv")
}

#[cfg(test)]
pub(super) fn format_register_value(register: &str, value: &str, show_ascii: bool) -> String {
    format_register_value_for_target(
        register,
        value,
        show_ascii,
        TargetArchitecture::Unknown,
        Some(TargetEndian::Little),
        64,
    )
}

pub(super) fn format_register_value_for_architecture(
    register: &str,
    value: &str,
    show_ascii: bool,
    architecture: TargetArchitecture,
) -> String {
    format_register_value_for_target(
        register,
        value,
        show_ascii,
        architecture,
        architecture.default_endian(),
        architecture.pointer_bits().unwrap_or(64),
    )
}

pub(super) fn format_register_value_for_target(
    register: &str,
    value: &str,
    show_ascii: bool,
    architecture: TargetArchitecture,
    endian: Option<TargetEndian>,
    pointer_bits: u32,
) -> String {
    if let Some(vector) = format_vector_register_value(register, value) {
        return vector;
    }
    if (vector_register_for_architecture(register, architecture)
        || floating_register_for_architecture(register, architecture))
        && value.trim_start().starts_with('{')
    {
        return compact_structured_register_value(value);
    }
    if value.starts_with('[') {
        return value.to_owned();
    }
    let Some(number) = hex_value(value) else {
        return value.lines().next().unwrap_or(value).to_owned();
    };
    let width = register_hex_width(register, architecture, pointer_bits);
    let mut formatted = format!("0x{number:0width$x}");
    if let Some((_, annotation)) = value.trim().split_once(char::is_whitespace) {
        formatted.push(' ');
        formatted.push_str(annotation.trim());
    } else if show_ascii
        && let Some(annotation) =
            endian.and_then(|endian| ascii_annotation(number, endian, pointer_bits))
    {
        formatted.push(' ');
        formatted.push_str(&annotation);
    }
    formatted
}

fn compact_structured_register_value(value: &str) -> String {
    const MAX_CHARS: usize = 1_024;
    let mut compact = String::with_capacity(value.len().min(MAX_CHARS));
    let mut char_count = 0_usize;
    let mut truncated = false;
    for part in value.split_whitespace() {
        let needed = part.chars().count() + usize::from(!compact.is_empty());
        if char_count.saturating_add(needed) > MAX_CHARS {
            truncated = true;
            break;
        }
        if !compact.is_empty() {
            compact.push(' ');
        }
        compact.push_str(part);
        char_count += needed;
    }
    if truncated {
        compact.push_str(" …");
    }
    compact
}

pub(super) fn format_vector_register_value(register: &str, value: &str) -> Option<String> {
    let lanes = vector_lane_values(register, value)?;
    if lanes.len() > 1 && lanes.iter().all(|lane| lane == &lanes[0]) {
        return Some(format!("q0…q{} = {}", lanes.len() - 1, lanes[0]));
    }
    Some(
        lanes
            .iter()
            .enumerate()
            .map(|(index, lane)| format!("q{index}={lane}"))
            .collect::<Vec<_>>()
            .join("  ·  "),
    )
}

pub(super) fn vector_lane_values(register: &str, value: &str) -> Option<Vec<String>> {
    let register_bytes = vector_register_bytes(register)?;
    let format = VectorLaneFormat::Int64;
    let lane_count = register_bytes / format.lane_bytes();
    vector_field_values(value, &format.field(register_bytes), lane_count, format)
}

pub(super) fn vector_register_bytes(register: &str) -> Option<usize> {
    [("xmm", 16), ("ymm", 32), ("zmm", 64)]
        .into_iter()
        .find_map(|(prefix, bytes)| {
            register.strip_prefix(prefix).and_then(|index| {
                (!index.is_empty() && index.chars().all(|character| character.is_ascii_digit()))
                    .then_some(bytes)
            })
        })
}

pub(super) fn vector_field_values(
    value: &str,
    field: &str,
    lane_count: usize,
    format: VectorLaneFormat,
) -> Option<Vec<String>> {
    let field = value
        .find(field)
        .map(|index| &value[index + field.len()..])?;
    let start = field.find('{')? + 1;
    let end = field[start..].find('}')? + start;
    let mut lanes = Vec::with_capacity(lane_count);
    for part in field[start..end]
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
    {
        let (lane, repeats) = if let Some((lane, repeats)) = part.split_once("<repeats") {
            let repeats = repeats
                .split_whitespace()
                .next()
                .and_then(|count| count.parse::<usize>().ok())
                .unwrap_or(1);
            (lane.trim(), repeats)
        } else {
            (part, 1)
        };
        let lane = format_vector_lane(lane, format);
        lanes.extend(std::iter::repeat_n(lane, repeats));
    }
    lanes.truncate(lane_count);
    (lanes.len() == lane_count).then_some(lanes)
}

pub(super) fn format_vector_lane(lane: &str, format: VectorLaneFormat) -> String {
    let lane = lane
        .rsplit_once('=')
        .map_or(lane, |(_, value)| value)
        .trim();
    if format.is_float() {
        if let Some(hex) = lane.strip_prefix("0x")
            && let Ok(bits) = u64::from_str_radix(hex, 16)
        {
            return if format == VectorLaneFormat::Float32 {
                format_float(f32::from_bits(bits as u32) as f64)
            } else {
                format_float(f64::from_bits(bits))
            };
        }
        return lane.split_whitespace().collect::<Vec<_>>().join(" ");
    }
    let width = format.lane_bytes() * 2;
    if let Some(hex) = lane.strip_prefix("0x")
        && let Ok(value) = u64::from_str_radix(hex, 16)
    {
        return format!("0x{value:0width$x}");
    }
    if let Ok(value) = lane.parse::<u64>() {
        return format!("0x{value:0width$x}");
    }
    if let Ok(value) = lane.parse::<i64>() {
        let bits = u32::try_from(format.lane_bytes() * 8).unwrap_or(64);
        let mask = if bits == 64 {
            u64::MAX
        } else {
            (1_u64 << bits) - 1
        };
        return format!("0x{:0width$x}", (value as u64) & mask);
    }
    lane.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(super) fn format_float(value: f64) -> String {
    if value.is_nan() {
        String::from("NaN")
    } else if value == f64::INFINITY {
        String::from("+Inf")
    } else if value == f64::NEG_INFINITY {
        String::from("-Inf")
    } else {
        format!("{value:.12}")
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_owned()
    }
}

pub(super) fn register_hex_width(
    register: &str,
    architecture: TargetArchitecture,
    target_pointer_bits: u32,
) -> usize {
    usize::try_from(architecture.scalar_register_bits(register, target_pointer_bits) / 4)
        .unwrap_or(16)
        .clamp(4, 32)
}

pub(super) fn hex_value(value: &str) -> Option<u64> {
    let hex = value
        .split_whitespace()
        .next()?
        .trim_end_matches(',')
        .strip_prefix("0x")?;
    u64::from_str_radix(hex, 16).ok()
}

pub(super) fn ascii_annotation(
    value: u64,
    endian: TargetEndian,
    pointer_bits: u32,
) -> Option<String> {
    let bytes = endian.word_bytes(value);
    let word_size = usize::try_from(pointer_bits / 8).unwrap_or(8).clamp(4, 8);
    let bytes = match endian {
        TargetEndian::Little => &bytes[..word_size],
        TargetEndian::Big => &bytes[8 - word_size..],
    };
    let printable = bytes
        .iter()
        .take_while(|byte| byte.is_ascii_graphic() || **byte == b' ')
        .copied()
        .collect::<Vec<_>>();
    if printable.len() < 2 {
        return None;
    }
    let text = printable
        .iter()
        .flat_map(|byte| std::ascii::escape_default(*byte))
        .map(char::from)
        .collect::<String>();
    if printable.len() >= 4 {
        let continuation = if printable.len() == bytes.len() {
            "…"
        } else {
            ""
        };
        Some(format!("'{text}{continuation}'"))
    } else {
        Some(format!("('{text}'?)"))
    }
}

#[cfg(test)]
pub(super) fn flags_markup(value: &str, ring: Option<u64>) -> String {
    let details = flags_details_markup("rflags", value, ring);
    let Some(value) = hex_value(value) else {
        return details;
    };
    format!("0x{value:x}  {details}")
}

pub(super) fn flags_details_markup(register: &str, value: &str, ring: Option<u64>) -> String {
    let Some(value) = hex_value(value) else {
        return gtk::glib::markup_escape_text(value).to_string();
    };
    let definitions: &[(u8, &str)] = if matches!(register, "cpsr" | "pstate" | "nzcv") {
        &[
            (31, "negative"),
            (30, "zero"),
            (29, "carry"),
            (28, "overflow"),
        ]
    } else {
        FLAGS
    };
    let flags = definitions
        .iter()
        .map(|(bit, name)| {
            if value & (1_u64 << bit) != 0 {
                format!("<b>{}</b>", name.to_uppercase())
            } else {
                (*name).to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    let ring = if matches!(register, "eflags" | "rflags") {
        ring.map_or_else(String::new, |ring| format!("  [Ring={ring}]"))
    } else {
        String::new()
    };
    format!("[{flags}]{ring}")
}

pub(super) fn build_context_legend() -> gtk::Box {
    let grid = gtk::Grid::builder()
        .column_spacing(8)
        .row_spacing(2)
        .build();
    let items = [
        ("Modified", "legend-modified"),
        ("Code", "memory-code"),
        ("Heap", "memory-heap"),
        ("Stack", "memory-stack"),
        ("Writable", "memory-writable"),
        ("Read-only", "memory-readonly"),
        ("None", "memory-none"),
        ("RWX", "memory-rwx"),
        ("String", "memory-string"),
    ];
    for (index, (text, class)) in items.into_iter().enumerate() {
        let item = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        let swatch = gtk::Label::new(Some("■"));
        swatch.add_css_class("legend-swatch");
        swatch.add_css_class(class);
        let label = gtk::Label::new(Some(text));
        label.set_halign(gtk::Align::Start);
        item.append(&swatch);
        item.append(&label);
        grid.attach(&item, (index % 2) as i32, (index / 2) as i32, 1, 1);
    }
    build_disclosure("LEGEND", &grid, false, "context-legend")
}

pub(super) fn build_disclosure(
    title: &str,
    child: &impl IsA<gtk::Widget>,
    expanded: bool,
    class: &str,
) -> gtk::Box {
    build_disclosure_with_content(title, child, expanded, class).0
}

pub(super) fn build_disclosure_with_content(
    title: &str,
    child: &impl IsA<gtk::Widget>,
    expanded: bool,
    class: &str,
) -> (gtk::Box, gtk::Box) {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.add_css_class("disclosure");
    root.add_css_class(class);
    let heading = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    let arrow = gtk::Label::new(Some(if expanded {
        DISCLOSURE_EXPANDED_ICON
    } else {
        DISCLOSURE_COLLAPSED_ICON
    }));
    arrow.add_css_class("disclosure-arrow");
    arrow.set_width_chars(1);
    arrow.set_xalign(0.5);
    let title = gtk::Label::new(Some(title));
    title.add_css_class("section-title");
    title.set_halign(gtk::Align::Start);
    title.set_xalign(0.0);
    title.set_hexpand(true);
    heading.append(&arrow);
    heading.append(&title);
    let button = gtk::Button::builder().child(&heading).build();
    button.add_css_class("disclosure-header");
    button.add_css_class(if expanded {
        "disclosure-expanded"
    } else {
        "disclosure-collapsed"
    });
    button.set_halign(gtk::Align::Fill);
    button.set_focus_on_click(false);
    button.set_tooltip_text(Some(if expanded {
        "Collapse section"
    } else {
        "Expand section"
    }));
    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.append(child);
    content.set_visible(expanded);
    let content_for_click = content.clone();
    let button_for_click = button.clone();
    button.connect_clicked(move |_| {
        let reveal = !content_for_click.is_visible();
        content_for_click.set_visible(reveal);
        arrow.set_text(if reveal {
            DISCLOSURE_EXPANDED_ICON
        } else {
            DISCLOSURE_COLLAPSED_ICON
        });
        if reveal {
            button_for_click.remove_css_class("disclosure-collapsed");
            button_for_click.add_css_class("disclosure-expanded");
        } else {
            button_for_click.remove_css_class("disclosure-expanded");
            button_for_click.add_css_class("disclosure-collapsed");
        }
        button_for_click.set_tooltip_text(Some(if reveal {
            "Collapse section"
        } else {
            "Expand section"
        }));
    });
    root.append(&button);
    root.append(&content);
    (root, content)
}

pub(super) fn stack_references(entry: &StackEntry) -> String {
    let mut references = Vec::new();
    if !entry.value_registers.is_empty() {
        references.push(
            entry
                .value_registers
                .iter()
                .map(|name| format!("${name}"))
                .collect::<Vec<_>>()
                .join(", "),
        );
    }
    if let Some(frame) = entry.return_frame {
        references.push(format!("retaddr[{frame}]"));
    }
    references.join(" · ")
}

pub(super) fn stack_word_role(entry: &StackEntry) -> String {
    let mut roles = vec![memory_kind_label(entry.memory_kind).to_owned()];
    if !entry.address_registers.is_empty() {
        roles.push(format!(
            "addressed by {}",
            entry
                .address_registers
                .iter()
                .map(|name| format!("${name}"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !entry.value_registers.is_empty() {
        roles.push(format!(
            "value held by {}",
            entry
                .value_registers
                .iter()
                .map(|name| format!("${name}"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if let Some(frame) = entry.return_frame {
        roles.push(format!("return address for frame #{frame}"));
    }
    roles.join("  ·  ")
}

pub(super) const fn memory_kind_label(kind: MemoryKind) -> &'static str {
    match kind {
        MemoryKind::Code => "CODE POINTER",
        MemoryKind::Heap => "HEAP POINTER",
        MemoryKind::Stack => "STACK POINTER",
        MemoryKind::Writable => "WRITABLE POINTER",
        MemoryKind::ReadOnly => "READ-ONLY POINTER",
        MemoryKind::Rwx => "RWX POINTER",
        MemoryKind::String => "ASCII / STRING",
        MemoryKind::None => "SCALAR / UNKNOWN",
    }
}

pub(super) fn stack_tooltip(entry: &StackEntry) -> String {
    let anchors = entry
        .address_registers
        .iter()
        .map(|name| format!("${name}"))
        .collect::<Vec<_>>()
        .join(", ");
    let references = stack_references(entry);
    let region = entry.region.as_deref().unwrap_or("unmapped");
    let width = usize::try_from(entry.pointer_bits / 4)
        .unwrap_or(16)
        .clamp(8, 16);
    format!(
        "0x{:0width$x}  +0x{:04x} / +{:03}\n{}\nanchors: {} · references: {}\n{}",
        entry.address,
        entry.offset,
        entry.index,
        stack_entry_text(entry),
        if anchors.is_empty() { "none" } else { &anchors },
        if references.is_empty() {
            "none"
        } else {
            &references
        },
        region,
        width = width,
    )
}

pub(super) fn stack_entry_text(entry: &StackEntry) -> String {
    let values = if entry.pointer_chain.is_empty() {
        std::slice::from_ref(&entry.value)
    } else {
        entry.pointer_chain.as_slice()
    };
    values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            let architecture = if entry.pointer_bits == 32 {
                TargetArchitecture::X86
            } else {
                TargetArchitecture::X86_64
            };
            format_register_value_for_target(
                "sp",
                value,
                index > 0,
                architecture,
                Some(entry.endian),
                entry.pointer_bits,
            )
        })
        .collect::<Vec<_>>()
        .join("  →  ")
}

pub(super) fn memory_kind_css(kind: MemoryKind) -> &'static str {
    match kind {
        MemoryKind::Code => "memory-code",
        MemoryKind::Heap => "memory-heap",
        MemoryKind::Stack => "memory-stack",
        MemoryKind::Writable => "memory-writable",
        MemoryKind::ReadOnly => "memory-readonly",
        MemoryKind::Rwx => "memory-rwx",
        MemoryKind::String => "memory-string",
        MemoryKind::None => "memory-none",
    }
}

pub(super) fn thread_os_id(target_id: &str) -> Option<String> {
    if let Some(lwp) = target_id
        .split_once("(LWP ")
        .and_then(|(_, suffix)| suffix.split_once(')'))
        .map(|(lwp, _)| lwp)
    {
        return Some(lwp.to_owned());
    }
    if let Some(tid) = target_id
        .split_once("tid:")
        .map(|(_, suffix)| suffix)
        .and_then(|suffix| suffix.split_whitespace().next())
    {
        return Some(tid.trim_end_matches([',', ']']).to_owned());
    }
    target_id
        .strip_prefix("process ")
        .and_then(|pid| pid.split_whitespace().next())
        .map(str::to_owned)
}

pub(super) fn stop_reason_label(reason: &str) -> String {
    match reason {
        "breakpoint-hit" => String::from("BREAKPOINT"),
        "end-stepping-range" => String::from("STEP"),
        "function-finished" => String::from("FINISH"),
        "location-reached" => String::from("UNTIL"),
        "signal-received" => String::from("SIGNAL"),
        "watchpoint-trigger" => String::from("WATCHPOINT"),
        other => other.replace('-', " ").to_uppercase(),
    }
}

pub(super) fn thread_detail(thread: &ThreadInfo, stop_reason: Option<&str>) -> String {
    let mut detail = thread.frame.as_ref().map_or_else(
        || thread.state.clone(),
        |frame| format!("{} at {}", thread.state, frame.address),
    );
    let metadata = thread_metadata(thread, stop_reason);
    if !metadata.is_empty() {
        if thread.frame.is_some() {
            detail.push(' ');
        } else {
            detail.push_str(", ");
        }
        detail.push_str(&metadata);
    }
    detail
}

pub(super) fn thread_detail_widget(thread: &ThreadInfo, stop_reason: Option<&str>) -> gtk::Box {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    if let Some(frame) = thread.frame.as_ref() {
        let location = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let state = gtk::Label::new(Some(&format!("{} at ", thread.state)));
        state.add_css_class("thread-detail");
        let address = gtk::Label::new(Some(&frame.address));
        address.add_css_class("thread-detail");
        address.add_css_class("memory-code");
        location.append(&state);
        location.append(&address);
        root.append(&location);
    } else {
        let state = gtk::Label::new(Some(&thread.state));
        state.add_css_class("thread-detail");
        state.set_halign(gtk::Align::Start);
        root.append(&state);
    }

    let metadata = thread_metadata(thread, stop_reason);
    if !metadata.is_empty() {
        let metadata = gtk::Label::new(Some(&metadata));
        metadata.add_css_class("thread-detail");
        metadata.set_halign(gtk::Align::Start);
        metadata.set_wrap(true);
        metadata.set_wrap_mode(pango::WrapMode::WordChar);
        root.append(&metadata);
    }
    root
}

pub(super) fn thread_metadata(thread: &ThreadInfo, stop_reason: Option<&str>) -> String {
    let mut metadata = Vec::new();
    if let Some(frame) = thread.frame.as_ref()
        && let Some(symbol) = thread
            .pc_symbol
            .clone()
            .or_else(|| (frame.function != "??").then(|| format!("<{}>", frame.function)))
    {
        metadata.push(compact_function_name(&symbol));
    }
    if let Some(core) = thread.core.as_deref() {
        metadata.push(format!("core:{core}"));
    }
    if let Some(reason) = stop_reason {
        metadata.push(format!("reason: {reason}"));
    }
    metadata.join(", ")
}

pub(super) fn full_address(address: &str, pointer_bits: u32) -> String {
    let width = usize::try_from(pointer_bits / 4).unwrap_or(16).clamp(8, 16);
    hex_value(address).map_or_else(
        || address.to_owned(),
        |address| format!("0x{address:0width$x}"),
    )
}

pub(super) fn split_instruction(instruction: &str) -> (&str, &str) {
    let instruction = instruction.trim();
    match instruction.find(char::is_whitespace) {
        Some(index) => (&instruction[..index], instruction[index..].trim()),
        None => (instruction, ""),
    }
}

fn is_call_instruction(mnemonic: &str, operands: &str, architecture: TargetArchitecture) -> bool {
    match architecture {
        TargetArchitecture::X86 | TargetArchitecture::X86_64 => mnemonic.starts_with("call"),
        TargetArchitecture::Arm => matches!(mnemonic, "bl" | "blx"),
        TargetArchitecture::AArch64 => matches!(mnemonic, "bl" | "blr"),
        TargetArchitecture::RiscV32 | TargetArchitecture::RiscV64 => {
            matches!(mnemonic, "call" | "jal")
        }
        TargetArchitecture::Mips32 | TargetArchitecture::Mips64 => {
            matches!(mnemonic, "jal" | "jalr" | "bal")
        }
        TargetArchitecture::PowerPc32 | TargetArchitecture::PowerPc64 => {
            matches!(mnemonic, "bl" | "bcl" | "bctrl")
        }
        TargetArchitecture::S390 | TargetArchitecture::S390x => {
            matches!(mnemonic, "brasl" | "basr")
        }
        TargetArchitecture::LoongArch64 => {
            mnemonic == "bl"
                || (mnemonic == "jirl"
                    && operands
                        .split(',')
                        .next()
                        .map(|operand| operand.trim().trim_start_matches('$'))
                        .is_some_and(|operand| matches!(operand, "ra" | "r1")))
        }
        TargetArchitecture::Unknown => {
            mnemonic.starts_with("call") || matches!(mnemonic, "bl" | "jal" | "brasl")
        }
    }
}

fn is_return_instruction(mnemonic: &str, operands: &str, architecture: TargetArchitecture) -> bool {
    if mnemonic == "ret" || mnemonic.starts_with("ret.") {
        return true;
    }
    match architecture {
        TargetArchitecture::Arm => {
            mnemonic == "bx" && operands.trim().trim_start_matches(['$', '%']) == "lr"
        }
        TargetArchitecture::AArch64 => {
            mnemonic == "br"
                && matches!(operands.trim().trim_start_matches(['$', '%']), "x30" | "lr")
        }
        TargetArchitecture::Mips32 | TargetArchitecture::Mips64 => {
            mnemonic == "jr" && operands.contains("ra")
        }
        TargetArchitecture::PowerPc32 | TargetArchitecture::PowerPc64 => mnemonic == "blr",
        TargetArchitecture::S390 | TargetArchitecture::S390x => {
            mnemonic == "br" && (operands.contains("r14") || operands.contains("%r14"))
        }
        TargetArchitecture::RiscV32 | TargetArchitecture::RiscV64 => {
            mnemonic == "jr" && operands.contains("ra")
        }
        TargetArchitecture::LoongArch64 => {
            if mnemonic != "jirl" {
                return false;
            }
            let operands = operands
                .split(',')
                .map(|operand| operand.trim().trim_start_matches('$'))
                .collect::<Vec<_>>();
            matches!(operands.as_slice(), ["zero" | "r0", "ra" | "r1", _])
        }
        _ => false,
    }
}

fn syscall_architecture(
    mnemonic: &str,
    operands: &str,
    architecture: TargetArchitecture,
) -> Option<TargetArchitecture> {
    match architecture {
        TargetArchitecture::X86 | TargetArchitecture::X86_64 => match mnemonic {
            "syscall" | "sysenter" => Some(architecture),
            "int" => {
                let vector = operands.trim().trim_start_matches(['$', '#']);
                matches!(vector, "0x80" | "80h" | "128").then_some(TargetArchitecture::X86)
            }
            _ => None,
        },
        TargetArchitecture::Arm => matches!(mnemonic, "svc" | "swi").then_some(architecture),
        TargetArchitecture::AArch64 => (mnemonic == "svc").then_some(architecture),
        TargetArchitecture::RiscV32 | TargetArchitecture::RiscV64 => {
            (mnemonic == "ecall").then_some(architecture)
        }
        TargetArchitecture::Mips32 | TargetArchitecture::Mips64 => {
            (mnemonic == "syscall").then_some(architecture)
        }
        TargetArchitecture::PowerPc32 | TargetArchitecture::PowerPc64 => {
            (mnemonic == "sc").then_some(architecture)
        }
        TargetArchitecture::S390 | TargetArchitecture::S390x => {
            (mnemonic == "svc").then_some(architecture)
        }
        TargetArchitecture::LoongArch64 => (mnemonic == "syscall").then_some(architecture),
        TargetArchitecture::Unknown => {
            matches!(mnemonic, "syscall" | "sysenter" | "svc" | "ecall" | "sc")
                .then_some(architecture)
        }
    }
}

fn is_unconditional_branch(mnemonic: &str, architecture: TargetArchitecture) -> bool {
    match architecture {
        TargetArchitecture::X86 | TargetArchitecture::X86_64 => mnemonic.starts_with("jmp"),
        TargetArchitecture::Arm => matches!(mnemonic, "b" | "bx"),
        TargetArchitecture::AArch64 => matches!(mnemonic, "b" | "br"),
        TargetArchitecture::RiscV32 | TargetArchitecture::RiscV64 => matches!(mnemonic, "j" | "jr"),
        TargetArchitecture::Mips32 | TargetArchitecture::Mips64 => {
            matches!(mnemonic, "b" | "j" | "jr")
        }
        TargetArchitecture::PowerPc32 | TargetArchitecture::PowerPc64 => {
            matches!(mnemonic, "b" | "ba" | "bctr")
        }
        TargetArchitecture::S390 | TargetArchitecture::S390x => matches!(mnemonic, "j" | "br"),
        TargetArchitecture::LoongArch64 => matches!(mnemonic, "b" | "jirl"),
        TargetArchitecture::Unknown => matches!(mnemonic, "jmp" | "b" | "j"),
    }
}

fn is_conditional_branch(mnemonic: &str, architecture: TargetArchitecture) -> bool {
    match architecture {
        TargetArchitecture::X86 | TargetArchitecture::X86_64 => {
            mnemonic.starts_with('j') || mnemonic.starts_with("loop")
        }
        TargetArchitecture::Arm | TargetArchitecture::AArch64 => {
            mnemonic.starts_with("b.")
                || matches!(
                    mnemonic,
                    "beq"
                        | "bne"
                        | "bcs"
                        | "bhs"
                        | "bcc"
                        | "blo"
                        | "bmi"
                        | "bpl"
                        | "bvs"
                        | "bvc"
                        | "bhi"
                        | "bls"
                        | "bge"
                        | "blt"
                        | "bgt"
                        | "ble"
                        | "cbz"
                        | "cbnz"
                        | "tbz"
                        | "tbnz"
                )
        }
        TargetArchitecture::RiscV32 | TargetArchitecture::RiscV64 => {
            matches!(
                mnemonic,
                "beq"
                    | "bne"
                    | "blt"
                    | "bge"
                    | "bltu"
                    | "bgeu"
                    | "beqz"
                    | "bnez"
                    | "blez"
                    | "bgez"
                    | "bltz"
                    | "bgtz"
            )
        }
        TargetArchitecture::Mips32 | TargetArchitecture::Mips64 => mnemonic.starts_with('b'),
        TargetArchitecture::PowerPc32 | TargetArchitecture::PowerPc64 => mnemonic.starts_with("bc"),
        TargetArchitecture::S390 | TargetArchitecture::S390x => mnemonic.starts_with('j'),
        TargetArchitecture::LoongArch64 => {
            mnemonic.starts_with('b') && mnemonic != "b" && mnemonic != "bl"
        }
        TargetArchitecture::Unknown => false,
    }
}

fn riscv_branch_taken(
    instruction: &Instruction,
    registers: &[Register],
    pointer_bits: u32,
) -> Option<bool> {
    let (mnemonic, operands) = split_instruction(&instruction.text);
    let operands = operands.split(',').map(str::trim).collect::<Vec<_>>();
    let value = |name: &str| register_number(registers, name.trim_start_matches(['$', '%']));
    let (left, right) = match mnemonic {
        "beqz" | "bnez" | "blez" | "bgez" | "bltz" | "bgtz" => (value(operands.first()?)?, 0),
        _ => (value(operands.first()?)?, value(operands.get(1)?)?),
    };
    let signed = |value: u64| {
        if pointer_bits == 32 {
            i64::from(value as u32 as i32)
        } else {
            value as i64
        }
    };
    match mnemonic {
        "beq" | "beqz" => Some(left == right),
        "bne" | "bnez" => Some(left != right),
        "blt" | "bltz" => Some(signed(left) < signed(right)),
        "bge" | "bgez" => Some(signed(left) >= signed(right)),
        "blez" => Some(signed(left) <= 0),
        "bgtz" => Some(signed(left) > 0),
        "bltu" => Some(left < right),
        "bgeu" => Some(left >= right),
        _ => None,
    }
}

pub(super) fn instruction_flow_description(
    instruction: &Instruction,
    registers: &[Register],
    architecture: TargetArchitecture,
) -> String {
    let (mnemonic, operands) = split_instruction(&instruction.text);
    let mnemonic = mnemonic.to_ascii_lowercase();
    let (kind, detail) = if is_call_instruction(&mnemonic, operands, architecture) {
        ("CALL", operands)
    } else if is_return_instruction(&mnemonic, operands, architecture) {
        ("RETURN", "return to caller")
    } else if syscall_architecture(&mnemonic, operands, architecture).is_some() {
        ("SYSCALL", "kernel transition")
    } else if is_unconditional_branch(&mnemonic, architecture) {
        ("JUMP", operands)
    } else if is_conditional_branch(&mnemonic, architecture) {
        let decision =
            conditional_branch_taken(instruction, registers, architecture).map(|taken| {
                if taken {
                    "BRANCH · TAKEN"
                } else {
                    "BRANCH · NOT TAKEN"
                }
            });
        (decision.unwrap_or("BRANCH"), operands)
    } else {
        ("FLOW", "sequential")
    };
    if detail.is_empty() {
        kind.to_owned()
    } else {
        format!("{kind}  ▶  {detail}")
    }
}

pub(super) fn instruction_flow_target(
    instruction: &Instruction,
    architecture: TargetArchitecture,
) -> Option<String> {
    let (mnemonic, operands) = split_instruction(&instruction.text);
    let mnemonic = mnemonic.to_ascii_lowercase();
    if !is_call_instruction(&mnemonic, operands, architecture)
        && !is_unconditional_branch(&mnemonic, architecture)
        && !is_conditional_branch(&mnemonic, architecture)
    {
        return None;
    }
    if let Some(address) = operands
        .split(|character: char| {
            character.is_whitespace() || matches!(character, ',' | '(' | ')' | '[' | ']')
        })
        .map(|part| {
            part.trim_matches(|character: char| matches!(character, '*' | '$' | '#' | ';' | ':'))
        })
        .find(|part| {
            part.strip_prefix("0x").is_some_and(|hex| {
                !hex.is_empty() && hex.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
        })
    {
        return Some(address.to_owned());
    }
    if let (Some(start), Some(end)) = (operands.find('<'), operands.rfind('>'))
        && start < end
    {
        let symbol = operands[start + 1..end].trim();
        if !symbol.is_empty() {
            return Some(symbol.to_owned());
        }
    }
    let candidate = operands
        .split(',')
        .next_back()?
        .trim()
        .trim_start_matches('*')
        .trim();
    if candidate.starts_with('[') || candidate.contains(char::is_whitespace) || candidate.is_empty()
    {
        return None;
    }
    let explicitly_register = candidate.starts_with(['$', '%']);
    let candidate = candidate.trim_start_matches(['$', '%']);
    candidate
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b':' | b'@'))
        .then(|| {
            if explicitly_register || instruction_operand_is_register(candidate, architecture) {
                format!("${candidate}")
            } else {
                candidate.to_owned()
            }
        })
}

fn instruction_operand_is_register(name: &str, architecture: TargetArchitecture) -> bool {
    let name = name.to_ascii_lowercase();
    let numbered = |prefix: char| {
        name.strip_prefix(prefix).is_some_and(|number| {
            !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit())
        })
    };
    match architecture {
        TargetArchitecture::X86 | TargetArchitecture::X86_64 => {
            matches!(
                name.as_str(),
                "al" | "ah"
                    | "ax"
                    | "eax"
                    | "rax"
                    | "bl"
                    | "bh"
                    | "bx"
                    | "ebx"
                    | "rbx"
                    | "cl"
                    | "ch"
                    | "cx"
                    | "ecx"
                    | "rcx"
                    | "dl"
                    | "dh"
                    | "dx"
                    | "edx"
                    | "rdx"
                    | "si"
                    | "esi"
                    | "rsi"
                    | "di"
                    | "edi"
                    | "rdi"
                    | "sp"
                    | "esp"
                    | "rsp"
                    | "bp"
                    | "ebp"
                    | "rbp"
                    | "ip"
                    | "eip"
                    | "rip"
            ) || name.strip_prefix('r').is_some_and(|number| {
                let number = number.trim_end_matches(['b', 'w', 'd']);
                number
                    .parse::<u8>()
                    .is_ok_and(|number| (8..=15).contains(&number))
            })
        }
        TargetArchitecture::Arm | TargetArchitecture::AArch64 => {
            matches!(name.as_str(), "sp" | "lr" | "pc")
                || numbered('r')
                || numbered('x')
                || numbered('w')
        }
        TargetArchitecture::RiscV32 | TargetArchitecture::RiscV64 => {
            numbered('x') || matches!(name.as_str(), "ra" | "sp" | "gp" | "tp" | "fp")
        }
        _ => numbered('r'),
    }
}

pub(super) fn instruction_arguments_description(
    instruction: &Instruction,
    registers: &[Register],
    architecture: TargetArchitecture,
) -> String {
    let (mnemonic, operands) = split_instruction(&instruction.text);
    let mnemonic = mnemonic.to_ascii_lowercase();
    if let Some(syscall_architecture) = syscall_architecture(&mnemonic, operands, architecture) {
        let encoded_number = match syscall_architecture {
            TargetArchitecture::S390 | TargetArchitecture::S390x if mnemonic == "svc" => {
                instruction_immediate(operands).filter(|number| *number != 0)
            }
            TargetArchitecture::Arm if matches!(mnemonic.as_str(), "svc" | "swi") => {
                instruction_immediate(operands)
                    .filter(|number| *number != 0)
                    .map(|number| number.saturating_sub(0x90_0000))
            }
            _ => None,
        };
        return syscall_arguments_description(registers, syscall_architecture, encoded_number);
    }
    if !is_call_instruction(&mnemonic, operands, architecture) {
        return String::new();
    }
    let arguments = architecture
        .call_argument_registers()
        .iter()
        .filter_map(|name| {
            registers
                .iter()
                .find(|register| register.name == *name)
                .map(|register| {
                    format!("${name}={}", register_primary_value(register, architecture))
                })
        })
        .collect::<Vec<_>>();
    if arguments.is_empty() {
        String::new()
    } else {
        format!("ARGS  {}", arguments.join("  "))
    }
}

pub(super) fn conditional_branch_taken(
    instruction: &Instruction,
    registers: &[Register],
    architecture: TargetArchitecture,
) -> Option<bool> {
    let mut mnemonic = split_instruction(&instruction.text).0.to_ascii_lowercase();
    if matches!(
        architecture,
        TargetArchitecture::Arm | TargetArchitecture::AArch64
    ) && let Some(condition) = mnemonic.strip_prefix("b.")
    {
        mnemonic = format!("b{condition}");
    }
    if matches!(
        architecture,
        TargetArchitecture::RiscV32 | TargetArchitecture::RiscV64
    ) {
        return riscv_branch_taken(
            instruction,
            registers,
            architecture.pointer_bits().unwrap_or(64),
        );
    }
    let flags = if matches!(
        architecture,
        TargetArchitecture::Arm | TargetArchitecture::AArch64
    ) {
        register_number(registers, "cpsr")
            .or_else(|| register_number(registers, "pstate"))
            .or_else(|| register_number(registers, "nzcv"))
    } else {
        register_number(registers, "rflags").or_else(|| register_number(registers, "eflags"))
    };
    let (carry_bit, zero_bit, sign_bit, overflow_bit, parity_bit) = if matches!(
        architecture,
        TargetArchitecture::Arm | TargetArchitecture::AArch64
    ) {
        (29, 30, 31, 28, None)
    } else {
        (0, 6, 7, 11, Some(2))
    };
    let flag = |bit: u8| flags.map(|flags| flags & (1_u64 << bit) != 0);
    let carry = || flag(carry_bit);
    let parity = || parity_bit.and_then(flag);
    let zero = || flag(zero_bit);
    let sign = || flag(sign_bit);
    let overflow = || flag(overflow_bit);
    match mnemonic.as_str() {
        "jo" => overflow(),
        "jno" => overflow().map(|value| !value),
        "jb" | "jc" | "jnae" | "bcs" | "bhs" => carry(),
        "jae" | "jnb" | "jnc" | "bcc" | "blo" => carry().map(|value| !value),
        "je" | "jz" | "beq" => zero(),
        "jne" | "jnz" | "bne" => zero().map(|value| !value),
        "jbe" | "jna" => Some(carry()? || zero()?),
        "ja" | "jnbe" => Some(!carry()? && !zero()?),
        "bls" => Some(!carry()? || zero()?),
        "bhi" => Some(carry()? && !zero()?),
        "js" | "bmi" => sign(),
        "jns" | "bpl" => sign().map(|value| !value),
        "bvs" => overflow(),
        "bvc" => overflow().map(|value| !value),
        "jp" | "jpe" => parity(),
        "jnp" | "jpo" => parity().map(|value| !value),
        "jl" | "jnge" | "blt" => Some(sign()? != overflow()?),
        "jge" | "jnl" | "bge" => Some(sign()? == overflow()?),
        "jle" | "jng" | "ble" => Some(zero()? || sign()? != overflow()?),
        "jg" | "jnle" | "bgt" => Some(!zero()? && sign()? == overflow()?),
        "jcxz" => register_number(registers, "cx")
            .or_else(|| register_number(registers, "ecx"))
            .or_else(|| register_number(registers, "rcx"))
            .map(|value| value & 0xffff == 0),
        "jecxz" => register_number(registers, "ecx")
            .or_else(|| register_number(registers, "rcx"))
            .map(|value| value & 0xffff_ffff == 0),
        "jrcxz" => register_number(registers, "rcx").map(|value| value == 0),
        "loop" | "loope" | "loopz" | "loopne" | "loopnz" => {
            let counter =
                register_number(registers, "rcx").or_else(|| register_number(registers, "ecx"))?;
            let repeats = counter.wrapping_sub(1) != 0;
            match mnemonic.as_str() {
                "loope" | "loopz" => Some(repeats && zero()?),
                "loopne" | "loopnz" => Some(repeats && !zero()?),
                _ => Some(repeats),
            }
        }
        _ => None,
    }
}

pub(super) fn register_number(registers: &[Register], name: &str) -> Option<u64> {
    registers
        .iter()
        .find(|register| register.name == name)
        .and_then(|register| hex_value(&register.value))
}

fn instruction_immediate(operand: &str) -> Option<u64> {
    let operand = operand
        .trim()
        .trim_start_matches(['$', '#'])
        .trim_end_matches(',');
    operand.strip_prefix("0x").map_or_else(
        || operand.parse().ok(),
        |hex| u64::from_str_radix(hex, 16).ok(),
    )
}

pub(super) fn syscall_arguments_description(
    registers: &[Register],
    architecture: TargetArchitecture,
    encoded_number: Option<u64>,
) -> String {
    let Some((number_register, argument_registers)) = architecture.syscall_registers() else {
        return String::from("SYSCALL  number unavailable");
    };
    let Some(number) = encoded_number.or_else(|| register_number(registers, number_register))
    else {
        return String::from("SYSCALL  number unavailable");
    };
    let number = architecture.normalize_syscall_number(number);
    let (name, argument_names) = syscall_signature(number, architecture);
    let values = argument_registers
        .iter()
        .zip(argument_names.iter())
        .filter_map(|(register_name, argument_name)| {
            registers
                .iter()
                .find(|register| register.name == *register_name)
                .map(|register| {
                    format!(
                        "{argument_name}={}",
                        register_primary_value(register, architecture)
                    )
                })
        })
        .collect::<Vec<_>>();
    if values.is_empty() {
        format!("SYSCALL  #{number} {name}")
    } else {
        format!("SYSCALL  #{number} {name}({})", values.join(", "))
    }
}

pub(super) fn syscall_signature(
    number: u64,
    architecture: TargetArchitecture,
) -> (&'static str, &'static [&'static str]) {
    let name = architecture.syscall_name(number);
    let arguments: &[&str] = match name {
        "read" | "write" => &["fd", "buffer", "count"],
        "open" => &["path", "flags", "mode"],
        "openat" => &["dir_fd", "path", "flags", "mode"],
        "openat2" => &["dir_fd", "path", "how", "size"],
        "close" => &["fd"],
        "lseek" => &["fd", "offset", "whence"],
        "mmap" | "mmap2" => &["address", "length", "protection", "flags", "fd", "offset"],
        "mprotect" => &["address", "length", "protection"],
        "munmap" | "brk" => &["address"],
        "ioctl" => &["fd", "request", "argument"],
        "pread64" | "pwrite64" => &["fd", "buffer", "count", "offset"],
        "readv" | "writev" => &["fd", "iov", "iov_count"],
        "clone" => &["flags", "stack", "parent_tid", "tls", "child_tid"],
        "clone3" => &["arguments", "size"],
        "execve" => &["path", "argv", "envp"],
        "exit" | "exit_group" => &["status"],
        "wait4" => &["pid", "status", "options", "usage"],
        "kill" => &["pid", "signal"],
        "futex" => &[
            "address",
            "operation",
            "value",
            "timeout",
            "address2",
            "value3",
        ],
        "set_robust_list" => &["head", "length"],
        "getrandom" => &["buffer", "count", "flags"],
        "statx" => &["dir_fd", "path", "flags", "mask", "statx"],
        "close_range" => &["first", "last", "flags"],
        _ => &["arg0", "arg1", "arg2", "arg3", "arg4", "arg5"],
    };
    (name, arguments)
}

pub(super) fn instruction_memory_expression(
    instruction: &Instruction,
    registers: &[Register],
    architecture: TargetArchitecture,
) -> Option<String> {
    if matches!(
        architecture,
        TargetArchitecture::X86 | TargetArchitecture::X86_64
    ) && let Some(comment) = instruction.text.split_once('#').map(|(_, comment)| comment)
        && let Some(address) = comment
            .split_whitespace()
            .find(|part| part.starts_with("0x"))
    {
        return Some(address.trim_end_matches([',', ';']).to_owned());
    }
    if registers.is_empty() {
        return None;
    }
    let operand = if let Some(start) = instruction.text.find('[') {
        let start = start + 1;
        let end = instruction.text[start..].find(']')? + start;
        instruction.text[start..end]
            .split(',')
            .map(|part| part.trim().replace('#', ""))
            .collect::<Vec<_>>()
            .join(" + ")
    } else {
        let open = instruction.text.find('(')?;
        let close = instruction.text[open..].find(')')? + open;
        let displacement = instruction.text[..open]
            .rsplit(|character: char| character == ',' || character.is_whitespace())
            .find(|part| !part.is_empty())
            .unwrap_or("0");
        let bases = instruction.text[open + 1..close]
            .split(',')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>();
        if bases.is_empty() {
            return None;
        }
        format!("{} + {displacement}", bases.join(" + "))
    }
    .replace(['$', '%'], "");
    if ["lsl", "lsr", "asr", "uxtw", "sxtw"]
        .iter()
        .any(|operator| operand.split_whitespace().any(|part| part == *operator))
    {
        return None;
    }
    let register_names = registers
        .iter()
        .map(|register| register.name.as_str())
        .collect::<Vec<_>>();
    let mut expression = String::with_capacity(operand.len() + 8);
    let mut token = String::new();
    let flush_token = |token: &mut String, expression: &mut String| {
        if token.is_empty() {
            return;
        }
        if register_names.contains(&token.as_str()) {
            expression.push('$');
        }
        expression.push_str(token);
        token.clear();
    };
    for character in operand.chars() {
        if character.is_ascii_alphanumeric() || character == '_' {
            token.push(character);
        } else {
            flush_token(&mut token, &mut expression);
            expression.push(character);
        }
    }
    flush_token(&mut token, &mut expression);
    let expression = expression.trim().trim_end_matches('!').trim();
    (!expression.is_empty()).then(|| format!("({expression})"))
}

pub(super) fn compact_memory_preview(bytes: &[u8]) -> String {
    let preview = bytes.iter().take(16).copied().collect::<Vec<_>>();
    let hex = preview
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join(" ");
    let ascii = preview
        .iter()
        .map(|byte| {
            if byte.is_ascii_graphic() || *byte == b' ' {
                char::from(*byte)
            } else {
                '·'
            }
        })
        .collect::<String>();
    format!("{hex}  |{ascii}|")
}

pub(super) fn instruction_symbol_full(instruction: &Instruction) -> String {
    let offset = instruction.offset.parse::<u64>().unwrap_or(0);
    if offset == 0 {
        format!("<{}>", instruction.function)
    } else {
        format!("<{}+0x{offset:x}>", instruction.function)
    }
}

pub(super) fn instruction_symbol(instruction: &Instruction) -> String {
    compact_function_name(&instruction_symbol_full(instruction))
}
