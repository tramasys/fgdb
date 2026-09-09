"""Read-only, bounded array pages in native coordinate order.

Printers may optionally provide fgdb_array_length() and
fgdb_array_element(index) for bounded random access without changing their
ordinary GDB children limits. Standard num_children()/child(index) printers
work too. Iterator-only printers have an explicit, bounded seek budget.
"""

import contextlib
import math
import operator

import gdb

from .common import MAX_ARRAY_BYTES, MAX_ARRAY_OUTPUT_BYTES, read_only
from .fortran import Array, CHARACTER_LIMIT, array_bounds, is_fortran_array, preview, resolve_member_path

BATCH_LIMIT = 64
SEQUENTIAL_LIMIT = 4096
INT_MIN = -(1 << 63)
INT_MAX = (1 << 63) - 1


class NativeArray:
    def __init__(self, value, expression=None):
        self.value = value
        self.bounds, self.element_type = array_bounds(value.type.strip_typedefs())
        self.fortran = is_fortran_array(value.type.strip_typedefs())
        self.order = "column" if self.fortran else "row"
        self.sequential = False
        self.length_known = True
        self.array = Array(value, expression) if self.fortran else None

        if self.fortran:
            self.bounds.reverse()

    def element(self, coordinates):
        if self.fortran:
            return self.array._element(list(reversed(coordinates)))

        value = self.value

        for index in coordinates:
            value = value[index]

        return value


class PrinterArray:
    def __init__(self, printer):
        self.printer = printer
        self.order = "sequence"
        self.extended = callable(getattr(printer, "fgdb_array_element", None))
        length = getattr(printer, "fgdb_array_length" if self.extended else "num_children", None)
        total = length() if callable(length) else None
        self.length_known = total is not None
        total = operator.index(total) if self.length_known else SEQUENTIAL_LIMIT

        if not 0 <= total <= INT_MAX:
            raise ValueError("Invalid printer array length")

        self.bounds = [(0, total - 1)]
        self.sequential = not self.extended and not callable(getattr(printer, "child", None))
        self.iterator = None
        self.position = -1

    def element(self, coordinates):
        try:
            return self._element(coordinates)
        except (StopIteration, IndexError) as error:
            if self.length_known:
                raise ValueError("Printer ended before its declared array length") from error
            raise StopIteration from error

    def _element(self, coordinates):
        index = coordinates[0]

        if self.extended:
            return self.printer.fgdb_array_element(index)

        if not self.sequential:
            return self.printer.child(index)[1]

        if not 0 <= index < SEQUENTIAL_LIMIT:
            raise ValueError("Sequential printer seek exceeds 4096 elements")

        if self.iterator is None or index <= self.position:
            self.iterator = iter(self.printer.children())
            self.position = -1

        while self.position < index:
            _, value = next(self.iterator)
            self.position += 1

        return value


def resolve(expression, member_path):
    value = resolve_member_path(expression, member_path)

    while value.type.strip_typedefs().code in (gdb.TYPE_CODE_REF, gdb.TYPE_CODE_RVALUE_REF):
        value = value.referenced_value()

    if value.type.strip_typedefs().code == gdb.TYPE_CODE_ARRAY:
        native_expression = expression if not member_path and not any(c in expression for c in "[].") else None
        return NativeArray(value, native_expression)

    printer = gdb.default_visualizer(value)

    if printer is not None and callable(getattr(printer, "display_hint", None)) and printer.display_hint() == "array":
        return PrinterArray(printer)

    # std::array implementations expose a native array field even without a
    # printer. Do not infer bounds from the size or layout of unrelated structs.
    value_type = value.type.strip_typedefs()

    if value_type.code == gdb.TYPE_CODE_STRUCT and str(value_type).startswith("std::array<"):
        for field in value_type.fields():
            if field.name in ("_M_elems", "__elems_", "__elems") and field.type.strip_typedefs().code == gdb.TYPE_CODE_ARRAY:
                return NativeArray(value[field])

    return None


def metadata(array):
    if not 1 <= len(array.bounds) <= 15:
        raise ValueError("Array rank exceeds the inspection limit")

    if any(not INT_MIN <= bound <= INT_MAX for bounds in array.bounds for bound in bounds):
        raise ValueError("Array bounds exceed the supported integer range")

    bounds = ",".join(str(lo) + ":" + str(hi) for lo, hi in array.bounds)
    return "FGDB_ARRAY_META:" + "\t".join((
        array.order, bounds, str(int(array.sequential)), str(int(array.length_known)),
    )) + "\n"


@contextlib.contextmanager
def read_settings():
    configured = gdb.parameter("max-value-size")
    byte_limit = min(configured, MAX_ARRAY_BYTES) if configured and configured > 0 else MAX_ARRAY_BYTES

    with read_only(), gdb.with_parameter("max-value-size", byte_limit):
        yield


def describe_array(expression, member_path=""):
    with read_settings():
        array = resolve(expression, member_path)
        gdb.write(metadata(array) if array is not None else "FGDB_ARRAY_UNSUPPORTED\n")


def validate(array, axes, offset, count):
    if not 1 <= count <= BATCH_LIMIT or not 0 <= offset <= (1 << 64) - 1:
        raise ValueError("Invalid array page")

    if len(axes) != len(array.bounds):
        raise ValueError("Slice dimensions do not match the array")

    for (start, length, stride), (lower, upper) in zip(axes, array.bounds):
        if not INT_MIN <= start <= INT_MAX or not INT_MIN <= stride <= INT_MAX or not stride:
            raise ValueError("Invalid slice start or stride")

        if not 0 <= length <= (1 << 64) - 1:
            raise ValueError("Invalid slice count")

        if not length and start != lower and not lower <= start <= upper:
            raise ValueError("Slice start is outside the dimension bounds")

        last = start + (length - 1) * stride if length else start

        if length and not (lower <= start <= upper and lower <= last <= upper):
            raise ValueError("Slice extends outside the dimension bounds")

    total = math.prod(length for _, length, _ in axes)

    if total > (1 << 64) - 1 or offset > total or (total and offset == total):
        raise ValueError("Slice size or page offset is too large")

    return total


def coordinates(array, axes, ordinal):
    result = [0] * len(axes)
    dimensions = range(len(axes)) if array.order == "column" else range(len(axes) - 1, -1, -1)

    for dimension in dimensions:
        start, count, stride = axes[dimension]
        ordinal, index = divmod(ordinal, count)
        result[dimension] = start + index * stride

    return result


def inspect_array(expression, member_path, axes, offset, count):
    with read_settings():
        array = resolve(expression, member_path)

        if array is None:
            raise ValueError("Array representation is no longer available")

        total = validate(array, axes, offset, count)
        if total and array.order == "column":
            array.array._normalize()

        header = metadata(array)
        budget = MAX_ARRAY_OUTPUT_BYTES - len(header) - 128
        rows = []
        ended = False
        positions = [coordinates(array, axes, index) for index in range(offset, min(total, offset + count))]

        if array.sequential:
            if any(not 0 <= coordinate[0] < SEQUENTIAL_LIMIT for coordinate in positions):
                raise ValueError("Sequential printer seek exceeds 4096 elements. Use a random-access printer for larger offsets")

            # Traverse once even for a reverse slice, then restore display order.
            values = {}

            for coordinate in sorted(positions):
                try:
                    values[coordinate[0]] = array.element(coordinate)
                except StopIteration:
                    break

            if values and positions[0][0] not in values:
                raise ValueError("Sequence ended before the requested start index. Choose a lower start index")

        for coordinate in positions:
            try:
                if array.sequential:
                    if coordinate[0] not in values:
                        ended = True
                        break

                    value = values[coordinate[0]]
                else:
                    value = array.element(coordinate)

                if isinstance(value, gdb.Value):
                    if array.order == "column":
                        text = preview(value)[:512]
                    else:
                        text = value.format_string(max_elements=4, max_depth=2, repeat_threshold=4, **CHARACTER_LIMIT)[:512]
                else:
                    text = str(value)[:512]
                value_type = str(value.type)[:256] if isinstance(value, gdb.Value) else type(value).__name__
            except StopIteration:
                ended = True
                break
            except IndexError as error:
                raise ValueError("Array storage no longer matches its declared bounds") from error
            except ValueError:
                raise
            except (gdb.error, OverflowError) as error:
                text = "<unavailable: " + str(error)[:256] + ">"
                value_type = "<unknown>"

            index = "(" + ",".join(map(str, coordinate)) + ")" if array.order == "column" else "".join("[" + str(i) + "]" for i in coordinate)
            fields = (index, text, value_type)
            line = "FGDB_ARRAY_ROW:" + "\t".join(field.encode("utf-8", "replace").hex() for field in fields) + "\n"

            if len(line) > budget:
                break

            budget -= len(line)
            rows.append(line)

        gdb.write(header + "".join(rows) + "FGDB_ARRAY_END:" + str(int(ended)) + "\n")


def request_array(operation, *args):
    try:
        if operation == "describe":
            describe_array(*args)
        elif operation == "page":
            inspect_array(*args)
        else:
            raise ValueError("Unsupported array operation")
    except (gdb.error, ValueError, OverflowError, TypeError) as error:
        message = str(error)[:512].encode("utf-8", "replace").hex()
        gdb.write("FGDB_ARRAY_ERROR:" + message + "\n")
