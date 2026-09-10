"""Bounded native expressions and structural MI paths shared by value queries."""

import re

import gdb

from .common import read_only
from .fortran import Array, is_fortran_array


def resolve_path(expression):
    # Parse MI's member/index grammar before evaluating anything. A C-style
    # path may also parse in a mixed-language frame, with incorrect strides.
    if len(expression) > 4096:
        raise ValueError("Array expression exceeds the inspection limit")

    # Ada MI paths already use native subscripts and access dereferences.
    if gdb.current_language() == "ada":
        with read_only():
            return gdb.parse_and_eval(expression)

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

    with read_only():
        try:
            root, steps = path()

            if position != len(tokens):
                raise ValueError("Not an MI path")
        except ValueError:
            # Native expressions, including a(i,j), remain GDB's responsibility.
            return gdb.parse_and_eval(expression)

        value = gdb.parse_and_eval(root)

        for step in steps:
            if isinstance(step, list):
                if is_fortran_array(value.type.strip_typedefs()):
                    value = Array(value)._element(step)
                else:
                    for index in step:
                        value = value[index]
            else:
                value = value[step]

        return value


def resolve_member_path(expression, member_path):
    if gdb.current_language() == "ada" and member_path:
        from .ada import resolve_member_path as resolve_ada_members
        return resolve_ada_members(expression, member_path)

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
