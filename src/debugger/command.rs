use super::quote;

/// Builds one GDB/MI command while keeping data arguments quoted.
///
/// `keyword` is only for fixed syntax selected by fgdb. Runtime values, paths,
/// expressions, and target endpoints must use `argument` or `number`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MiCommandBuilder {
    command: String,
}

impl MiCommandBuilder {
    pub(crate) fn new(operation: &'static str) -> Self {
        debug_assert!(operation.starts_with('-'));

        Self {
            command: operation.to_owned(),
        }
    }

    pub(crate) fn keyword(mut self, keyword: &'static str) -> Self {
        debug_assert!(!keyword.chars().any(char::is_whitespace));
        self.push(keyword);

        self
    }

    pub(crate) fn argument(mut self, argument: &str) -> Self {
        self.push(&quote(argument));

        self
    }

    pub(crate) fn number(mut self, number: impl std::fmt::Display) -> Self {
        self.push(&number.to_string());

        self
    }

    pub(crate) fn finish(self) -> String {
        self.command
    }

    fn push(&mut self, argument: &str) {
        self.command.push(' ');
        self.command.push_str(argument);
    }
}

/// Builds a GDB CLI command that will be carried through GDB/MI.
///
/// CLI grammars differ in whether they parse words or consume a verbatim tail;
/// the builder makes that distinction explicit and always quotes the complete
/// command for the MI transport layer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CliCommandBuilder {
    command: String,
}

impl CliCommandBuilder {
    pub(crate) fn new(operation: &'static str) -> Self {
        debug_assert!(!operation.chars().any(char::is_whitespace));

        Self {
            command: operation.to_owned(),
        }
    }

    pub(crate) fn keyword(mut self, keyword: &'static str) -> Self {
        debug_assert!(!keyword.chars().any(char::is_whitespace));
        self.push(keyword);

        self
    }

    /// Appends data for CLI commands whose grammar consumes the entire
    /// remainder of the line verbatim (for example `rbreak REGEXP` and string
    /// settings). Quoting those values would change their meaning in GDB.
    pub(crate) fn verbatim_tail(mut self, argument: &str) -> Result<Self, &'static str> {
        if argument
            .bytes()
            .any(|byte| matches!(byte, b'\0' | b'\n' | b'\r'))
        {
            return Err("GDB CLI arguments cannot contain NUL or line breaks");
        }

        self.push(argument);

        Ok(self)
    }

    pub(crate) fn finish(self) -> String {
        console_command(&self.command)
    }

    fn push(&mut self, argument: &str) {
        self.command.push(' ');
        self.command.push_str(argument);
    }
}

pub(crate) fn console_command(command: &str) -> String {
    MiCommandBuilder::new("-interpreter-exec")
        .keyword("console")
        .argument(command)
        .finish()
}

pub(crate) fn gdb_cli_string(value: &str) -> Result<String, &'static str> {
    if value
        .bytes()
        .any(|byte| matches!(byte, b'\0' | b'\n' | b'\r'))
    {
        return Err("GDB CLI strings cannot contain NUL or line breaks");
    }

    let mut quoted = String::with_capacity(value.len().saturating_add(2));
    quoted.push('"');
    for character in value.chars() {
        if matches!(character, '\\' | '"') {
            quoted.push('\\');
        }

        quoted.push(character);
    }

    quoted.push('"');
    Ok(quoted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_mi_runtime_arguments() {
        let command = MiCommandBuilder::new("-target-select")
            .keyword("extended-remote")
            .argument("host name:\"port\\suffix")
            .finish();
        assert_eq!(
            command,
            "-target-select extended-remote \"host name:\\\"port\\\\suffix\""
        );
    }

    #[test]
    fn preserves_verbatim_cli_tails_but_rejects_command_boundaries() {
        let value = "/srv/app \"debug\"\\bin";

        let command = CliCommandBuilder::new("set")
            .keyword("remote")
            .keyword("exec-file")
            .verbatim_tail(value)
            .unwrap()
            .finish();

        assert_eq!(
            command,
            console_command(&format!("set remote exec-file {value}"))
        );

        assert!(
            CliCommandBuilder::new("rbreak")
                .verbatim_tail("main\nkill")
                .is_err()
        );
    }
}
