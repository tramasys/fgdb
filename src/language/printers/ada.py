"""GNAT member paths retain native bounds and access dereferences."""

import re

import gdb

from .common import read_only


def array_index(value_type, member):
    if re.fullmatch(r"-?\d+", member):
        return int(member)

    index_type = value_type.fields()[0].type.strip_typedefs()

    for _ in range(16):
        if index_type.code != gdb.TYPE_CODE_RANGE:
            break

        index_type = index_type.target().strip_typedefs()

    if index_type.code == gdb.TYPE_CODE_ENUM:
        for field in index_type.fields():
            if field.name == member:
                return field.enumval

    raise ValueError("GDB did not expose a supported Ada array index")


def resolve_member_path(expression, member_path):
    if len(expression) > 4096 or len(member_path) > 4096 or member_path.count(".") >= 64:
        raise ValueError("Ada member path exceeds the inspection limit")

    members = member_path.split(".")

    with read_only():
        value = gdb.parse_and_eval(expression)

        for position, member in enumerate(members):
            value_type = value.type.strip_typedefs()

            if value_type.code == gdb.TYPE_CODE_ARRAY:
                index = array_index(value_type, member)
                lower, upper = value_type.range()

                if not lower <= index <= upper:
                    raise ValueError("Ada array index is outside its bounds")

                value = value[index]
            elif value_type.code == gdb.TYPE_CODE_PTR and member == "all":
                value = value.dereference()
            elif (value_type.code == gdb.TYPE_CODE_PTR
                    and position + 1 < len(members) and members[position + 1] == "all"
                    and re.fullmatch(r"[A-Za-z_][A-Za-z_0-9]*", member)):
                # MI includes the access variable's name before its .all child.
                continue
            elif re.fullmatch(r"[A-Za-z_][A-Za-z_0-9]*", member):
                value = value[member]
            else:
                raise ValueError("GDB did not expose a supported Ada member path")

        return value
