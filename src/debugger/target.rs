#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TargetArchitecture {
    X86,
    X86_64,
    Arm,
    AArch64,
    RiscV32,
    RiscV64,
    Mips32,
    Mips64,
    PowerPc32,
    PowerPc64,
    S390,
    S390x,
    LoongArch64,
    #[default]
    Unknown,
}

impl TargetArchitecture {
    pub fn from_gdb_description(description: &str) -> Self {
        let value = description.to_ascii_lowercase();
        if value.contains("i386:x86-64")
            || value.contains("i386:x64-32")
            || value.contains("x64-32")
            || value.contains("x86_64")
            || value.contains("x86-64")
            || value.contains("amd64")
        {
            Self::X86_64
        } else if ["i386", "i486", "i586", "i686"]
            .iter()
            .any(|name| value.contains(name))
        {
            Self::X86
        } else if value.contains("aarch64") || value.contains("arm64") {
            Self::AArch64
        } else if value.contains("loongarch") {
            Self::LoongArch64
        } else if value.contains("riscv") || value.contains("risc-v") {
            if value.contains("rv32") || value.contains("riscv:rv32") {
                Self::RiscV32
            } else {
                Self::RiscV64
            }
        } else if value.contains("mips") {
            if value.contains("mips64") || value.contains("isa64") {
                Self::Mips64
            } else {
                Self::Mips32
            }
        } else if value.contains("powerpc") || value.contains("ppc") {
            if value.contains("64") {
                Self::PowerPc64
            } else {
                Self::PowerPc32
            }
        } else if value.contains("s390") {
            if value.contains("s390x") || value.contains("64") {
                Self::S390x
            } else {
                Self::S390
            }
        } else if value.starts_with("arm") || value.contains(" arm") {
            Self::Arm
        } else {
            Self::Unknown
        }
    }

    pub fn pointer_bits_from_gdb_description(description: &str) -> Option<u32> {
        Self::explicit_pointer_bits_from_gdb_description(description).or_else(|| {
            let value = description.to_ascii_lowercase();
            Self::from_gdb_description(&value).pointer_bits()
        })
    }

    pub fn explicit_pointer_bits_from_gdb_description(description: &str) -> Option<u32> {
        let value = description.to_ascii_lowercase();
        (value.contains("i386:x64-32")
            || value.contains("x64-32")
            || value.contains("ilp32")
            || (value.contains("mips") && value.contains("n32")))
        .then_some(32)
    }

    pub fn from_elf(machine: u16, elf_class: u8) -> Self {
        match machine {
            3 => Self::X86,
            8 => {
                if elf_class == 2 {
                    Self::Mips64
                } else {
                    Self::Mips32
                }
            }
            20 => Self::PowerPc32,
            21 => Self::PowerPc64,
            22 => {
                if elf_class == 2 {
                    Self::S390x
                } else {
                    Self::S390
                }
            }
            40 => Self::Arm,
            62 => Self::X86_64,
            183 => Self::AArch64,
            243 => {
                if elf_class == 2 {
                    Self::RiscV64
                } else {
                    Self::RiscV32
                }
            }
            258 => Self::LoongArch64,
            _ => Self::Unknown,
        }
    }

    pub fn from_elf_ident(bytes: &[u8]) -> Option<(Self, TargetEndian, u32)> {
        if bytes.get(..4)? != b"\x7fELF" {
            return None;
        }
        let (elf_class, pointer_bits) = match bytes.get(4)? {
            1 => (1, 32),
            2 => (2, 64),
            _ => return None,
        };
        let endian = match bytes.get(5)? {
            1 => TargetEndian::Little,
            2 => TargetEndian::Big,
            _ => return None,
        };
        let machine: [u8; 2] = bytes.get(18..20)?.try_into().ok()?;
        let machine = match endian {
            TargetEndian::Little => u16::from_le_bytes(machine),
            TargetEndian::Big => u16::from_be_bytes(machine),
        };
        let mut architecture = Self::from_elf(machine, elf_class);
        // MIPS n32 uses ELFCLASS32 pointers with a 64-bit ISA. Without the
        // ABI2 flag it is indistinguishable from o32 by class alone.
        if machine == 8 && elf_class == 1 {
            let flags = bytes.get(36..40).and_then(|bytes| {
                let bytes: [u8; 4] = bytes.try_into().ok()?;
                Some(match endian {
                    TargetEndian::Little => u32::from_le_bytes(bytes),
                    TargetEndian::Big => u32::from_be_bytes(bytes),
                })
            });
            if flags.is_some_and(|flags| flags & 0x20 != 0) {
                architecture = Self::Mips64;
            }
        }
        Some((architecture, endian, pointer_bits))
    }

    pub fn infer_from_register_names<T: AsRef<str>>(names: &[T]) -> Self {
        Self::infer_from_register_names_with_bits(names, None)
    }

    pub fn infer_from_register_names_with_bits<I, T>(names: I, pointer_bits: Option<u32>) -> Self
    where
        I: IntoIterator<Item = T>,
        T: AsRef<str>,
    {
        let mut rip_or_rax = false;
        let mut eip_or_eax = false;
        let mut s390 = false;
        let mut powerpc = false;
        let mut x30 = false;
        let mut sp_or_pc = false;
        let mut cpsr = false;
        let mut orig_a0_or_badv = false;
        let mut r31 = false;
        let mut x31 = false;
        let mut a7 = false;
        let mut zero = false;
        let mut ra = false;
        let mut tp = false;
        let mut badvaddr = false;
        let mut hi = false;
        let mut lo = false;
        for name in names {
            match name.as_ref() {
                "rip" | "rax" => rip_or_rax = true,
                "eip" | "eax" => eip_or_eax = true,
                "pswa" | "pswm" => s390 = true,
                "xer" | "ctr" => powerpc = true,
                "x30" => x30 = true,
                "sp" | "pc" => sp_or_pc = true,
                "cpsr" => cpsr = true,
                "orig_a0" | "badv" => orig_a0_or_badv = true,
                "r31" => r31 = true,
                "x31" => x31 = true,
                "a7" => a7 = true,
                "zero" => zero = true,
                "ra" => ra = true,
                "tp" => tp = true,
                "badvaddr" => badvaddr = true,
                "hi" => hi = true,
                "lo" => lo = true,
                _ => {}
            }
        }
        if rip_or_rax {
            Self::X86_64
        } else if eip_or_eax {
            Self::X86
        } else if s390 {
            if pointer_bits == Some(64) {
                Self::S390x
            } else if pointer_bits == Some(32) {
                Self::S390
            } else {
                Self::Unknown
            }
        } else if powerpc {
            if pointer_bits == Some(64) {
                Self::PowerPc64
            } else if pointer_bits == Some(32) {
                Self::PowerPc32
            } else {
                Self::Unknown
            }
        } else if x30 && sp_or_pc {
            Self::AArch64
        } else if cpsr {
            Self::Arm
        } else if orig_a0_or_badv && r31 {
            Self::LoongArch64
        } else if x31 || (a7 && zero && ra && tp) {
            match pointer_bits {
                Some(32) => Self::RiscV32,
                Some(64) => Self::RiscV64,
                _ => Self::Unknown,
            }
        } else if badvaddr && hi && lo {
            match pointer_bits {
                Some(64) => Self::Mips64,
                Some(32) => Self::Mips32,
                _ => Self::Unknown,
            }
        } else {
            Self::Unknown
        }
    }

    pub fn pointer_bits(self) -> Option<u32> {
        match self {
            Self::X86 | Self::Arm | Self::RiscV32 | Self::Mips32 | Self::PowerPc32 | Self::S390 => {
                Some(32)
            }
            Self::X86_64
            | Self::AArch64
            | Self::RiscV64
            | Self::Mips64
            | Self::PowerPc64
            | Self::S390x
            | Self::LoongArch64 => Some(64),
            Self::Unknown => None,
        }
    }

    /// Refines architectures whose 32/64-bit ISA variant follows the target
    /// pointer ABI. x86-64, AArch64 and MIPS64 are intentionally not narrowed:
    /// their ILP32 ABIs retain the 64-bit ISA and register file.
    pub fn refine_for_pointer_bits(self, pointer_bits: u32) -> Self {
        match (self, pointer_bits) {
            (Self::RiscV32 | Self::RiscV64, 32) => Self::RiscV32,
            (Self::RiscV32 | Self::RiscV64, 64) => Self::RiscV64,
            (Self::PowerPc32 | Self::PowerPc64, 32) => Self::PowerPc32,
            (Self::PowerPc32 | Self::PowerPc64, 64) => Self::PowerPc64,
            (Self::S390 | Self::S390x, 32) => Self::S390,
            (Self::S390 | Self::S390x, 64) => Self::S390x,
            _ => self,
        }
    }

    pub fn default_endian(self) -> Option<TargetEndian> {
        match self {
            Self::X86 | Self::X86_64 | Self::LoongArch64 => Some(TargetEndian::Little),
            Self::S390 | Self::S390x => Some(TargetEndian::Big),
            Self::Arm
            | Self::AArch64
            | Self::RiscV32
            | Self::RiscV64
            | Self::Mips32
            | Self::Mips64
            | Self::PowerPc32
            | Self::PowerPc64
            | Self::Unknown => None,
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::X86 => "x86",
            Self::X86_64 => "x86-64",
            Self::Arm => "ARM",
            Self::AArch64 => "AArch64",
            Self::RiscV32 => "RISC-V 32",
            Self::RiscV64 => "RISC-V 64",
            Self::Mips32 => "MIPS32",
            Self::Mips64 => "MIPS64",
            Self::PowerPc32 => "PowerPC 32",
            Self::PowerPc64 => "PowerPC 64",
            Self::S390 => "s390",
            Self::S390x => "s390x",
            Self::LoongArch64 => "LoongArch64",
            Self::Unknown => "unknown architecture",
        }
    }

    pub fn effective_for_registers(self, names: &[String]) -> Self {
        if self == Self::Unknown {
            Self::infer_from_register_names(names)
        } else {
            self
        }
    }

    pub fn is_core_register(self, name: &str) -> bool {
        match self {
            Self::X86 => is_one_of(name, X86_CORE),
            Self::X86_64 => is_one_of(name, X86_64_CORE),
            Self::Arm => {
                numbered(name, "r", 15) || is_one_of(name, &["sp", "lr", "pc", "cpsr", "fpscr"])
            }
            Self::AArch64 => {
                numbered(name, "x", 30)
                    || is_one_of(
                        name,
                        &[
                            "sp",
                            "pc",
                            "cpsr",
                            "pstate",
                            "nzcv",
                            "tpidr_el0",
                            "tpidrro_el0",
                            "fpsr",
                            "fpcr",
                        ],
                    )
            }
            Self::RiscV32 | Self::RiscV64 => {
                numbered(name, "x", 31)
                    || riscv_abi_register(name)
                    || is_one_of(name, &["pc", "fcsr", "fflags", "frm"])
            }
            Self::Mips32 | Self::Mips64 => {
                numbered(name, "r", 31)
                    || mips_abi_register(name)
                    || is_one_of(
                        name,
                        &["pc", "hi", "lo", "status", "cause", "badvaddr", "fcsr"],
                    )
            }
            Self::PowerPc32 | Self::PowerPc64 => {
                numbered(name, "r", 31)
                    || is_one_of(
                        name,
                        &[
                            "pc", "nip", "msr", "cr", "lr", "ctr", "xer", "orig_r3", "trap",
                            "fpscr", "vscr",
                        ],
                    )
            }
            Self::S390 | Self::S390x => {
                numbered(name, "r", 15)
                    || numbered(name, "a", 15)
                    || numbered(name, "acr", 15)
                    || is_one_of(name, &["pswa", "pswm", "fpc"])
            }
            Self::LoongArch64 => {
                numbered(name, "r", 31)
                    || loongarch_abi_register(name)
                    || is_one_of(name, &["pc", "badv", "orig_a0", "fcsr"])
            }
            Self::Unknown => generic_core_register(name),
        }
    }

    pub fn is_address_register(self, name: &str) -> bool {
        match self {
            Self::X86 | Self::X86_64 => {
                self.is_core_register(name)
                    && !self.is_status_register(name)
                    && !matches!(name, "cs" | "ss" | "ds" | "es" | "fs" | "gs")
            }
            Self::Arm => {
                numbered(name, "r", 15)
                    || is_one_of(name, &["sp", "lr", "pc", "tpidruro", "tpidrurw"])
            }
            Self::AArch64 => {
                numbered(name, "x", 30)
                    || is_one_of(name, &["sp", "pc", "tpidr_el0", "tpidrro_el0"])
            }
            Self::RiscV32 | Self::RiscV64 => {
                (numbered(name, "x", 31) && name != "x0")
                    || (riscv_abi_register(name) && name != "zero")
                    || name == "pc"
            }
            Self::Mips32 | Self::Mips64 => {
                (numbered(name, "r", 31) && name != "r0")
                    || (mips_abi_register(name) && name != "zero")
                    || is_one_of(name, &["pc", "badvaddr", "tp", "thread_pointer"])
            }
            Self::PowerPc32 | Self::PowerPc64 => {
                numbered(name, "r", 31) || is_one_of(name, &["pc", "nip", "lr", "ctr"])
            }
            Self::S390 | Self::S390x => numbered(name, "r", 15) || name == "pswa",
            Self::LoongArch64 => {
                (numbered(name, "r", 31) && name != "r0")
                    || (loongarch_abi_register(name) && name != "zero")
                    || is_one_of(name, &["pc", "badv"])
            }
            Self::Unknown => generic_address_register(name),
        }
    }

    pub fn is_status_register(self, name: &str) -> bool {
        is_one_of(
            name,
            &[
                "eflags", "rflags", "cpsr", "pstate", "nzcv", "fcsr", "fflags", "frm", "status",
                "cause", "msr", "cr", "xer", "pswm", "fpc", "fpscr", "fpsr", "fpcr", "vscr",
            ],
        )
    }

    pub fn is_vector_register(self, name: &str) -> bool {
        match self {
            Self::X86 | Self::X86_64 => {
                numbered_in(name, &["xmm", "ymm", "zmm"], 63)
                    || numbered(name, "mm", 7)
                    || name == "mxcsr"
            }
            Self::Arm => numbered(name, "q", 15),
            Self::AArch64 => {
                numbered_in(name, &["v", "q", "z"], 31)
                    || numbered(name, "p", 15)
                    || is_one_of(name, &["ffr", "vg"])
            }
            Self::RiscV32 | Self::RiscV64 => {
                numbered(name, "v", 31)
                    || is_one_of(
                        name,
                        &["vl", "vtype", "vlenb", "vstart", "vxrm", "vxsat", "vcsr"],
                    )
            }
            Self::Mips32 | Self::Mips64 => numbered(name, "w", 31),
            Self::PowerPc32 | Self::PowerPc64 => {
                numbered(name, "vr", 31) || numbered(name, "vs", 63)
            }
            Self::S390 | Self::S390x => numbered(name, "v", 31),
            Self::LoongArch64 => numbered(name, "vr", 31) || numbered(name, "xr", 31),
            Self::Unknown => false,
        }
    }

    pub fn is_floating_register(self, name: &str) -> bool {
        match self {
            Self::X86 | Self::X86_64 => {
                is_one_of(
                    name,
                    &[
                        "fctrl", "fstat", "ftag", "fiseg", "fioff", "foseg", "fooff", "fop",
                    ],
                ) || numbered(name, "st", 7)
            }
            Self::Arm | Self::AArch64 => numbered_in(name, &["s", "d"], 31),
            Self::RiscV32
            | Self::RiscV64
            | Self::Mips32
            | Self::Mips64
            | Self::PowerPc32
            | Self::PowerPc64 => numbered(name, "f", 31),
            Self::S390 | Self::S390x => numbered(name, "f", 15),
            Self::LoongArch64 => numbered(name, "f", 31) || numbered(name, "fcc", 7),
            Self::Unknown => false,
        }
    }

    /// Width of a scalar register value. This is intentionally independent
    /// from pointer width: x86-64 x32 and MIPS n32 have 64-bit registers but
    /// 32-bit pointers.
    pub fn scalar_register_bits(self, name: &str, pointer_bits: u32) -> u32 {
        if matches!(name, "cs" | "ss" | "ds" | "es" | "fs" | "gs") {
            return 16;
        }
        if matches!(
            name,
            "cpsr"
                | "pstate"
                | "nzcv"
                | "fpscr"
                | "fpsr"
                | "fpcr"
                | "fcsr"
                | "fpc"
                | "cr"
                | "xer"
                | "vscr"
        ) {
            return 32;
        }
        if matches!(self, Self::X86 | Self::X86_64)
            && (name.starts_with('e') && name.len() == 3 || name == "eip" || name == "eflags")
        {
            return 32;
        }
        if matches!(self, Self::Arm | Self::AArch64) && numbered(name, "s", 31) {
            return 32;
        }
        if self == Self::AArch64 && numbered(name, "w", 31) {
            return 32;
        }
        if matches!(self, Self::S390 | Self::S390x)
            && (numbered(name, "a", 15) || numbered(name, "acr", 15))
        {
            return 32;
        }
        self.pointer_bits().unwrap_or(pointer_bits).clamp(16, 128)
    }

    pub fn is_program_counter(self, name: &str) -> bool {
        match self {
            Self::X86 => name == "eip",
            Self::X86_64 => name == "rip",
            Self::Arm => is_one_of(name, &["pc", "r15"]),
            Self::PowerPc32 | Self::PowerPc64 => is_one_of(name, &["pc", "nip"]),
            Self::S390 | Self::S390x => name == "pswa",
            _ => name == "pc",
        }
    }

    pub fn is_stack_or_frame_pointer(self, name: &str) -> bool {
        match self {
            Self::X86 => is_one_of(name, &["esp", "ebp"]),
            Self::X86_64 => is_one_of(name, &["rsp", "rbp"]),
            Self::Arm => is_one_of(name, &["sp", "r13", "fp", "r11"]),
            Self::AArch64 => is_one_of(name, &["sp", "x29"]),
            Self::RiscV32 | Self::RiscV64 => is_one_of(name, &["sp", "x2", "fp", "s0", "x8"]),
            Self::Mips32 | Self::Mips64 => is_one_of(name, &["sp", "r29", "fp", "s8", "r30"]),
            Self::PowerPc32 | Self::PowerPc64 => name == "r1",
            Self::S390 | Self::S390x => name == "r15",
            Self::LoongArch64 => is_one_of(name, &["sp", "r3", "fp", "r22"]),
            Self::Unknown => is_one_of(name, &["rsp", "rbp", "esp", "ebp", "sp", "fp"]),
        }
    }

    /// Registers whose ABI role is intrinsically address-like. General
    /// purpose registers are deliberately excluded: forcing `$rax` or `$x0`
    /// to hexadecimal would make ordinary integer editing unnecessarily
    /// restrictive, while PCs, stack/frame/link and thread pointers should
    /// remain address editors.
    pub fn is_dedicated_address_register(self, name: &str) -> bool {
        if self.is_program_counter(name)
            || self.is_stack_or_frame_pointer(name)
            || self.is_thread_pointer(name)
        {
            return true;
        }
        match self {
            Self::Arm => is_one_of(name, &["lr", "r14"]),
            Self::AArch64 => name == "x30",
            Self::RiscV32 | Self::RiscV64 => is_one_of(name, &["ra", "x1", "gp", "x3", "tp", "x4"]),
            Self::Mips32 | Self::Mips64 => is_one_of(name, &["gp", "r28", "ra", "r31", "badvaddr"]),
            Self::PowerPc32 | Self::PowerPc64 => is_one_of(name, &["r2", "r13", "lr"]),
            Self::S390 | Self::S390x => is_one_of(name, &["a0", "acr0"]),
            Self::LoongArch64 => is_one_of(name, &["ra", "r1", "tp", "r2", "badv"]),
            Self::X86 | Self::X86_64 | Self::Unknown => false,
        }
    }

    pub fn stack_pointer<'a>(
        self,
        names: impl IntoIterator<Item = &'a str>,
    ) -> Option<&'static str> {
        let candidates: &[&str] = match self {
            Self::X86 => &["esp"],
            Self::X86_64 => &["rsp"],
            Self::Arm => &["sp", "r13"],
            Self::AArch64 => &["sp"],
            Self::RiscV32 | Self::RiscV64 => &["sp", "x2"],
            Self::Mips32 | Self::Mips64 => &["sp", "r29"],
            Self::PowerPc32 | Self::PowerPc64 => &["r1"],
            Self::S390 | Self::S390x => &["r15"],
            Self::LoongArch64 => &["sp", "r3"],
            Self::Unknown => &["rsp", "esp", "sp"],
        };
        names.into_iter().find_map(|name| {
            candidates
                .iter()
                .copied()
                .find(|candidate| *candidate == name)
        })
    }

    pub fn thread_pointer_candidates(self) -> &'static [&'static str] {
        match self {
            Self::X86 | Self::X86_64 => &["fs_base", "gs_base"],
            Self::Arm => &["tpidruro", "tpidrurw"],
            Self::AArch64 => &["tpidr_el0", "tpidrro_el0"],
            Self::RiscV32 | Self::RiscV64 => &["tp", "x4"],
            Self::Mips32 | Self::Mips64 => &["tp", "thread_pointer"],
            Self::PowerPc32 => &["r2"],
            Self::PowerPc64 => &["r13"],
            Self::S390 | Self::S390x => &["a0", "acr0"],
            Self::LoongArch64 => &["tp", "r2"],
            Self::Unknown => &[
                "fs_base",
                "gs_base",
                "tpidr_el0",
                "tpidrro_el0",
                "tpidruro",
                "tpidrurw",
                "tp",
                "thread_pointer",
            ],
        }
    }

    pub fn is_thread_pointer(self, name: &str) -> bool {
        self.thread_pointer_candidates().contains(&name)
    }

    pub fn call_argument_registers(self) -> &'static [&'static str] {
        match self {
            Self::X86 => &[],
            Self::X86_64 => &["rdi", "rsi", "rdx", "rcx", "r8", "r9"],
            Self::Arm => &["r0", "r1", "r2", "r3"],
            Self::AArch64 => &["x0", "x1", "x2", "x3", "x4", "x5", "x6", "x7"],
            Self::RiscV32 | Self::RiscV64 | Self::LoongArch64 => {
                &["a0", "a1", "a2", "a3", "a4", "a5", "a6", "a7"]
            }
            Self::Mips32 => &["a0", "a1", "a2", "a3"],
            Self::Mips64 => &["a0", "a1", "a2", "a3", "a4", "a5", "a6", "a7"],
            Self::PowerPc32 | Self::PowerPc64 => &["r3", "r4", "r5", "r6", "r7", "r8", "r9", "r10"],
            Self::S390 | Self::S390x => &["r2", "r3", "r4", "r5", "r6"],
            Self::Unknown => &[],
        }
    }

    pub fn call_return_registers(self) -> &'static [&'static str] {
        match self {
            Self::X86 => &["eax", "edx"],
            Self::X86_64 => &["rax", "rdx"],
            Self::Arm => &["r0", "r1"],
            Self::AArch64 => &["x0", "x1"],
            Self::RiscV32 | Self::RiscV64 | Self::LoongArch64 => &["a0", "a1"],
            Self::Mips32 | Self::Mips64 => &["v0", "v1"],
            Self::PowerPc32 | Self::PowerPc64 => &["r3", "r4"],
            Self::S390 | Self::S390x => &["r2", "r3"],
            Self::Unknown => &[],
        }
    }

    pub fn syscall_registers(self) -> Option<(&'static str, &'static [&'static str])> {
        match self {
            Self::X86 => Some(("eax", &["ebx", "ecx", "edx", "esi", "edi", "ebp"])),
            Self::X86_64 => Some(("rax", &["rdi", "rsi", "rdx", "r10", "r8", "r9"])),
            Self::Arm => Some(("r7", &["r0", "r1", "r2", "r3", "r4", "r5"])),
            Self::AArch64 => Some(("x8", &["x0", "x1", "x2", "x3", "x4", "x5"])),
            Self::RiscV32 | Self::RiscV64 | Self::LoongArch64 => {
                Some(("a7", &["a0", "a1", "a2", "a3", "a4", "a5"]))
            }
            Self::Mips32 => Some(("v0", &["a0", "a1", "a2", "a3"])),
            Self::Mips64 => Some(("v0", &["a0", "a1", "a2", "a3", "a4", "a5"])),
            Self::PowerPc32 | Self::PowerPc64 => {
                Some(("r0", &["r3", "r4", "r5", "r6", "r7", "r8"]))
            }
            Self::S390 | Self::S390x => Some(("r1", &["r2", "r3", "r4", "r5", "r6", "r7"])),
            Self::Unknown => None,
        }
    }

    pub fn syscall_name(self, number: u64) -> &'static str {
        let number = self.normalize_syscall_number(number);
        match self {
            Self::X86_64 => x86_64_syscall_name(number),
            Self::AArch64 | Self::RiscV32 | Self::RiscV64 | Self::LoongArch64 => {
                generic_syscall_name(number)
            }
            Self::X86 => i386_syscall_name(number),
            Self::Arm => arm_syscall_name(number),
            Self::Mips32 => mips_o32_syscall_name(number.saturating_sub(4_000)),
            Self::Mips64 => mips64_syscall_name(number),
            Self::PowerPc32 | Self::PowerPc64 => powerpc_syscall_name(number),
            Self::S390 | Self::S390x => s390_syscall_name(number),
            Self::Unknown => "syscall",
        }
    }

    pub fn normalize_syscall_number(self, number: u64) -> u64 {
        if self == Self::X86_64 {
            number & !0x4000_0000
        } else {
            number
        }
    }
}

const X86_CORE: &[&str] = &[
    "eax", "ebx", "ecx", "edx", "esp", "ebp", "esi", "edi", "eip", "eflags", "cs", "ss", "ds",
    "es", "fs", "gs", "fs_base", "gs_base",
];
const X86_64_CORE: &[&str] = &[
    "rax", "rbx", "rcx", "rdx", "rsp", "rbp", "rsi", "rdi", "rip", "r8", "r9", "r10", "r11", "r12",
    "r13", "r14", "r15", "rflags", "eflags", "cs", "ss", "ds", "es", "fs", "gs", "fs_base",
    "gs_base",
];

fn is_one_of(name: &str, values: &[&str]) -> bool {
    values.contains(&name)
}

fn numbered(name: &str, prefix: &str, maximum: u8) -> bool {
    name.strip_prefix(prefix)
        .and_then(|number| number.parse::<u8>().ok())
        .is_some_and(|number| number <= maximum)
}

fn numbered_in(name: &str, prefixes: &[&str], maximum: u8) -> bool {
    prefixes
        .iter()
        .any(|prefix| numbered(name, prefix, maximum))
}

fn riscv_abi_register(name: &str) -> bool {
    is_one_of(name, &["zero", "ra", "sp", "gp", "tp", "fp"])
        || numbered_in(name, &["a"], 7)
        || numbered_in(name, &["s"], 11)
        || numbered_in(name, &["t"], 6)
}

fn mips_abi_register(name: &str) -> bool {
    is_one_of(
        name,
        &["zero", "at", "v0", "v1", "gp", "sp", "fp", "s8", "ra"],
    ) || numbered_in(name, &["a"], 7)
        || numbered_in(name, &["s"], 7)
        || numbered_in(name, &["t"], 9)
        || numbered_in(name, &["k"], 1)
}

fn loongarch_abi_register(name: &str) -> bool {
    is_one_of(name, &["zero", "ra", "tp", "sp", "fp"])
        || numbered_in(name, &["a"], 7)
        || numbered_in(name, &["t"], 8)
        || numbered_in(name, &["s"], 8)
}

fn generic_core_register(name: &str) -> bool {
    X86_CORE.contains(&name)
        || X86_64_CORE.contains(&name)
        || numbered(name, "x", 31)
        || numbered(name, "r", 31)
        || is_one_of(
            name,
            &[
                "sp", "fp", "lr", "pc", "ra", "gp", "tp", "pswa", "pswm", "cpsr", "pstate", "nzcv",
                "cr", "xer", "ctr",
            ],
        )
}

fn generic_address_register(name: &str) -> bool {
    is_one_of(
        name,
        &[
            "rsp",
            "rbp",
            "rip",
            "esp",
            "ebp",
            "eip",
            "sp",
            "fp",
            "lr",
            "pc",
            "ra",
            "gp",
            "tp",
            "pswa",
            "fs_base",
            "gs_base",
            "tpidr_el0",
            "tpidrro_el0",
            "tpidruro",
            "tpidrurw",
            "thread_pointer",
        ],
    )
}

fn x86_64_syscall_name(number: u64) -> &'static str {
    match number {
        0 => "read",
        1 => "write",
        2 => "open",
        3 => "close",
        8 => "lseek",
        9 => "mmap",
        10 => "mprotect",
        11 => "munmap",
        12 => "brk",
        16 => "ioctl",
        17 => "pread64",
        18 => "pwrite64",
        19 => "readv",
        20 => "writev",
        39 => "getpid",
        56 => "clone",
        57 => "fork",
        58 => "vfork",
        59 => "execve",
        60 => "exit",
        61 => "wait4",
        62 => "kill",
        72 => "fcntl",
        158 => "arch_prctl",
        186 => "gettid",
        202 => "futex",
        217 => "getdents64",
        231 => "exit_group",
        257 => "openat",
        262 => "newfstatat",
        273 => "set_robust_list",
        318 => "getrandom",
        332 => "statx",
        435 => "clone3",
        436 => "close_range",
        437 => "openat2",
        449 => "futex_waitv",
        _ => "syscall",
    }
}

fn generic_syscall_name(number: u64) -> &'static str {
    match number {
        29 => "ioctl",
        56 => "openat",
        57 => "close",
        62 => "lseek",
        63 => "read",
        64 => "write",
        65 => "readv",
        66 => "writev",
        67 => "pread64",
        68 => "pwrite64",
        93 => "exit",
        94 => "exit_group",
        98 => "futex",
        99 => "set_robust_list",
        129 => "kill",
        172 => "getpid",
        178 => "gettid",
        214 => "brk",
        215 => "munmap",
        220 => "clone",
        221 => "execve",
        222 => "mmap",
        226 => "mprotect",
        260 => "wait4",
        261 => "prlimit64",
        278 => "getrandom",
        291 => "statx",
        435 => "clone3",
        436 => "close_range",
        437 => "openat2",
        449 => "futex_waitv",
        _ => "syscall",
    }
}

fn i386_syscall_name(number: u64) -> &'static str {
    match number {
        1 => "exit",
        2 => "fork",
        3 => "read",
        4 => "write",
        5 => "open",
        6 => "close",
        11 => "execve",
        19 => "lseek",
        20 => "getpid",
        37 => "kill",
        45 => "brk",
        54 => "ioctl",
        90 => "mmap",
        91 => "munmap",
        114 => "wait4",
        120 => "clone",
        145 => "readv",
        146 => "writev",
        162 => "nanosleep",
        192 => "mmap2",
        224 => "gettid",
        240 => "futex",
        252 => "exit_group",
        295 => "openat",
        311 => "set_robust_list",
        355 => "getrandom",
        383 => "statx",
        435 => "clone3",
        436 => "close_range",
        437 => "openat2",
        449 => "futex_waitv",
        _ => "syscall",
    }
}

fn arm_syscall_name(number: u64) -> &'static str {
    match number {
        180 => "pread64",
        181 => "pwrite64",
        190 => "vfork",
        192 => "mmap2",
        224 => "gettid",
        240 => "futex",
        248 => "exit_group",
        322 => "openat",
        327 => "newfstatat",
        338 => "set_robust_list",
        384 => "getrandom",
        397 => "statx",
        _ => common_legacy_syscall_name(number),
    }
}

fn powerpc_syscall_name(number: u64) -> &'static str {
    match number {
        179 => "pread64",
        180 => "pwrite64",
        207 => "gettid",
        221 => "futex",
        234 => "exit_group",
        286 => "openat",
        291 => "newfstatat",
        300 => "set_robust_list",
        359 => "getrandom",
        383 => "statx",
        _ => common_legacy_syscall_name(number),
    }
}

fn s390_syscall_name(number: u64) -> &'static str {
    match number {
        180 => "pread64",
        181 => "pwrite64",
        190 => "vfork",
        236 => "gettid",
        238 => "futex",
        248 => "exit_group",
        288 => "openat",
        293 => "newfstatat",
        304 => "set_robust_list",
        349 => "getrandom",
        379 => "statx",
        _ => common_legacy_syscall_name(number),
    }
}

fn mips_o32_syscall_name(number: u64) -> &'static str {
    match number {
        200 => "pread64",
        201 => "pwrite64",
        210 => "mmap2",
        222 => "gettid",
        238 => "futex",
        246 => "exit_group",
        288 => "openat",
        293 => "newfstatat",
        309 => "set_robust_list",
        353 => "getrandom",
        366 => "statx",
        _ => common_legacy_syscall_name(number),
    }
}

fn mips64_syscall_name(number: u64) -> &'static str {
    if !(5_000..7_000).contains(&number) {
        return "syscall";
    }
    let (base, abi) = if number >= 6_000 {
        (6_000, Mips64SyscallAbi::N32)
    } else {
        (5_000, Mips64SyscallAbi::N64)
    };
    let number = number.saturating_sub(base);
    match (abi, number) {
        (_, 0) => "read",
        (_, 1) => "write",
        (_, 2) => "open",
        (_, 3) => "close",
        (_, 8) => "lseek",
        (_, 9) => "mmap",
        (_, 10) => "mprotect",
        (_, 11) => "munmap",
        (_, 12) => "brk",
        (_, 15) => "ioctl",
        (_, 16) => "pread64",
        (_, 17) => "pwrite64",
        (_, 18) => "readv",
        (_, 19) => "writev",
        (_, 38) => "getpid",
        (_, 55) => "clone",
        (_, 56) => "fork",
        (_, 57) => "execve",
        (_, 58) => "exit",
        (_, 59) => "wait4",
        (_, 60) => "kill",
        (_, 70) => "fcntl",
        (_, 178) => "gettid",
        (_, 194) => "futex",
        (_, 205) => "exit_group",
        (Mips64SyscallAbi::N64, 247) => "openat",
        (Mips64SyscallAbi::N64, 252) => "newfstatat",
        (Mips64SyscallAbi::N64, 268) => "set_robust_list",
        (Mips64SyscallAbi::N64, 313) => "getrandom",
        (Mips64SyscallAbi::N64, 326) => "statx",
        (Mips64SyscallAbi::N32, 251) => "openat",
        (Mips64SyscallAbi::N32, 256) => "newfstatat",
        (Mips64SyscallAbi::N32, 272) => "set_robust_list",
        (Mips64SyscallAbi::N32, 317) => "getrandom",
        (Mips64SyscallAbi::N32, 330) => "statx",
        (_, 435) => "clone3",
        (_, 436) => "close_range",
        (_, 437) => "openat2",
        (_, 449) => "futex_waitv",
        _ => "syscall",
    }
}

#[derive(Clone, Copy)]
enum Mips64SyscallAbi {
    N32,
    N64,
}

fn common_legacy_syscall_name(number: u64) -> &'static str {
    match number {
        1 => "exit",
        2 => "fork",
        3 => "read",
        4 => "write",
        5 => "open",
        6 => "close",
        11 => "execve",
        19 => "lseek",
        20 => "getpid",
        37 => "kill",
        45 => "brk",
        54 => "ioctl",
        55 => "fcntl",
        90 => "mmap",
        91 => "munmap",
        114 => "wait4",
        120 => "clone",
        125 => "mprotect",
        145 => "readv",
        146 => "writev",
        162 => "nanosleep",
        435 => "clone3",
        436 => "close_range",
        437 => "openat2",
        449 => "futex_waitv",
        _ => "syscall",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_major_gdb_architecture_descriptions() {
        for (description, expected, bits) in [
            ("i386:x86-64", TargetArchitecture::X86_64, 64),
            ("i386:x64-32", TargetArchitecture::X86_64, 64),
            ("i386", TargetArchitecture::X86, 32),
            ("aarch64", TargetArchitecture::AArch64, 64),
            ("armv7", TargetArchitecture::Arm, 32),
            ("riscv:rv64", TargetArchitecture::RiscV64, 64),
            ("riscv:rv32", TargetArchitecture::RiscV32, 32),
            ("powerpc:common64", TargetArchitecture::PowerPc64, 64),
            ("powerpc:common", TargetArchitecture::PowerPc32, 32),
            ("s390:64-bit", TargetArchitecture::S390x, 64),
            ("mips:isa32", TargetArchitecture::Mips32, 32),
            ("mips:isa64", TargetArchitecture::Mips64, 64),
        ] {
            let architecture = TargetArchitecture::from_gdb_description(description);
            assert_eq!(architecture, expected, "{description}");
            assert_eq!(architecture.pointer_bits(), Some(bits));
        }
    }

    #[test]
    fn chooses_stack_and_syscall_registers_per_abi() {
        assert_eq!(
            TargetArchitecture::PowerPc64.stack_pointer(["r1", "r3"]),
            Some("r1")
        );
        assert_eq!(
            TargetArchitecture::S390x.stack_pointer(["r15", "pswa"]),
            Some("r15")
        );
        assert_eq!(
            TargetArchitecture::AArch64.syscall_registers().unwrap().0,
            "x8"
        );
        assert_eq!(
            TargetArchitecture::RiscV32.syscall_registers().unwrap().0,
            "a7"
        );
        assert_eq!(
            TargetArchitecture::X86_64.call_return_registers(),
            &["rax", "rdx"]
        );
        assert_eq!(
            TargetArchitecture::AArch64.call_return_registers(),
            &["x0", "x1"]
        );
        assert_eq!(
            TargetArchitecture::S390x.call_return_registers(),
            &["r2", "r3"]
        );
        assert_eq!(TargetArchitecture::X86.syscall_name(3), "read");
        assert_eq!(TargetArchitecture::AArch64.syscall_name(63), "read");
        assert_eq!(TargetArchitecture::Mips32.syscall_name(4_003), "read");
        assert_eq!(TargetArchitecture::Mips64.syscall_name(5_000), "read");
        assert_eq!(TargetArchitecture::Mips64.syscall_name(5_015), "ioctl");
        assert_eq!(TargetArchitecture::Mips64.syscall_name(6_251), "openat");
        assert_eq!(TargetArchitecture::Arm.syscall_name(322), "openat");
        assert_eq!(TargetArchitecture::PowerPc64.syscall_name(286), "openat");
        assert_eq!(TargetArchitecture::S390x.syscall_name(288), "openat");
        assert_eq!(TargetArchitecture::X86.syscall_name(295), "openat");
        assert_eq!(
            TargetArchitecture::X86_64.normalize_syscall_number(0x4000_0001),
            1
        );
    }

    #[test]
    fn decodes_elf_class_machine_and_byte_order() {
        let mut i386 = [0_u8; 20];
        i386[..4].copy_from_slice(b"\x7fELF");
        i386[4] = 1;
        i386[5] = 1;
        i386[18..20].copy_from_slice(&3_u16.to_le_bytes());
        assert_eq!(
            TargetArchitecture::from_elf_ident(&i386),
            Some((TargetArchitecture::X86, TargetEndian::Little, 32))
        );

        let mut x32 = i386;
        x32[18..20].copy_from_slice(&62_u16.to_le_bytes());
        assert_eq!(
            TargetArchitecture::from_elf_ident(&x32),
            Some((TargetArchitecture::X86_64, TargetEndian::Little, 32))
        );

        let mut aarch64_ilp32 = i386;
        aarch64_ilp32[18..20].copy_from_slice(&183_u16.to_le_bytes());
        assert_eq!(
            TargetArchitecture::from_elf_ident(&aarch64_ilp32),
            Some((TargetArchitecture::AArch64, TargetEndian::Little, 32))
        );

        let mut s390x = [0_u8; 20];
        s390x[..4].copy_from_slice(b"\x7fELF");
        s390x[4] = 2;
        s390x[5] = 2;
        s390x[18..20].copy_from_slice(&22_u16.to_be_bytes());
        assert_eq!(
            TargetArchitecture::from_elf_ident(&s390x),
            Some((TargetArchitecture::S390x, TargetEndian::Big, 64))
        );

        let mut mips_n32 = [0_u8; 40];
        mips_n32[..4].copy_from_slice(b"\x7fELF");
        mips_n32[4] = 1;
        mips_n32[5] = 2;
        mips_n32[18..20].copy_from_slice(&8_u16.to_be_bytes());
        mips_n32[36..40].copy_from_slice(&0x20_u32.to_be_bytes());
        assert_eq!(
            TargetArchitecture::from_elf_ident(&mips_n32),
            Some((TargetArchitecture::Mips64, TargetEndian::Big, 32))
        );
    }

    #[test]
    fn keeps_register_and_pointer_widths_independent() {
        assert_eq!(
            TargetArchitecture::pointer_bits_from_gdb_description("i386:x64-32"),
            Some(32)
        );
        assert_eq!(
            TargetArchitecture::pointer_bits_from_gdb_description("aarch64:ilp32"),
            Some(32)
        );
        assert_eq!(
            TargetArchitecture::pointer_bits_from_gdb_description("mips:isa64:n32"),
            Some(32)
        );
        assert_eq!(
            TargetArchitecture::explicit_pointer_bits_from_gdb_description("aarch64"),
            None
        );
        assert_eq!(
            TargetArchitecture::X86_64.refine_for_pointer_bits(32),
            TargetArchitecture::X86_64
        );
        assert_eq!(
            TargetArchitecture::Mips64.refine_for_pointer_bits(32),
            TargetArchitecture::Mips64
        );
        assert_eq!(
            TargetArchitecture::X86_64.scalar_register_bits("rax", 32),
            64
        );
        assert_eq!(
            TargetArchitecture::Mips64.scalar_register_bits("a0", 32),
            64
        );
        assert_eq!(
            TargetArchitecture::X86_64.scalar_register_bits("eax", 32),
            32
        );
        assert_eq!(
            TargetArchitecture::X86_64.scalar_register_bits("cs", 32),
            16
        );
    }

    #[test]
    fn only_uses_explicit_architecture_endianness() {
        assert_eq!(
            TargetEndian::from_architecture_description("mips:isa32r2"),
            None
        );
        assert_eq!(
            TargetEndian::from_architecture_description("mipsel:isa32r2"),
            Some(TargetEndian::Little)
        );
        assert_eq!(
            TargetEndian::from_architecture_description("powerpc:common64be"),
            Some(TargetEndian::Big)
        );
        assert_eq!(
            TargetArchitecture::Unknown.stack_pointer(["r1", "r15"]),
            None
        );
        assert_eq!(
            TargetArchitecture::S390x.thread_pointer_candidates(),
            ["a0", "acr0"]
        );
    }

    #[test]
    fn recognizes_loongarch_without_treating_zero_as_a_pointer() {
        assert_eq!(
            TargetArchitecture::infer_from_register_names_with_bits(
                ["r0", "r31", "orig_a0", "badv"],
                Some(64),
            ),
            TargetArchitecture::LoongArch64
        );
        assert!(!TargetArchitecture::Mips64.is_address_register("zero"));
        assert!(!TargetArchitecture::Mips64.is_address_register("r0"));
        assert!(!TargetArchitecture::LoongArch64.is_address_register("zero"));
        assert!(!TargetArchitecture::LoongArch64.is_address_register("r0"));
        assert!(TargetArchitecture::AArch64.is_vector_register("z31"));
        assert!(TargetArchitecture::AArch64.is_vector_register("p15"));
        assert!(TargetArchitecture::RiscV64.is_vector_register("vtype"));
        assert!(TargetArchitecture::Mips64.is_vector_register("w31"));
        assert_eq!(TargetArchitecture::S390x.scalar_register_bits("a0", 64), 32);
        assert_eq!(
            TargetArchitecture::infer_from_register_names_with_bits(
                ["x0", "x30", "sp", "pc", "pstate"],
                Some(32),
            ),
            TargetArchitecture::AArch64
        );
        assert_eq!(
            TargetArchitecture::infer_from_register_names(&["badvaddr", "hi", "lo", "r31"]),
            TargetArchitecture::Unknown
        );
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TargetEndian {
    Little,
    Big,
}

impl TargetEndian {
    pub fn from_gdb_description(value: &str) -> Option<Self> {
        let value = value.to_ascii_lowercase();
        if value.contains("little endian") {
            Some(Self::Little)
        } else if value.contains("big endian") {
            Some(Self::Big)
        } else {
            None
        }
    }

    /// Extracts byte order only when GDB's architecture name states it.
    /// Ambiguous names deliberately return `None`; guessing is worse than
    /// disabling target-memory decoding until ELF/GDB supplies an answer.
    pub fn from_architecture_description(value: &str) -> Option<Self> {
        let value = value.to_ascii_lowercase();
        if value.contains("little endian")
            || value.contains("mipsel")
            || value.contains("aarch64:little")
            || value.contains("arm:little")
            || ((value.contains("powerpc") || value.contains("ppc"))
                && value.trim_end().ends_with("le"))
        {
            Some(Self::Little)
        } else if value.contains("big endian")
            || value.contains("mipseb")
            || value.contains("aarch64_be")
            || value.contains("aarch64:big")
            || value.contains("armeb")
            || value.contains("arm:big")
            || ((value.contains("powerpc") || value.contains("ppc"))
                && value.trim_end().ends_with("be"))
        {
            Some(Self::Big)
        } else {
            None
        }
    }

    pub fn decode_u32(self, bytes: [u8; 4]) -> u32 {
        match self {
            Self::Little => u32::from_le_bytes(bytes),
            Self::Big => u32::from_be_bytes(bytes),
        }
    }

    pub fn decode_u64(self, bytes: [u8; 8]) -> u64 {
        match self {
            Self::Little => u64::from_le_bytes(bytes),
            Self::Big => u64::from_be_bytes(bytes),
        }
    }

    pub fn word_bytes(self, value: u64) -> [u8; 8] {
        match self {
            Self::Little => value.to_le_bytes(),
            Self::Big => value.to_be_bytes(),
        }
    }
}
