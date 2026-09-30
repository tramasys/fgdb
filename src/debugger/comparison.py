"""On-demand value snapshots using compiler fields and installed GDB printers."""

import contextlib
import math
import time

import gdb

from .array import NativeArray, read_settings
from .fortran import CHARACTER_LIMIT
from .paths import resolve_member_path

MAX_ROWS = 256
MAX_BYTES = 65536


class Limit(Exception):
    pass


def snapshot(expression, members, name):
    rows = []
    names = set()
    size = 0
    nodes = 0
    complete = True
    deadline = time.monotonic() + 0.5

    def budget():
        if len(rows) >= MAX_ROWS or nodes >= 512 or time.monotonic() >= deadline:
            raise Limit()

    def emit(path, text, known=True):
        nonlocal size, complete
        budget()
        text = str(text)

        if len(path) > 2048 or path in names:
            raise Limit()

        if len(text) > 4096:
            text = text[:4096] + '…'
            known = False

        encoded_path = path.encode('utf-8', 'replace')
        encoded_text = text.encode('utf-8', 'replace')
        size += len(encoded_path) + len(encoded_text)

        if size > MAX_BYTES:
            raise Limit()

        names.add(path)
        rows.append((known, encoded_path.hex(), encoded_text.hex()))
        complete &= known

    def walk(value, path, depth):
        nonlocal nodes
        budget()
        nodes += 1

        if depth > 8:
            emit(path, '<depth limit>', False)
            return

        try:
            type_ = value.type.strip_typedefs()

            if value.is_optimized_out or getattr(value, 'is_unavailable', False):
                emit(path, '<unavailable>', False)
                return

            if type_.code in (gdb.TYPE_CODE_REF, gdb.TYPE_CODE_RVALUE_REF):
                walk(value.referenced_value(), path, depth + 1)
                return

            if type_.code == gdb.TYPE_CODE_PTR:
                # Object graphs are not followed implicitly, including char pointers.
                emit(path + ' [' + str(value.type) + ']', hex(int(value)))
                return

            if type_.code == gdb.TYPE_CODE_ARRAY:
                array = NativeArray(value)
                lengths = [max(0, upper - lower + 1) for lower, upper in array.bounds]
                total = math.prod(lengths)
                emit(path + ' / <length>', str(total))

                for ordinal in range(total):
                    budget()
                    indices = [0] * len(lengths)

                    for axis in range(len(lengths) - 1, -1, -1):
                        ordinal, offset = divmod(ordinal, lengths[axis])
                        indices[axis] = array.bounds[axis][0] + offset

                    walk(array.element(indices), path + ' / [' + ','.join(map(str, indices)) + ']', depth + 1)

                return

            printer = gdb.default_visualizer(value)

            if printer is not None:
                children = getattr(printer, 'children', None)
                child = getattr(printer, 'child', None)
                count = getattr(printer, 'num_children', None)
                has_children = callable(children) or (callable(child) and callable(count))
                summary = printer.to_string() if callable(getattr(printer, 'to_string', None)) else None

                if isinstance(summary, gdb.Value):
                    walk(summary, path + ' / <value>', depth + 1)
                elif summary is not None:
                    if isinstance(summary, gdb.LazyString):
                        text = summary.value().format_string(raw=True, max_elements=128, **CHARACTER_LIMIT)
                        known = 0 <= summary.length <= 128
                    else:
                        text = str(summary)
                        hint = printer.display_hint() if callable(getattr(printer, 'display_hint', None)) else None
                        known = has_children or hint == 'string'

                    known &= not any(marker in text for marker in ('...', '…', '<unreadable', '<unavailable', '{...}'))
                    emit(path + ' / <summary>', text, known)

                if has_children:
                    iterator = iter(children()) if callable(children) else (child(i) for i in range(count()))
                    index = 0

                    while True:
                        budget()

                        try:
                            label, item = next(iterator)
                        except StopIteration:
                            break

                        walk(item, path + ' / [' + str(index) + '] ' + str(label), depth + 1)
                        index += 1

                    emit(path + ' / <captured children>', str(index))
                    return

                if summary is not None:
                    return

                # A summary-only printer can abbreviate its value. Retain the
                # preview, but do not let matching summaries prove equality.
                text = value.format_string(max_elements=256, max_depth=8, **CHARACTER_LIMIT)
                emit(path + ' [' + str(value.type) + ']', text, False)
                return

            if type_.code in (gdb.TYPE_CODE_STRUCT, gdb.TYPE_CODE_UNION):
                fields = type_.fields()

                if not fields:
                    emit(path + ' [' + str(value.type) + ']', '{}')

                for index, field in enumerate(fields):
                    budget()
                    label = field.name or '<anonymous ' + str(index) + '>'

                    if field.is_base_class:
                        label = '<base ' + str(field.type) + '>'

                    try:
                        item = value.cast(field.type) if field.is_base_class else value[field]
                    except Exception as error:
                        emit(path + ' / ' + label, '<unavailable: ' + str(error) + '>', False)
                        continue

                    walk(item, path + ' / ' + label, depth + 1)

                return

            text = value.format_string(raw=True, max_elements=256, max_depth=8, **CHARACTER_LIMIT)
            known = not any(marker in text for marker in ('...', '…', '<unavailable', '<optimized out'))
            emit(path + ' [' + str(value.type) + ']', text, known)
        except Limit:
            raise
        except Exception as error:
            emit(path, '<unavailable: ' + str(error) + '>', False)

    with read_settings(), contextlib.ExitStack() as settings:
        configured = gdb.parameter('max-value-size')
        settings.enter_context(gdb.with_parameter('max-value-size', min(configured, MAX_BYTES) if configured else MAX_BYTES))

        try:
            walk(resolve_member_path(expression, members), name, 0)
        except Limit:
            complete = False

    for known, path, text in rows:
        gdb.write('FGDB_COMPARE 1\t' + str(int(known)) + '\t' + path + '\t' + text + '\n')

    gdb.write('FGDB_COMPARE_END 1\t' + str(len(rows)) + '\t' + str(int(complete)) + '\n')
