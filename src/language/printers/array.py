"""On-demand native array inspection using DWARF bounds, not ABI descriptors."""

import gdb
from .common import MAX_ARRAY_OUTPUT_BYTES, MAX_CHILDREN
from .fortran import Array, preview, resolve_member_path


def inspect_array(expression, limit, member_path=""):
    if not 0 < limit <= MAX_CHILDREN:
        raise ValueError("Invalid array inspection limit")

    array = Array(resolve_member_path(expression, member_path))
    total = array._total

    if total:
        array._normalize()

    shape = ", ".join(str(lower) + ":" + str(upper) for lower, upper in reversed(array._bounds))
    unit = " element" if total == 1 else " elements"
    suffix = " of " + str(total) + unit + "  Bounds (" + shape + ")  Column-major order"
    limited = "  Limited to keep GDB responsive"
    reserve = "FGDB_ARRAY_SUMMARY:" + (str(limit) + suffix + limited).encode().hex() + "\n"
    budget = MAX_ARRAY_OUTPUT_BYTES - len(reserve)
    rows = []
    element_type = str(array._element_type)[:256]

    for ordinal in range(min(total, limit)):
        coordinates = array._coordinates(ordinal)

        try:
            text = preview(array._element(coordinates))[:512]
        except (gdb.error, ValueError) as error:
            text = "<unavailable: " + str(error)[:256] + ">"

        index = "(" + ",".join(str(index) for index in reversed(coordinates)) + ")"
        fields = [index, text, element_type]
        line = "FGDB_ARRAY_ROW:" + "\t".join(field.encode("utf-8", "replace").hex() for field in fields) + "\n"
        if len(line) > budget:
            break

        budget -= len(line)
        rows.append(line)

    summary = str(len(rows)) + suffix

    if len(rows) < total:
        summary += limited

    # Emit only after building a complete, bounded response.
    gdb.write("".join(rows))
    gdb.write("FGDB_ARRAY_SUMMARY:" + summary.encode().hex() + "\n")
