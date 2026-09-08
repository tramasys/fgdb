import gdb
import time


class UnsupportedLayout(Exception):
    pass


class ReadBudget(Exception):
    pass


class Reader:
    MAX_ROWS = 1024
    MAX_NODES = 4096
    MAX_READ_BYTES = 256 * 1024

    def __init__(self):
        # Cold DWARF expansion can dominate the first lookup in a statically
        # linked Rust binary. Give symbol/type discovery a separate allowance,
        # then tighten the budget before traversing target metadata.
        self.deadline = time.monotonic() + 4.0
        self.rows = []
        self.read_bytes = 0
        self.nodes = 0
        self.fields_cache = {}
        self.output_bytes = 0
        self.objfile = None
        self.block = None

    def check(self, size=0):
        self.read_bytes += size
        if self.read_bytes > self.MAX_READ_BYTES or time.monotonic() > self.deadline:
            raise ReadBudget("Metadata read budget reached")

    def fields(self, value):
        self.check()
        typ = value.type.strip_typedefs()
        if typ.code not in (gdb.TYPE_CODE_STRUCT, gdb.TYPE_CODE_UNION):
            raise UnsupportedLayout("Expected a structure described by allocator debug symbols")
        key = str(typ)
        cached = self.fields_cache.get(key)
        if cached is None or cached[0] != typ:
            fields = frozenset(field.name for field in typ.fields() if field.name)
            self.fields_cache[key] = (typ, fields)
            return fields
        return cached[1]

    def field(self, value, *names):
        fields = self.fields(value)
        for name in names:
            if name in fields:
                return value[name]
        raise UnsupportedLayout("Missing typed field " + " / ".join(names))

    def path(self, value, path):
        for name in path.split("."):
            if value.type.strip_typedefs().code == gdb.TYPE_CODE_PTR:
                value = self.dereference(value)
            value = self.field(value, name)
        return value

    def scalar(self, value):
        # Atomic wrappers are decoded by their actual debug-info fields.
        for _ in range(5):
            typ = value.type.strip_typedefs()
            if typ.code in (gdb.TYPE_CODE_INT, gdb.TYPE_CODE_BOOL,
                            gdb.TYPE_CODE_ENUM, gdb.TYPE_CODE_PTR):
                if typ.sizeof > 8:
                    raise UnsupportedLayout("Scalar is wider than 64 bits")
                self.check(typ.sizeof)
                return int(value)
            value = self.field(value, "repr", "_M_i", "_M_b", "_M_p", "_M_base", "__a_value", "__val")
        raise UnsupportedLayout("Unknown atomic representation")

    def number(self, value, *names):
        return self.scalar(self.field(value, *names))

    def optional_number(self, value, *paths):
        for path in paths:
            try:
                return self.scalar(self.path(value, path))
            except UnsupportedLayout:
                pass
        return None

    def symbol(self, *names):
        # Ignore similarly named locals. Metadata must belong to the same
        # allocator object file, including its separate debug-info object.
        # Try every global spelling before expanding static debug-info tables.
        # A failed unprefixed search must not hide a Rust-prefixed allocator.
        for static_lookup in (False, True):
            for name in names:
                self.check()
                try:
                    if static_lookup:
                        symbol = gdb.lookup_static_symbol(name)
                    else:
                        symbol = gdb.lookup_symbol(name, self.block)[0] if self.block is not None else None
                        if symbol is None:
                            symbol = gdb.lookup_global_symbol(name)
                    # GDB marks some external declarations LOC_UNRESOLVED,
                    # even when their typed value resolves through a minimal
                    # symbol. is_variable alone would reject static Rust links.
                    if symbol is None or symbol.is_function or symbol.symtab is None:
                        continue
                    if self.objfile is not None and symbol.symtab.objfile != self.objfile:
                        continue
                    value = symbol.value()
                    self.objfile = symbol.symtab.objfile
                    if self.block is None:
                        self.block = symbol.symtab.static_block()
                    return value
                except gdb.error:
                    pass
        raise UnsupportedLayout("Allocator internals are unavailable. Load matching allocator debug symbols in Debug data and retry")

    def dereference(self, pointer):
        address = self.scalar(pointer)
        if not address:
            raise UnsupportedLayout("Allocator state is not initialized")
        if pointer.type.strip_typedefs().code != gdb.TYPE_CODE_PTR:
            raise UnsupportedLayout("Expected a typed allocator pointer")
        return pointer.dereference()

    def array(self, value, limit):
        typ = value.type.strip_typedefs()
        if typ.code != gdb.TYPE_CODE_ARRAY:
            raise UnsupportedLayout("Expected an array described by debug symbols")
        low, high = typ.range()
        if low != 0 or high < -1 or high >= limit:
            raise UnsupportedLayout("Allocator array bounds are unsupported")
        return range(high + 1)

    def walk(self, pointer, next_field, limit=256, circular=False):
        seen = set()
        first = self.scalar(pointer)
        while self.scalar(pointer):
            address = self.scalar(pointer)
            if address in seen:
                if circular and address == first:
                    return
                raise UnsupportedLayout("Cycle in allocator metadata")
            if len(seen) >= limit or self.nodes >= self.MAX_NODES:
                raise ReadBudget("Allocator traversal limit reached")
            seen.add(address)
            self.nodes += 1
            self.check()
            value = self.dereference(pointer)
            yield address, value
            pointer = self.field(value, next_field)

    def row(self, kind, location="", metric="", state="", details=""):
        if len(self.rows) >= self.MAX_ROWS:
            raise ReadBudget("Allocator row limit reached")
        self.check()
        if not self.rows:
            self.deadline = min(self.deadline, time.monotonic() + 1.5)
        self.output_bytes += sum(len(str(value).encode("utf-8")) * 2 + 1
                                 for value in (kind, location, metric, state, details)) + 2
        if self.output_bytes > 480 * 1024:
            raise ReadBudget("Allocator output budget reached")
        self.rows.append((kind, location, metric, state, details))

    def counters(self, value, location, counters):
        count = 0
        for label, paths, unit in counters:
            number = self.optional_number(value, *paths)
            if number is not None:
                self.row("Counter", location, str(number) + (" " + unit if unit else ""), label)
                count += 1
        return count


def run_inspector(backend, inspect):
    reader = Reader()
    status = "ok"
    truncated = False
    summary = ""
    try:
        inferior = gdb.selected_inferior()
        threads = inferior.threads()
        if len(threads) > 4096:
            raise ReadBudget("The heap reader cannot verify more than 4096 stopped threads")
        if not threads or any(not thread.is_stopped() for thread in threads):
            raise UnsupportedLayout("Pause every thread in the selected inferior before inspecting allocator metadata")
        # Keep future adapters inside the same no-inferior-calls contract.
        # The user's permission is restored on every exit, including errors.
        with gdb.with_parameter("may-call-functions", False):
            summary = inspect(reader)
    except ReadBudget as error:
        status = "partial"
        truncated = True
        summary = str(error) + ". Showing bounded partial state"
    except (UnsupportedLayout, gdb.error, ValueError, OverflowError) as error:
        status = "partial" if reader.rows else "unavailable"
        summary = str(error)
    except Exception as error:
        status = "partial" if reader.rows else "unavailable"
        summary = "Allocator layout could not be decoded (" + type(error).__name__ + ")"

    def cell(value):
        text = str(value)
        text = "".join(char if char.isprintable() else " " for char in text)
        return text.encode("utf-8")[:2048].decode("utf-8", "ignore").encode("utf-8").hex()

    # Emit one bounded response, only after reading. No partial console updates.
    lines = ["FGDB_HEAP\t1\t" + backend]
    lines.extend("R\t" + "\t".join(cell(value) for value in row) for row in reader.rows)
    lines.append("E\t" + status + "\t" + str(int(truncated)) + "\t"
                 + str(len(reader.rows)) + "\t" + cell(summary))
    gdb.write("\n".join(lines) + "\n")
