"""Bounded Fortran inspection shared by locals and the array viewer."""

import contextlib
import math
import re

import gdb

from .common import MAX_ARRAY_BYTES, MAX_CHILDREN

try:
    gdb.Value(0).format_string(max_characters=128)
    CHARACTER_LIMIT = {"max_characters": 128}
except TypeError:
    # Older GDB versions use max_elements for strings as well.
    CHARACTER_LIMIT = {}


def array_bounds(value_type):
    bounds = []

    while value_type.code == gdb.TYPE_CODE_ARRAY:
        if len(bounds) == 15:
            raise ValueError("Array rank exceeds the inspection limit")

        lower, upper = value_type.range()

        if lower is None or upper is None:
            raise ValueError("Array bounds are unavailable")

        bounds.append((int(lower), int(upper)))
        value_type = value_type.target().strip_typedefs()

    return bounds, value_type


def is_fortran_array(value_type):
    if value_type.code != gdb.TYPE_CODE_ARRAY:
        return False

    _, leaf = array_bounds(value_type)
    name = str(leaf).lower()
    return name.startswith((
        "integer(", "real(", "logical(", "complex(",
        "character(", "character*", "type ",
    ))


class Array(gdb.ValuePrinter):
    def __init__(self, value):
        self._value = value
        self._bounds, self._element_type = array_bounds(value.type.strip_typedefs())

        if not self._bounds:
            raise ValueError("GDB did not expose a native array")

        self._lengths = [max(0, upper - lower + 1) for lower, upper in self._bounds]
        self._total = math.prod(self._lengths)
        self._packed = None

        if self._total and value.address is not None and int(value.address) == 0:
            raise ValueError("Array storage is not allocated or associated")

    def to_string(self):
        shape = ", ".join(str(lo) + ":" + str(hi) for lo, hi in reversed(self._bounds))
        unit = " element" if self._total == 1 else " elements"
        return str(self._total) + unit + ", bounds (" + shape + ")"

    def display_hint(self):
        # Keep native coordinate labels instead of GDB's zero-based array labels.
        return "fgdb-fortran-array"

    def num_children(self):
        return min(self._total, MAX_CHILDREN)

    def _normalize(self):
        if self._packed is not None:
            return self._packed

        # Generic GDB indexing ignores negative strides. Its Fortran slice
        # evaluator handles them, provided the entire array is normalized first.
        # Slicing a prefix before repacking also gives incorrect negative strides.
        value = self._value
        configured_limit = gdb.parameter("max-value-size")
        byte_limit = MAX_ARRAY_BYTES

        if configured_limit and configured_limit > 0:
            byte_limit = min(configured_limit, byte_limit)

        if value.type.dynamic:
            if value.type.sizeof > byte_limit:
                raise ValueError("Dynamic array exceeds the bounded repacking limit")
        elif value.address is not None:
            # A pointer to a static type retains its strides and avoids copying
            # large contiguous arrays. Dynamic descriptor types must use a value.
            value = value.address

        name = "_fgdb_fortran_array"
        previous = gdb.convenience_variable(name)

        with contextlib.ExitStack() as settings:
            settings.enter_context(gdb.with_parameter("language", "fortran"))
            settings.enter_context(gdb.with_parameter("may-call-functions", False))
            settings.enter_context(gdb.with_parameter("fortran repack-array-slices", True))
            settings.enter_context(gdb.with_parameter("max-value-size", byte_limit))

            try:
                gdb.set_convenience_variable(name, value)
                expression = "$" + name + "(" + ",".join(":" for _ in self._bounds) + ")"
                packed = gdb.parse_and_eval(expression)
            finally:
                gdb.set_convenience_variable(name, previous)

        bounds, _ = array_bounds(packed.type.strip_typedefs())

        if [max(0, hi - lo + 1) for lo, hi in bounds] != self._lengths:
            raise ValueError("GDB changed the array shape while repacking")

        self._packed = (packed, bounds)
        return self._packed

    def _coordinates(self, ordinal):
        coordinates = [0] * len(self._bounds)

        for dimension in range(len(coordinates) - 1, -1, -1):
            ordinal, offset = divmod(ordinal, self._lengths[dimension])
            coordinates[dimension] = self._bounds[dimension][0] + offset

        return coordinates

    def _element(self, coordinates):
        child, bounds = self._normalize()

        if len(coordinates) > len(bounds):
            raise ValueError("Too many array indices")

        for index, (lower, upper), (packed_lower, _) in zip(coordinates, self._bounds, bounds):
            if not lower <= index <= upper:
                raise ValueError("Array index is outside its bounds")

            child = child[packed_lower + index - lower]

        return child

    def child(self, index):
        if not 0 <= index < self.num_children():
            raise IndexError(index)

        coordinates = self._coordinates(index)
        label = "(" + ",".join(str(index) for index in reversed(coordinates)) + ")"
        return label, self._element(coordinates)

    def children(self):
        for index in range(self.num_children()):
            yield self.child(index)


def resolve_path(expression):
    # Parse MI's member/index grammar before evaluating anything. A C-style
    # path may also parse in a mixed-language frame, with incorrect strides.
    if len(expression) > 4096:
        raise ValueError("Array expression exceeds the inspection limit")

    tokens = re.findall(r"[A-Za-z_$][A-Za-z_0-9$]*|-?\d+|[^\s]", expression)

    if len(tokens) > 128:
        raise ValueError("Array expression exceeds the inspection limit")

    position = 0

    def path():
        nonlocal position

        if position >= len(tokens):
            raise ValueError("Incomplete path")

        token = tokens[position]
        position += 1

        if token == "(":
            root, steps = path()

            if position >= len(tokens) or tokens[position] != ")":
                raise ValueError("Unclosed path")

            position += 1
        elif re.fullmatch(r"[A-Za-z_$][A-Za-z_0-9$]*", token):
            root, steps = token, []
        else:
            raise ValueError("Not an MI path")

        while position < len(tokens):
            if tokens[position] in (".", "%"):
                position += 1

                if position >= len(tokens) or not re.fullmatch(r"[A-Za-z_][A-Za-z_0-9]*", tokens[position]):
                    raise ValueError("Invalid member")

                steps.append(tokens[position])
                position += 1
            elif tokens[position] == "[":
                if (position + 2 >= len(tokens)
                        or not re.fullmatch(r"-?\d+", tokens[position + 1])
                        or tokens[position + 2] != "]"):
                    raise ValueError("Invalid index")

                if not steps or not isinstance(steps[-1], list):
                    steps.append([])

                steps[-1].append(int(tokens[position + 1]))
                position += 3
            else:
                break

        return root, steps

    with gdb.with_parameter("may-call-functions", False):
        try:
            root, steps = path()

            if position != len(tokens):
                raise ValueError("Not an MI path")
        except ValueError:
            # Native expressions, including a(i,j), remain GDB's responsibility.
            return gdb.parse_and_eval(expression)

        value = gdb.parse_and_eval(root)

        for step in steps:
            value = Array(value)._element(step) if isinstance(step, list) else value[step]

        return value


def preview(value, depth=0):
    value_type = value.type.strip_typedefs()

    if value_type.code in (gdb.TYPE_CODE_STRUCT, gdb.TYPE_CODE_UNION, gdb.TYPE_CODE_ARRAY):
        if depth >= 2:
            return "{...}"

        if value_type.code == gdb.TYPE_CODE_ARRAY:
            array = Array(value)
            entries = [
                preview(array.child(index)[1], depth + 1)
                for index in range(min(array.num_children(), 4))
            ]

            more = array._total > 4
        else:
            fields = value_type.fields()

            entries = [
                field.name[:64] + " = " + preview(value[field], depth + 1)
                for field in fields[:4] if field.name
            ]

            more = len(fields) > 4

        if more:
            entries.append("...")

        return "{" + ", ".join(entries) + "}"

    # Explicit character limits apply before GDB builds a string. The global
    # print settings may be unlimited, even when max_elements is supplied.
    if value_type.code == gdb.TYPE_CODE_PTR:
        return value.format_string(raw=True, format="x")

    if value_type.code == gdb.TYPE_CODE_STRING and value.address is not None:
        character = value_type.target()
        length = value_type.sizeof // max(1, character.sizeof)
        text = value.address.cast(character.pointer()).string(errors="replace", length=min(length, 128))
        return repr(text) + ("..." if length > 128 else "")

    return value.format_string(raw=True, max_elements=4, max_depth=2, repeat_threshold=4, **CHARACTER_LIMIT)


def resolve_member_path(expression, member_path):
    value = resolve_path(expression)

    if not member_path:
        return value

    if len(member_path) > 4096 or member_path.count(".") >= 64:
        raise ValueError("Array member path exceeds the inspection limit")

    for member in member_path.split("."):
        if re.fullmatch(r"[A-Za-z_][A-Za-z_0-9]*", member):
            value = value[member]
        elif re.fullmatch(r"\(-?\d+(?:,-?\d+)*\)", member):
            coordinates = [int(index) for index in member[1:-1].split(",")]
            value = Array(value)._element(list(reversed(coordinates)))
        else:
            raise ValueError("GDB did not expose a supported Fortran member path")

    return value


def lookup(value, value_type):
    if is_fortran_array(value_type):
        return Array(value)

    return None
