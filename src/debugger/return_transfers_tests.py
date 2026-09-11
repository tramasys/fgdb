"""Bounded transfer proofs, including target-independent AArch64 coverage."""

from _fgdb_languages_v1 import return_transfers as transfers
from _fgdb_languages_v1 import returns


def tokens(name):
    return tuple((name, index) for index in range(16)) + (None,) * 48


def transfer(architecture, instructions):
    registers = {name: tokens(name) for name in ("rax", "rdx", "xmm0", "xmm1", "x0", "x1", "v0", "v1", "v2", "v3")}
    memory = {}
    bases = {"rbp": 1024, "rsp": 1000, "sp": 1000, "x29": 1024, "fp": 1024}

    for instruction in instructions:
        assert transfers._transfer(instruction, architecture, registers, memory, bases), instruction

    return registers, memory


_, memory = transfer("i386:x86-64", ["mov edi,eax", "mov esi,edx", "mov DWORD PTR [rbp-0x8],edi", "mov DWORD PTR [rbp-0x4],esi"])
assert [memory[index] for index in range(1016, 1024)] == list(tokens("rax")[:4] + tokens("rdx")[:4])
_, memory = transfer("i386:x86-64", ["movq rax,xmm0", "mov QWORD PTR [rbp-0x10],rax", "movsd QWORD PTR [rbp-0x8],xmm1"])
assert [memory[index] for index in range(1008, 1024)] == list(tokens("xmm0")[:8] + tokens("xmm1")[:8])
_, memory = transfer("i386:x86-64", ["mov QWORD PTR [rsp],rax", "mov rcx,QWORD PTR [rsp]", "mov QWORD PTR [rbp-0x8],rcx"])
assert [memory[index] for index in range(1016, 1024)] == list(tokens("rax")[:8])
registers, _ = transfer("i386:x86-64", ["mov eax,edx", "movss xmm0,xmm1"])
assert registers["rax"][4:8] == (None,) * 4
assert registers["xmm0"][4:16] == (None,) * 12
_, memory = transfer("aarch64", ["stp x0, x1, [sp, #16]"])
assert [memory[index] for index in range(1016, 1032)] == list(tokens("x0")[:8] + tokens("x1")[:8])
_, memory = transfer("aarch64", ["stp s0, s1, [sp]", "stp s2, s3, [sp, #8]"])
assert [memory[index] for index in range(1000, 1016)] == list(tokens("v0")[:4] + tokens("v1")[:4] + tokens("v2")[:4] + tokens("v3")[:4])
registers, memory = transfer("aarch64", ["fmov x0, d0", "str x0, [sp, #-8]", "mov v0.8b, v1.8b"])
assert [memory[index] for index in range(992, 1000)] == list(tokens("v0")[:8])
assert registers["v0"][8:16] == (None,) * 8

for architecture, instruction in [("i386:x86-64", "call 0x100"), ("i386:x86-64", "jmp 0x100"),
                                  ("i386:x86-64", "mov rsp,rax"), ("i386:x86-64", "mov QWORD PTR [rdi],rax"),
                                  ("aarch64", "stp x0, x1, [sp, #-16]!"), ("aarch64", "str x0, [sp], #8"),
                                  ("aarch64", "mov sp,x0"), ("aarch64", "bl 0x100")]:
    assert not transfers._transfer(instruction, architecture, {}, {}, {"sp": 1000, "rsp": 1000}), instruction


class Architecture:
    def __init__(self, instructions):
        self.instructions = instructions

    def disassemble(self, start, **_kwargs):
        return [{"addr": start + index * 4, "length": 4, "asm": instruction}
                for index, instruction in enumerate(self.instructions)]


class Frame:
    def __init__(self, instructions):
        self.instructions = instructions

    def architecture(self):
        return Architecture(self.instructions)

    def read_register(self, name):
        return {"rbp": 1024, "rsp": 1000, "sp": 1000, "x29": 1024}[name]


original = transfers._destinations

try:
    transfers._destinations = lambda *_args: {1000}
    wide = gdb.lookup_type("Wide")
    expected = tokens("rax")[:8] + tokens("rdx")[:8]
    assert transfers.plan(Frame(["mov QWORD PTR [rsp],rax", "mov QWORD PTR [rsp+0x8],rdx"]), 100, wide, "i386:x86-64") == expected
    assert transfers.plan(Frame(["movups XMMWORD PTR [rsp],xmm0", "movsd QWORD PTR [rsp+0x8],xmm1"]), 100, wide, "i386:x86-64") == tokens("xmm0")[:8] + tokens("xmm1")[:8]
    assert transfers.plan(Frame(["movups XMMWORD PTR [rsp],xmm0", "mov QWORD PTR [rsp+0x8],rcx"]), 100, wide, "i386:x86-64") is None
    assert transfers.plan(Frame(["mov QWORD PTR [rsp],rax"]), 100, wide, "i386:x86-64") is None
    assert transfers.plan(Frame(["mov QWORD PTR [rsp],rax", "call 0x200", "mov QWORD PTR [rsp+0x8],rdx"]), 100, wide, "i386:x86-64") is None
    assert transfers.plan(Frame(["mov rdx,rcx", "mov QWORD PTR [rsp],rax", "mov QWORD PTR [rsp+0x8],rdx"]), 100, wide, "i386:x86-64") is None
    assert transfers.plan(Frame(["nop"] * transfers.MAX_INSTRUCTIONS), 100, wide, "i386:x86-64") is None
    assert transfers.plan(Frame(["stp x0, x1, [sp]"]), 100, wide, "aarch64") == tokens("x0")[:8] + tokens("x1")[:8]
    assert transfers.plan(Frame([]), 100, gdb.lookup_type("Choice"), "i386:x86-64") is None
    assert transfers.plan(Frame([]), 100, wide.array(1000), "i386:x86-64") is None
finally:
    transfers._destinations = original

for instruction, registers in [("mov %edx,%ecx", ("rax", "rdx")), ("movsd %xmm1,-8(%rbp)", ("xmm0", "xmm1")),
                               ("stp x0, x1, [sp, #16]", ("x0", "x1")), ("str s3, [sp]", ("v0", "v1", "v2", "v3"))]:
    assert returns._preserved_until(Frame([instruction]), 100, 104, registers), instruction

for instruction, registers in [("mov 8(%rip),%edx", ("rax", "rdx")), ("movsd %xmm0,%xmm1", ("xmm0", "xmm1")),
                               ("mov w1, w2", ("x0", "x1")), ("fmov s3, s4", ("v0", "v1", "v2", "v3")),
                               ("stp x0, x1, [sp], #16", ("x0", "x1"))]:
    assert not returns._preserved_until(Frame([instruction]), 100, 104, registers), instruction

for instruction in ("ldr x2, [x0], #8", "ldr x2, [x0, #8]!"):
    assert not returns._preserved_until(Frame([instruction]), 100, 104, ("x0", "x1")), instruction
