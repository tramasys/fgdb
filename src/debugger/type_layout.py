"""Read compiler-described layout only. Never assume a language or target ABI."""

import contextlib
import gdb


def layout(value, historical):
    lines = []
    remaining = [256, 65536]

    def emit(kind, *values):
        cells = []
        for value in values:
            text = str(value).replace('\\', '\\\\').replace('\t', '\\t').replace('\n', '\\n').replace('\r', '\\r').replace('\0', '\\0')
            cells.append(text if len(text) <= 2048 else text[:2048] + '… [truncated]')
        text = '\t'.join([kind] + cells)
        remaining[0] -= 1
        remaining[1] -= len(text.encode('utf-8', 'replace'))
        if remaining[0] < 0 or remaining[1] < 0:
            raise ValueError('Layout truncated at 256 rows or 64 KiB')
        lines.append(text)

    def size(type_):
        try:
            return int(type_.sizeof) * 8
        except (gdb.error, RuntimeError):
            return None

    def amount(bits):
        if bits is None:
            return '?'
        return str(bits // 8) if bits % 8 == 0 else str(bits) + 'b'

    def fields(type_, offset, prefix, depth):
        type_ = type_.strip_typedefs()
        if type_.code not in (gdb.TYPE_CODE_STRUCT, gdb.TYPE_CODE_UNION):
            return
        if depth >= 8:
            emit('note', prefix + '  [nested layout depth limit]')
            return
        end = 0
        complete = True
        union = type_.code == gdb.TYPE_CODE_UNION
        for field in type_.fields():
            name = prefix + (field.name or '<anonymous>')
            try:
                position = int(field.bitpos)
                width = int(field.bitsize) or size(field.type)
            except (gdb.error, RuntimeError, AttributeError, TypeError):
                emit('field', '?', '?', name, '[static or dynamic offset]')
                complete = False
                continue
            if position < 0 or width is None:
                complete = False
            if complete and not union and position > end:
                emit('field', amount(offset + end), amount(position - end), '<padding>', '')
            emit('field', amount(offset + position) if position >= 0 else '?', amount(width), name, field.type)
            if position >= 0:
                fields(field.type, offset + position, name + '.', depth + 1)
            if width is not None:
                end = max(end, position + width)
        total = size(type_)
        if complete and not union and total is not None and total > end:
            emit('field', amount(offset + end), amount(total - end), '<tail padding>', '')

    type_ = value.type.strip_typedefs()
    total = size(type_)
    address = '-'
    try:
        if not historical and value.address is not None:
            address = hex(int(value.address))
    except (gdb.error, RuntimeError):
        pass
    try:
        alignment = str(type_.alignof)
    except (gdb.error, RuntimeError, AttributeError):
        alignment = 'unavailable'
    emit('type', value.type, amount(total), alignment)
    try:
        fields(type_, 0, '', 0)
        if type_.code in (gdb.TYPE_CODE_PTR, gdb.TYPE_CODE_REF, gdb.TYPE_CODE_RVALUE_REF):
            target = type_.target()
            emit('pointee', target, amount(size(target)))
            emit('note', 'Offsets are from the pointee start. No pointee memory was read.')
            fields(target, 0, '', 0)
    except ValueError as error:
        lines.append('note\t' + str(error))
    text = '\n'.join(lines).encode('utf-8', 'replace').hex()
    gdb.write('FGDB_LAYOUT ' + address + ' ' + str((total or 0) // 8) + ' ' + text + '\n')
