"""Capture typed returns only at a verified forward instruction boundary."""

import gdb
import re
from collections import namedtuple
from . import return_transfers

Candidate = namedtuple("Candidate", "inferior thread pc sp caller return_type registers origins callee history_count")
_candidate = None
_completed = None


def _function_address(frame):
    symbol = frame.function()
    return int(symbol.value().address) if symbol is not None else None


def _register(return_type, architecture):
    plain = return_type.strip_typedefs()
    size = int(plain.sizeof)

    if plain.code in (gdb.TYPE_CODE_INT, gdb.TYPE_CODE_ENUM, gdb.TYPE_CODE_BOOL,
                      gdb.TYPE_CODE_CHAR, gdb.TYPE_CODE_PTR) and 0 < size <= 8:
        return "rax" if architecture == "i386:x86-64" else "x0"

    if plain.code == gdb.TYPE_CODE_FLT and size in (4, 8):
        return "xmm0" if architecture == "i386:x86-64" else ("s0" if size == 4 else "d0")

    return None


def _prepare():
    thread = gdb.selected_thread()
    inferior = gdb.selected_inferior()

    # No extra remote reads, tracing, breakpoints, or inferior execution.
    if thread is None or not thread.is_stopped() or inferior.connection.type != "native":
        return None

    if str(gdb.parameter("exec-direction")) != "forward":
        return None

    if gdb.current_recording() is not None:
        return None

    frame = gdb.newest_frame()
    architecture = frame.architecture().name()

    if architecture not in ("i386:x86-64", "aarch64"):
        return None

    instruction = frame.architecture().disassemble(frame.pc(), count=1)[0]
    parts = instruction["asm"].strip().split(None, 1)
    mnemonic = parts[0].lower()
    operands = parts[1].strip() if len(parts) > 1 else ""
    symbol = None

    if mnemonic in ("call", "callq", "bl"):
        address = re.match(r"^(0x[0-9a-fA-F]+)(?:\s|$)", operands)

        if address is None:
            return None

        target = int(address.group(1), 16)
        block = gdb.block_for_pc(target)

        while block is not None and block.function is None:
            block = block.superblock

        if block is None:
            return None

        symbol = block.function
        caller = frame
        expected_pc = int(frame.pc()) + instruction["length"]
        expected_sp = int(frame.read_register("sp"))
        expected_function = _function_address(frame)

        if int(symbol.value().address) != target:
            return None
    else:
        caller = frame.older()

        if caller is None or frame.type() != gdb.NORMAL_FRAME:
            return None

        symbol = frame.function()
        expected_pc = int(caller.pc())
        expected_sp = int(caller.read_register("sp"))
        expected_function = _function_address(caller)

    if symbol is None or expected_function is None:
        return None

    function_type = symbol.type.strip_typedefs()

    if function_type.code != gdb.TYPE_CODE_FUNC:
        return None

    return_type = function_type.target()
    register = _register(return_type, architecture)
    origins = None

    if register is None:
        if return_type.strip_typedefs().code not in (gdb.TYPE_CODE_STRUCT, gdb.TYPE_CODE_ARRAY):
            return None

        origins = return_transfers.plan(caller, expected_pc, return_type, architecture)

        if origins is None:
            return None

    registers = (register,) if register is not None else tuple({origin[0] for origin in origins if origin})

    if "little endian" not in gdb.execute("show endian", to_string=True).lower():
        return None

    return Candidate(inferior.num, thread.global_num, expected_pc, expected_sp, expected_function,
                     return_type, registers, origins, int(symbol.value().address),
                     getattr(gdb, "history_count", lambda: None)())


def _at_return(candidate, frame):
    thread = gdb.selected_thread()
    x86 = any(name.startswith(("r", "xmm")) for name in candidate.registers)

    if thread is None or thread.global_num != candidate.thread or gdb.selected_inferior().num != candidate.inferior:
        return False

    if _function_address(frame) != candidate.callee or int(frame.read_register("sp")) + (8 if x86 else 0) != candidate.sp:
        return False

    instruction = frame.architecture().disassemble(frame.pc(), count=1)[0]
    return instruction["asm"].strip() in ("ret", "retq")


def _preserved_until(frame, start, end, register):
    if start == end:
        return True

    # Source stepping can pass the caller's assignment before stopping. Accept
    # only a short straight-line suffix that cannot overwrite the return bits.
    if end < start or end - start > 64:
        return False

    registers = (register,) if isinstance(register, str) else register
    x86 = any(name.startswith(("r", "xmm")) for name in registers)
    architecture = "i386:x86-64" if x86 else "aarch64"
    canonical = {return_transfers.register_slice(name, architecture)[0] for name in registers}
    instructions = frame.architecture().disassemble(start, end_pc=end - 1)

    if len(instructions) > 12:
        return False

    next_pc = start

    for instruction in instructions:
        if instruction["addr"] != next_pc:
            return False

        next_pc += instruction["length"]
        assembly = instruction["asm"].strip().lower()

        if x86:
            assembly = assembly.split("#", 1)[0].strip()

        parts = assembly.split(None, 1)

        if not parts:
            return False

        if parts[0] in ("nop", "nopl", "nopw", "endbr64"):
            continue

        if len(parts) != 2 or parts[0] not in ("mov", "movb", "movw", "movl", "movq",
                                             "movabs", "movss", "movsd", "movd", "movaps", "movups",
                                             "movapd", "movupd", "movdqa", "movdqu",
                                             "fmov", "ldr", "ldur", "str", "stur", "stp"):
            return False

        operands = parts[1]

        if parts[0] in ("str", "stur", "stp"):
            if "!" in operands or "]" not in operands or not operands.endswith("]"):
                return False

            continue

        if parts[0] in ("ldr", "ldur") and ("!" in operands or not operands.endswith("]")):
            return False

        if "," not in operands:
            return False

        destination = operands.rsplit(",", 1)[1] if "%" in operands else operands.split(",", 1)[0]
        destination = destination.strip().lstrip("%").split(".", 1)[0]

        destination_register = return_transfers.register_slice(destination, architecture)

        if destination_register is not None and destination_register[0] in canonical:
            return False

    return next_pc == end


def _stopped(event):
    global _candidate, _completed
    candidate = _candidate
    _candidate = None
    _completed = None

    details = getattr(event, "details", {})
    reason = details.get("reason")

    if candidate is None or reason not in ("end-stepping-range", "function-finished"):
        return

    try:
        replaces = None

        if reason == "function-finished":
            finish_value = details.get("finish-value")
            reference = getattr(gdb, "history_count", lambda: None)()

            if candidate.origins is None or finish_value is None or candidate.history_count is None or reference != candidate.history_count + 1:
                return

            native_value = gdb.history(reference)

            if native_value.type != finish_value.type or native_value.bytes != finish_value.bytes:
                return

            replaces = "$" + str(reference)

        thread = gdb.selected_thread()

        if thread is None or thread.global_num != candidate.thread or gdb.selected_inferior().num != candidate.inferior:
            return

        if str(gdb.parameter("exec-direction")) != "forward" or gdb.current_recording() is not None:
            return

        frame = gdb.newest_frame()

        if _function_address(frame) != candidate.caller:
            # Some compilers lose the caller's unwind description after LEAVE.
            # Keep the verified plan across that one final RET instruction.
            if _at_return(candidate, frame):
                _candidate = candidate

            return

        if int(frame.read_register("sp")) != candidate.sp or not _preserved_until(frame, candidate.pc, int(frame.pc()), candidate.registers):
            return

        if candidate.origins is None:
            raw = frame.read_register(candidate.registers[0]).bytes
            value = gdb.Value(raw[:int(candidate.return_type.sizeof)], candidate.return_type)
        else:
            value = return_transfers.read(frame, candidate.origins, candidate.return_type)

        if replaces is not None:
            native = gdb.history(int(replaces[1:])).bytes
            verified = value.bytes

            if len(native) == len(verified) and all(native[index] == verified[index] for index, origin in enumerate(candidate.origins) if origin):
                return

        reference = gdb.add_history(value)
        _completed = {"thread": str(candidate.thread), "inferior": "i" + str(candidate.inferior),
                      "history": "$" + str(reference), "value": value.format_string(max_elements=128),
                      "replaces": replaces}
    except (gdb.error, RuntimeError, ValueError, TypeError, AttributeError, KeyError, IndexError):
        _completed = None


def snapshot(enabled=True, discard=False):
    global _candidate, _completed
    result = _completed if enabled and not discard else None
    previous = _candidate if not discard else None
    _completed = None
    _candidate = None

    if enabled:
        try:
            _candidate = _prepare()

            if (_candidate is None and previous is not None
                    and str(gdb.parameter("exec-direction")) == "forward" and gdb.current_recording() is None
                    and _at_return(previous, gdb.newest_frame())):
                _candidate = previous
        except (gdb.error, RuntimeError, ValueError, TypeError, AttributeError, KeyError, IndexError):
            pass

    if result is None:
        print("FGDB_RETURNS none")
    else:
        fields = [result["thread"], result["inferior"], result["history"], result["value"].encode("utf-8").hex()]

        if result["replaces"] is not None:
            fields.append(result["replaces"])

        print("FGDB_RETURNS " + " ".join(fields))


def _reset(_event):
    global _candidate, _completed
    _candidate = None
    _completed = None


def register():
    gdb.events.stop.connect(_stopped)

    for name in ("exited", "clear_objfiles", "new_objfile", "free_objfile", "register_changed", "memory_changed"):
        event = getattr(gdb.events, name, None)

        if event is not None:
            event.connect(_reset)
