use super::fixture;

const ASSERTIONS: &str = r#"
d = __import__('_fgdb_languages_v1.d', fromlist=['*'])
values = __import__('_fgdb_languages_v1.values', fromlist=['*'])
assert gdb.current_language() == 'd'
assert array.resolve('slice', '').bounds == [(0, 2)]
assert array.resolve('empty', '').bounds == [(0, -1)]
assert not inspect('empty', [(0, 0, 1)])
assert [(r[0], r[1]) for r in inspect('slice', [(2, 3, -1)])] == [('[2]', '10'), ('[1]', '0'), ('[0]', '-10')]
assert array.resolve('values', '').bounds == [(0, 4)]
assert [r[1] for r in inspect('values', [(4, 3, -2)])] == ['20', '0', '-20']
assert [r[1] for r in inspect('large_slice', [(8000, 3, 2)])] == ['24000', '24006', '24012']
assert gdb.default_visualizer(gdb.parse_and_eval('large_slice')).num_children() == 4096
assert array.resolve('lookup', '') is None

for name, text in [('text', 'Hello from D'), ('unicode', 'λ🦀'), ('editable', 'mutable')]:
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
    assert preview == repr('a' * 255) + '... (300 code units)', preview
    assert reads == [256], reads
    preview = gdb.default_visualizer(gdb.parse_and_eval('invalid')).to_string()
    assert preview == repr(bytes([255, 254])) + ' (2 code units)', preview
finally:
    gdb.selected_inferior = original_inferior

wide = gdb.parse_and_eval('wide')
printer = gdb.default_visualizer(wide)
if wide['ptr'].type.target().sizeof == 2:
    assert printer is not None and 'Grüße' in printer.to_string()
else:
    # Some DMD versions describe wchar as four bytes. Do not guess at storage.
    assert printer is not None and 'unsupported' in printer.to_string()
    assert array.resolve('wide', '') is None

before = gdb.parameter('language')
with gdb.with_parameter('language', 'c'):
    assert 'Hello from D' in gdb.default_visualizer(gdb.parse_and_eval('text')).to_string()
assert gdb.parameter('language') == before
values.assign('counter', '43')
values.assign('enabled', 'false')
assert int(gdb.parse_and_eval('counter')) == 43
assert not bool(gdb.parse_and_eval('enabled'))
assert gdb.parameter('language') == before

length = int(gdb.parse_and_eval('slice.length'))
pointer = int(gdb.parse_and_eval('slice.ptr'))
try:
    values.assign('slice.length', '-1')
    assert gdb.default_visualizer(gdb.parse_and_eval('slice')) is None
    values.assign('slice.length', str(length))
    values.assign('slice.ptr', '0')
    assert gdb.default_visualizer(gdb.parse_and_eval('slice')) is None
finally:
    values.assign('slice.length', str(length))
    values.assign('slice.ptr', 'cast(int*)' + hex(pointer))

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
"#;

#[test]
#[ignore = "requires Python-enabled GDB and the GDC variable-viewer fixture"]
fn live_d_gdc_arrays_strings_and_assignments() {
    fixture("d-variable-viewer-target", "d_values_ready", ASSERTIONS);
}

#[test]
#[ignore = "requires Python-enabled GDB and the DMD variable-viewer fixture"]
fn live_d_dmd_arrays_strings_and_assignments() {
    fixture("dmd-variable-viewer-target", "d_values_ready", ASSERTIONS);
}
