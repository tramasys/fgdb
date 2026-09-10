use super::*;
use std::{path::Path, process::Command, time::Duration};

mod ada;
mod d;

#[test]
fn assignments_quote_both_expressions_without_interpolating_python() {
    let command = assignment_command("object.field", "\"quoted\\value\"");
    assert!(command.starts_with("-interpreter-exec console "));
    assert!(command.contains(".assign("));
    assert!(!command.contains('\n'));
    let hostile = assignment_command("name\nquit", "\"; raise RuntimeError('injected')");
    assert!(!hostile.contains('\n'));
}

fn fixture(name: &str, breakpoint: &str, assertions: &str) {
    let executable = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/debug-fixtures")
        .join(name);

    let prelude = r#"import gdb
array = __import__('_fgdb_languages_v1.array', fromlist=['*'])
def inspect_output(expression, axes, offset=0, count=64):
    chunks = []
    previous = gdb.write
    gdb.write = chunks.append
    try:
        array.inspect_array(expression, '', axes, offset, count)
    finally:
        gdb.write = previous
    output = ''.join(chunks)
    assert len(output) <= array.MAX_ARRAY_OUTPUT_BYTES
    return output

def inspect(expression, axes, offset=0, count=64):
    output = inspect_output(expression, axes, offset, count)
    return [tuple(bytes.fromhex(field).decode() for field in line.split(':', 1)[1].split('\t'))
            for line in output.splitlines() if line.startswith('FGDB_ARRAY_ROW:')]
"#;

    let script = format!("{prelude}\n{assertions}");

    let wrapper = format!(
        "import traceback\ntry:\n exec({})\nexcept BaseException:\n gdb.write(traceback.format_exc())\nelse:\n gdb.write('FGDB_ARRAY_TEST_OK\\n')",
        crate::debugger::quote(&script),
    );

    let mut command = Command::new("gdb");

    command
        .args([
            "--nx",
            "--quiet",
            "--batch",
            "-ex",
            "set confirm off",
            "-ex",
            "set debuginfod enabled off",
        ])
        .arg(&executable)
        .args([
            "-ex",
            &format!("break {breakpoint}"),
            "-ex",
            "run",
            "-ex",
            "up",
            "-ex",
        ])
        .arg(installation_script())
        .arg("-ex")
        .arg(format!("python exec({})", crate::debugger::quote(&wrapper)));

    let output = crate::language::toolchain::probe::output(&mut command, Duration::from_secs(20))
        .expect("GDB fixture failed or timed out");

    let output = String::from_utf8(output).unwrap();
    assert!(output.contains("FGDB_ARRAY_TEST_OK"), "{name}: {output}");
}

#[test]
#[ignore = "requires Python-enabled GDB and the C/C++/Rust array fixtures"]
fn live_native_array_pages_preserve_coordinates_and_skip_unrequested_elements() {
    let assertions = r#"
assert array.resolve('matrix', '').bounds == [(0, 23), (0, 31)]
assert array.resolve('cube', '').bounds == [(0, 3), (0, 4), (0, 5)]
assert array.resolve('matrix[2]', '').bounds == [(0, 31)]
assert [(r[0], r[1]) for r in inspect('large', [(8000, 12, 2)], count=3)] == [('[8000]', '24000'), ('[8002]', '24006'), ('[8004]', '24012')]
rows = inspect('matrix', [(22, 2, 1), (31, 3, -2)])
assert [(r[0], r[1]) for r in rows] == [('[22][31]', '2231'), ('[22][29]', '2229'), ('[22][27]', '2227'), ('[23][31]', '2331'), ('[23][29]', '2329'), ('[23][27]', '2327')], rows
rows = inspect('cube', [(3, 1, 1), (4, 2, -1), (5, 3, -2)], offset=2, count=3)
assert [(r[0], r[1]) for r in rows] == [('[3][4][1]', '341'), ('[3][3][5]', '335'), ('[3][3][3]', '333')], rows
assert not inspect('large', [(0, 0, 1)])
for axes in [[(8191, 2, 1)], [(0, 1, 0)], [(-1, 1, 1)]]:
    try:
        inspect('large', axes)
    except ValueError:
        pass
    else:
        raise AssertionError('Invalid slice accepted')
assert gdb.parameter('may-call-functions')

class RandomAccess:
    def fgdb_array_length(self):
        return 1000000000
    def fgdb_array_element(self, index):
        calls.append(index)
        return gdb.Value(index)
class Sequential:
    def num_children(self):
        return 8192
    def children(self):
        for index in range(8192):
            calls.append(index)
            yield ('[' + str(index) + ']', gdb.Value(index))
calls = []
original_resolve = array.resolve
try:
    array.resolve = lambda *args: array.PrinterArray(RandomAccess())
    rows = inspect('custom', [(10000000, 4, 3)])
    assert calls == [10000000, 10000003, 10000006, 10000009], calls
    assert [r[1] for r in rows] == [str(index) for index in calls]
    calls.clear()
    array.resolve = lambda *args: array.PrinterArray(Sequential())
    rows = inspect('custom', [(520, 3, -2)])
    assert [r[1] for r in rows] == ['520', '518', '516'], rows
    assert len(calls) == 521, len(calls)
    calls.clear()
    try:
        inspect('custom', [(4096, 1, 1)])
    except ValueError:
        pass
    else:
        raise AssertionError('Unbounded sequential seek accepted')
    assert not calls
finally:
    array.resolve = original_resolve
"#;

    fixture("c-array-viewer-target", "c_arrays_ready", assertions);
    fixture("cpp-array-viewer-target", "c_arrays_ready", assertions);
    fixture("rust-array-viewer-target", "rust_arrays_ready", assertions);
}

#[test]
#[ignore = "requires Python-enabled GDB and the C array fixture"]
fn live_array_printer_failures_and_read_only_settings_are_preserved() {
    fixture(
        "c-array-viewer-target",
        "c_arrays_ready",
        r#"
parameters = ('may-call-functions', 'may-write-memory', 'may-write-registers')
for parameter in parameters:
    assert gdb.parameter(parameter)
original = int(gdb.parse_and_eval('large[0]'))
for expression in ['large[0] = 99', '$pc = $pc', 'c_arrays_ready()']:
    try:
        with array.read_settings():
            assert not any(gdb.parameter(parameter) for parameter in parameters)
            gdb.parse_and_eval(expression)
    except gdb.error:
        pass
    else:
        raise AssertionError('Inspection allowed a target side effect: ' + expression)
    assert all(gdb.parameter(parameter) for parameter in parameters)
assert int(gdb.parse_and_eval('large[0]')) == original
with gdb.with_parameter('may-write-memory', False):
    with array.read_settings():
        pass
    assert not gdb.parameter('may-write-memory')

class UnknownLength:
    def children(self):
        for index in range(5):
            yield (str(index), gdb.Value(index))
class ShortPrinter(UnknownLength):
    def num_children(self):
        return 10
class ShortRandomAccess:
    def num_children(self):
        return 10
    def child(self, index):
        raise IndexError('missing')
class FractionalLength(UnknownLength):
    def num_children(self):
        return 4.5
original_resolve = array.resolve
original_budget = array.MAX_ARRAY_OUTPUT_BYTES
try:
    for printer in [ShortPrinter(), ShortRandomAccess(), FractionalLength()]:
        array.resolve = lambda *args: array.PrinterArray(printer)
        try:
            inspect('custom', [(0, 10, 1)])
        except (ValueError, TypeError):
            pass
        else:
            raise AssertionError('Invalid printer length accepted')
    array.resolve = lambda *args: array.PrinterArray(UnknownLength())
    array.MAX_ARRAY_OUTPUT_BYTES = 260
    offset = 0
    for _ in range(10):
        output = inspect_output('custom', [(0, 10, 1)], offset=offset)
        rows = [line for line in output.splitlines() if line.startswith('FGDB_ARRAY_ROW:')]
        offset += len(rows)
        if 'FGDB_ARRAY_END:1' in output:
            break
        assert rows, output
    assert offset == 5, (offset, output)
    try:
        inspect('custom', [(6, 7, -1)])
    except ValueError:
        pass
    else:
        raise AssertionError('Reverse slice silently omitted its starting elements')
finally:
    array.resolve = original_resolve
    array.MAX_ARRAY_OUTPUT_BYTES = original_budget

for axes, offset in [([(9000, 0, 1)], 0), ([(0, 1, 1)], 1)]:
    try:
        inspect('large', axes, offset)
    except ValueError:
        pass
    else:
        raise AssertionError('Invalid empty slice or end offset accepted')
"#,
    );
}

#[test]
#[ignore = "requires Python-enabled GDB, libstdc++ printers and the C++ variable-viewer fixture"]
fn live_cpp_array_references_and_printer_values_remain_readable() {
    fixture(
        "cpp-variable-viewer-target",
        "variable_viewer_checkpoint",
        r#"
gdb.execute('down', to_string=True)
assert array.resolve('native_values', '').bounds == [(0, 9)]
assert array.resolve('fixed_values', '').bounds == [(0, 7)]
rows = inspect('fixed_values', [(7, 3, -2)])
assert [r[1] for r in rows] == ['80', '60', '40'], rows
rows = inspect('words', [(2, 2, 1)])
assert 'two words' in rows[0][1], rows
assert 'three' in rows[1][1], rows
"#,
    );
}

#[test]
#[ignore = "requires Python-enabled GDB and the Fortran array fixture"]
fn live_fortran_slices_preserve_native_bounds_and_strided_storage() {
    fixture(
        "fortran-array-viewer-target",
        "fortran_arrays_ready",
        r#"
assert array.resolve('matrix', '').bounds == [(-20, 79), (4, 103)]
rows = inspect('matrix', [(78, 2, 1), (103, 2, -2)])
assert [(r[0], r[1]) for r in rows] == [('(78,103)', '78103'), ('(79,103)', '79103'), ('(78,101)', '78101'), ('(79,101)', '79101')], rows
rows = inspect('cube', [(2, 1, 1), (8, 2, -2), (2, 3, -2)], offset=1, count=3)
assert [(r[0], r[1]) for r in rows] == [('(2,6,2)', '262'), ('(2,8,0)', '280'), ('(2,6,0)', '260')], rows
rows = inspect('reversed', [(1, 3, 1), (1, 2, 1)])
assert [r[1] for r in rows] == ['79103', '78103', '77103', '79102', '78102', '77102'], rows
rows = inspect('strided', [(2, 2, 1), (3, 2, 1)])
assert [r[1] for r in rows] == ['-16992', '-13992', '-16990', '-13990'], rows
rows = inspect('large', [(99000, 3, 2)])
assert [r[1] for r in rows] == ['297000', '297006', '297012'], rows
"#,
    );
}
