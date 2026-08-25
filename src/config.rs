use std::{env, path::PathBuf};

#[derive(Clone, Debug)]
pub struct LaunchConfig {
    pub gdb_executable: String,
    pub gdb_startup_arguments: Vec<String>,
    pub target_arguments: Vec<String>,
    pub working_directory: PathBuf,
}

impl LaunchConfig {
    pub fn from_process() -> Result<Self, shell_words::ParseError> {
        let target_arguments = env::args().skip(1).collect();
        let gdb_executable = env::var("FGDB_GDB").unwrap_or_else(|_| String::from("gdb"));
        let gdb_startup_arguments = env::var("FGDB_GDB_ARGS").ok().map_or_else(
            || Ok(Vec::new()),
            |arguments| shell_words::split(&arguments),
        )?;
        let working_directory = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

        Ok(Self {
            gdb_executable,
            gdb_startup_arguments,
            target_arguments,
            working_directory,
        })
    }

    pub fn gdb_arguments(&self) -> Vec<String> {
        let mut arguments = vec![self.gdb_executable.clone(), String::from("--quiet")];
        arguments.extend(self.gdb_startup_arguments.iter().cloned());
        if !self.target_arguments.is_empty() {
            arguments.push(String::from("--args"));
            arguments.extend(self.target_arguments.iter().cloned());
        }
        arguments
    }

    pub fn target_name(&self) -> &str {
        self.target_arguments
            .first()
            .map_or("No target selected", String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::LaunchConfig;
    use std::path::PathBuf;

    #[test]
    fn assembles_special_gef_startup_before_target() {
        let configuration = LaunchConfig {
            gdb_executable: String::from("/usr/bin/gdb"),
            gdb_startup_arguments: vec![String::from("-ex"), String::from("init-gef-special")],
            target_arguments: vec![String::from("/tmp/debug target"), String::from("arg")],
            working_directory: PathBuf::from("/tmp"),
        };

        assert_eq!(
            configuration.gdb_arguments(),
            [
                "/usr/bin/gdb",
                "--quiet",
                "-ex",
                "init-gef-special",
                "--args",
                "/tmp/debug target",
                "arg",
            ]
        );
    }
}
