//! Source-expression resolution shared by read-only metadata and value editors.

use crate::debugger::Variable;

pub(super) fn variable_path_root<'a>(
    variable: &Variable,
    varobj: &'a str,
) -> (&'a str, Option<&'a str>) {
    if variable
        .type_name
        .as_deref()
        .is_some_and(crate::language::is_fortran_type)
    {
        let (root, members) = varobj.split_once('.').unwrap_or((varobj, ""));

        (root, Some(members))
    } else {
        (varobj, None)
    }
}

pub(super) fn value_python(expression: &str, members: Option<&str>) -> String {
    if let Some(members) = members {
        crate::language::python::native_value_expression(expression, members)
    } else {
        format!("gdb.parse_and_eval({})", crate::debugger::quote(expression))
    }
}
