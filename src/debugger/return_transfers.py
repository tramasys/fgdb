"""Recover register-returned aggregates from verified, typed caller stores.

Compiler ABIs can split the same record differently. Only bytes actually moved
from result registers into a matching caller variable are accepted. Result bytes
are read only from registers. Unknown instructions and incomplete layouts fail closed.
"""

import gdb
import re
from contextlib import nullcontext

MAX_BYTES = 256
MAX_FIELDS = 64
MAX_SYMBOLS = 128
MAX_INSTRUCTIONS = 20
MAX_CODE_BYTES = 128


def _layout(value_type, offset, required, budget):
    plain = value_type.strip_typedefs()
    size = int(plain.sizeof)
    budget[0] -= 1

    if budget[0] < 0 or plain.dynamic or size <= 0 or offset < 0 or offset + size > len(required):
        return False

    if plain.code in (gdb.TYPE_CODE_INT, gdb.TYPE_CODE_ENUM, gdb.TYPE_CODE_BOOL,
                      gdb.TYPE_CODE_CHAR, gdb.TYPE_CODE_PTR, gdb.TYPE_CODE_FLT,
                      gdb.TYPE_CODE_RANGE):
        required[offset:offset + size] = [True] * size
        return True

    if plain.code == gdb.TYPE_CODE_ARRAY:
        element = plain.target()
        first, last = plain.range()
        count = int(last) - int(first) + 1

        if count <= 0 or count > budget[0] or count * int(element.sizeof) != size:
            return False

        return all(_layout(element, offset + index * int(element.sizeof), required, budget)
                   for index in range(count))

    if plain.code != gdb.TYPE_CODE_STRUCT:
        return False

    fields = plain.fields()

    if not fields or len(fields) > budget[0]:
        return False

    for field in fields:
        if field.artificial or field.bitsize or field.bitpos is None or field.bitpos % 8:
            return False

        position = int(field.bitpos) // 8

        if position < 0 or position + int(field.type.sizeof) > size:
            return False

        if not _layout(field.type, offset + position, required, budget):
            return False

    return True


def _destinations(frame, value_type, instructions):
    addresses = set()
    remaining = MAX_SYMBOLS
    plain = value_type.strip_typedefs().unqualified()
    visited = []

    # A new Rust binding enters scope after its initializer. Inspect the bounded
    # assignment suffix's lexical scopes without changing the selected frame/PC.
    for instruction in instructions:
        block = gdb.block_for_pc(instruction["addr"])

        while block is not None and not block.is_global and not block.is_static and block not in visited:
            visited.append(block)
            remaining -= 1

            if remaining < 0:
                return ()

            owner = block

            while owner.function is None and owner.superblock is not None:
                remaining -= 1

                if remaining < 0:
                    return ()

                owner = owner.superblock

            if owner.function != frame.function():
                break

            for symbol in block:
                remaining -= 1

                if remaining < 0:
                    return ()

                if not symbol.is_variable or symbol.is_argument or symbol.is_artificial:
                    continue

                if symbol.type.strip_typedefs().unqualified() != plain:
                    continue

                try:
                    address = frame.read_var(symbol).address

                    if address is not None:
                        addresses.add(int(address))
                except (gdb.error, ValueError):
                    continue

            block = block.superblock

    return addresses


def register_slice(name, architecture):
    name = name.strip().lower()

    if architecture == "i386:x86-64":
        for full, aliases in (("rax", ("eax", "ax", "al", "ah")),
                              ("rbx", ("ebx", "bx", "bl", "bh")),
                              ("rcx", ("ecx", "cx", "cl", "ch")),
                              ("rdx", ("edx", "dx", "dl", "dh")),
                              ("rsi", ("esi", "si", "sil")),
                              ("rdi", ("edi", "di", "dil")),
                              ("rbp", ("ebp", "bp", "bpl")),
                              ("rsp", ("esp", "sp", "spl"))):
            if name == full:
                return full, 0, 8

            if name in aliases:
                index = aliases.index(name)
                return full, int(index == 3), (4, 2, 1, 1)[index]

        match = re.fullmatch(r"(r(?:[89]|1[0-5]))([dwb]?)", name)

        if match:
            return match[1], 0, {"": 8, "d": 4, "w": 2, "b": 1}[match[2]]

        match = re.fullmatch(r"[xyz]mm(\d+)", name)

        if match and int(match[1]) < 32:
            return "xmm" + match[1], 0, {"x": 16, "y": 32, "z": 64}[name[0]]
    elif architecture == "aarch64":
        match = re.fullmatch(r"([xw])(\d+)", name)

        if match and int(match[2]) <= 30:
            return "x" + match[2], 0, 8 if match[1] == "x" else 4

        match = re.fullmatch(r"([bhsdqv])(\d+)(?:\.(\d+)([bhsd]))?", name)

        if match and int(match[2]) < 32:
            size = {"b": 1, "h": 2, "s": 4, "d": 8, "q": 16, "v": 16}[match[1]]

            if match[3]:
                size = int(match[3]) * {"b": 1, "h": 2, "s": 4, "d": 8}[match[4]]

                if match[1] != "v" or size not in (8, 16):
                    return None

            return "v" + match[2], 0, size

        if name in ("sp", "fp"):
            return "sp" if name == "sp" else "x29", 0, 8

    return None


def _operands(text):
    # Commas within an address belong to that operand, not the instruction.
    return re.split(r",\s*(?![^\[]*\])", text)


def _address(operand, bases, architecture):
    if architecture == "i386:x86-64":
        match = re.fullmatch(r"(?:(?:BYTE|WORD|DWORD|QWORD|XMMWORD) PTR )?\[(rbp|rsp)([+-](?:0x[0-9a-f]+|\d+))?\]",
                            operand, re.IGNORECASE)
    else:
        match = re.fullmatch(r"\[(sp|x29|fp)(?:,\s*#(-?(?:0x[0-9a-f]+|\d+)))?\]", operand)

    if match is None or match[1] not in bases:
        return None

    return bases[match[1]] + (int(match[2], 0) if match[2] else 0)


def _memory_size(operand):
    return {"BYTE": 1, "WORD": 2, "DWORD": 4, "QWORD": 8, "XMMWORD": 16}.get(operand.split()[0])


def _transfer(assembly, architecture, registers, memory, bases):
    parts = assembly.strip().split(None, 1)

    if not parts or architecture not in ("i386:x86-64", "aarch64"):
        return False

    mnemonic = parts[0].lower()

    if mnemonic in ("nop", "endbr64"):
        return True

    if len(parts) != 2:
        return False

    operands = _operands(parts[1])

    if architecture == "aarch64" and mnemonic == "stp" and len(operands) == 3:
        first = register_slice(operands[0], architecture)
        second = register_slice(operands[1], architecture)
        address = _address(operands[2], bases, architecture)

        if first is None or second is None or address is None or first[2] != second[2]:
            return False

        for name, offset, size in (first, second):
            for origin in registers.get(name, (None,) * 64)[offset:offset + size]:
                memory[address] = origin
                address += 1

        return True

    if len(operands) != 2:
        return False

    if architecture == "i386:x86-64":
        if mnemonic not in ("mov", "movq", "movd", "movss", "movsd", "movaps", "movups",
                            "movapd", "movupd", "movdqa", "movdqu"):
            return False

        destination, source = operands
    else:
        if mnemonic not in ("mov", "fmov", "str", "stur", "ldr", "ldur"):
            return False

        source, destination = operands if mnemonic in ("str", "stur") else reversed(operands)

    source_register = register_slice(source, architecture)
    destination_register = register_slice(destination, architecture)
    source_address = _address(source, bases, architecture)
    destination_address = _address(destination, bases, architecture)
    sizes = [item[2] for item in (source_register, destination_register) if item is not None]

    if architecture == "i386:x86-64":
        sizes += [size for size in (_memory_size(source), _memory_size(destination)) if size]
        fixed = {"movq": 8, "movd": 4, "movss": 4, "movsd": 8}.get(mnemonic)

        if fixed:
            sizes.append(fixed)

    if not sizes:
        return False

    size = min(sizes)

    if source_register:
        name, offset, _ = source_register
        data = registers.get(name, (None,) * 64)[offset:offset + size]
    elif source_address is not None:
        data = tuple(memory.get(source_address + index) for index in range(size))
    else:
        return False

    if destination_register:
        name, offset, width = destination_register

        if name in bases:
            return False

        data_before = [None] * 64 if name.startswith(("xmm", "v")) else list(registers.get(name, (None,) * 64))
        data_before[offset:offset + size] = data

        # Narrow integer writes zero the upper half. Those bits are not result
        # provenance and cannot stand in for fields the callee never returned.
        if width == 4 and not name.startswith(("xmm", "v")):
            data_before[4:8] = [None] * 4

        registers[name] = tuple(data_before)
    elif destination_address is not None:
        for index, origin in enumerate(data):
            memory[destination_address + index] = origin
    else:
        return False

    return True


def plan(frame, pc, value_type, architecture):
    size = int(value_type.sizeof)

    if size <= 0 or size > MAX_BYTES or architecture not in ("i386:x86-64", "aarch64"):
        return None

    required = [False] * size

    if not _layout(value_type, 0, required, [MAX_FIELDS]) or not any(required):
        return None

    # Count bounds the disassembly allocation as well as the interpreter work.
    with gdb.with_parameter("disassembly-flavor", "intel") if architecture == "i386:x86-64" else nullcontext():
        instructions = frame.architecture().disassemble(pc, count=MAX_INSTRUCTIONS)

    instructions = [instruction for instruction in instructions if instruction["addr"] - pc < MAX_CODE_BYTES]
    addresses = _destinations(frame, value_type, instructions)

    if not addresses:
        return None

    if architecture == "i386:x86-64":
        bases = {name: int(frame.read_register(name)) for name in ("rbp", "rsp")}
        result_registers = ("rax", "rdx", "xmm0", "xmm1")
    else:
        bases = {name: int(frame.read_register(name)) for name in ("sp", "x29")}
        bases["fp"] = bases["x29"]
        result_registers = ("x0", "x1", "v0", "v1", "v2", "v3")

    registers = {name: tuple((name, index) for index in range(16 if name.startswith(("xmm", "v")) else 8))
                 + (None,) * (48 if name.startswith(("xmm", "v")) else 56) for name in result_registers}
    memory = {}
    next_pc = pc

    for instruction in instructions:
        if instruction["addr"] != next_pc or next_pc - pc >= MAX_CODE_BYTES:
            break

        next_pc += instruction["length"]

        if not _transfer(instruction["asm"], architecture, registers, memory, bases):
            break

    matches = []

    for address in addresses:
        origins = tuple(memory.get(address + index) if needed else None for index, needed in enumerate(required))

        if all(origin is not None or not needed for origin, needed in zip(origins, required)):
            matches.append(origins)

    if matches:
        return matches[0] if all(match == matches[0] for match in matches) else None

    return None


def read(frame, origins, value_type):
    registers = {name: frame.read_register(name).bytes for name in {origin[0] for origin in origins if origin}}
    raw = bytes(registers[origin[0]][origin[1]] if origin else 0 for origin in origins)
    return gdb.Value(raw, value_type)
