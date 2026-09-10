use super::fixture;

#[test]
#[ignore = "requires Python-enabled GDB and the GNAT variable-viewer fixture"]
fn live_ada_arrays_preserve_bounds_paths_and_assignment_syntax() {
    fixture(
        "ada-variable-viewer-target",
        "ada_values_ready",
        r#"
values = __import__('_fgdb_languages_v1.values', fromlist=['*'])
assert gdb.current_language() == 'ada'
assert array.resolve('values', '').bounds == [(-2, 2)]
assert array.resolve('grid', '').bounds == [(-1, 1), (4, 5)]
assert array.resolve('(grid)(-1)', '').bounds == [(4, 5)]
assert array.resolve('sample.position', '').bounds == [(1, 3)]
assert array.resolve('dynamic', '').bounds == [(4, 8)]
assert array.resolve('empty', '').bounds == [(1, 0)]
assert not inspect('empty', [(1, 0, 1)])
assert [(r[0], r[1]) for r in inspect('values', [(2, 3, -2)])] == [('(2)', '20'), ('(0)', '0'), ('(-2)', '-20')]
rows = inspect('grid', [(1, 3, -1), (5, 2, -1)])
assert [(r[0], r[1]) for r in rows] == [('(1,5)', '6'), ('(1,4)', '5'), ('(0,5)', '4'), ('(0,4)', '3'), ('(-1,5)', '2'), ('(-1,4)', '1')], rows
assert [r[1] for r in inspect('large', [(8000, 3, 2)])] == ['24000', '24006', '24012']
assert values._location('((grid)(-1))(4)', None)[1] == hex(int(gdb.parse_and_eval('grid(-1,4)').address))
assert values._location('pointer', None)[1] != values._location('pointer.all', None)[1]
assert values._location('values', '-2')[1] == hex(int(gdb.parse_and_eval('values(-2)').address))
assert values._location('grid', '-1.4')[1] == hex(int(gdb.parse_and_eval('grid(-1,4)').address))
assert values._location('sample', 'position.2')[1] == hex(int(gdb.parse_and_eval('sample.position(2)').address))
assert values._location('pointer', 'pointer.all')[1] == hex(int(gdb.parse_and_eval('counter').address))
assert values._location('values', '-3')[0] == 'unknown'
assert values._location('counts', 'ready')[1] == hex(int(gdb.parse_and_eval('counts(ready)').address))
assert values._location('counts', 'missing')[0] == 'unknown'

before = gdb.parameter('language')
for expression, value in [('counter', '43'), ('enabled', 'false'), ('state', 'idle'), ('values(0)', '99'), ('sample.id', '8'), ('pointer.all', '44')]:
    values.assign(expression, value)
assert int(gdb.parse_and_eval('counter')) == 44
assert not bool(gdb.parse_and_eval('enabled'))
assert str(gdb.parse_and_eval('state')) == 'idle'
assert int(gdb.parse_and_eval('values(0)')) == 99
assert int(gdb.parse_and_eval('sample.id')) == 8
assert gdb.parameter('language') == before

for expression in ['counter := 999', 'ada_values_ready()']:
    with array.read_settings():
        assert values._location(expression, None)[0] == 'unknown'
assert int(gdb.parse_and_eval('counter')) == 44
assert all(gdb.parameter(p) for p in ('may-call-functions', 'may-write-memory', 'may-write-registers'))
"#,
    );
}
