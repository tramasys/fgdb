//! Stable layout identities, never derived from translated headings or indexes.

macro_rules! tables {
    ($($variant:ident => $key:literal),+ $(,)?) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub(in crate::ui) enum TableId { $($variant),+ }

        impl TableId {
            pub(super) fn key(self) -> &'static str {
                match self { $(Self::$variant => $key),+ }
            }

            pub(super) fn parse(key: &str) -> Option<Self> {
                match key { $($key => Some(Self::$variant),)+ _ => None }
            }
        }
    };
}

tables! {
    Locals => "locals",
    Watches => "watches",
    Instructions => "instructions",
    GeneralRegisters => "registers-general",
    BaseRegisters => "registers-bases",
    FlagRegisters => "registers-flags",
    SegmentRegisters => "registers-segments",
    FloatRegisters => "registers-float",
    OtherRegisters => "registers-other",
    Stack => "stack",
    MemoryMappings => "memory-mappings",
    MemoryBytes => "memory-bytes",
    MemorySearch => "memory-search",
    ArrayViewer => "array-viewer",
    LinkedListViewer => "linked-list-viewer",
    ThreadFrames => "thread-frames",
    ThreadRegisters => "thread-registers",
    AttachProcesses => "attach-processes",
    Syscalls => "syscalls",
    TlsModules => "tls-modules",
    TlsSymbols => "tls-symbols",
    MappingChanges => "mapping-changes",
    KernelThreads => "kernel-threads",
    KernelSignals => "kernel-signals",
    ProcessTree => "process-tree",
    PrivateCategories => "private-categories",
    PrivateMappings => "private-mappings",
    KernelMappings => "kernel-mappings",
    FileDescriptors => "file-descriptors",
    Limits => "limits",
    Arguments => "arguments",
    Environment => "environment",
    Auxv => "auxv",
    CallArguments => "call-arguments",
    CallAbi => "call-abi",
    AllocatorMappings => "allocator-mappings",
    Heap => "heap",
    LockWaits => "lock-waits",
    LockDependencies => "lock-dependencies",
    CoreNotes => "core-notes",
    CoreMappings => "core-mappings",
}
