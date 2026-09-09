"""Bundled, read-only GDB printers. No inferior calls or compiler-layout offsets."""

import codecs
import contextlib

import gdb

MAX_CHILDREN = 4096
MAX_STRING_BYTES = 256
MAX_ARRAY_BYTES = 256 * 1024
MAX_ARRAY_OUTPUT_BYTES = 256 * 1024


@contextlib.contextmanager
def read_only():
    """Prevent target calls and writes, restoring the caller's GDB settings."""
    with contextlib.ExitStack() as settings:
        for parameter in ("may-call-functions", "may-write-memory", "may-write-registers"):
            settings.enter_context(gdb.with_parameter(parameter, False))
        yield


class Sequence(gdb.ValuePrinter):
    def __init__(self, value, pointer_name, capacity=False):
        self._pointer = value[pointer_name]
        pointer_type = self._pointer.type.strip_typedefs()

        if pointer_type.code != gdb.TYPE_CODE_PTR or value["len"].type.strip_typedefs().code != gdb.TYPE_CODE_INT:
            raise ValueError("invalid sequence field types")

        if capacity and value["cap"].type.strip_typedefs().code != gdb.TYPE_CODE_INT:
            raise ValueError("invalid sequence capacity type")

        self._length = int(value["len"])
        self._capacity = int(value["cap"]) if capacity else None

        size = int(pointer_type.target().sizeof)
        address = int(self._pointer)
        maximum = (1 << (int(pointer_type.sizeof) * 8)) - 1

        if self._length < 0 or not 0 <= address <= maximum or (self._length and not address):
            raise ValueError("invalid sequence length or address")

        if self._length > maximum // max(size, 1) or self._length * size > maximum - address:
            raise ValueError("sequence address range overflows")

        if self._capacity is not None and not self._length <= self._capacity <= maximum // max(size, 1):
            raise ValueError("invalid sequence capacity")

    def to_string(self):
        summary = "length " + str(self._length)

        if self._capacity is not None:
            summary += ", capacity " + str(self._capacity)

        if self._length > MAX_CHILDREN:
            summary += " (inspection limited to " + str(MAX_CHILDREN) + ")"

        return summary

    def display_hint(self):
        return "array"

    def num_children(self):
        return min(self._length, MAX_CHILDREN)

    def fgdb_array_length(self):
        return self._length

    def fgdb_array_element(self, index):
        if not 0 <= index < self._length:
            raise IndexError(index)

        return self._pointer[index]

    def child(self, index):
        if not 0 <= index < self.num_children():
            raise IndexError(index)

        return ("[" + str(index) + "]", self._pointer[index])

    def children(self):
        for index in range(self.num_children()):
            yield self.child(index)


class ByteSequence(Sequence):
    def to_string(self):
        count = min(self._length, MAX_STRING_BYTES)
        truncated = count < self._length

        try:
            data = bytes(gdb.selected_inferior().read_memory(self._pointer, count)) if count else b""
        except (gdb.error, ValueError, OverflowError):
            return "<unreadable bytes> (" + str(self._length) + " bytes)"

        try:
            # A capped read can end inside a UTF-8 character. Decode only the
            # complete prefix, without reading past the inspection budget.
            text, _ = codecs.utf_8_decode(data, "strict", not truncated)
            summary = repr(text)
        except UnicodeDecodeError:
            # Binary slices retain an exact escaped byte representation.
            summary = repr(data)

        if truncated:
            summary += "..."

        return summary + " (" + str(self._length) + " bytes)"


class Text(Sequence):
    def __init__(self, value):
        super().__init__(value, "data")
        self._fields = (("data", value["data"]), ("len", value["len"]))

    def to_string(self):
        if not self._length:
            return ""

        try:
            text = self._pointer.string(encoding="utf-8", errors="replace", length=min(self._length, MAX_STRING_BYTES))
        except (gdb.error, ValueError, OverflowError):
            return "<unreadable string - expand for data and length>"

        if self._length > MAX_STRING_BYTES:
            text += "... [" + str(self._length) + " bytes]"

        return text

    def display_hint(self):
        return "string"

    def num_children(self):
        return len(self._fields)

    def child(self, index):
        if not 0 <= index < len(self._fields):
            raise IndexError(index)

        return self._fields[index]


class ActiveValue(gdb.ValuePrinter):
    def __init__(self, summary, child=None):
        self._summary = summary
        self._active = child

    def to_string(self):
        return self._summary

    def display_hint(self):
        # GDB can keep an old child type when the active variant changes.
        # The frontend uses this marker to retire the owning variable tree.
        return "fgdb-variant"

    def num_children(self):
        return int(self._active is not None)

    def child(self, index):
        if index != 0 or self._active is None:
            raise IndexError(index)

        return self._active

    def children(self):
        if self._active is not None:
            yield self._active
