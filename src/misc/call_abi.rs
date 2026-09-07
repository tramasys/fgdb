//! Calling-convention facts and the current instruction transfer.

use crate::debugger::{Register, StackFrame, TargetArchitecture};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct CallAbiSnapshot {
    pub architecture: String,
    pub calling_convention: String,
    pub pointer_bits: u32,
    pub current_frame: Option<CallAbiFrame>,
    pub contract: Vec<CallAbiFact>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CallAbiFrame {
    pub level: u32,
    pub address: String,
    pub function: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CallAbiRegister {
    pub role: String,
    pub name: String,
    pub value: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CallAbiFact {
    pub aspect: String,
    pub value: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CallAbiPhase {
    OutgoingCall { target: Option<String> },
    IncomingEntry { function: String },
    Returning,
    Returned { target: Option<String> },
    Sequential,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct CallAbiTransfer {
    pub context: String,
    pub registers: Vec<CallAbiRegister>,
}

pub(crate) fn call_abi_snapshot(
    architecture: TargetArchitecture,
    pointer_bits: u32,
    selected_level: u32,
    frames: &[StackFrame],
) -> CallAbiSnapshot {
    let frames = frames
        .iter()
        .map(|frame| CallAbiFrame {
            level: frame.level,
            address: frame.address.clone(),
            function: frame.function.clone(),
        })
        .collect::<Vec<_>>();

    CallAbiSnapshot {
        architecture: architecture.display_name().to_owned(),
        calling_convention: linux_calling_convention(architecture, pointer_bits).to_owned(),
        pointer_bits,
        current_frame: frames
            .iter()
            .find(|frame| frame.level == selected_level)
            .cloned()
            .or_else(|| frames.first().cloned()),
        contract: call_abi_contract(architecture),
    }
}

pub(crate) fn call_abi_transfer(
    architecture: TargetArchitecture,
    phase: CallAbiPhase,
    registers: &[Register],
) -> CallAbiTransfer {
    let context = match &phase {
        CallAbiPhase::OutgoingCall { target } => {
            transfer_context("OUTGOING CALL", target.as_deref())
        }
        CallAbiPhase::IncomingEntry { function } => format!("FUNCTION ENTRY  {function}"),
        CallAbiPhase::Returning => String::from("FUNCTION RETURN  outgoing return value"),
        CallAbiPhase::Returned { target } => {
            transfer_context("RETURNED FROM CALL", target.as_deref())
        }
        CallAbiPhase::Sequential => String::from("No ABI call transfer at the current instruction"),
    };

    let mut selected = Vec::new();

    let mut add = |role: String, name: &str| {
        if let Some(register) = registers.iter().find(|register| register.name == name) {
            selected.push(CallAbiRegister {
                role,
                name: format!("${name}"),
                value: register.value.clone(),
            });
        }
    };

    match phase {
        CallAbiPhase::OutgoingCall { .. } => {
            for (index, name) in architecture.call_argument_registers().iter().enumerate() {
                add(
                    format!("Outgoing integer / pointer slot {}", index + 1),
                    name,
                );
            }

            add_stack_pointer(&mut add, architecture, registers, "Call-site stack pointer");
        }
        CallAbiPhase::IncomingEntry { .. } => {
            for (index, name) in architecture.call_argument_registers().iter().enumerate() {
                add(
                    format!("Incoming integer / pointer slot {}", index + 1),
                    name,
                );
            }

            add_stack_pointer(&mut add, architecture, registers, "Entry stack pointer");
        }
        CallAbiPhase::Returning | CallAbiPhase::Returned { .. } => {
            for (index, name) in architecture.call_return_registers().iter().enumerate() {
                let role = if index == 0 {
                    "Primary return register"
                } else {
                    "Secondary / wide return register"
                };

                add(role.to_owned(), name);
            }

            add_stack_pointer(
                &mut add,
                architecture,
                registers,
                "Return-site stack pointer",
            );
        }
        CallAbiPhase::Sequential => {}
    }

    CallAbiTransfer {
        context,
        registers: selected,
    }
}

fn add_stack_pointer(
    add: &mut impl FnMut(String, &str),
    architecture: TargetArchitecture,
    registers: &[Register],
    role: &str,
) {
    if let Some(name) =
        architecture.stack_pointer(registers.iter().map(|register| register.name.as_str()))
    {
        add(role.to_owned(), name);
    }
}

fn transfer_context(kind: &str, target: Option<&str>) -> String {
    target.map_or_else(|| kind.to_owned(), |target| format!("{kind}  {target}"))
}

fn call_abi_contract(architecture: TargetArchitecture) -> Vec<CallAbiFact> {
    let argument_registers = architecture.call_argument_registers();
    let return_registers = architecture.call_return_registers();

    vec![
        CallAbiFact {
            aspect: String::from("Integer / pointer arguments"),
            value: if argument_registers.is_empty() {
                match architecture {
                    TargetArchitecture::X86 => String::from("stack"),
                    _ => String::from("target-defined"),
                }
            } else {
                format_register_list(argument_registers)
            },
        },
        CallAbiFact {
            aspect: String::from("Integer / pointer return"),
            value: if return_registers.is_empty() {
                String::from("target-defined")
            } else {
                format_register_list(return_registers)
            },
        },
        CallAbiFact {
            aspect: String::from("Call linkage"),
            value: call_linkage(architecture).to_owned(),
        },
        CallAbiFact {
            aspect: String::from("Stack contract"),
            value: call_stack_contract(architecture).to_owned(),
        },
    ]
}

fn format_register_list(registers: &[&str]) -> String {
    registers
        .iter()
        .map(|register| format!("${register}"))
        .collect::<Vec<_>>()
        .join("  ")
}

fn call_linkage(architecture: TargetArchitecture) -> &'static str {
    match architecture {
        TargetArchitecture::X86 | TargetArchitecture::X86_64 => "return address pushed on stack",
        TargetArchitecture::Arm | TargetArchitecture::AArch64 => "link register $lr",
        TargetArchitecture::RiscV32 | TargetArchitecture::RiscV64 => "return address register $ra",
        TargetArchitecture::Mips32 | TargetArchitecture::Mips64 => "return address register $ra",
        TargetArchitecture::PowerPc32 | TargetArchitecture::PowerPc64 => "link register $lr",
        TargetArchitecture::S390 | TargetArchitecture::S390x => "return address register $r14",
        TargetArchitecture::LoongArch64 => "return address register $ra",
        TargetArchitecture::Unknown => "target-defined",
    }
}

fn call_stack_contract(architecture: TargetArchitecture) -> &'static str {
    match architecture {
        TargetArchitecture::X86 => "downward-growing  arguments continue on stack",
        TargetArchitecture::X86_64 => "downward-growing  16-byte call alignment  128-byte red zone",
        TargetArchitecture::Arm => "downward-growing  8-byte public-interface alignment",
        TargetArchitecture::AArch64 => "downward-growing  16-byte alignment",
        TargetArchitecture::RiscV32 | TargetArchitecture::RiscV64 => {
            "downward-growing  16-byte alignment"
        }
        TargetArchitecture::Mips32 | TargetArchitecture::Mips64 => {
            "downward-growing  ABI argument area"
        }
        TargetArchitecture::PowerPc32 | TargetArchitecture::PowerPc64 => {
            "downward-growing  stack-frame back chain"
        }
        TargetArchitecture::S390 | TargetArchitecture::S390x => {
            "downward-growing  register save area"
        }
        TargetArchitecture::LoongArch64 => "downward-growing  16-byte alignment",
        TargetArchitecture::Unknown => "target-defined",
    }
}

fn linux_calling_convention(architecture: TargetArchitecture, pointer_bits: u32) -> &'static str {
    match (architecture, pointer_bits) {
        (TargetArchitecture::X86, _) => "System V i386 ABI",
        (TargetArchitecture::X86_64, 32) => "System V AMD64 x32 ABI",
        (TargetArchitecture::X86_64, _) => "System V AMD64 ABI",
        (TargetArchitecture::Arm, _) => "AAPCS32",
        (TargetArchitecture::AArch64, 32) => "AAPCS64 ILP32",
        (TargetArchitecture::AArch64, _) => "AAPCS64",
        (TargetArchitecture::RiscV32 | TargetArchitecture::RiscV64, _) => "RISC-V ELF psABI",
        (TargetArchitecture::Mips32, _) => "MIPS o32 ABI",
        (TargetArchitecture::Mips64, 32) => "MIPS n32 ABI",
        (TargetArchitecture::Mips64, _) => "MIPS n64 ABI",
        (TargetArchitecture::PowerPc32 | TargetArchitecture::PowerPc64, _) => "PowerPC ELF ABI",
        (TargetArchitecture::S390 | TargetArchitecture::S390x, _) => "zSeries ELF ABI",
        (TargetArchitecture::LoongArch64, _) => "LoongArch ELF psABI",
        (TargetArchitecture::Unknown, _) => "calling convention unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn call_abi_snapshot_and_transfer_use_only_exact_available_facts() {
        let frames = [StackFrame {
            level: 0,
            address: String::from("0x401000"),
            function: String::from("main"),
            architecture: None,
            file: Some(String::from("main.c")),
            fullname: None,
            line: Some(7),
        }];

        let registers = [
            Register {
                name: String::from("rip"),
                value: String::from("0x401000"),
                pointer_chain: Vec::new(),
            },
            Register {
                name: String::from("rsp"),
                value: String::from("0x7fff0000"),
                pointer_chain: Vec::new(),
            },
            Register {
                name: String::from("rdi"),
                value: String::from("0x2a"),
                pointer_chain: Vec::new(),
            },
        ];

        let snapshot = call_abi_snapshot(TargetArchitecture::X86_64, 64, 0, &frames);
        assert_eq!(snapshot.current_frame.unwrap().function, "main");

        assert_eq!(
            snapshot.contract[0].value,
            "$rdi  $rsi  $rdx  $rcx  $r8  $r9"
        );

        let transfer = call_abi_transfer(
            TargetArchitecture::X86_64,
            CallAbiPhase::OutgoingCall {
                target: Some(String::from("malloc")),
            },
            &registers,
        );

        assert_eq!(transfer.context, "OUTGOING CALL  malloc");
        assert_eq!(transfer.registers.len(), 2);
        assert_eq!(transfer.registers[0].name, "$rdi");
        assert_eq!(transfer.registers[0].value, "0x2a");
        assert_eq!(transfer.registers[1].name, "$rsp");
    }
}
