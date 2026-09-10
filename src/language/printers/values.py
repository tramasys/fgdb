"""Bounded structural queries and explicit, frame-scoped scalar edits."""

import gdb

from .common import read_only


def assign(expression, value):
    if not expression or not value or max(len(expression), len(value)) > 16384:
        raise ValueError("Assignment expression exceeds the editor budget or is empty")

    # Source tabs can differ from the stopped frame. Select assignment syntax
    # in the frame-scoped request, without changing the user's GDB language.
    operator = ":=" if gdb.current_language() == "ada" else "="
    gdb.parse_and_eval(expression + " " + operator + " (" + value + ")")


def _location(expression, members):
    # A missing MI path often represents a synthetic printer child. Do not
    # invent a field expression or report the owning container's address.
    if not expression:
        return "unknown", "", "GDB did not expose a source expression for this value"

    if len(expression) > 16384 or (members is not None and len(members) > 16384):
        return "unknown", "", "Location expression exceeds the inspection budget"

    try:
        if members is None:
            value = gdb.parse_and_eval(expression)
        else:
            from .paths import resolve_member_path
            value = resolve_member_path(expression, members)

        referenced = value.type.strip_typedefs().code in (gdb.TYPE_CODE_REF, gdb.TYPE_CODE_RVALUE_REF)

        if referenced:
            value = value.referenced_value()

        address = value.address

        if address is None:
            # Availability checks can force a lazy value to be fetched. They
            # are unnecessary when GDB already knows its storage address, and
            # would turn locating a large array into a full target-memory read.
            if value.is_optimized_out:
                return "optimized", "", ""

            if getattr(value, "is_unavailable", False):
                return "unavailable", "", ""

            return "no-address", "", ""

        address = int(address)

        if not 0 <= address < (1 << 64):
            return "unknown", "", "Storage address exceeds the supported target width"

        return "reference" if referenced else "memory", hex(address), ""
    except (gdb.error, ValueError, TypeError, OverflowError) as error:
        return "unknown", "", str(error)[:256]


def locations(paths):
    if len(paths) > 16:
        raise ValueError("Location batch exceeds the inspection budget")

    with read_only():
        for index, (expression, members) in enumerate(paths):
            kind, address, detail = _location(expression, members)
            detail = detail.encode("utf-8", "replace").hex()
            gdb.write("FGDB_LOCATION:1\t{}\t{}\t{}\t{}\n".format(index, kind, address, detail))


def member_address(address, type_name, members):
    if not 0 < address < (1 << 64) or len(type_name) > 16384 or len(members) > 16:
        raise ValueError("Invalid structural address query")

    # MI represents Rust references as raw pointers. Resolve the pointee's
    # DWARF type without asking Rust's expression parser to parse an MI path.
    for prefix in ("*mut ", "*const ", "&mut ", "&"):
        if type_name.startswith(prefix):
            type_name = type_name[len(prefix):]
            break
    else:
        type_name = type_name.rstrip().removesuffix("*").rstrip()

    with read_only():
        value_type = gdb.lookup_type(type_name).strip_typedefs()
        value = gdb.Value(address).cast(value_type.pointer()).dereference()

        for member in members:
            if not member or len(member) > 256:
                raise ValueError("Invalid structural member")
            value = value[member]

        if value.address is None:
            raise ValueError("Node has no addressable storage")

        gdb.write("FGDB_MEMBER_ADDRESS:" + hex(int(value.address)) + "\n")
