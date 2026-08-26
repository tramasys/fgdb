use super::*;

pub(super) fn populate_register_group<'a>(
    group: &RegisterGroupView,
    registers: impl IntoIterator<Item = &'a Register>,
    previous: &HashMap<String, String>,
    ring: Option<u64>,
) {
    let rows = registers
        .into_iter()
        .map(|register| RegisterRowData {
            register: register.clone(),
            changed: register_changed(register, previous),
            ring,
        })
        .collect::<Vec<_>>();
    let count = rows.len() as i32;
    replace_boxed_store(&group.store, rows);
    if count == 0 {
        return;
    }
    group.panel.set_visible(true);
    group.view.set_size_request(-1, 24 + count * 26);
}

pub(super) fn register_in_group(group: RegisterGroupKind, name: &str) -> bool {
    match group {
        RegisterGroupKind::General => GENERAL_REGISTERS.contains(&name),
        RegisterGroupKind::Bases => BASE_REGISTERS.contains(&name),
        RegisterGroupKind::Flags => FLAG_REGISTERS.contains(&name),
        RegisterGroupKind::Segments => SEGMENT_REGISTERS.contains(&name),
        RegisterGroupKind::Vector => {
            ["xmm", "ymm", "zmm", "mm"].iter().any(|prefix| {
                name.strip_prefix(prefix).is_some_and(|index| {
                    !index.is_empty() && index.chars().all(|character| character.is_ascii_digit())
                })
            }) || name == "mxcsr"
        }
        RegisterGroupKind::FloatingPoint => {
            matches!(
                name,
                "fctrl" | "fstat" | "ftag" | "fiseg" | "fioff" | "foseg" | "fooff" | "fop"
            ) || name.strip_prefix("st").is_some_and(|index| {
                !index.is_empty() && index.chars().all(|character| character.is_ascii_digit())
            })
        }
        RegisterGroupKind::Other => true,
    }
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

pub(super) fn register_value_css(register: &Register) -> &'static str {
    if matches!(register.name.as_str(), "rip" | "eip") {
        "memory-code"
    } else if matches!(register.name.as_str(), "rsp" | "rbp" | "esp" | "ebp") {
        "memory-stack"
    } else if register.pointer_chain.iter().skip(1).any(|value| {
        value.contains('"')
            || hex_value(value).is_some_and(|value| {
                ascii_annotation(value).is_some_and(|annotation| !annotation.starts_with('('))
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

pub(super) fn register_text(register: &Register) -> String {
    let values = if register.pointer_chain.is_empty() {
        std::slice::from_ref(&register.value)
    } else {
        register.pointer_chain.as_slice()
    };
    values
        .iter()
        .enumerate()
        .map(|(index, value)| format_register_value(&register.name, value, index > 0))
        .collect::<Vec<_>>()
        .join("  →  ")
}

pub(super) fn register_primary_value(register: &Register) -> String {
    let value = register.pointer_chain.first().unwrap_or(&register.value);
    format_register_value(&register.name, value, false)
}

pub(super) fn register_details(register: &Register) -> String {
    register
        .pointer_chain
        .iter()
        .skip(1)
        .map(|value| format_register_value(&register.name, value, true))
        .collect::<Vec<_>>()
        .join("  →  ")
}

pub(super) fn is_flags_register(name: &str) -> bool {
    matches!(name, "eflags" | "rflags" | "cpsr")
}

pub(super) fn format_register_value(register: &str, value: &str, show_ascii: bool) -> String {
    if let Some(vector) = format_vector_register_value(register, value) {
        return vector;
    }
    if value.starts_with('[') {
        return value.to_owned();
    }
    let Some(number) = hex_value(value) else {
        return value.lines().next().unwrap_or(value).to_owned();
    };
    let width = register_hex_width(register);
    let mut formatted = format!("0x{number:0width$x}");
    if let Some((_, annotation)) = value.trim().split_once(char::is_whitespace) {
        formatted.push(' ');
        formatted.push_str(annotation.trim());
    } else if show_ascii && let Some(annotation) = ascii_annotation(number) {
        formatted.push(' ');
        formatted.push_str(&annotation);
    }
    formatted
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

pub(super) fn register_hex_width(register: &str) -> usize {
    if matches!(register, "cs" | "ss" | "ds" | "es" | "fs" | "gs") {
        4
    } else if matches!(
        register,
        "eax" | "ebx" | "ecx" | "edx" | "esp" | "ebp" | "esi" | "edi" | "eip" | "cpsr"
    ) {
        8
    } else {
        16
    }
}

pub(super) fn hex_value(value: &str) -> Option<u64> {
    let hex = value
        .split_whitespace()
        .next()?
        .trim_end_matches(',')
        .strip_prefix("0x")?;
    u64::from_str_radix(hex, 16).ok()
}

pub(super) fn ascii_annotation(value: u64) -> Option<String> {
    let bytes = value.to_le_bytes();
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
    let details = flags_details_markup(value, ring);
    let Some(value) = hex_value(value) else {
        return details;
    };
    format!("0x{value:x}  {details}")
}

pub(super) fn flags_details_markup(value: &str, ring: Option<u64>) -> String {
    let Some(value) = hex_value(value) else {
        return gtk::glib::markup_escape_text(value).to_string();
    };
    let flags = FLAGS
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
    let ring = ring.map_or_else(String::new, |ring| format!("  [Ring={ring}]"));
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
    let arrow = gtk::Label::new(Some(if expanded { "⌄" } else { "›" }));
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
        arrow.set_text(if reveal { "⌄" } else { "›" });
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
    format!(
        "0x{:016x}  +0x{:04x} / +{:03}\n{}\nanchors: {} · references: {}\n{}",
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
        .map(|(index, value)| format_register_value("rsp", value, index > 0))
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

pub(super) fn full_address(address: &str) -> String {
    hex_value(address).map_or_else(|| address.to_owned(), |address| format!("0x{address:016x}"))
}

pub(super) fn split_instruction(instruction: &str) -> (&str, &str) {
    let instruction = instruction.trim();
    match instruction.find(char::is_whitespace) {
        Some(index) => (&instruction[..index], instruction[index..].trim()),
        None => (instruction, ""),
    }
}

pub(super) fn instruction_flow_description(
    instruction: &Instruction,
    registers: &[Register],
) -> String {
    let (mnemonic, operands) = split_instruction(&instruction.text);
    let mnemonic = mnemonic.to_ascii_lowercase();
    let (kind, detail) = if mnemonic.starts_with("call") {
        ("CALL", operands)
    } else if mnemonic == "ret" || mnemonic.starts_with("ret ") {
        ("RETURN", "pop target from stack")
    } else if mnemonic == "syscall" || mnemonic == "sysenter" {
        ("SYSCALL", "kernel transition")
    } else if mnemonic == "jmp" || mnemonic.starts_with("jmp") {
        ("JUMP", operands)
    } else if mnemonic.starts_with('j') || mnemonic.starts_with("loop") {
        let decision = conditional_branch_taken(instruction, registers).map(|taken| {
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
        format!("{kind}  →  {detail}")
    }
}

pub(super) fn instruction_arguments_description(
    instruction: &Instruction,
    registers: &[Register],
) -> String {
    let mnemonic = split_instruction(&instruction.text).0.to_ascii_lowercase();
    if matches!(mnemonic.as_str(), "syscall" | "sysenter") {
        return syscall_arguments_description(registers);
    }
    if !mnemonic.starts_with("call") {
        return String::new();
    }
    let arguments = ["rdi", "rsi", "rdx", "rcx", "r8", "r9"]
        .iter()
        .filter_map(|name| {
            registers
                .iter()
                .find(|register| register.name == *name)
                .map(|register| format!("${name}={}", register_primary_value(register)))
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
) -> Option<bool> {
    let mnemonic = split_instruction(&instruction.text).0.to_ascii_lowercase();
    let flags =
        register_number(registers, "rflags").or_else(|| register_number(registers, "eflags"));
    let flag = |bit: u8| flags.map(|flags| flags & (1_u64 << bit) != 0);
    let carry = || flag(0);
    let parity = || flag(2);
    let zero = || flag(6);
    let sign = || flag(7);
    let overflow = || flag(11);
    match mnemonic.as_str() {
        "jo" => overflow(),
        "jno" => overflow().map(|value| !value),
        "jb" | "jc" | "jnae" => carry(),
        "jae" | "jnb" | "jnc" => carry().map(|value| !value),
        "je" | "jz" => zero(),
        "jne" | "jnz" => zero().map(|value| !value),
        "jbe" | "jna" => Some(carry()? || zero()?),
        "ja" | "jnbe" => Some(!carry()? && !zero()?),
        "js" => sign(),
        "jns" => sign().map(|value| !value),
        "jp" | "jpe" => parity(),
        "jnp" | "jpo" => parity().map(|value| !value),
        "jl" | "jnge" => Some(sign()? != overflow()?),
        "jge" | "jnl" => Some(sign()? == overflow()?),
        "jle" | "jng" => Some(zero()? || sign()? != overflow()?),
        "jg" | "jnle" => Some(!zero()? && sign()? == overflow()?),
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

pub(super) fn syscall_arguments_description(registers: &[Register]) -> String {
    let Some(number) = register_number(registers, "rax") else {
        return String::from("SYSCALL  number unavailable");
    };
    let (name, argument_names) = syscall_signature(number);
    let values = ["rdi", "rsi", "rdx", "r10", "r8", "r9"]
        .iter()
        .zip(argument_names.iter())
        .filter_map(|(register_name, argument_name)| {
            registers
                .iter()
                .find(|register| register.name == *register_name)
                .map(|register| format!("{argument_name}={}", register_primary_value(register)))
        })
        .collect::<Vec<_>>();
    if values.is_empty() {
        format!("SYSCALL  #{number} {name}")
    } else {
        format!("SYSCALL  #{number} {name}({})", values.join(", "))
    }
}

pub(super) fn syscall_signature(number: u64) -> (&'static str, &'static [&'static str]) {
    match number {
        0 => ("read", &["fd", "buf", "count"]),
        1 => ("write", &["fd", "buf", "count"]),
        2 => ("open", &["path", "flags", "mode"]),
        3 => ("close", &["fd"]),
        8 => ("lseek", &["fd", "offset", "whence"]),
        9 => ("mmap", &["addr", "length", "prot", "flags", "fd", "offset"]),
        10 => ("mprotect", &["addr", "length", "prot"]),
        11 => ("munmap", &["addr", "length"]),
        12 => ("brk", &["addr"]),
        13 => (
            "rt_sigaction",
            &["signal", "action", "old_action", "sigset_size"],
        ),
        14 => ("rt_sigprocmask", &["how", "set", "old_set", "sigset_size"]),
        16 => ("ioctl", &["fd", "request", "argument"]),
        17 => ("pread64", &["fd", "buf", "count", "offset"]),
        18 => ("pwrite64", &["fd", "buf", "count", "offset"]),
        19 => ("readv", &["fd", "iov", "iov_count"]),
        20 => ("writev", &["fd", "iov", "iov_count"]),
        21 => ("access", &["path", "mode"]),
        32 => ("dup", &["old_fd"]),
        33 => ("dup2", &["old_fd", "new_fd"]),
        39 => ("getpid", &[]),
        41 => ("socket", &["domain", "type", "protocol"]),
        42 => ("connect", &["fd", "address", "length"]),
        43 => ("accept", &["fd", "address", "length"]),
        56 => (
            "clone",
            &["flags", "stack", "parent_tid", "child_tid", "tls"],
        ),
        57 => ("fork", &[]),
        58 => ("vfork", &[]),
        59 => ("execve", &["path", "argv", "envp"]),
        60 => ("exit", &["status"]),
        61 => ("wait4", &["pid", "status", "options", "usage"]),
        62 => ("kill", &["pid", "signal"]),
        72 => ("fcntl", &["fd", "command", "argument"]),
        80 => ("chdir", &["path"]),
        87 => ("unlink", &["path"]),
        158 => ("arch_prctl", &["code", "address"]),
        186 => ("gettid", &[]),
        202 => (
            "futex",
            &[
                "address",
                "operation",
                "value",
                "timeout",
                "address2",
                "value3",
            ],
        ),
        231 => ("exit_group", &["status"]),
        257 => ("openat", &["dir_fd", "path", "flags", "mode"]),
        262 => ("newfstatat", &["dir_fd", "path", "stat", "flags"]),
        263 => ("unlinkat", &["dir_fd", "path", "flags"]),
        273 => ("set_robust_list", &["head", "length"]),
        318 => ("getrandom", &["buf", "count", "flags"]),
        332 => ("statx", &["dir_fd", "path", "flags", "mask", "statx"]),
        435 => ("clone3", &["arguments", "size"]),
        436 => ("close_range", &["first", "last", "flags"]),
        437 => ("openat2", &["dir_fd", "path", "how", "size"]),
        _ => ("unknown", &["arg0", "arg1", "arg2", "arg3", "arg4", "arg5"]),
    }
}

pub(super) fn instruction_memory_expression(
    instruction: &Instruction,
    registers: &[Register],
) -> Option<String> {
    if let Some(comment) = instruction.text.split_once('#').map(|(_, comment)| comment)
        && let Some(address) = comment
            .split_whitespace()
            .find(|part| part.starts_with("0x"))
    {
        return Some(address.trim_end_matches([',', ';']).to_owned());
    }
    if registers.is_empty() {
        return None;
    }
    let start = instruction.text.find('[')? + 1;
    let end = instruction.text[start..].find(']')? + start;
    let operand = &instruction.text[start..end];
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
    let expression = expression.trim();
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
