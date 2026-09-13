use super::fixture;

#[test]
#[ignore = "requires Python-enabled GDB and the C3 variable-viewer fixture"]
fn live_c3_slices_strings_assignments_and_read_limits() {
    fixture(
        "c3-variable-viewer-target",
        "c3_variable_viewer_target.c3_values_ready",
        r#"
values = __import__('_fgdb_languages_v1.values', fromlist=['*'])
assert gdb.current_language() == 'c'
assert gdb.selected_frame().find_sal().symtab.producer.startswith('c3c')
assert array.resolve('slice', '').bounds == [(0, 2)]
assert array.resolve('empty', '').bounds == [(0, -1)]
assert not inspect('empty', [(0, 0, 1)])
assert [(r[0], r[1]) for r in inspect('slice', [(2, 3, -1)])] == [('[2]', '10'), ('[1]', '0'), ('[0]', '-10')]
assert array.resolve('values', '').bounds == [(0, 4)]
assert [r[1] for r in inspect('values', [(4, 3, -2)])] == ['20', '0', '-20']
assert [r[1] for r in inspect('large_slice', [(8000, 3, 2)])] == ['24000', '24006', '24012']
assert gdb.default_visualizer(gdb.parse_and_eval('large_slice')).num_children() == 4096
assert gdb.default_visualizer(gdb.parse_and_eval('particle')) is None
assert gdb.parse_and_eval('missing') == 0
records = gdb.default_visualizer(gdb.parse_and_eval('records'))
assert int(records.child(1)[1]['id']) == 9
assert array.resolve('records', '').bounds == [(0, 1)]
nested = gdb.default_visualizer(gdb.parse_and_eval('nested'))
assert nested.num_children() == 2
assert int(gdb.default_visualizer(nested.child(0)[1]).child(2)[1]) == 10
assert gdb.default_visualizer(nested.child(1)[1]).num_children() == 0

for name, text in [('text', 'Hello from C3'), ('editable', 'mutable'), ('raw', 'raw ` literal')]:
    printer = gdb.default_visualizer(gdb.parse_and_eval(name))
    assert printer is not None, name
    assert text in printer.to_string(), (name, printer.to_string())
    assert printer.display_hint() == 'array'

inferior = gdb.selected_inferior()
reads = []

class TrackingInferior:
    def read_memory(self, address, length):
        reads.append(length)
        return inferior.read_memory(address, length)

original_inferior = gdb.selected_inferior

try:
    gdb.selected_inferior = TrackingInferior
    preview = gdb.default_visualizer(gdb.parse_and_eval('truncated')).to_string()
    assert preview == repr('a' * 255) + '... (300 bytes)', preview
    assert reads == [256], reads
    preview = gdb.default_visualizer(gdb.parse_and_eval('invalid')).to_string()
    assert preview == repr(bytes([255, 254])) + ' (2 bytes)', preview
finally:
    gdb.selected_inferior = original_inferior

before = gdb.parameter('language')
values.assign('counter', '43')
values.assign('enabled', '0')
assert int(gdb.parse_and_eval('counter')) == 43
assert not bool(gdb.parse_and_eval('enabled'))
assert gdb.parameter('language') == before
assert values._location('slice.ptr[1]', None)[1] == hex(int(gdb.parse_and_eval('&values[2]')))
assert values._location('pointer', None)[1] == hex(int(gdb.parse_and_eval('&pointer')))

length = int(gdb.parse_and_eval('slice.len'))
pointer = int(gdb.parse_and_eval('slice.ptr'))

try:
    for invalid_length in ['-1', str((1 << 63) - 1)]:
        values.assign('slice.len', invalid_length)
        assert gdb.default_visualizer(gdb.parse_and_eval('slice')) is None

    values.assign('slice.len', str(length))
    values.assign('slice.ptr', '0')
    assert gdb.default_visualizer(gdb.parse_and_eval('slice')) is None
finally:
    values.assign('slice.len', str(length))
    values.assign('slice.ptr', '(int*)' + hex(pointer))

original = int(gdb.parse_and_eval('counter'))

try:
    with array.read_settings():
        values.assign('counter', '999')
except gdb.error:
    pass
else:
    raise AssertionError('Read-only inspection allowed an assignment')

assert int(gdb.parse_and_eval('counter')) == original
assert all(gdb.parameter(p) for p in ('may-call-functions', 'may-write-memory', 'may-write-registers'))

printer = next(p for p in gdb.pretty_printers if getattr(p, 'name', '') == 'fgdb-languages')
adapter = next(p for p in printer.subprinters if p.name == 'c3')
adapter.enabled = False

try:
    assert gdb.default_visualizer(gdb.parse_and_eval('slice')) is None
finally:
    adapter.enabled = True

class CustomPrinter(gdb.ValuePrinter):
    def to_string(self):
        return 'custom C3 slice'

def custom(value):
    return CustomPrinter() if value.type.strip_typedefs().name == 'int[]' else None

gdb.pretty_printers.insert(0, custom)

try:
    assert gdb.default_visualizer(gdb.parse_and_eval('slice')).to_string() == 'custom C3 slice'
finally:
    gdb.pretty_printers.remove(custom)
"#,
    );
}
