//! Shared instruction semantics for execution controls and presentation.

use super::{Instruction, TargetArchitecture};
use std::borrow::Cow;

#[cfg(test)]
mod tests;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum UntilAction {
    CurrentLine,
    FunctionReturns,
    NextCall,
    NextReturn,
    NextSyscall,
    NextIndirectBranch,
    NextControlFlow,
    MemoryAccess,
    UserCode,
    LibcCode,
    RegionChange,
    Expression(String),
}

pub(crate) fn hex_value(value: &str) -> Option<u64> {
    let hex = value
        .split_whitespace()
        .next()?
        .trim_end_matches(',')
        .strip_prefix("0x")?;

    u64::from_str_radix(hex, 16).ok()
}

pub(crate) fn split_instruction(instruction: &str) -> (&str, &str) {
    let instruction = instruction.trim();
    match instruction.find(char::is_whitespace) {
        Some(index) => (&instruction[..index], instruction[index..].trim()),
        None => (instruction, ""),
    }
}

pub(crate) fn is_call_instruction(
    mnemonic: &str,
    operands: &str,
    architecture: TargetArchitecture,
) -> bool {
    match architecture {
        TargetArchitecture::X86 | TargetArchitecture::X86_64 => {
            mnemonic.starts_with("call") || mnemonic.starts_with("lcall")
        }

        TargetArchitecture::Arm => matches!(mnemonic, "bl" | "blx"),
        TargetArchitecture::AArch64 => matches!(mnemonic, "bl" | "blr"),
        TargetArchitecture::RiscV32 | TargetArchitecture::RiscV64 => {
            matches!(mnemonic, "call" | "jal")
                || (mnemonic == "jalr"
                    && operands
                        .split(',')
                        .next()
                        .map(|operand| operand.trim().trim_start_matches(['$', '%']))
                        .is_none_or(|operand| !matches!(operand, "zero" | "x0")))
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

pub(crate) fn is_return_instruction(
    mnemonic: &str,
    operands: &str,
    architecture: TargetArchitecture,
) -> bool {
    if mnemonic.starts_with("ret") {
        return true;
    }

    match architecture {
        TargetArchitecture::X86 | TargetArchitecture::X86_64 => {
            mnemonic.starts_with("lret") || mnemonic.starts_with("iret")
        }

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

            let mut operands = operands
                .split(',')
                .map(|operand| operand.trim().trim_start_matches('$'));

            matches!(
                (
                    operands.next(),
                    operands.next(),
                    operands.next(),
                    operands.next()
                ),
                (Some("zero" | "r0"), Some("ra" | "r1"), Some(_), None)
            )
        }

        _ => false,
    }
}

pub(crate) fn syscall_architecture(
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

pub(crate) fn is_unconditional_branch(mnemonic: &str, architecture: TargetArchitecture) -> bool {
    match architecture {
        TargetArchitecture::X86 | TargetArchitecture::X86_64 => {
            mnemonic.starts_with("jmp") || mnemonic.starts_with("ljmp")
        }

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

pub(crate) fn is_conditional_branch(mnemonic: &str, architecture: TargetArchitecture) -> bool {
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

pub(crate) fn instruction_matches_until(
    action: &UntilAction,
    instruction: &Instruction,
    architecture: TargetArchitecture,
) -> bool {
    let (mnemonic, operands) = normalized_instruction_parts(&instruction.text, architecture);
    let mnemonic = mnemonic.as_ref();
    match action {
        UntilAction::NextCall => is_call_instruction(mnemonic, operands, architecture),
        UntilAction::NextReturn => is_return_instruction(mnemonic, operands, architecture),
        UntilAction::NextSyscall => {
            syscall_architecture(mnemonic, operands, architecture).is_some()
        }

        UntilAction::NextIndirectBranch => is_indirect_branch(mnemonic, operands, architecture),
        UntilAction::NextControlFlow => {
            is_call_instruction(mnemonic, operands, architecture)
                || is_return_instruction(mnemonic, operands, architecture)
                || is_unconditional_branch(mnemonic, architecture)
                || is_conditional_branch(mnemonic, architecture)
        }

        UntilAction::MemoryAccess => instruction_accesses_memory(mnemonic, operands, architecture),
        UntilAction::CurrentLine
        | UntilAction::FunctionReturns
        | UntilAction::UserCode
        | UntilAction::LibcCode
        | UntilAction::RegionChange
        | UntilAction::Expression(_) => false,
    }
}

pub(crate) fn instruction_ends_linear_flow(
    instruction: &Instruction,
    architecture: TargetArchitecture,
) -> bool {
    let (mnemonic, operands) = normalized_instruction_parts(&instruction.text, architecture);
    let mnemonic = mnemonic.as_ref();
    if is_call_instruction(mnemonic, operands, architecture)
        || is_return_instruction(mnemonic, operands, architecture)
        || is_unconditional_branch(mnemonic, architecture)
        || is_conditional_branch(mnemonic, architecture)
        || syscall_architecture(mnemonic, operands, architecture).is_some()
    {
        return true;
    }

    matches!(
        architecture,
        TargetArchitecture::X86 | TargetArchitecture::X86_64
    ) && (matches!(
        mnemonic,
        "int"
            | "int1"
            | "int3"
            | "into"
            | "sysret"
            | "sysexit"
            | "rsm"
            | "vmrun"
            | "vmlaunch"
            | "xabort"
            | "xbegin"
    ) || mnemonic.starts_with("sysret")
        || mnemonic.starts_with("sysexit"))
}

pub(crate) fn normalized_instruction_parts<'a>(
    instruction: &'a str,
    architecture: TargetArchitecture,
) -> (Cow<'a, str>, &'a str) {
    let (mut mnemonic, mut operands) = split_instruction(instruction);

    if matches!(
        architecture,
        TargetArchitecture::X86 | TargetArchitecture::X86_64
    ) {
        while is_x86_instruction_prefix(mnemonic) {
            let next = split_instruction(operands);

            if next.0.is_empty() {
                break;
            }

            (mnemonic, operands) = next;
        }
    }

    let mnemonic = if mnemonic.bytes().any(|byte| byte.is_ascii_uppercase()) {
        Cow::Owned(mnemonic.to_ascii_lowercase())
    } else {
        Cow::Borrowed(mnemonic)
    };

    (mnemonic, operands)
}

fn is_x86_instruction_prefix(mnemonic: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "lock", "rep", "repe", "repz", "repne", "repnz", "bnd", "notrack", "data16", "addr16",
        "addr32", "rex", "rex.w", "rex2", "xacquire", "xrelease", "cs", "ds", "es", "fs", "gs",
        "ss",
    ];

    PREFIXES.contains(&mnemonic)
        || (mnemonic.bytes().any(|byte| byte.is_ascii_uppercase())
            && PREFIXES
                .iter()
                .any(|prefix| mnemonic.eq_ignore_ascii_case(prefix)))
}

pub(crate) fn is_indirect_branch(
    mnemonic: &str,
    operands: &str,
    architecture: TargetArchitecture,
) -> bool {
    if !is_call_instruction(mnemonic, operands, architecture)
        && !is_unconditional_branch(mnemonic, architecture)
        && !is_conditional_branch(mnemonic, architecture)
    {
        return false;
    }

    let operand = operands
        .split_once('<')
        .map_or(operands, |(arguments, _)| arguments)
        .trim();

    match architecture {
        TargetArchitecture::X86 | TargetArchitecture::X86_64 => {
            operand.starts_with('*')
                || operand.contains(['[', '('])
                || instruction_operand_is_register(
                    operand.trim_start_matches(['*', '$', '%']),
                    architecture,
                )
        }

        TargetArchitecture::Arm => matches!(mnemonic, "bx" | "blx"),
        TargetArchitecture::AArch64 => matches!(mnemonic, "br" | "blr"),
        TargetArchitecture::RiscV32 | TargetArchitecture::RiscV64 => {
            matches!(mnemonic, "jr" | "jalr")
        }

        TargetArchitecture::Mips32 | TargetArchitecture::Mips64 => {
            matches!(mnemonic, "jr" | "jalr")
        }

        TargetArchitecture::PowerPc32 | TargetArchitecture::PowerPc64 => {
            matches!(mnemonic, "bctr" | "bctrl" | "blr" | "blrl")
        }

        TargetArchitecture::S390 | TargetArchitecture::S390x => {
            matches!(mnemonic, "br" | "basr" | "bcr")
        }

        TargetArchitecture::LoongArch64 => mnemonic == "jirl",
        TargetArchitecture::Unknown => operand.starts_with('*') || operand.contains(['[', '(']),
    }
}

pub(crate) fn direct_control_flow_address(
    mnemonic: &str,
    operands: &str,
    architecture: TargetArchitecture,
) -> Option<u64> {
    hex_value(direct_control_flow_literal(
        mnemonic,
        operands,
        architecture,
    )?)
}

/// A direct hexadecimal destination, excluding symbol annotations and offsets
/// into indirect memory operands. The returned spelling is suitable for display.
pub(crate) fn direct_control_flow_literal<'a>(
    mnemonic: &str,
    operands: &'a str,
    architecture: TargetArchitecture,
) -> Option<&'a str> {
    if is_indirect_branch(mnemonic, operands, architecture) {
        return None;
    }

    operands
        .split_once('<')
        .map_or(operands, |(arguments, _)| arguments)
        .rsplit(',')
        .next()?
        .split(|character: char| {
            character.is_whitespace() || matches!(character, ',' | '(' | ')' | '[' | ']')
        })
        .map(|part| part.trim_matches(|character: char| matches!(character, '$' | '#' | ';' | ':')))
        .find(|part| {
            part.strip_prefix("0x").is_some_and(|hex| {
                !hex.is_empty() && hex.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
        })
}

fn instruction_accesses_memory(
    mnemonic: &str,
    operands: &str,
    architecture: TargetArchitecture,
) -> bool {
    if operands.contains('[') || (operands.contains('(') && operands.contains(')')) {
        return true;
    }

    match architecture {
        TargetArchitecture::X86 | TargetArchitecture::X86_64 => {
            mnemonic.starts_with("push")
                || mnemonic.starts_with("pop")
                || mnemonic.starts_with("movs")
                || mnemonic.starts_with("cmps")
                || mnemonic.starts_with("scas")
                || mnemonic.starts_with("lods")
                || mnemonic.starts_with("stos")
                || matches!(mnemonic, "enter" | "leave" | "xlat")
        }

        TargetArchitecture::Arm | TargetArchitecture::AArch64 => {
            mnemonic.starts_with("ldr")
                || mnemonic.starts_with("str")
                || mnemonic.starts_with("ldp")
                || mnemonic.starts_with("stp")
                || mnemonic.starts_with("ldm")
                || mnemonic.starts_with("stm")
        }

        TargetArchitecture::RiscV32
        | TargetArchitecture::RiscV64
        | TargetArchitecture::Mips32
        | TargetArchitecture::Mips64
        | TargetArchitecture::LoongArch64 => matches!(
            mnemonic,
            "lb" | "lbu"
                | "lh"
                | "lhu"
                | "lw"
                | "lwu"
                | "ld"
                | "sb"
                | "sh"
                | "sw"
                | "sd"
                | "ll"
                | "sc"
        ),
        TargetArchitecture::PowerPc32 | TargetArchitecture::PowerPc64 => {
            ["lb", "lh", "lw", "ld", "lm", "ls", "lf", "lv", "st"]
                .iter()
                .any(|prefix| mnemonic.starts_with(prefix))
        }

        TargetArchitecture::S390 | TargetArchitecture::S390x => {
            matches!(mnemonic, "l" | "lg" | "ly" | "st" | "stg" | "sty")
                || mnemonic.starts_with("lm")
                || mnemonic.starts_with("stm")
        }

        TargetArchitecture::Unknown => false,
    }
}

pub(crate) fn instruction_operand_is_register(
    name: &str,
    architecture: TargetArchitecture,
) -> bool {
    let name = if name.bytes().any(|byte| byte.is_ascii_uppercase()) {
        Cow::Owned(name.to_ascii_lowercase())
    } else {
        Cow::Borrowed(name)
    };

    let numbered = |prefix: char| {
        name.strip_prefix(prefix).is_some_and(|number| {
            !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit())
        })
    };

    match architecture {
        TargetArchitecture::X86 | TargetArchitecture::X86_64 => {
            matches!(
                name.as_ref(),
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
            matches!(name.as_ref(), "sp" | "lr" | "pc")
                || numbered('r')
                || numbered('x')
                || numbered('w')
        }

        TargetArchitecture::RiscV32 | TargetArchitecture::RiscV64 => {
            numbered('x') || matches!(name.as_ref(), "ra" | "sp" | "gp" | "tp" | "fp")
        }

        _ => numbered('r'),
    }
}
