use std::{collections::HashSet, path::Path};

use super::{
    DebugSession, EventCatchpoint, GEF_COMMAND_CAPABILITIES, IntegerFormat, IntegerRadix,
    RefreshGate, StringStorage, TargetConnection, TerminalClipboardAction, UntilAction,
    VariableNode, VectorLaneFormat, breakpoint_command_number_at_address,
    breakpoint_command_numbers, call_abi_phase, compact_function_name, compact_variable_type,
    conditional_branch_taken, configured_target_can_start, event_catchpoint_command_number,
    event_catchpoint_command_numbers, flags_markup, format_register_value,
    format_register_value_for_architecture, format_register_value_for_target, full_address,
    instruction_arguments_description, instruction_flow_description, instruction_flow_target,
    instruction_matches_until, instruction_memory_expression, integer_decimal_value,
    normalized_signal_name, parse_character_input, parse_integer_input, parse_string_input,
    register_details, register_integer_format, register_value_css, set_breakpoint_enabled,
    signal_catchpoint_command_number, signal_catchpoint_command_numbers, source_location_score,
    source_symbol_at_offset, source_tab_title, stop_reason_label, string_edit,
    terminal_clipboard_action, thread_os_id, variable_boolean_value, variable_character_format,
    variable_details, variable_integer_format, variable_is_address, variable_node_matches_filter,
    variable_search_text, variable_value_parts, vector_field_values, without_generic_arguments,
};
use crate::debugger::{
    Breakpoint, Instruction, Register, SourceLocation, TargetArchitecture, TargetEndian, Variable,
};
use crate::misc::CallAbiPhase;

#[test]
fn stop_point_submissions_recheck_execution_and_selection_locks() {
    use crate::model::{DebuggerModel, DebuggerStateDelta, actions::*};

    let model = DebuggerModel::new(None);
    let available = || super::stop_point_actions_available(&model);
    assert!(!available());
    model.set_controls_ready(true);
    assert!(available());
    model.apply_debugger_state_delta(DebuggerStateDelta::establish_stopped_target(
        TargetConnection::Local,
    ));
    model.set_debug_state_stale(false);
    assert!(available());

    model.set_command_pending(true);
    assert!(!available());
    model.set_command_pending(false);
    model.set_debug_state_stale(true);
    assert!(!available());
    model.set_debug_state_stale(false);
    model.set_thread_action_pending(Some(ThreadActionPending::Analysis));
    assert!(!available());
    model.set_thread_action_pending(None);
    model.set_inferior_action_pending(Some(InferiorActionPending::Selection));
    assert!(!available());
    model.set_inferior_action_pending(None);
    assert!(available());

    model.mark_inferior_running(None);
    assert!(!available());
    model.apply_debugger_state_delta(DebuggerStateDelta::establish_stopped_target(
        TargetConnection::Local,
    ));
    model.set_debug_state_stale(false);
    assert!(available());
    model.set_controls_ready(false);
    assert!(!available());
}

#[test]
fn gef_capability_probes_are_unique() {
    let unique = GEF_COMMAND_CAPABILITIES
        .iter()
        .copied()
        .collect::<HashSet<_>>();

    assert_eq!(unique.len(), GEF_COMMAND_CAPABILITIES.len());
}

#[test]
fn configured_remote_session_cannot_start_without_a_live_connection() {
    let session = DebugSession::Remote {
        endpoint: String::from("host:1234"),
        executable: None,
        extended: true,
        remote_executable: Some(String::from("/srv/app")),
    };

    assert!(!configured_target_can_start(
        Some(&session),
        TargetConnection::None
    ));

    assert!(configured_target_can_start(
        Some(&session),
        TargetConnection::Remote
    ));
}

#[test]
fn terminal_clipboard_shortcuts_preserve_gdb_interrupts() {
    use gtk::gdk::{Key, ModifierType};

    let control = ModifierType::CONTROL_MASK;
    let shift = ModifierType::SHIFT_MASK;

    assert_eq!(
        terminal_clipboard_action(Key::v, control, false),
        Some(TerminalClipboardAction::Paste)
    );

    assert_eq!(
        terminal_clipboard_action(Key::V, control | shift, false),
        Some(TerminalClipboardAction::Paste)
    );

    assert_eq!(
        terminal_clipboard_action(Key::Insert, shift, false),
        Some(TerminalClipboardAction::Paste)
    );

    assert_eq!(
        terminal_clipboard_action(Key::KP_Insert, control, false),
        Some(TerminalClipboardAction::Copy)
    );

    assert_eq!(
        terminal_clipboard_action(Key::c, control, true),
        Some(TerminalClipboardAction::Copy)
    );

    assert_eq!(terminal_clipboard_action(Key::c, control, false), None);

    assert_eq!(
        terminal_clipboard_action(Key::C, control | shift, false),
        Some(TerminalClipboardAction::Copy)
    );

    assert_eq!(
        terminal_clipboard_action(Key::v, control | ModifierType::ALT_MASK, false),
        None
    );
}

#[test]
fn coalesces_bursty_model_refreshes() {
    let gate = RefreshGate::default();
    assert!(gate.begin());
    assert!(!gate.begin());
    assert!(!gate.begin());
    assert!(gate.finish());
    assert!(gate.begin());
    assert!(!gate.finish());
    assert!(gate.begin());
    gate.invalidate();
    assert!(gate.finish());
}

#[test]
fn formats_pointer_words_and_ascii_previews() {
    assert_eq!(
        format_register_value("r12", "0x61732f656d6f682f", true),
        "0x61732f656d6f682f '/home/sa…'"
    );

    assert_eq!(
        format_register_value("rip", "0x40116f <main+15>", false),
        "0x000000000040116f <main+15>"
    );
}

#[test]
fn separates_raw_variable_values_from_gdb_details() {
    assert_eq!(
        variable_value_parts(r#"0x555555559010 "YUU\005""#),
        ("0x555555559010", r#""YUU\005""#)
    );

    assert_eq!(
        variable_value_parts(
            "0x7ffff7ac2010 <error: Cannot access memory at address 0x7ffff7ac2010>"
        ),
        (
            "0x7ffff7ac2010",
            "<error: Cannot access memory at address 0x7ffff7ac2010>"
        )
    );

    assert_eq!(variable_value_parts("65 'A'"), ("65", "'A'"));

    assert_eq!(
        variable_value_parts("{x = 1, y = 2}"),
        ("{x = 1, y = 2}", "")
    );

    assert_eq!(variable_value_parts("0x1"), ("0x1", ""));

    let integer = |type_name: &str, value: &str| Variable {
        local_index: None,
        name: String::from("value"),
        value: value.to_owned(),
        type_name: Some(type_name.to_owned()),
        argument: false,
        varobj: None,
        num_children: 0,
        has_more: false,
        display_hint: None,
        dynamic: false,
    };

    let details = |variable: &Variable, value: &str, annotation: &str| {
        variable_details(variable, value, annotation, 64)
    };

    assert_eq!(details(&integer("int", "0x2a"), "0x2a", ""), "42");

    assert_eq!(
        details(&integer("char", "0x41 'A'"), "0x41", "'A'"),
        "65  'A'"
    );

    assert_eq!(details(&integer("pid_t", "-0x1"), "-0x1", ""), "-1");

    assert_eq!(
        details(&integer("int", "0xffffffff"), "0xffffffff", ""),
        "-1"
    );

    assert_eq!(
        details(&integer("unsigned int", "0xffffffff"), "0xffffffff", ""),
        "4294967295"
    );

    assert_eq!(details(&integer("int", "0xff"), "0xff", ""), "255");
    assert_eq!(details(&integer("i8", "0xff"), "0xff", ""), "-1");
    assert_eq!(details(&integer("void *", "0x2a"), "0x2a", ""), "");
    assert_eq!(details(&integer("double", "0x2a"), "0x2a", ""), "");
}

#[test]
fn compacts_cpp_and_rust_debug_types_without_losing_user_types() {
    assert_eq!(
        compact_variable_type(
            "const std::__cxx11::basic_string<char, std::char_traits<char>, std::allocator<char> >"
        ),
        "const std::string"
    );

    assert_eq!(
        compact_variable_type(
            "core::option::Option<alloc::boxed::Box<demo::Node, alloc::alloc::Global>>"
        ),
        "Option<Box<demo::Node>>"
    );

    assert_eq!(
        compact_variable_type(
            "std::collections::hash::map::HashMap<alloc::string::String, usize, std::hash::random::RandomState, alloc::alloc::Global>"
        ),
        "HashMap<String, usize>"
    );
}

#[test]
fn filters_variables_across_scope_type_and_pretty_value() {
    let variable = Variable {
        local_index: None,
        name: String::from("state"),
        value: String::from("PacketKind::Payload"),
        type_name: Some(String::from("core::option::Option<demo::PacketKind>")),
        argument: true,
        varobj: None,
        num_children: 0,
        has_more: false,
        display_hint: None,
        dynamic: false,
    };

    let search_text = variable_search_text(&variable);
    assert!(search_text.contains("state"));
    assert!(search_text.contains("payload"));
    assert!(search_text.contains("option"));
    assert!(search_text.contains("packet"));
    assert!(search_text.contains("argument"));
    assert!(!search_text.contains("vector"));

    let root = VariableNode::new(Variable {
        local_index: None,
        name: String::from("fixture"),
        value: String::from("{...}"),
        type_name: Some(String::from("struct Fixture")),
        argument: false,
        varobj: Some(String::from("var1")),
        num_children: 1,
        has_more: false,
        display_hint: None,
        dynamic: false,
    });

    root.children
        .append(&gtk::glib::BoxedAnyObject::new(VariableNode::new(variable)));

    assert!(variable_node_matches_filter(&root, "packet payload"));
}

#[test]
fn pointer_updates_retire_children_when_the_object_is_no_longer_readable() {
    use gtk::prelude::*;

    let pointer = VariableNode::new(Variable {
        local_index: None,
        name: String::from("tailward"),
        value: String::from("0x1234"),
        type_name: Some(String::from("struct CustomNode *")),
        argument: false,
        varobj: Some(String::from("var1.tailward")),
        num_children: 2,
        has_more: false,
        display_hint: None,
        dynamic: false,
    });

    pointer
        .children
        .append(&gtk::glib::BoxedAnyObject::new(VariableNode::new(
            Variable {
                name: String::from("value"),
                value: String::from("2004"),
                type_name: Some(String::from("int")),
                varobj: Some(String::from("var1.tailward.value")),
                num_children: 0,
                ..pointer.variable.clone()
            },
        )));
    pointer.children_loaded.set(true);
    pointer.expanded.set(true);

    let mut index = super::domain::VariableNodeIndex::default();
    index.insert(pointer.clone());
    index.index_store(&pointer.children);
    assert!(index.contains("var1.tailward.value"));

    let unchanged = pointer.updated(pointer.variable.clone(), true);
    assert_eq!(unchanged.children.n_items(), 1);
    assert!(unchanged.children_loaded.get());
    assert!(unchanged.expanded.get());

    for value in ["0x0", "", "<not available>", "0x5678"] {
        let updated = pointer.updated(
            Variable {
                value: value.into(),
                ..pointer.variable.clone()
            },
            true,
        );

        assert_eq!(updated.children.n_items(), 0, "{value}");
        assert!(!updated.children_loaded.get());
        assert!(!updated.children_loading.get());
        assert_eq!(updated.expanded.get(), value == "0x5678");
        index.replace(&pointer, &updated);
        let indexed = index.get("var1.tailward").unwrap();
        assert_eq!(indexed.variable.value, value);
        assert_eq!(indexed.children.n_items(), 0);
        assert!(!index.contains("var1.tailward.value"));
    }
}

#[test]
fn decodes_rust_c_and_cpp_integer_types() {
    let decimal = |type_name: &str, value: &str, pointer_bits| {
        let variable = Variable {
            local_index: None,
            name: String::from("value"),
            value: value.to_owned(),
            type_name: Some(type_name.to_owned()),
            argument: false,
            varobj: None,
            num_children: 0,
            has_more: false,
            display_hint: None,
            dynamic: false,
        };

        integer_decimal_value(&variable, value, pointer_bits)
    };

    assert_eq!(
        decimal("i128", "0xffffffffffffffffffffffffffffffff", 64),
        Some("-1".into())
    );

    assert_eq!(
        decimal("u128", "0xffffffffffffffffffffffffffffffff", 64),
        Some("340282366920938463463374607431768211455".into())
    );

    assert_eq!(
        decimal("usize", "0xffffffffffffffff", 64),
        Some("18446744073709551615".into())
    );

    assert_eq!(decimal("isize", "0xffffffff", 32), Some("-1".into()));

    assert_eq!(
        decimal("const signed short int", "0xffff", 64),
        Some("-1".into())
    );

    assert_eq!(
        decimal("long unsigned int", "0xffffffffffffffff", 64),
        Some("18446744073709551615".into())
    );

    assert_eq!(
        decimal("std::uint_least16_t", "0xffff", 64),
        Some("65535".into())
    );

    assert_eq!(
        decimal("int_fast16_t", "0xffffffffffffffff", 64),
        Some("-1".into())
    );

    assert_eq!(
        decimal("unsigned __int64", "0xffffffffffffffff", 64),
        Some("18446744073709551615".into())
    );

    assert_eq!(
        decimal("__int128", "0xffffffffffffffffffffffffffffffff", 64),
        Some("-1".into())
    );

    assert_eq!(
        decimal("unsigned _BitInt(17)", "0x1ffff", 64),
        Some("131071".into())
    );

    assert_eq!(decimal("_BitInt(17)", "0x1ffff", 64), Some("-1".into()));
}

#[test]
fn parses_and_converts_type_aware_editor_values() {
    let signed = IntegerFormat::signed(32);
    let unsigned = IntegerFormat::unsigned(16);

    assert_eq!(
        parse_integer_input("-1", signed, IntegerRadix::Decimal),
        Ok(0xffff_ffff)
    );

    assert_eq!(
        parse_integer_input("0xffffffff", signed, IntegerRadix::Hexadecimal),
        Ok(0xffff_ffff)
    );

    assert_eq!(
        parse_integer_input("1111_1111", unsigned, IntegerRadix::Binary),
        Ok(255)
    );

    assert_eq!(
        parse_integer_input("0o177", unsigned, IntegerRadix::Decimal),
        Ok(127)
    );

    assert!(
        parse_integer_input("32768", IntegerFormat::signed(16), IntegerRadix::Decimal).is_err()
    );

    assert!(parse_integer_input("-1", unsigned, IntegerRadix::Decimal).is_err());
    assert_eq!(parse_character_input("'A'", unsigned), Ok(65));
    assert_eq!(parse_character_input("\\n", unsigned), Ok(10));
    assert!(parse_character_input("AB", unsigned).is_err());

    assert_eq!(
        parse_string_input(r"line\nA\101\x42\\"),
        Ok(b"line\nAAB\\".to_vec())
    );
}

#[test]
fn chooses_safe_editor_semantics_from_type_and_register_role() {
    let variable = |name: &str, type_name: &str, value: &str| Variable {
        local_index: None,
        name: name.to_owned(),
        value: value.to_owned(),
        type_name: Some(type_name.to_owned()),
        argument: false,
        varobj: None,
        num_children: 0,
        has_more: false,
        display_hint: None,
        dynamic: false,
    };

    assert!(!variable("message", "character*24", "'Fortran'").is_pointer());
    assert!(variable("pointer", "real *", "0x1000").is_pointer());
    assert!(variable("function", "integer (*)(int)", "0x1000").is_pointer());
    assert_eq!(
        variable_boolean_value(&variable("enabled", "logical(kind=4)", ".TRUE."), None),
        Some(true)
    );
    assert_eq!(
        variable_boolean_value(&variable("enabled", "logical*8", ".FALSE."), None),
        Some(false)
    );

    assert_eq!(
        variable_integer_format(&variable("count", "std::uint32_t", "0x2a"), 64, None),
        Some(IntegerFormat::unsigned(32))
    );

    assert_eq!(
        variable_character_format(
            &variable("separator", "char16_t", "65 'A'"),
            64,
            crate::language::Language::Cpp,
            None,
        ),
        Some(IntegerFormat::unsigned(16))
    );

    assert_eq!(
        variable_character_format(
            &variable("letter", "char", "'🦀'"),
            64,
            crate::language::Language::Rust,
            None
        ),
        Some(IntegerFormat::unsigned(32))
    );

    assert!(variable_is_address(
        &variable("data", "char *", "0x1000 \"x\""),
        TargetArchitecture::X86_64,
    ));

    assert!(register_integer_format("$rax", 64, TargetArchitecture::X86_64).is_some());

    assert_eq!(
        register_integer_format("$rax", 32, TargetArchitecture::X86_64),
        Some(IntegerFormat::unsigned(64))
    );

    assert_eq!(
        register_integer_format("$a0", 32, TargetArchitecture::Mips64),
        Some(IntegerFormat::unsigned(64))
    );

    assert!(register_integer_format("$rsp", 64, TargetArchitecture::X86_64).is_none());
    assert!(register_integer_format("$r29", 32, TargetArchitecture::Mips32).is_none());

    assert_eq!(
        variable_boolean_value(&variable("enabled", "bool", "true"), None),
        Some(true)
    );

    assert_eq!(
        variable_boolean_value(&variable("enabled", "const _Bool", "0"), None),
        Some(false)
    );

    assert_eq!(
        variable_boolean_value(&variable("enabled", "core::ffi::c_bool", "0x1"), None),
        Some(true)
    );

    let c_buffer = string_edit(&variable("text", "char[8]", r#""hello""#)).unwrap();

    assert_eq!(
        c_buffer.storage,
        StringStorage::Buffer {
            capacity: 7,
            pointer: false
        }
    );

    let cpp = string_edit(&variable("text", "std::string &", r#""hello""#)).unwrap();
    assert_eq!(cpp.storage, StringStorage::CppString);
    let rust = string_edit(&variable("text", "alloc::string::String", r#""hello""#)).unwrap();
    assert_eq!(rust.storage, StringStorage::RustString { length: 5 });
    assert!(string_edit(&variable("wide", "char32_t[8]", r#"U"hello""#)).is_none());
}

#[test]
fn formats_pretty_printed_vector_registers_as_u64_lanes() {
    let ymm = "{\n  v16_half = {0 <repeats 16 times>},\n  v4_int64 = {\n    [0x0] = 0x1,\n    [0x1] = 0x2,\n    [0x2] = 0x3,\n    [0x3] = 0x4\n  },\n  v2_int128 = {0, 0}\n}";

    assert_eq!(
        format_register_value("ymm0", ymm, false),
        "q0=0x0000000000000001  q1=0x0000000000000002  q2=0x0000000000000003  q3=0x0000000000000004"
    );

    let zero_ymm = "{v4_int64 = {[0x0] = 0x0, [0x1] = 0x0, [0x2] = 0x0, [0x3] = 0x0}}";

    assert_eq!(
        format_register_value("ymm1", zero_ymm, false),
        "q0…q3 = 0x0000000000000000"
    );

    assert_eq!(
        register_value_css(
            &Register {
                name: String::from("ymm1"),
                value: zero_ymm.to_owned(),
                pointer_chain: Vec::new(),
            },
            TargetArchitecture::X86_64,
            Some(TargetEndian::Little),
            64,
        ),
        "register-zero"
    );

    let mixed_ymm = "{v4_int64 = {[0x0] = 0x0 <repeats 3 times>, [0x3] = 0x1}}";

    assert_eq!(
        register_value_css(
            &Register {
                name: String::from("ymm2"),
                value: mixed_ymm.to_owned(),
                pointer_chain: Vec::new(),
            },
            TargetArchitecture::X86_64,
            Some(TargetEndian::Little),
            64,
        ),
        "memory-none"
    );
}

#[test]
fn interprets_vector_union_fields_for_editing() {
    let value = "{ v8_float = {[0x0] = 0x3fc00000, [0x1] = 0xc0000000, [0x2] = 0x0 <repeats 6 times>}, v4_int64 = {[0x0] = 0x1 <repeats 4 times>} }";

    assert_eq!(
        vector_field_values(value, "v8_float", 8, VectorLaneFormat::Float32).unwrap(),
        ["1.5", "-2", "0", "0", "0", "0", "0", "0"]
    );

    assert_eq!(
        vector_field_values(value, "v4_int64", 4, VectorLaneFormat::Int64).unwrap(),
        [
            "0x0000000000000001",
            "0x0000000000000001",
            "0x0000000000000001",
            "0x0000000000000001",
        ]
    );

    let oversized_repeat = "{v4_int64 = {0 <repeats 1000000 times>}}";

    assert_eq!(
        vector_field_values(oversized_repeat, "v4_int64", 4, VectorLaneFormat::Int64,).unwrap(),
        ["0x0000000000000000"; 4]
    );
}

#[test]
fn emphasizes_only_active_flags() {
    let markup = flags_markup("0x206", Some(3));
    assert!(markup.contains("<b>INTERRUPT</b>"));
    assert!(markup.contains("<b>PARITY</b>"));
    assert!(markup.contains(" carry"));
    assert!(markup.ends_with("[Ring=3]"));
}

#[test]
fn keeps_full_addresses_and_colors_register_roles() {
    assert_eq!(full_address("0x55555555516f", 64), "0x000055555555516f");
    assert_eq!(full_address("0x8048123", 32), "0x08048123");

    let register = |name: &str, chain: &[&str]| Register {
        name: name.to_owned(),
        value: String::from("0x7fffffffcf40"),
        pointer_chain: chain.iter().map(|value| (*value).to_owned()).collect(),
    };

    assert_eq!(
        register_value_css(
            &register("rip", &[]),
            TargetArchitecture::X86_64,
            Some(TargetEndian::Little),
            64,
        ),
        "memory-code"
    );

    assert_eq!(
        register_value_css(
            &register("rsp", &[]),
            TargetArchitecture::X86_64,
            Some(TargetEndian::Little),
            64,
        ),
        "memory-stack"
    );

    assert_eq!(
        register_value_css(
            &register("rsi", &["0x1", "0x61732f656d6f682f"]),
            TargetArchitecture::X86_64,
            Some(TargetEndian::Little),
            64,
        ),
        "memory-string"
    );

    assert_eq!(
        register_details(
            &register("rax", &["0x123456789", "0x8048123"]),
            TargetArchitecture::X86_64,
            Some(TargetEndian::Little),
            32,
        ),
        "0x08048123"
    );
}

#[test]
fn formats_gef_style_thread_metadata() {
    assert_eq!(
        thread_os_id("Thread 0x7ffff7c00740 (LWP 90140)").as_deref(),
        Some("90140")
    );

    assert_eq!(stop_reason_label("breakpoint-hit"), "BREAKPOINT");
    assert_eq!(stop_reason_label("end-stepping-range"), "STEP");
}

#[test]
fn uses_compact_file_names_for_source_tabs() {
    assert_eq!(
        source_tab_title(Path::new("/project/src/parser.rs")),
        "parser.rs"
    );
}

#[test]
fn derives_instruction_flow_arguments_and_memory() {
    let instruction = Instruction {
        address: String::from("0x401000"),
        function: String::from("main"),
        offset: String::from("12"),
        opcodes: Some(String::from("e8 00 00 00 00")),
        text: String::from("call 0x402000 <mmap@plt>"),
        source: None,
    };

    let registers = ["rdi", "rsi", "rdx", "rcx", "r8", "r9"]
        .iter()
        .enumerate()
        .map(|(index, name)| Register {
            name: (*name).to_owned(),
            value: format!("0x{index:x}"),
            pointer_chain: Vec::new(),
        })
        .collect::<Vec<_>>();

    assert_eq!(
        instruction_flow_description(&instruction, &registers, TargetArchitecture::X86_64,),
        "CALL  ▶  0x402000 <mmap@plt>"
    );

    let arguments =
        instruction_arguments_description(&instruction, &registers, TargetArchitecture::X86_64);

    assert!(arguments.contains("$rdi=0x0000000000000000"));
    assert!(arguments.contains("$r9=0x0000000000000005"));

    let memory_instruction = Instruction {
        text: String::from("mov rax,QWORD PTR [rbp-0x10]"),
        ..instruction.clone()
    };

    let mut with_rbp = registers;

    with_rbp.push(Register {
        name: String::from("rbp"),
        value: String::from("0x7fff0000"),
        pointer_chain: Vec::new(),
    });

    assert_eq!(
        instruction_memory_expression(&memory_instruction, &with_rbp, TargetArchitecture::X86_64,)
            .as_deref(),
        Some("($rbp-0x10)")
    );
}

#[test]
fn classifies_live_call_abi_boundaries_without_guessing_sequential_state() {
    let call = Instruction {
        address: String::from("0x401000"),
        function: String::from("main"),
        offset: String::from("16"),
        opcodes: Some(String::from("e8 00 00 00 00")),
        text: String::from("call 0x402000 <worker>"),
        source: None,
    };

    assert_eq!(
        call_abi_phase(&call, None, TargetArchitecture::X86_64),
        CallAbiPhase::OutgoingCall {
            target: Some(String::from("0x402000")),
        }
    );

    let entry = Instruction {
        address: String::from("0x402000"),
        function: String::from("worker"),
        offset: String::from("0"),
        opcodes: None,
        text: String::from("push rbp"),
        source: None,
    };

    assert_eq!(
        call_abi_phase(&entry, None, TargetArchitecture::X86_64),
        CallAbiPhase::IncomingEntry {
            function: String::from("worker"),
        }
    );

    let after_call = Instruction {
        address: String::from("0x401005"),
        function: String::from("main"),
        offset: String::from("21"),
        opcodes: None,
        text: String::from("mov rbx,rax"),
        source: None,
    };

    assert_eq!(
        call_abi_phase(&after_call, Some(&call), TargetArchitecture::X86_64),
        CallAbiPhase::Returned {
            target: Some(String::from("0x402000")),
        }
    );

    assert_eq!(
        call_abi_phase(
            &Instruction {
                text: String::from("ret"),
                ..after_call
            },
            None,
            TargetArchitecture::X86_64,
        ),
        CallAbiPhase::Returning
    );
}

#[test]
fn resolves_direct_symbol_and_register_flow_targets() {
    let instruction = Instruction {
        address: String::from("0x401000"),
        function: String::from("main"),
        offset: String::from("12"),
        opcodes: None,
        text: String::from("call 0x402000 <worker>"),
        source: None,
    };

    assert_eq!(
        instruction_flow_target(&instruction, TargetArchitecture::X86_64).as_deref(),
        Some("0x402000")
    );

    assert_eq!(
        instruction_flow_target(
            &Instruction {
                text: String::from("call rax"),
                ..instruction.clone()
            },
            TargetArchitecture::X86_64,
        )
        .as_deref(),
        Some("$rax")
    );

    assert_eq!(
        instruction_flow_target(
            &Instruction {
                text: String::from("bl worker"),
                ..instruction.clone()
            },
            TargetArchitecture::AArch64,
        )
        .as_deref(),
        Some("worker")
    );

    assert!(
        instruction_flow_target(
            &Instruction {
                text: String::from("mov rax,rbx"),
                ..instruction
            },
            TargetArchitecture::X86_64,
        )
        .is_none()
    );
}

#[test]
fn prefixed_calls_have_consistent_until_flow_and_abi_classification() {
    let instruction = Instruction {
        address: String::from("0x401000"),
        function: String::from("main"),
        offset: String::from("12"),
        opcodes: None,
        text: String::from("NOTRACK CALL *%rax"),
        source: None,
    };

    assert!(instruction_matches_until(
        &UntilAction::NextCall,
        &instruction,
        TargetArchitecture::X86_64
    ));
    assert!(
        instruction_flow_description(&instruction, &[], TargetArchitecture::X86_64)
            .starts_with("CALL")
    );
    assert_eq!(
        call_abi_phase(&instruction, None, TargetArchitecture::X86_64),
        CallAbiPhase::OutgoingCall {
            target: Some(String::from("$rax"))
        },
    );

    for text in [
        "call *0x402000",
        "call QWORD PTR [rip+0x20]",
        "call *0x20(%rax)",
        "call *0x20(%rax,%rbx,8)",
    ] {
        let instruction = Instruction {
            text: text.to_owned(),
            ..instruction.clone()
        };
        assert_eq!(
            instruction_flow_target(&instruction, TargetArchitecture::X86_64),
            None
        );
    }

    let instruction = Instruction {
        text: String::from("tbz x0, #0x3, 0x402000 <worker<int, int>>"),
        ..instruction
    };

    assert_eq!(
        instruction_flow_target(&instruction, TargetArchitecture::AArch64).as_deref(),
        Some("0x402000")
    );
}

#[test]
fn predicts_x86_branches_and_decodes_linux_syscalls() {
    let branch = Instruction {
        address: String::from("0x401000"),
        function: String::from("main"),
        offset: String::from("12"),
        opcodes: Some(String::from("75 0a")),
        text: String::from("jne 0x40100c <main+0x1c>"),
        source: None,
    };

    let flags = Register {
        name: String::from("eflags"),
        value: String::from("0x246"),
        pointer_chain: Vec::new(),
    };

    assert_eq!(
        instruction_flow_description(
            &branch,
            std::slice::from_ref(&flags),
            TargetArchitecture::X86_64,
        ),
        "BRANCH  NOT TAKEN  ▶  0x40100c <main+0x1c>"
    );

    let syscall = Instruction {
        text: String::from("syscall"),
        ..branch
    };

    let registers = [
        ("rax", "0x1"),
        ("rdi", "0x2"),
        ("rsi", "0x7fff0000"),
        ("rdx", "0x20"),
    ]
    .map(|(name, value)| Register {
        name: name.to_owned(),
        value: value.to_owned(),
        pointer_chain: Vec::new(),
    });

    let arguments =
        instruction_arguments_description(&syscall, &registers, TargetArchitecture::X86_64);

    assert!(arguments.starts_with("SYSCALL  #1 write("));
    assert!(arguments.contains("fd=0x0000000000000002"));
    assert!(arguments.contains("count=0x0000000000000020"));

    let carry = Register {
        name: String::from("eflags"),
        value: String::from("0x1"),
        pointer_chain: Vec::new(),
    };

    for (mnemonic, expected) in [("jbe 0x10", Some(true)), ("ja 0x10", Some(false))] {
        let conditional = Instruction {
            text: mnemonic.to_owned(),
            ..syscall.clone()
        };

        assert_eq!(
            conditional_branch_taken(
                &conditional,
                std::slice::from_ref(&carry),
                TargetArchitecture::X86,
            ),
            expected,
            "{mnemonic}"
        );
    }

    let i386_registers = [
        ("eax", "0x4"),
        ("ebx", "0x1"),
        ("ecx", "0x8049000"),
        ("edx", "0x4"),
    ]
    .map(|(name, value)| Register {
        name: name.to_owned(),
        value: value.to_owned(),
        pointer_chain: Vec::new(),
    });

    let int80 = Instruction {
        text: String::from("int 0x80"),
        ..syscall
    };

    let arguments =
        instruction_arguments_description(&int80, &i386_registers, TargetArchitecture::X86);

    assert!(arguments.starts_with("SYSCALL  #4 write("));
    assert!(arguments.contains("fd=0x00000001"));

    let arguments =
        instruction_arguments_description(&int80, &i386_registers, TargetArchitecture::X86_64);

    assert!(arguments.starts_with("SYSCALL  #4 write("));

    let trap = Instruction {
        text: String::from("int3"),
        ..int80
    };

    assert!(
        !instruction_flow_description(&trap, &i386_registers, TargetArchitecture::X86_64,)
            .contains("SYSCALL")
    );

    assert!(
        instruction_arguments_description(&trap, &i386_registers, TargetArchitecture::X86_64,)
            .is_empty()
    );
}

#[test]
fn applies_arm_and_riscv_abis_to_instruction_insight() {
    assert_eq!(
        format_register_value_for_architecture("r8", "0x1234", false, TargetArchitecture::Arm,),
        "0x00001234"
    );

    assert_eq!(
        format_register_value_for_architecture("r8", "0x1234", false, TargetArchitecture::X86_64,),
        "0x0000000000001234"
    );

    let svc = Instruction {
        address: String::from("0x4000"),
        function: String::from("write_one"),
        offset: String::from("4"),
        opcodes: None,
        text: String::from("svc #0"),
        source: None,
    };

    let aarch64_registers = [
        ("x8", "0x40"),
        ("x0", "0x1"),
        ("x1", "0x8000"),
        ("x2", "0x4"),
    ]
    .map(|(name, value)| Register {
        name: name.to_owned(),
        value: value.to_owned(),
        pointer_chain: Vec::new(),
    });

    let arguments =
        instruction_arguments_description(&svc, &aarch64_registers, TargetArchitecture::AArch64);

    assert!(arguments.starts_with("SYSCALL  #64 write("));
    assert!(arguments.contains("fd=0x0000000000000001"));

    let branch = Instruction {
        text: String::from("beq a0,a1,0x1010"),
        ..svc.clone()
    };

    let riscv_registers = [("a0", "0x2a"), ("a1", "0x2a")].map(|(name, value)| Register {
        name: name.to_owned(),
        value: value.to_owned(),
        pointer_chain: Vec::new(),
    });

    assert_eq!(
        instruction_flow_description(&branch, &riscv_registers, TargetArchitecture::RiscV32,),
        "BRANCH  TAKEN  ▶  a0,a1,0x1010"
    );

    let load = Instruction {
        text: String::from("ldr x3,[x0, #0x10]"),
        ..svc
    };

    assert_eq!(
        instruction_memory_expression(&load, &aarch64_registers, TargetArchitecture::AArch64,)
            .as_deref(),
        Some("($x0 + 0x10)")
    );

    let arm_return = Instruction {
        text: String::from("bx lr"),
        ..load.clone()
    };

    assert_eq!(
        instruction_flow_description(&arm_return, &[], TargetArchitecture::Arm),
        "RETURN  ▶  return to caller"
    );

    let aarch64_return = Instruction {
        text: String::from("br x30"),
        ..load.clone()
    };

    assert_eq!(
        instruction_flow_description(&aarch64_return, &[], TargetArchitecture::AArch64),
        "RETURN  ▶  return to caller"
    );

    let s390_svc = Instruction {
        text: String::from("svc 4"),
        ..load
    };

    let s390_registers =
        [("r2", "0x1"), ("r3", "0x2000"), ("r4", "0x8")].map(|(name, value)| Register {
            name: name.to_owned(),
            value: value.to_owned(),
            pointer_chain: Vec::new(),
        });

    assert!(
        instruction_arguments_description(&s390_svc, &s390_registers, TargetArchitecture::S390x,)
            .starts_with("SYSCALL  #4 write(")
    );
}

#[test]
fn formats_non_native_register_values_without_host_assumptions() {
    assert_eq!(
        format_register_value_for_target(
            "r3",
            "0x54455854",
            true,
            TargetArchitecture::PowerPc32,
            Some(TargetEndian::Big),
            32,
        ),
        "0x54455854 'TEXT…'"
    );

    let vector = format_register_value_for_target(
        "v0",
        "{ uint128 = 0x1, u = { 0x1, 0x2 } }",
        false,
        TargetArchitecture::AArch64,
        Some(TargetEndian::Little),
        64,
    );

    assert!(vector.contains("uint128 = 0x1"));
    assert_ne!(vector, "{");
}

#[test]
fn finds_ctrl_click_source_symbols() {
    let rust = "let values = Vec::new();";

    assert_eq!(
        source_symbol_at_offset(rust, rust.find("new").unwrap() + 1).as_deref(),
        Some("Vec::new")
    );

    assert_eq!(
        source_symbol_at_offset(rust, rust.find("values").unwrap() + 2),
        None
    );

    let c = "void *region = mmap(NULL, size, 0, 0, -1, 0);";

    assert_eq!(
        source_symbol_at_offset(c, c.find("mmap").unwrap() + 2).as_deref(),
        Some("mmap")
    );

    assert_eq!(
        source_symbol_at_offset(c, c.find("region").unwrap() + 2),
        None
    );

    assert_eq!(
        source_symbol_at_offset(c, c.find("size").unwrap() + 1),
        None
    );

    assert_eq!(source_symbol_at_offset(c, c.find('*').unwrap()), None);
    let generic = "let value = factory::build::<Vec<u8>> (input);";

    assert_eq!(
        source_symbol_at_offset(generic, generic.find("build").unwrap() + 2).as_deref(),
        Some("factory::build")
    );

    let control = "if (ready) { worker.run(); }";
    assert_eq!(source_symbol_at_offset(control, 1), None);

    assert_eq!(
        source_symbol_at_offset(control, control.find("run").unwrap() + 1).as_deref(),
        Some("run")
    );

    assert_eq!(
        source_symbol_at_offset(control, control.find("ready").unwrap() + 1),
        None
    );
}

#[test]
fn ranks_generic_rust_method_definitions() {
    let location = |function: &str, file: &str| SourceLocation {
        function: function.to_owned(),
        file: file.to_owned(),
        fullname: None,
        line: 1,
    };

    let vec = location(
        "alloc::vec::Vec<u8, alloc::alloc::Global>::new<u8>",
        "vec/mod.rs",
    );

    let small_vec = location("smallvec::SmallVec<[u8; 16]>::new<[u8; 16]>", "smallvec.rs");

    assert_eq!(
        without_generic_arguments(&vec.function),
        "alloc::vec::Vec::new"
    );

    assert!(
        source_location_score("Vec::new", &vec) > source_location_score("Vec::new", &small_vec)
    );

    let malloc = location("__GI___libc_malloc", "malloc.c");
    let cleanup = location("__malloc_arena_thread_freeres", "arena.c");

    assert!(source_location_score("malloc", &malloc) > source_location_score("malloc", &cleanup));

    let verbose =
        "alloc::vec::Vec<alloc::boxed::Box<dyn core::fmt::Debug>, alloc::alloc::Global>::push";

    assert_eq!(compact_function_name(verbose), "alloc::vec::Vec<…>::push");
    assert_eq!(compact_function_name("core::ptr::read"), "core::ptr::read");
}

#[test]
fn separates_bulk_breakpoint_and_watchpoint_numbers() {
    let stop_point = |number: &str, kind: &str| Breakpoint {
        number: number.to_owned(),
        kind: kind.to_owned(),
        enabled: true,
        condition: None,
        catch_type: None,
        address: None,
        function: None,
        file: None,
        fullname: None,
        line: None,
        original_location: None,
        disposition: Some(String::from("keep")),
        hit_count: 0,
        ignore_count: 0,
        thread: None,
        inferior: None,
        pending: None,
        commands: Vec::new(),
        parent_number: None,
        location_count: 0,
    };

    let stop_points = vec![
        stop_point("1.1", "breakpoint"),
        stop_point("1.2", "breakpoint"),
        stop_point("2", "hw watchpoint"),
        Breakpoint {
            original_location: Some(String::from("SIGSEGV")),
            ..stop_point("3", "catchpoint")
        },
        Breakpoint {
            catch_type: Some(String::from("throw")),
            original_location: Some(String::from("exception throw")),
            ..stop_point("4", "catchpoint")
        },
        Breakpoint {
            original_location: Some(String::from("rust_panic")),
            ..stop_point("5", "breakpoint")
        },
        Breakpoint {
            catch_type: Some(String::from("syscall")),
            original_location: Some(String::from("openat, read")),
            ..stop_point("6", "catchpoint")
        },
        Breakpoint {
            catch_type: Some(String::from("syscall")),
            original_location: Some(String::from("<any syscall>")),
            ..stop_point("7", "catchpoint")
        },
    ];

    assert_eq!(breakpoint_command_numbers(&stop_points, false), ["1"]);
    assert_eq!(breakpoint_command_numbers(&stop_points, true), ["2"]);
    assert_eq!(signal_catchpoint_command_numbers(&stop_points), ["3"]);

    assert_eq!(
        event_catchpoint_command_numbers(&stop_points),
        ["4", "5", "6", "7"]
    );

    assert_eq!(
        event_catchpoint_command_number(&stop_points, EventCatchpoint::CxxThrow).as_deref(),
        Some("4")
    );

    assert_eq!(
        event_catchpoint_command_number(&stop_points, EventCatchpoint::RustPanic).as_deref(),
        Some("5")
    );

    assert_eq!(
        event_catchpoint_command_number(&stop_points, EventCatchpoint::Syscall).as_deref(),
        Some("7")
    );

    assert_eq!(
        signal_catchpoint_command_number(&stop_points, "segv").as_deref(),
        Some("3")
    );

    assert_eq!(normalized_signal_name(" usr1 ").as_deref(), Some("SIGUSR1"));

    assert_eq!(
        normalized_signal_name("SIGRTMIN+1").as_deref(),
        Some("SIGRTMIN+1")
    );

    assert!(normalized_signal_name("SIGSEGV; quit").is_none());
    let mut stop_points = stop_points;
    assert!(set_breakpoint_enabled(&mut stop_points, "1", false));
    assert!(!stop_points[0].enabled);
    assert!(!stop_points[1].enabled);
    assert!(stop_points[2].enabled);
    assert!(!set_breakpoint_enabled(&mut stop_points, "1", false));
    assert!(set_breakpoint_enabled(&mut stop_points, "1.1", true));
    assert!(stop_points[0].enabled);
    assert!(!stop_points[1].enabled);
    stop_points[0].address = Some(String::from("0x0000000000401000"));

    assert_eq!(
        breakpoint_command_number_at_address(&stop_points, "0x401000").as_deref(),
        Some("1")
    );

    assert_eq!(
        breakpoint_command_number_at_address(&stop_points, "0x402000"),
        None
    );
}
