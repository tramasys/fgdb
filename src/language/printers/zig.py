"""Zig DWARF layouts. Expression parsing remains GDB's responsibility."""

import gdb

from .common import ActiveValue, ByteSequence, Sequence


def lookup(value, value_type):
    name = value_type.name or value_type.tag or ""

    if not (name.startswith(("[]", "[:", "?", "error{")) or "." in name):
        return None

    fields = value_type.fields()

    if len(fields) > 64:
        return None

    names = {field.name for field in fields}

    if name.startswith(("[]", "[:")):
        if names == {"ptr", "len"}:
            pointer_type = value["ptr"].type.strip_typedefs()

            if pointer_type.code == gdb.TYPE_CODE_PTR:
                element = pointer_type.target().strip_typedefs().unqualified()

                if element.name == "u8" and element.code == gdb.TYPE_CODE_INT and element.sizeof == 1:
                    return ByteSequence(value, "ptr")

            return Sequence(value, "ptr")

    if name.startswith("?") and names == {"payload", "some"}:
        tag = int(value["some"])

        if tag == 0:
            return ActiveValue("null")

        if tag == 1:
            return ActiveValue("some", ("value", value["payload"]))

    if name.startswith("error{") and "!" in name and names == {"error", "payload"}:
        error = value["error"]

        if int(error) == 0:
            return ActiveValue("ok", ("value", value["payload"]))

        return ActiveValue("error", ("error", error))

    if "." in name and names == {"payload", "tag"}:
        tag = value["tag"]
        tag_type = tag.type.strip_typedefs()
        payload = value["payload"]

        if tag_type.code == gdb.TYPE_CODE_ENUM and payload.type.strip_typedefs().code == gdb.TYPE_CODE_UNION:
            variants = tag_type.fields()

            if len(variants) > 64:
                return None

            active_tag = int(tag)
            active = next((variant.name for variant in variants if variant.enumval == active_tag), None)

            if active is not None:
                return ActiveValue(active, (active, payload[active]))
    return None
