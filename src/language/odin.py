"""Odin DWARF layouts, with objfile-scoped provenance for ambiguous names."""

import gdb

from .common import ActiveValue, Sequence, Text


def is_odin_string(value_type):
    objfile = value_type.objfile

    if objfile is None:
        return False

    if not hasattr(objfile, "_fgdb_odin_string_type"):
        canonical = None
        symbol = objfile.lookup_global_symbol("runtime::default_context")
        producer = getattr(symbol.symtab, "producer", "") if symbol is not None and symbol.symtab is not None else ""

        if (producer or "").lower().startswith("odin"):
            try:
                location = gdb.lookup_type("struct runtime::Source_Code_Location", symbol.symtab.global_block())

                canonical = next(
                    (field.type.strip_typedefs() for field in location.fields() if field.name == "file_path"),
                    None,
                )
            except gdb.error:
                pass

        # The cache belongs to the objfile, so unloading it retires the type too.
        objfile._fgdb_odin_string_type = canonical

    return objfile._fgdb_odin_string_type is not None and value_type == objfile._fgdb_odin_string_type


def lookup(value, value_type):
    name = value_type.name or value_type.tag or ""

    if not (name.startswith(("[]", "[dynamic]", "union{", "union#no_nil{")) or name == "string"):
        return None

    fields = value_type.fields()

    if len(fields) > 64:
        return None

    names = {field.name for field in fields}

    if name.startswith("[]") and names == {"data", "len"}:
        return Sequence(value, "data")

    if name.startswith("[dynamic]") and names == {"data", "len", "cap", "allocator"}:
        return Sequence(value, "data", capacity=True)

    if name == "string" and names == {"data", "len"} and is_odin_string(value_type):
        pointer_type = value["data"].type.strip_typedefs()

        if pointer_type.code != gdb.TYPE_CODE_PTR:
            return None

        element = pointer_type.target().strip_typedefs()

        if element.code in (gdb.TYPE_CODE_INT, gdb.TYPE_CODE_CHAR) and element.sizeof == 1:
            return Text(value)

    if name.startswith(("union{", "union#no_nil{")) and "tag" in names:
        variants = names - {"tag"}
        first = 0 if name.startswith("union#no_nil{") else 1

        if variants != {"v" + str(index) for index in range(first, len(variants) + first)}:
            return None

        tag = int(value["tag"])

        if tag == 0 and first == 1:
            return ActiveValue("nil")

        member = "v" + str(tag)

        if member in variants:
            active = value[member]
            return ActiveValue(str(active.type), ("value", active))

    return None
