//! Language-independent commands for internal register and stack inspection.
//!
//! These probes use registers and numeric addresses, not source-language values.
//! Keep their generated syntax and command language together so adding a target
//! language cannot change how they are parsed. User expressions remain separate.

use crate::debugger::MiCommandBuilder;

pub(super) mod reads;

pub(super) fn register_command(register: &str, depth: usize) -> String {
    let expression = if depth == 0 {
        format!("(void*)(${register})")
    } else {
        dereference(format!("${register}"), depth)
    };

    evaluate(&expression)
}

#[cfg(test)]
pub(super) fn stack_command(register: &str, offset: usize, depth: usize) -> String {
    let expression = format!("*(void**)(${register}+0x{offset:x})");
    evaluate(&dereference(expression, depth))
}

pub(super) fn string_command(address: u64) -> String {
    evaluate(&format!("(char*)0x{address:x}"))
}

pub(super) fn read_command(address: u64) -> String {
    evaluate(&format!("*(void**)0x{address:x}"))
}

fn dereference(mut expression: String, depth: usize) -> String {
    for _ in 0..depth {
        expression = format!("*(void**)({expression})");
    }

    expression
}

fn evaluate(expression: &str) -> String {
    // MI restores the selected language after this command, including errors.
    // Never change the session language or infer syntax from the current frame.
    MiCommandBuilder::new("-data-evaluate-expression")
        .keyword("--language")
        .keyword("c")
        .argument(expression)
        .finish()
}

#[cfg(test)]
mod tests;
