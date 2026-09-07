//! The private Python bridge is installed once per backend. Feature requests
//! call its helpers, without resending implementations or touching sys.path.

const PACKAGE: &str = "_fgdb_languages_v1";
const MODULES: &[(&str, &str)] = &[
    ("common", include_str!("common.py")),
    ("fortran", include_str!("fortran.py")),
    ("zig", include_str!("zig.py")),
    ("odin", include_str!("odin.py")),
    ("array", include_str!("array.py")),
    ("printers", include_str!("printers.py")),
];

pub(crate) fn install_command() -> String {
    let mut command = format!(
        "python exec({}, {{'_sources': [",
        crate::debugger::quote(include_str!("runtime.py")),
    );

    for (name, source) in MODULES {
        command.push('(');
        command.push_str(&crate::debugger::quote(name));
        command.push(',');
        command.push_str(&crate::debugger::quote(source));
        command.push_str("),");
    }

    command.push_str("]})");
    crate::debugger::console_command(&command)
}

pub(crate) fn fortran_value_expression(expression: &str, members: &str) -> String {
    format!(
        "{}.resolve_member_path({}, {})",
        module("fortran"),
        crate::debugger::quote(expression),
        crate::debugger::quote(members),
    )
}

pub(crate) fn array_inspection_command(expression: &str, members: &str, limit: usize) -> String {
    crate::debugger::console_command(&format!(
        "python {}.inspect_array({}, {limit}, {})",
        module("array"),
        crate::debugger::quote(expression),
        crate::debugger::quote(members),
    ))
}

fn module(name: &str) -> String {
    format!("__import__('{PACKAGE}.{name}', fromlist=['*'])")
}
