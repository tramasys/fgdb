use super::*;

#[test]
fn classifies_native_until_instruction_events_across_syntaxes() {
    let instruction = |text: &str| Instruction {
        address: String::from("0x401000"),
        function: String::from("main"),
        offset: String::from("0"),
        opcodes: None,
        text: text.to_owned(),
        source: None,
    };

    assert!(instruction_matches_until(
        &UntilAction::NextCall,
        &instruction("call 0x402000 <worker>"),
        TargetArchitecture::X86_64,
    ));

    assert!(instruction_matches_until(
        &UntilAction::NextIndirectBranch,
        &instruction("call *%rax"),
        TargetArchitecture::X86_64,
    ));

    assert!(!instruction_matches_until(
        &UntilAction::NextIndirectBranch,
        &instruction("call 0x402000 <worker>"),
        TargetArchitecture::X86_64,
    ));

    assert!(instruction_matches_until(
        &UntilAction::MemoryAccess,
        &instruction("mov 0x10(%rsp),%rax"),
        TargetArchitecture::X86_64,
    ));

    assert!(instruction_matches_until(
        &UntilAction::NextSyscall,
        &instruction("svc #0"),
        TargetArchitecture::AArch64,
    ));

    assert!(instruction_matches_until(
        &UntilAction::NextControlFlow,
        &instruction("jalr ra,a0,0"),
        TargetArchitecture::RiscV64,
    ));
}

#[test]
fn direct_targets_exclude_immediates_symbols_and_indirect_displacements() {
    assert_eq!(
        direct_control_flow_address(
            "tbz",
            "x0, #0x3, 0x402000 <worker<int, int>>",
            TargetArchitecture::AArch64
        ),
        Some(0x402000),
    );

    for text in [
        "call *0x402000",
        "call *0x20(%rax,%rbx,8)",
        "call QWORD PTR [rip+0x20]",
        "NOTRACK CALL *%rax",
    ] {
        let (mnemonic, operands) = normalized_instruction_parts(text, TargetArchitecture::X86_64);

        assert!(
            is_indirect_branch(&mnemonic, operands, TargetArchitecture::X86_64),
            "{text}"
        );

        assert_eq!(
            direct_control_flow_address(&mnemonic, operands, TargetArchitecture::X86_64),
            None,
            "{text}"
        );
    }

    let operands = "0x0000402000 <worker(int, int)>";
    assert!(!is_indirect_branch(
        "call",
        operands,
        TargetArchitecture::X86_64
    ));

    assert_eq!(
        direct_control_flow_address("call", operands, TargetArchitecture::X86_64),
        Some(0x402000)
    );

    assert_eq!(
        direct_control_flow_literal("call", operands, TargetArchitecture::X86_64),
        Some("0x0000402000")
    );
}

#[test]
fn lowercase_instruction_normalization_borrows_the_input() {
    for text in ["call *%rax", "notrack call *%rax", "repz ret"] {
        let (mnemonic, _) = normalized_instruction_parts(text, TargetArchitecture::X86_64);
        assert!(matches!(mnemonic, Cow::Borrowed(_)));
    }

    let (mnemonic, operands) =
        normalized_instruction_parts("NOTRACK CALL *%rax", TargetArchitecture::X86_64);
    assert_eq!(mnemonic, "call");
    assert_eq!(operands, "*%rax");
}
