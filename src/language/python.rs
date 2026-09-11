//! The private Python bridge is installed once per backend. Feature requests
//! call its helpers, without resending implementations or touching sys.path.

const PACKAGE: &str = "_fgdb_languages_v1";
#[cfg(test)]
mod tests;
const MODULES: &[(&str, &str)] = &[
    ("common", include_str!("printers/common.py")),
    ("values", include_str!("printers/values.py")),
    ("d", include_str!("printers/d.py")),
    ("fortran", include_str!("printers/fortran.py")),
    ("ada", include_str!("printers/ada.py")),
    ("paths", include_str!("printers/paths.py")),
    ("zig", include_str!("printers/zig.py")),
    ("odin", include_str!("printers/odin.py")),
    ("array", include_str!("printers/array.py")),
    ("printers", include_str!("printers/printers.py")),
    (
        "return_transfers",
        include_str!("../debugger/return_transfers.py"),
    ),
    ("returns", include_str!("../debugger/returns.py")),
];

pub(crate) fn install_command() -> String {
    crate::debugger::console_command(&installation_script())
}

pub(crate) fn return_values_command(enabled: bool, discard: bool) -> String {
    crate::debugger::console_command(&format!(
        "python {}.snapshot({}, {})",
        module("returns"),
        if enabled { "True" } else { "False" },
        if discard { "True" } else { "False" },
    ))
}

pub(crate) fn assignment_command(expression: &str, value: &str) -> String {
    crate::debugger::console_command(&format!(
        "python {}.assign({}, {})",
        module("values"),
        crate::debugger::quote(expression),
        crate::debugger::quote(value),
    ))
}

fn installation_script() -> String {
    let mut command = format!(
        "python exec({}, {{'_sources': [",
        crate::debugger::quote(include_str!("printers/runtime.py")),
    );

    for (name, source) in MODULES {
        command.push('(');
        command.push_str(&crate::debugger::quote(name));
        command.push(',');
        command.push_str(&crate::debugger::quote(source));
        command.push_str("),");
    }

    command.push_str("]})");
    command
}

pub(crate) fn native_value_expression(expression: &str, members: &str) -> String {
    format!(
        "{}.resolve_member_path({}, {})",
        module("paths"),
        crate::debugger::quote(expression),
        crate::debugger::quote(members),
    )
}

pub(crate) fn array_description_command(expression: &str, members: &str) -> String {
    crate::debugger::console_command(&format!(
        "python {}.request_array('describe', {}, {})",
        module("array"),
        crate::debugger::quote(expression),
        crate::debugger::quote(members),
    ))
}

pub(crate) fn member_address_command(address: u64, type_name: &str, members: &[String]) -> String {
    let members = members
        .iter()
        .map(|member| crate::debugger::quote(member))
        .collect::<Vec<_>>()
        .join(",");

    crate::debugger::console_command(&format!(
        "python {}.member_address({address}, {}, [{members}])",
        module("values"),
        crate::debugger::quote(type_name),
    ))
}

pub(crate) fn value_locations_command(paths: &[(String, Option<String>)]) -> String {
    let paths = paths
        .iter()
        .map(|(expression, members)| {
            format!(
                "({}, {})",
                crate::debugger::quote(expression),
                members
                    .as_deref()
                    .map(crate::debugger::quote)
                    .unwrap_or_else(|| "None".into())
            )
        })
        .collect::<Vec<_>>()
        .join(",");

    crate::debugger::console_command(&format!("python {}.locations([{paths}])", module("values"),))
}

pub(crate) fn array_inspection_command(
    expression: &str,
    members: &str,
    page: &crate::debugger::array::ArrayPage,
    offset: u64,
    count: usize,
) -> String {
    let axes = page
        .slice
        .axes
        .iter()
        .map(|axis| format!("({},{},{})", axis.start, axis.count, axis.stride))
        .collect::<Vec<_>>()
        .join(",");

    crate::debugger::console_command(&format!(
        "python {}.request_array('page', {}, {}, [{axes}], {offset}, {count})",
        module("array"),
        crate::debugger::quote(expression),
        crate::debugger::quote(members),
    ))
}

fn module(name: &str) -> String {
    format!("__import__('{PACKAGE}.{name}', fromlist=['*'])")
}
