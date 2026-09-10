"""GDB-native, individually disableable language adapters."""

import gdb
import gdb.printing

from . import d, fortran, odin, zig


class Adapter(gdb.printing.SubPrettyPrinter):
    def __init__(self, name, codes, lookup):
        super().__init__(name)
        self.codes = codes
        self.lookup = lookup


class LanguagePrinter(gdb.printing.PrettyPrinter):
    def __init__(self):
        aggregates = (gdb.TYPE_CODE_STRUCT, gdb.TYPE_CODE_UNION)
        adapters = (
            Adapter("d", (gdb.TYPE_CODE_STRUCT,), d.lookup),
            Adapter("fortran", (gdb.TYPE_CODE_ARRAY,), fortran.lookup),
            Adapter("zig", aggregates, zig.lookup),
            Adapter("odin", aggregates, odin.lookup),
        )
        super().__init__("fgdb-languages", adapters)
        self._by_code = {}

        for adapter in adapters:
            for code in adapter.codes:
                self._by_code.setdefault(code, []).append(adapter)

    def __call__(self, value):
        try:
            value_type = value.type.strip_typedefs()
        except gdb.error:
            return None

        # Scalars and unrelated kinds do not allocate a name or inspect fields.
        for adapter in self._by_code.get(value_type.code, ()):
            if not adapter.enabled:
                continue

            try:
                printer = adapter.lookup(value, value_type)
            except (gdb.error, ValueError, OverflowError, TypeError):
                # Unavailable storage and unsupported layouts retain raw values.
                continue

            if printer is not None:
                return printer

        return None


def register():
    printer = LanguagePrinter()
    gdb.printing.register_pretty_printer(None, printer)
    # This is a cross-language fallback. Objfile, progspace and existing global
    # printers retain priority, including scripts supplied by the user.
    gdb.pretty_printers.remove(printer)
    gdb.pretty_printers.append(printer)
    return printer
