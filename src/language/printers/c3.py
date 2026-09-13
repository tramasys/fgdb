"""C3 slice descriptors, identified by their emitted names and typed fields."""

import gdb

from .common import ByteSequence, Sequence


def lookup(value, value_type):
    value_type = value_type.unqualified()
    name = value_type.name or value_type.tag or ""

    if not name.endswith("[]"):
        return None

    fields = value_type.fields()

    # D uses length/ptr and Zig uses prefix [] names. Do not apply a slice
    # adapter to ordinary user structs or another language's descriptor.
    if len(fields) != 2 or {field.name for field in fields} != {"ptr", "len"}:
        return None

    pointer = value["ptr"].type.strip_typedefs()

    if pointer.code != gdb.TYPE_CODE_PTR:
        return None

    element = pointer.target().strip_typedefs().unqualified()

    if element.name == "char" and element.code in (gdb.TYPE_CODE_INT, gdb.TYPE_CODE_CHAR) and element.sizeof == 1:
        return ByteSequence(value, "ptr")

    return Sequence(value, "ptr")
