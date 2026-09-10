"""D slice descriptors, using DWARF fields and bounded target-endian text reads."""

import codecs

import gdb

from .common import MAX_STRING_BYTES, Sequence

STRING_TYPES = {"string": "char", "wstring": "wchar", "dstring": "dchar"}
CHARACTER_WIDTHS = {"char": 1, "wchar": 2, "dchar": 4}


def is_string_alias(value_type, name):
    objfile = value_type.objfile

    if objfile is None:
        return False

    if not hasattr(objfile, "_fgdb_d_string_types"):
        aliases = {}
        main = objfile.lookup_global_symbol("D main")

        if main is not None and main.symtab is not None:
            block = main.symtab.global_block()

            for alias in STRING_TYPES:
                try:
                    aliases[alias] = gdb.lookup_type(alias, block).strip_typedefs().unqualified()
                except gdb.error:
                    pass

        # Ambiguous string names need D type identity. Unloading the objfile
        # also retires the cache, including any missing aliases.
        objfile._fgdb_d_string_types = aliases

    return objfile._fgdb_d_string_types.get(name) == value_type


class UnsupportedText(gdb.ValuePrinter):
    def __init__(self, element):
        self._name = element.name

    def to_string(self):
        return "<unsupported D " + self._name + " debug width>"


class Text(Sequence):
    def __init__(self, value, element):
        super().__init__(value, "ptr", length_name="length")
        self._width = CHARACTER_WIDTHS[element.name]

        if element.sizeof != self._width:
            raise ValueError("D character width disagrees with the debug information")

        if self._width == 1:
            self._encoding = "utf-8"
        else:
            # A debugger-created scalar has target byte order without reading
            # inferior memory or issuing a console command for every string.
            little = bytes(gdb.Value(1).cast(element).bytes)[0] == 1
            self._encoding = "utf-" + str(self._width * 8) + ("-le" if little else "-be")

    def to_string(self):
        count = min(self._length, MAX_STRING_BYTES // self._width)
        truncated = count < self._length

        try:
            data = bytes(gdb.selected_inferior().read_memory(self._pointer, count * self._width)) if count else b""
        except (gdb.error, ValueError, OverflowError):
            return "<unreadable string> (" + str(self._length) + " code units)"

        decoder = codecs.getincrementaldecoder(self._encoding)(errors="strict")

        try:
            text = repr(decoder.decode(data, final=not truncated))
        except UnicodeDecodeError:
            text = repr(data)

        suffix = "..." if truncated else ""
        return text + suffix + " (" + str(self._length) + " code units)"


def lookup(value, value_type):
    value_type = value_type.unqualified()
    name = value_type.name or value_type.tag or ""

    if name in STRING_TYPES:
        if not is_string_alias(value_type, name):
            return None
    elif not name.endswith("[]"):
        return None

    fields = value_type.fields()

    if len(fields) != 2 or {field.name for field in fields} != {"length", "ptr"}:
        return None

    pointer = value["ptr"].type.strip_typedefs()

    if pointer.code != gdb.TYPE_CODE_PTR:
        return None

    element = pointer.target().strip_typedefs().unqualified()

    if element.name in CHARACTER_WIDTHS and element.code in (gdb.TYPE_CODE_INT, gdb.TYPE_CODE_CHAR):
        if element.sizeof != CHARACTER_WIDTHS[element.name]:
            return UnsupportedText(element)

        return Text(value, element)

    if name in STRING_TYPES:
        return None

    return Sequence(value, "ptr", length_name="length")
