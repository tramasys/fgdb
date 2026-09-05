use std::{
    collections::{BTreeMap, BTreeSet},
    env, fmt, fs,
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
};

use clap::{Parser, error::ErrorKind};

use crate::{cpp_toolchain::GccPrettyPrinter, rust_toolchain::RustToolchain};

const MAX_CONFIG_BYTES: usize = 64 * 1024;
const DEFAULT_CONFIG: &str = "# fgdb configuration\n# Environment variables override these values for one launch.\ngdb=gdb\ngdb_args=\nsource_path=\n# Pretty-printer scripts execute inside GDB. Use the platform path separator for multiple scripts.\n# pretty_printer_path=/path/to/printer.py\ngef_context=hide\nsafe_mode=false\n# Move source breakpoints to GDB's next executable line in the same file.\n# Set false to require the exact clicked line.\nbreakpoint_auto_relocate=true\n# working_directory=/path/to/project\n\n# Named profiles can contain these settings and a startup session.\n# [profile example]\n# executable=/path/to/program\n# arguments=--flag 'argument with spaces'\n# working_directory=/path/to/project\n";
const DEFAULT_SECTION: &str = "<default>";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigurationIssue {
    source: String,
    line: Option<usize>,
    message: String,
}

impl ConfigurationIssue {
    fn file(path: &Path, line: Option<usize>, message: impl Into<String>) -> Self {
        Self {
            source: path.display().to_string(),
            line,
            message: message.into(),
        }
    }

    fn external(source: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            line: None,
            message: message.into(),
        }
    }

    pub fn location(&self) -> String {
        self.line.map_or_else(
            || self.source.clone(),
            |line| format!("{}:{line}", self.source),
        )
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EffectiveConfigurationEntry {
    name: String,
    value: String,
}

impl EffectiveConfigurationEntry {
    fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn value(&self) -> &str {
        &self.value
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConfigurationReport {
    active_path: PathBuf,
    loaded_paths: Vec<PathBuf>,
    created: bool,
    selected_profile: Option<String>,
    issues: Vec<ConfigurationIssue>,
    effective: Vec<EffectiveConfigurationEntry>,
}

impl ConfigurationReport {
    pub fn active_path(&self) -> &Path {
        &self.active_path
    }

    pub fn loaded_paths(&self) -> &[PathBuf] {
        &self.loaded_paths
    }

    pub const fn created(&self) -> bool {
        self.created
    }

    pub fn selected_profile(&self) -> Option<&str> {
        self.selected_profile.as_deref()
    }

    pub fn issues(&self) -> &[ConfigurationIssue] {
        &self.issues
    }

    pub fn effective(&self) -> &[EffectiveConfigurationEntry] {
        &self.effective
    }

    pub fn menu_detail(&self) -> String {
        match self.issues.len() {
            0 => String::from("loaded"),
            1 => String::from("1 issue"),
            count => format!("{count} issues"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DebugSession {
    Launch {
        executable: PathBuf,
        arguments: Vec<String>,
        environment: Vec<(String, String)>,
        working_directory: PathBuf,
    },
    Attach {
        pid: u32,
        executable: Option<PathBuf>,
    },
    CoreDump {
        executable: PathBuf,
        core_dump: PathBuf,
    },
    Remote {
        endpoint: String,
        executable: Option<PathBuf>,
        extended: bool,
        remote_executable: Option<String>,
    },
}

impl DebugSession {
    pub fn executable(&self) -> Option<&std::path::Path> {
        match self {
            Self::Launch { executable, .. } | Self::CoreDump { executable, .. } => Some(executable),
            Self::Attach { executable, .. } | Self::Remote { executable, .. } => {
                executable.as_deref()
            }
        }
    }

    pub fn title(&self) -> String {
        match self {
            Self::Launch { executable, .. } => executable.to_string_lossy().into_owned(),
            Self::Attach { pid, executable } => executable.as_ref().map_or_else(
                || format!("PID {pid}"),
                |path| format!("{} · PID {pid}", path.to_string_lossy()),
            ),
            Self::Remote {
                endpoint,
                executable,
                ..
            } => executable.as_ref().map_or_else(
                || endpoint.clone(),
                |path| format!("{} · {endpoint}", path.to_string_lossy()),
            ),
            Self::CoreDump {
                executable,
                core_dump,
            } => format!(
                "{} · {}",
                executable.to_string_lossy(),
                core_dump.to_string_lossy()
            ),
        }
    }

    pub const fn kind_label(&self) -> &'static str {
        match self {
            Self::Launch { .. } => "Launch",
            Self::Attach { .. } => "Attached",
            Self::CoreDump { .. } => "Core dump",
            Self::Remote {
                extended: false, ..
            } => "Remote",
            Self::Remote { extended: true, .. } => "Extended remote",
        }
    }

    pub const fn supports_execution(&self) -> bool {
        !matches!(self, Self::CoreDump { .. })
    }

    pub const fn can_start(&self) -> bool {
        matches!(
            self,
            Self::Launch { .. }
                | Self::Remote {
                    extended: true,
                    remote_executable: Some(_),
                    ..
                }
        )
    }

    pub const fn supports_restart(&self) -> bool {
        self.can_start()
    }

    pub const fn supports_kill(&self) -> bool {
        !matches!(self, Self::CoreDump { .. })
    }

    pub const fn supports_detach(&self) -> bool {
        !matches!(self, Self::CoreDump { .. })
    }

    pub fn working_directory(&self) -> Option<&std::path::Path> {
        match self {
            Self::Launch {
                working_directory, ..
            } => Some(working_directory),
            Self::Attach { .. } | Self::CoreDump { .. } | Self::Remote { .. } => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct LaunchConfig {
    pub gdb_executable: String,
    pub gdb_startup_arguments: Vec<String>,
    pub gef_context_visible: bool,
    pub source_paths: Vec<PathBuf>,
    pub pretty_printer_paths: Vec<PathBuf>,
    pub working_directory: PathBuf,
    pub safe_mode: bool,
    pub breakpoint_auto_relocate: bool,
    gcc_pretty_printer: Option<Arc<GccPrettyPrinter>>,
    rust_toolchain: Option<Arc<RustToolchain>>,
    initial_session: Option<DebugSession>,
    configuration_report: Arc<ConfigurationReport>,
}

impl LaunchConfig {
    pub fn from_process() -> Result<StartupAction, StartupError> {
        let arguments: Vec<_> = env::args_os().collect();

        let check_only = arguments
            .iter()
            .any(|argument| argument == "--check-config");

        let cli = match Cli::try_parse_from(arguments) {
            Ok(cli) => cli,
            Err(error)
                if matches!(
                    error.kind(),
                    ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
                ) =>
            {
                return Ok(StartupAction::Print(error.to_string()));
            }
            Err(error) => return Err(StartupError::new(error.to_string(), check_only)),
        };

        let mut loaded = read_user_config();

        if cli.check_config {
            validate_check_config_arguments(&cli)
                .map_err(|message| StartupError::with_config(message, true, &loaded.path))?;

            if let Some(profile) = cli.profile.as_deref()
                && !loaded.config.profiles.contains_key(profile)
            {
                return Err(StartupError::with_config(
                    format!(
                        "Profile '{profile}' does not exist in {}",
                        loaded.path.display()
                    ),
                    true,
                    &loaded.path,
                ));
            }

            collect_check_pretty_printer_path_issues(
                &mut loaded,
                cli.profile.as_deref(),
                env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            );

            if !loaded.issues.is_empty() {
                return Err(StartupError::with_config(
                    config_check_report(&loaded, cli.profile.as_deref()),
                    true,
                    &loaded.path,
                ));
            }

            return Ok(StartupAction::Print(config_check_report(
                &loaded,
                cli.profile.as_deref(),
            )));
        }

        let (environment, environment_issues) = read_environment_overrides();
        loaded.issues.extend(environment_issues);
        let current_directory = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

        resolve_launch_config(cli, &loaded, environment, current_directory)
            .map(|configuration| StartupAction::Run(Box::new(configuration)))
            .map_err(|message| StartupError::with_config(message, false, &loaded.path))
    }

    pub fn gdb_arguments(&self) -> Vec<String> {
        let mut arguments = vec![self.gdb_executable.clone(), String::from("--quiet")];

        if self.safe_mode {
            arguments.push(String::from("--nx"));
        } else {
            if let Some(pretty_printer) = self.gcc_pretty_printer.as_deref() {
                arguments.extend(pretty_printer.gdb_arguments());
            }

            if !debugger_is_rust_gdb(&self.gdb_executable)
                && let Some(toolchain) = self.rust_toolchain.as_deref()
            {
                arguments.extend(toolchain.gdb_printer_arguments());
            }

            for path in &self.pretty_printer_paths {
                if let Some(path) = path.to_str()
                    && let Ok(path) = crate::debugger::gdb_cli_string(path)
                {
                    arguments.extend([String::from("-iex"), format!("source {path}")]);
                }
            }

            arguments.extend(self.gdb_startup_arguments.iter().cloned());
        }

        if let Some(DebugSession::Launch {
            executable,
            arguments: target_arguments,
            ..
        }) = self.initial_session.as_ref()
        {
            arguments.push(String::from("--args"));
            arguments.push(executable.to_string_lossy().into_owned());
            arguments.extend(target_arguments.iter().cloned());
        }

        arguments
    }

    pub fn target_name(&self) -> String {
        self.initial_session
            .as_ref()
            .map_or_else(|| String::from("No target selected"), DebugSession::title)
    }

    pub fn initial_session(&self) -> Option<DebugSession> {
        self.initial_session.clone()
    }

    pub fn rust_sysroot(&self) -> Option<&Path> {
        self.rust_toolchain.as_deref().map(RustToolchain::sysroot)
    }

    pub fn gcc_pretty_printer_directory(&self) -> Option<&Path> {
        self.gcc_pretty_printer
            .as_deref()
            .map(GccPrettyPrinter::python_directory)
    }

    pub fn needs_deferred_session_configuration(&self) -> bool {
        self.initial_session.is_some()
    }

    pub fn configuration_report(&self) -> &ConfigurationReport {
        self.configuration_report.as_ref()
    }
}

fn debugger_is_rust_gdb(executable: &str) -> bool {
    Path::new(executable)
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == "rust-gdb" || name.starts_with("rust-gdb-"))
}

#[derive(Debug)]
pub enum StartupAction {
    Run(Box<LaunchConfig>),
    Print(String),
}

#[derive(Debug)]
pub struct StartupError {
    message: String,
    check_only: bool,
    active_config_path: Option<PathBuf>,
}

impl StartupError {
    fn new(message: impl Into<String>, check_only: bool) -> Self {
        Self {
            message: message.into(),
            check_only,
            active_config_path: None,
        }
    }

    fn with_config(message: impl Into<String>, check_only: bool, path: &Path) -> Self {
        Self {
            message: message.into(),
            check_only,
            active_config_path: Some(path.to_path_buf()),
        }
    }

    pub const fn should_show_graphically(&self) -> bool {
        !self.check_only
    }

    pub fn active_config_path(&self) -> Option<&Path> {
        self.active_config_path.as_deref()
    }
}

impl fmt::Display for StartupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for StartupError {}

#[derive(Parser, Debug)]
#[command(
    name = "fgdb",
    version,
    about = "A native GDB frontend",
    long_about = "A native GDB frontend. Start fgdb without a target, launch an executable, attach to a process, inspect a core dump, or connect to a remote GDB server.",
    disable_help_subcommand = true
)]
struct Cli {
    /// Attach to a local process ID
    #[arg(long, value_name = "PID")]
    attach: Option<u32>,

    /// Open a core dump
    #[arg(long, value_name = "CORE")]
    core: Option<PathBuf>,

    /// Connect to a gdbserver endpoint such as localhost:1234
    #[arg(long, value_name = "HOST:PORT")]
    remote: Option<String>,

    /// Executable for an attach, core, remote, or launch session
    #[arg(long, value_name = "EXE")]
    executable: Option<PathBuf>,

    /// Working directory for GDB and launched programs
    #[arg(long, value_name = "PATH")]
    working_directory: Option<PathBuf>,

    /// Apply a named profile from the fgdb configuration file
    #[arg(long, value_name = "NAME")]
    profile: Option<String>,

    /// Start GDB without init files or configured startup arguments
    #[arg(long)]
    safe_mode: bool,

    /// Validate the configuration file and exit
    #[arg(long)]
    check_config: bool,

    /// Executable to launch. Place fgdb options before it.
    #[arg(value_name = "EXECUTABLE")]
    target: Option<String>,

    /// Arguments passed to the executable
    #[arg(value_name = "ARGUMENT", num_args = 0.., trailing_var_arg = true, allow_hyphen_values = true)]
    target_arguments: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ConfigLayer {
    gdb_executable: Option<String>,
    gdb_startup_arguments: Option<String>,
    gef_context_visible: Option<bool>,
    source_paths: Option<Vec<PathBuf>>,
    pretty_printer_paths: Option<Vec<PathBuf>>,
    working_directory: Option<PathBuf>,
    safe_mode: Option<bool>,
    breakpoint_auto_relocate: Option<bool>,
    executable: Option<PathBuf>,
    arguments: Option<String>,
    attach: Option<u32>,
    core_dump: Option<PathBuf>,
    remote: Option<String>,
}

impl ConfigLayer {
    fn overlay(&mut self, overlay: &Self) {
        if overlay.attach.is_some() || overlay.core_dump.is_some() || overlay.remote.is_some() {
            self.attach = None;
            self.core_dump = None;
            self.remote = None;
        }

        macro_rules! overlay_fields {
            ($($field:ident),+ $(,)?) => {
                $(if overlay.$field.is_some() {
                    self.$field.clone_from(&overlay.$field);
                })+
            };
        }

        overlay_fields!(
            gdb_executable,
            gdb_startup_arguments,
            gef_context_visible,
            source_paths,
            pretty_printer_paths,
            working_directory,
            safe_mode,
            breakpoint_auto_relocate,
            executable,
            arguments,
            attach,
            core_dump,
            remote,
        );
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct FileConfig {
    defaults: ConfigLayer,
    profiles: BTreeMap<String, ConfigLayer>,
}

#[derive(Debug)]
struct LoadedConfig {
    path: PathBuf,
    loaded_paths: Vec<PathBuf>,
    config: FileConfig,
    created: bool,
    issues: Vec<ConfigurationIssue>,
    locations: BTreeMap<(String, &'static str), usize>,
}

#[derive(Debug)]
struct ParsedConfig {
    config: FileConfig,
    locations: BTreeMap<(String, &'static str), usize>,
    issues: Vec<ConfigurationIssue>,
}

#[derive(Debug, Default)]
struct EnvironmentOverrides {
    layer: ConfigLayer,
    profile: Option<String>,
}

fn config_path() -> PathBuf {
    gtk::glib::user_config_dir().join("fgdb/config.conf")
}

fn read_user_config() -> LoadedConfig {
    let path = config_path();

    match crate::bounded::read_string(&path, MAX_CONFIG_BYTES) {
        Ok(contents) => loaded_config_from_contents(path, &contents, false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            match create_default_config(&path) {
                Ok(()) => loaded_config_from_contents(path, DEFAULT_CONFIG, true),
                Err(error) => fallback_loaded_config(
                    path,
                    format!("Could not create the default configuration: {error}"),
                ),
            }
        }
        Err(error) => {
            fallback_loaded_config(path, format!("Could not read the configuration: {error}"))
        }
    }
}

fn loaded_config_from_contents(path: PathBuf, contents: &str, created: bool) -> LoadedConfig {
    let mut parsed = parse_user_config_with_diagnostics(contents, &path);
    collect_validation_issues(&parsed.config, &parsed.locations, &path, &mut parsed.issues);

    LoadedConfig {
        loaded_paths: vec![path.clone()],
        path,
        config: parsed.config,
        created,
        issues: parsed.issues,
        locations: parsed.locations,
    }
}

fn fallback_loaded_config(path: PathBuf, message: String) -> LoadedConfig {
    let mut parsed = parse_user_config_with_diagnostics(DEFAULT_CONFIG, &path);

    parsed
        .issues
        .push(ConfigurationIssue::file(&path, None, message));

    LoadedConfig {
        path,
        loaded_paths: Vec::new(),
        config: parsed.config,
        created: false,
        issues: parsed.issues,
        locations: parsed.locations,
    }
}

fn create_default_config(path: &Path) -> std::io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "the configuration path has no parent directory",
        )
    })?;

    fs::create_dir_all(parent)?;

    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;

    file.write_all(DEFAULT_CONFIG.as_bytes())
}

#[cfg(test)]
fn parse_user_config(contents: &str) -> Result<FileConfig, String> {
    let parsed = parse_user_config_with_diagnostics(contents, Path::new("<memory>"));

    if let Some(issue) = parsed.issues.first() {
        Err(format!("{}: {}", issue.location(), issue.message()))
    } else {
        Ok(parsed.config)
    }
}

fn parse_user_config_with_diagnostics(contents: &str, path: &Path) -> ParsedConfig {
    let mut config = FileConfig::default();
    let mut profile = Ok(None::<String>);
    let mut seen = BTreeSet::new();
    let mut locations = BTreeMap::new();
    let mut issues = Vec::new();

    for (index, raw_line) in contents.lines().enumerate() {
        let line_number = index + 1;
        let line = raw_line.trim();

        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        if line.starts_with('[') {
            profile = parse_profile_header(line).map(Some).map_err(|message| {
                issues.push(ConfigurationIssue::file(path, Some(line_number), message));
            });

            continue;
        }

        let Some((key, value)) = line.split_once('=') else {
            issues.push(ConfigurationIssue::file(
                path,
                Some(line_number),
                "Expected KEY=VALUE",
            ));

            continue;
        };

        let Some(key) = canonical_config_key(key.trim()) else {
            issues.push(ConfigurationIssue::file(
                path,
                Some(line_number),
                format!("Unknown setting '{}'", key.trim()),
            ));

            continue;
        };

        let Ok(profile) = &profile else {
            continue;
        };

        let section = profile.as_deref().unwrap_or(DEFAULT_SECTION);

        if !seen.insert((section.to_owned(), key)) {
            issues.push(ConfigurationIssue::file(
                path,
                Some(line_number),
                format!("Duplicate '{key}' setting in {section}"),
            ));

            continue;
        }

        let layer = profile.as_ref().map_or(&mut config.defaults, |name| {
            config.profiles.entry(name.clone()).or_default()
        });

        match set_config_value(layer, key, value.trim()) {
            Ok(()) => {
                locations.insert((section.to_owned(), key), line_number);
            }
            Err(message) => {
                issues.push(ConfigurationIssue::file(path, Some(line_number), message));
            }
        }
    }

    ParsedConfig {
        config,
        locations,
        issues,
    }
}

fn parse_profile_header(line: &str) -> Result<String, String> {
    let header = line
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .ok_or_else(|| String::from("invalid section header"))?
        .trim();

    let name = header
        .strip_prefix("profile ")
        .or_else(|| header.strip_prefix("profile."))
        .map(str::trim)
        .map(unquote_config_value)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| String::from("expected [profile NAME]"))?;

    Ok(name.to_owned())
}

fn canonical_config_key(key: &str) -> Option<&'static str> {
    match key {
        "gdb" | "gdb_executable" => Some("gdb"),
        "gdb_args" | "gdb_arguments" => Some("gdb_args"),
        "gef_context" | "gef.context" => Some("gef_context"),
        "source_path" | "source_paths" => Some("source_path"),
        "pretty_printer_path" | "pretty_printer_paths" => Some("pretty_printer_path"),
        "working_directory" | "cwd" => Some("working_directory"),
        "safe_mode" => Some("safe_mode"),
        "breakpoint_auto_relocate" => Some("breakpoint_auto_relocate"),
        "executable" => Some("executable"),
        "arguments" | "args" => Some("arguments"),
        "attach" | "pid" => Some("attach"),
        "core" | "core_dump" => Some("core"),
        "remote" => Some("remote"),
        _ => None,
    }
}

fn set_config_value(layer: &mut ConfigLayer, key: &'static str, value: &str) -> Result<(), String> {
    let unquoted = unquote_config_value(value);

    let required = || {
        (!unquoted.is_empty())
            .then_some(unquoted)
            .ok_or_else(|| format!("'{key}' cannot be empty"))
    };

    match key {
        "gdb" => layer.gdb_executable = Some(required()?.to_owned()),
        "gdb_args" => layer.gdb_startup_arguments = Some(value.to_owned()),
        "gef_context" => {
            layer.gef_context_visible = Some(
                parse_boolean(unquoted)
                    .ok_or_else(|| format!("Invalid gef_context value '{value}'"))?,
            );
        }
        "source_path" => {
            layer.source_paths = Some(env::split_paths(unquoted).collect());
        }
        "pretty_printer_path" => {
            layer.pretty_printer_paths = Some(env::split_paths(unquoted).collect());
        }
        "working_directory" => layer.working_directory = Some(PathBuf::from(required()?)),
        "safe_mode" => {
            layer.safe_mode = Some(
                parse_boolean(unquoted)
                    .ok_or_else(|| format!("Invalid safe_mode value '{value}'"))?,
            );
        }
        "breakpoint_auto_relocate" => {
            layer.breakpoint_auto_relocate = Some(
                parse_boolean(unquoted)
                    .ok_or_else(|| format!("Invalid breakpoint_auto_relocate value '{value}'"))?,
            );
        }
        "executable" => layer.executable = Some(PathBuf::from(required()?)),
        "arguments" => layer.arguments = Some(value.to_owned()),
        "attach" => {
            let pid = required()?
                .parse::<u32>()
                .map_err(|_| format!("Invalid process ID '{value}'"))?;

            if pid == 0 {
                return Err(String::from("Process ID must be greater than zero"));
            }

            layer.attach = Some(pid);
        }
        "core" => layer.core_dump = Some(PathBuf::from(required()?)),
        "remote" => layer.remote = Some(required()?.to_owned()),
        _ => unreachable!("canonical configuration key"),
    }

    Ok(())
}

#[cfg(test)]
fn validate_file_config(config: &FileConfig) -> Result<(), String> {
    validate_config_layer(&config.defaults, "the default configuration")?;

    for (name, profile) in &config.profiles {
        let mut merged = config.defaults.clone();
        merged.overlay(profile);
        validate_config_layer(&merged, &format!("profile '{name}'"))?;
    }

    Ok(())
}

#[cfg(test)]
fn validate_config_layer(layer: &ConfigLayer, context: &str) -> Result<(), String> {
    let modes = usize::from(layer.attach.is_some())
        + usize::from(layer.core_dump.is_some())
        + usize::from(layer.remote.is_some());

    if modes > 1 {
        return Err(format!(
            "Invalid {context}: attach, core, and remote select different session types"
        ));
    }

    if layer.core_dump.is_some() && layer.executable.is_none() {
        return Err(format!(
            "Invalid {context}: a core dump requires an executable"
        ));
    }

    for (label, arguments) in [
        ("gdb_args", layer.gdb_startup_arguments.as_deref()),
        ("arguments", layer.arguments.as_deref()),
    ] {
        if let Some(arguments) = arguments {
            shell_words::split(arguments)
                .map_err(|error| format!("Invalid {context} {label}: {error}"))?;
        }
    }

    Ok(())
}

fn collect_validation_issues(
    config: &FileConfig,
    locations: &BTreeMap<(String, &'static str), usize>,
    path: &Path,
    issues: &mut Vec<ConfigurationIssue>,
) {
    collect_layer_validation_issues(
        &config.defaults,
        "the default configuration",
        DEFAULT_SECTION,
        None,
        locations,
        path,
        issues,
    );

    for (name, profile) in &config.profiles {
        let mut merged = config.defaults.clone();
        merged.overlay(profile);

        collect_layer_validation_issues(
            &merged,
            &format!("profile '{name}'"),
            name,
            Some(profile),
            locations,
            path,
            issues,
        );
    }
}

fn collect_layer_validation_issues(
    layer: &ConfigLayer,
    context: &str,
    section: &str,
    declared: Option<&ConfigLayer>,
    locations: &BTreeMap<(String, &'static str), usize>,
    path: &Path,
    issues: &mut Vec<ConfigurationIssue>,
) {
    let modes = [
        ("attach", layer.attach.is_some()),
        ("core", layer.core_dump.is_some()),
        ("remote", layer.remote.is_some()),
    ];

    let selected_modes = modes
        .iter()
        .filter_map(|(key, selected)| selected.then_some(*key))
        .collect::<Vec<_>>();

    let validates_session = declared.is_none_or(|layer| {
        layer.attach.is_some()
            || layer.core_dump.is_some()
            || layer.remote.is_some()
            || layer.executable.is_some()
    });

    if validates_session && selected_modes.len() > 1 {
        let line = selected_modes
            .iter()
            .filter_map(|key| configuration_line(locations, section, key))
            .max();

        push_configuration_issue(
            issues,
            ConfigurationIssue::file(
                path,
                line,
                format!(
                    "Invalid {context}: attach, core, and remote select different session types"
                ),
            ),
        );
    }

    if validates_session && layer.core_dump.is_some() && layer.executable.is_none() {
        push_configuration_issue(
            issues,
            ConfigurationIssue::file(
                path,
                configuration_line(locations, section, "core"),
                format!("Invalid {context}: a core dump requires an executable"),
            ),
        );
    }

    for (key, arguments) in [
        ("gdb_args", layer.gdb_startup_arguments.as_deref()),
        ("arguments", layer.arguments.as_deref()),
    ] {
        let declared_here = declared.is_none_or(|layer| match key {
            "gdb_args" => layer.gdb_startup_arguments.is_some(),
            "arguments" => layer.arguments.is_some(),
            _ => false,
        });

        if declared_here
            && let Some(arguments) = arguments
            && let Err(error) = shell_words::split(arguments)
        {
            push_configuration_issue(
                issues,
                ConfigurationIssue::file(
                    path,
                    configuration_line(locations, section, key),
                    format!("Invalid {context} {key}: {error}"),
                ),
            );
        }
    }
}

fn configuration_line(
    locations: &BTreeMap<(String, &'static str), usize>,
    section: &str,
    key: &'static str,
) -> Option<usize> {
    locations
        .get(&(section.to_owned(), key))
        .or_else(|| locations.get(&(DEFAULT_SECTION.to_owned(), key)))
        .copied()
}

fn push_configuration_issue(issues: &mut Vec<ConfigurationIssue>, issue: ConfigurationIssue) {
    if !issues.contains(&issue) {
        issues.push(issue);
    }
}

fn sanitize_config_layer(layer: &mut ConfigLayer) {
    if layer
        .gdb_startup_arguments
        .as_deref()
        .is_some_and(|arguments| shell_words::split(arguments).is_err())
    {
        layer.gdb_startup_arguments = None;
    }

    if layer
        .arguments
        .as_deref()
        .is_some_and(|arguments| shell_words::split(arguments).is_err())
    {
        layer.arguments = None;
    }

    let modes = usize::from(layer.attach.is_some())
        + usize::from(layer.core_dump.is_some())
        + usize::from(layer.remote.is_some());

    if modes > 1 {
        layer.attach = None;
        layer.core_dump = None;
        layer.remote = None;
    } else if layer.core_dump.is_some() && layer.executable.is_none() {
        layer.core_dump = None;
    }
}

fn read_environment_overrides() -> (EnvironmentOverrides, Vec<ConfigurationIssue>) {
    let mut overrides = EnvironmentOverrides::default();
    let mut issues = Vec::new();

    overrides.layer.gdb_executable =
        environment_string("FGDB_GDB", &mut issues).and_then(|value| {
            if value.trim().is_empty() {
                issues.push(ConfigurationIssue::external(
                    "FGDB_GDB",
                    "The debugger executable cannot be empty",
                ));

                None
            } else {
                Some(value)
            }
        });

    overrides.layer.gdb_startup_arguments = environment_string("FGDB_GDB_ARGS", &mut issues)
        .inspect(|value| {
            if let Err(error) = shell_words::split(value) {
                issues.push(ConfigurationIssue::external(
                    "FGDB_GDB_ARGS",
                    format!("Invalid GDB startup arguments: {error}"),
                ));
            }
        });

    overrides.layer.gef_context_visible = environment_string("FGDB_GEF_CONTEXT", &mut issues)
        .and_then(|value| {
            parse_boolean(&value).or_else(|| {
                issues.push(ConfigurationIssue::external(
                    "FGDB_GEF_CONTEXT",
                    format!("Invalid value '{value}'"),
                ));

                None
            })
        });

    overrides.layer.source_paths =
        env::var_os("FGDB_SOURCE_PATH").map(|paths| env::split_paths(&paths).collect());

    overrides.layer.pretty_printer_paths =
        env::var_os("FGDB_PRETTY_PRINTER_PATH").map(|paths| env::split_paths(&paths).collect());

    overrides.layer.working_directory = env::var_os("FGDB_WORKING_DIRECTORY").map(PathBuf::from);

    overrides.layer.safe_mode =
        environment_string("FGDB_SAFE_MODE", &mut issues).and_then(|value| {
            parse_boolean(&value).or_else(|| {
                issues.push(ConfigurationIssue::external(
                    "FGDB_SAFE_MODE",
                    format!("Invalid value '{value}'"),
                ));

                None
            })
        });

    overrides.layer.breakpoint_auto_relocate =
        environment_string("FGDB_BREAKPOINT_AUTO_RELOCATE", &mut issues).and_then(|value| {
            parse_boolean(&value).or_else(|| {
                issues.push(ConfigurationIssue::external(
                    "FGDB_BREAKPOINT_AUTO_RELOCATE",
                    format!("Invalid value '{value}'"),
                ));

                None
            })
        });

    overrides.profile = environment_string("FGDB_PROFILE", &mut issues).and_then(|profile| {
        if profile.trim().is_empty() {
            issues.push(ConfigurationIssue::external(
                "FGDB_PROFILE",
                "The profile name cannot be empty",
            ));

            None
        } else {
            Some(profile)
        }
    });

    (overrides, issues)
}

fn environment_string(name: &str, issues: &mut Vec<ConfigurationIssue>) -> Option<String> {
    match env::var(name) {
        Ok(value) => Some(value),
        Err(env::VarError::NotPresent) => None,
        Err(env::VarError::NotUnicode(_)) => {
            issues.push(ConfigurationIssue::external(
                name,
                "The value is not valid UTF-8",
            ));

            None
        }
    }
}

fn resolve_launch_config(
    cli: Cli,
    loaded: &LoadedConfig,
    environment: EnvironmentOverrides,
    current_directory: PathBuf,
) -> Result<LaunchConfig, String> {
    let config = &loaded.config;
    let selected_profile = cli.profile.clone().or_else(|| environment.profile.clone());
    let pretty_printer_paths_from_environment = environment.layer.pretty_printer_paths.is_some();
    let mut settings = config.defaults.clone();

    if let Some(name) = selected_profile.as_ref() {
        let profile = config
            .profiles
            .get(name)
            .ok_or_else(|| format!("Unknown fgdb profile '{name}'"))?;

        settings.overlay(profile);
    }

    settings.overlay(&environment.layer);
    sanitize_config_layer(&mut settings);

    let working_directory = cli
        .working_directory
        .clone()
        .or_else(|| settings.working_directory.clone())
        .unwrap_or(current_directory);

    let initial_session = resolve_initial_session(&cli, &settings, &working_directory)?;
    let safe_mode = cli.safe_mode || settings.safe_mode.unwrap_or(false);
    let breakpoint_auto_relocate = settings.breakpoint_auto_relocate.unwrap_or(true);

    let gdb_startup_arguments = if safe_mode {
        Vec::new()
    } else {
        settings
            .gdb_startup_arguments
            .as_deref()
            .map_or_else(|| Ok(Vec::new()), shell_words::split)
            .map_err(|error| format!("Invalid GDB startup arguments: {error}"))?
    };

    let gdb_executable = settings
        .gdb_executable
        .unwrap_or_else(|| String::from("gdb"));

    let rust_toolchain =
        RustToolchain::discover(&working_directory, std::time::Duration::from_millis(250))
            .map(Arc::new);

    let gcc_pretty_printer = (!safe_mode)
        .then(|| {
            GccPrettyPrinter::discover(&working_directory, std::time::Duration::from_millis(250))
        })
        .flatten()
        .map(Arc::new);

    let source_paths = settings.source_paths.unwrap_or_default();
    let (pretty_printer_paths, pretty_printer_path_errors) = resolve_pretty_printer_paths(
        settings.pretty_printer_paths.unwrap_or_default(),
        &working_directory,
    );
    let mut configuration_issues = loaded.issues.clone();

    for (path, error) in pretty_printer_path_errors {
        let message = format!(
            "Could not use pretty-printer script '{}': {error}",
            path.display()
        );

        if pretty_printer_paths_from_environment {
            configuration_issues.push(ConfigurationIssue::external(
                "FGDB_PRETTY_PRINTER_PATH",
                message,
            ));
        } else {
            let section = selected_profile.as_deref().unwrap_or(DEFAULT_SECTION);
            let line = configuration_line(&loaded.locations, section, "pretty_printer_path");

            configuration_issues.push(ConfigurationIssue::file(&loaded.path, line, message));
        }
    }

    let configuration_report = Arc::new(ConfigurationReport {
        active_path: loaded.path.clone(),
        loaded_paths: loaded.loaded_paths.clone(),
        created: loaded.created,
        selected_profile: selected_profile.clone(),
        issues: configuration_issues,
        effective: effective_configuration(
            selected_profile.as_deref(),
            &gdb_executable,
            &gdb_startup_arguments,
            settings.gef_context_visible.unwrap_or(false),
            &source_paths,
            &pretty_printer_paths,
            &working_directory,
            safe_mode,
            breakpoint_auto_relocate,
            initial_session.as_ref(),
        ),
    });

    Ok(LaunchConfig {
        gdb_executable,
        gdb_startup_arguments,
        gef_context_visible: settings.gef_context_visible.unwrap_or(false),
        source_paths,
        pretty_printer_paths,
        working_directory,
        safe_mode,
        breakpoint_auto_relocate,
        gcc_pretty_printer,
        rust_toolchain,
        initial_session,
        configuration_report,
    })
}

fn collect_check_pretty_printer_path_issues(
    loaded: &mut LoadedConfig,
    selected_profile: Option<&str>,
    current_directory: PathBuf,
) {
    let mut settings = loaded.config.defaults.clone();

    if let Some(profile) = selected_profile.and_then(|name| loaded.config.profiles.get(name)) {
        settings.overlay(profile);
    }

    let working_directory = settings.working_directory.unwrap_or(current_directory);

    let (_, errors) = resolve_pretty_printer_paths(
        settings.pretty_printer_paths.unwrap_or_default(),
        &working_directory,
    );
    let section = selected_profile.unwrap_or(DEFAULT_SECTION);
    let line = configuration_line(&loaded.locations, section, "pretty_printer_path");

    for (path, error) in errors {
        push_configuration_issue(
            &mut loaded.issues,
            ConfigurationIssue::file(
                &loaded.path,
                line,
                format!(
                    "Could not use pretty-printer script '{}': {error}",
                    path.display()
                ),
            ),
        );
    }
}

fn resolve_pretty_printer_paths(
    paths: Vec<PathBuf>,
    working_directory: &Path,
) -> (Vec<PathBuf>, Vec<(PathBuf, String)>) {
    let mut resolved = Vec::new();
    let mut errors = Vec::new();
    let mut seen = BTreeSet::new();

    for path in paths {
        if path.as_os_str().is_empty() {
            continue;
        }

        let candidate = if path.is_absolute() {
            path.clone()
        } else {
            working_directory.join(&path)
        };

        let canonical = match candidate.canonicalize() {
            Ok(canonical) => canonical,
            Err(error) => {
                errors.push((path, error.to_string()));

                continue;
            }
        };

        match canonical.metadata() {
            Ok(metadata) if metadata.is_file() => {}
            Ok(_) => {
                errors.push((path, String::from("the path is not a regular file")));

                continue;
            }
            Err(error) => {
                errors.push((path, error.to_string()));

                continue;
            }
        }

        if canonical.to_str().is_none() {
            errors.push((
                path,
                String::from("the path is not valid UTF-8 for this GDB session"),
            ));

            continue;
        }

        if seen.insert(canonical.clone()) {
            resolved.push(canonical);
        }
    }

    (resolved, errors)
}

#[allow(clippy::too_many_arguments)]
fn effective_configuration(
    selected_profile: Option<&str>,
    gdb_executable: &str,
    gdb_startup_arguments: &[String],
    gef_context_visible: bool,
    source_paths: &[PathBuf],
    pretty_printer_paths: &[PathBuf],
    working_directory: &Path,
    safe_mode: bool,
    breakpoint_auto_relocate: bool,
    initial_session: Option<&DebugSession>,
) -> Vec<EffectiveConfigurationEntry> {
    let mut entries = vec![
        EffectiveConfigurationEntry::new("profile", selected_profile.unwrap_or("none")),
        EffectiveConfigurationEntry::new("gdb", gdb_executable),
        EffectiveConfigurationEntry::new(
            "gdb_args",
            if gdb_startup_arguments.is_empty() {
                String::from("none")
            } else {
                shell_words::join(gdb_startup_arguments)
            },
        ),
        EffectiveConfigurationEntry::new(
            "gef_context",
            if gef_context_visible { "show" } else { "hide" },
        ),
        EffectiveConfigurationEntry::new(
            "source_path",
            if source_paths.is_empty() {
                String::from("none")
            } else {
                source_paths
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(":")
            },
        ),
        EffectiveConfigurationEntry::new(
            "pretty_printer_path",
            if pretty_printer_paths.is_empty() {
                String::from("none")
            } else {
                pretty_printer_paths
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(":")
            },
        ),
        EffectiveConfigurationEntry::new(
            "working_directory",
            working_directory.display().to_string(),
        ),
        EffectiveConfigurationEntry::new("safe_mode", safe_mode.to_string()),
        EffectiveConfigurationEntry::new(
            "breakpoint_auto_relocate",
            breakpoint_auto_relocate.to_string(),
        ),
        EffectiveConfigurationEntry::new(
            "session",
            initial_session.map_or("none", DebugSession::kind_label),
        ),
    ];

    match initial_session {
        Some(DebugSession::Launch {
            executable,
            arguments,
            ..
        }) => {
            entries.push(EffectiveConfigurationEntry::new(
                "executable",
                executable.display().to_string(),
            ));

            entries.push(EffectiveConfigurationEntry::new(
                "arguments",
                if arguments.is_empty() {
                    String::from("none")
                } else {
                    shell_words::join(arguments)
                },
            ));
        }
        Some(DebugSession::Attach { pid, executable }) => {
            entries.push(EffectiveConfigurationEntry::new("attach", pid.to_string()));

            if let Some(executable) = executable {
                entries.push(EffectiveConfigurationEntry::new(
                    "executable",
                    executable.display().to_string(),
                ));
            }
        }

        Some(DebugSession::CoreDump {
            executable,
            core_dump,
        }) => {
            entries.push(EffectiveConfigurationEntry::new(
                "executable",
                executable.display().to_string(),
            ));

            entries.push(EffectiveConfigurationEntry::new(
                "core",
                core_dump.display().to_string(),
            ));
        }

        Some(DebugSession::Remote {
            endpoint,
            executable,
            ..
        }) => {
            entries.push(EffectiveConfigurationEntry::new("remote", endpoint));

            if let Some(executable) = executable {
                entries.push(EffectiveConfigurationEntry::new(
                    "executable",
                    executable.display().to_string(),
                ));
            }
        }
        None => {}
    }

    entries
}

fn resolve_initial_session(
    cli: &Cli,
    settings: &ConfigLayer,
    working_directory: &Path,
) -> Result<Option<DebugSession>, String> {
    let explicit_modes = usize::from(cli.attach.is_some())
        + usize::from(cli.core.is_some())
        + usize::from(cli.remote.is_some());

    if explicit_modes > 1 {
        return Err(String::from(
            "--attach, --core, and --remote cannot be used together",
        ));
    }

    if cli.target.is_some() && (explicit_modes > 0 || cli.executable.is_some()) {
        return Err(String::from(
            "A positional executable cannot be combined with --attach, --core, --remote, or --executable",
        ));
    }

    if let Some(executable) = cli.target.as_ref() {
        return Ok(Some(DebugSession::Launch {
            executable: PathBuf::from(executable),
            arguments: cli.target_arguments.clone(),
            environment: Vec::new(),
            working_directory: working_directory.to_path_buf(),
        }));
    }

    let configured_modes = usize::from(settings.attach.is_some())
        + usize::from(settings.core_dump.is_some())
        + usize::from(settings.remote.is_some());

    if explicit_modes == 0 && configured_modes > 1 {
        return Err(String::from(
            "The selected configuration combines attach, core, and remote session types",
        ));
    }

    let executable = cli
        .executable
        .clone()
        .or_else(|| settings.executable.clone());

    if let Some(pid) = cli
        .attach
        .or_else(|| (explicit_modes == 0).then_some(settings.attach).flatten())
    {
        if pid == 0 {
            return Err(String::from("--attach PID must be greater than zero"));
        }

        return Ok(Some(DebugSession::Attach { pid, executable }));
    }

    if let Some(core_dump) = cli.core.clone().or_else(|| {
        (explicit_modes == 0)
            .then(|| settings.core_dump.clone())
            .flatten()
    }) {
        let executable = executable.ok_or_else(|| {
            String::from("--core CORE requires --executable EXE or a profile executable")
        })?;

        return Ok(Some(DebugSession::CoreDump {
            executable,
            core_dump,
        }));
    }

    if let Some(endpoint) = cli.remote.clone().or_else(|| {
        (explicit_modes == 0)
            .then(|| settings.remote.clone())
            .flatten()
    }) {
        if endpoint.trim().is_empty() {
            return Err(String::from("--remote HOST:PORT cannot be empty"));
        }

        return Ok(Some(DebugSession::Remote {
            endpoint,
            executable,
            extended: false,
            remote_executable: None,
        }));
    }

    if let Some(executable) = executable {
        let arguments = settings
            .arguments
            .as_deref()
            .map_or_else(|| Ok(Vec::new()), shell_words::split)
            .map_err(|error| format!("Invalid target arguments: {error}"))?;

        return Ok(Some(DebugSession::Launch {
            executable,
            arguments,
            environment: Vec::new(),
            working_directory: working_directory.to_path_buf(),
        }));
    }

    Ok(None)
}

fn validate_check_config_arguments(cli: &Cli) -> Result<(), String> {
    if cli.attach.is_some()
        || cli.core.is_some()
        || cli.remote.is_some()
        || cli.executable.is_some()
        || cli.working_directory.is_some()
        || cli.safe_mode
        || cli.target.is_some()
        || !cli.target_arguments.is_empty()
    {
        return Err(String::from(
            "--check-config can only be combined with --profile",
        ));
    }

    Ok(())
}

fn config_check_report(loaded: &LoadedConfig, selected_profile: Option<&str>) -> String {
    let status = if loaded.issues.is_empty() {
        if loaded.created {
            "Created and validated"
        } else {
            "Validated"
        }
    } else {
        "Configuration has errors"
    };

    let profile = selected_profile.map_or_else(
        || {
            if loaded.config.profiles.is_empty() {
                String::from("none")
            } else {
                loaded
                    .config
                    .profiles
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        },
        |name| name.to_owned(),
    );

    let loaded_files = if loaded.loaded_paths.is_empty() {
        String::from("none")
    } else {
        loaded
            .loaded_paths
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    };

    let mut report = format!(
        "{status}\nActive file: {}\nLoaded files: {loaded_files}\nProfiles: {profile}\n",
        loaded.path.display()
    );

    if !loaded.issues.is_empty() {
        report.push_str("Issues:\n");

        for issue in &loaded.issues {
            report.push_str(&format!("  {}: {}\n", issue.location(), issue.message()));
        }
    }

    report
}

fn unquote_config_value(value: &str) -> &str {
    let value = value.trim();

    if value.len() >= 2
        && ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')))
    {
        &value[1..value.len() - 1]
    } else {
        value
    }
}

fn parse_boolean(value: &str) -> Option<bool> {
    match value
        .trim()
        .trim_matches(['\'', '"'])
        .to_ascii_lowercase()
        .as_str()
    {
        "show" | "visible" | "on" | "true" | "yes" | "1" => Some(true),
        "hide" | "hidden" | "off" | "false" | "no" | "0" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Cli, ConfigLayer, DebugSession, EnvironmentOverrides, FileConfig, LaunchConfig,
        RustToolchain, fallback_loaded_config, loaded_config_from_contents, parse_user_config,
        resolve_launch_config, resolve_pretty_printer_paths, validate_file_config,
    };
    use clap::Parser;
    use std::{path::PathBuf, sync::Arc};

    fn resolve(arguments: &[&str], contents: &str) -> LaunchConfig {
        let cli = Cli::try_parse_from(arguments).unwrap();
        let loaded =
            loaded_config_from_contents(PathBuf::from("/tmp/config.conf"), contents, false);
        resolve_launch_config(
            cli,
            &loaded,
            EnvironmentOverrides::default(),
            PathBuf::from("/current"),
        )
        .unwrap()
    }

    #[test]
    fn assembles_special_gef_startup_before_launch_target() {
        let mut configuration = LaunchConfig {
            gdb_executable: String::from("/usr/bin/gdb"),
            gdb_startup_arguments: vec![String::from("-ex"), String::from("init-gef-special")],
            gef_context_visible: false,
            source_paths: Vec::new(),
            pretty_printer_paths: Vec::new(),
            working_directory: PathBuf::from("/tmp"),
            safe_mode: false,
            breakpoint_auto_relocate: true,
            gcc_pretty_printer: None,
            rust_toolchain: Some(Arc::new(RustToolchain::with_printer_directory(
                "/opt/rust",
                "/opt/rust/lib/rustlib/etc",
            ))),
            initial_session: Some(DebugSession::Launch {
                executable: PathBuf::from("/tmp/debug target"),
                arguments: vec![String::from("arg")],
                environment: Vec::new(),
                working_directory: PathBuf::from("/tmp"),
            }),
            configuration_report: Arc::new(super::ConfigurationReport::default()),
        };

        assert_eq!(
            configuration.gdb_arguments(),
            [
                "/usr/bin/gdb",
                "--quiet",
                "--directory=/opt/rust/lib/rustlib/etc",
                "-iex",
                "add-auto-load-safe-path \"/opt/rust/lib/rustlib/etc\"",
                "-ex",
                "init-gef-special",
                "--args",
                "/tmp/debug target",
                "arg",
            ]
        );

        configuration.safe_mode = true;

        assert_eq!(
            configuration.gdb_arguments(),
            [
                "/usr/bin/gdb",
                "--quiet",
                "--nx",
                "--args",
                "/tmp/debug target",
                "arg",
            ]
        );
    }

    #[test]
    fn parses_a_positional_launch_and_preserves_trailing_options() {
        let configuration = resolve(
            &[
                "fgdb",
                "--working-directory",
                "/work",
                "/tmp/program",
                "--flag",
                "two words",
            ],
            "gdb=gdb\n",
        );

        assert_eq!(
            configuration.initial_session(),
            Some(DebugSession::Launch {
                executable: PathBuf::from("/tmp/program"),
                arguments: vec![String::from("--flag"), String::from("two words")],
                environment: Vec::new(),
                working_directory: PathBuf::from("/work"),
            })
        );

        assert!(configuration.needs_deferred_session_configuration());
    }

    #[test]
    fn parses_attach_core_and_remote_sessions() {
        assert_eq!(
            resolve(&["fgdb", "--attach", "42"], "gdb=gdb\n").initial_session(),
            Some(DebugSession::Attach {
                pid: 42,
                executable: None,
            })
        );

        assert_eq!(
            resolve(
                &["fgdb", "--core", "/tmp/core", "--executable", "/tmp/app",],
                "gdb=gdb\n",
            )
            .initial_session(),
            Some(DebugSession::CoreDump {
                executable: PathBuf::from("/tmp/app"),
                core_dump: PathBuf::from("/tmp/core"),
            })
        );

        assert_eq!(
            resolve(
                &[
                    "fgdb",
                    "--remote",
                    "localhost:1234",
                    "--executable",
                    "/tmp/app",
                ],
                "gdb=gdb\n",
            )
            .initial_session(),
            Some(DebugSession::Remote {
                endpoint: String::from("localhost:1234"),
                executable: Some(PathBuf::from("/tmp/app")),
                extended: false,
                remote_executable: None,
            })
        );
    }

    #[test]
    fn safe_mode_skips_configured_startup_arguments() {
        let configuration = resolve(
            &["fgdb", "--safe-mode", "/tmp/program"],
            "gdb=/usr/bin/gdb\ngdb_args=-ex init-gef-special\n",
        );

        assert_eq!(
            configuration.gdb_arguments(),
            ["/usr/bin/gdb", "--quiet", "--nx", "--args", "/tmp/program"]
        );
    }

    #[test]
    fn safe_mode_can_recover_from_malformed_startup_arguments() {
        let configuration = resolve(
            &["fgdb", "--safe-mode"],
            "gdb=/usr/bin/gdb\ngdb_args='unterminated\n",
        );

        assert_eq!(
            configuration.gdb_arguments(),
            ["/usr/bin/gdb", "--quiet", "--nx"]
        );
    }

    #[test]
    fn named_profiles_supply_sessions_and_can_be_overridden() {
        let contents = "gdb=/usr/bin/gdb\n[profile local]\nexecutable=/tmp/app\narguments=--count 4\nworking_directory=/tmp/project\n";
        let configuration = resolve(&["fgdb", "--profile", "local"], contents);

        assert_eq!(
            configuration.working_directory,
            PathBuf::from("/tmp/project")
        );

        assert_eq!(
            configuration.initial_session(),
            Some(DebugSession::Launch {
                executable: PathBuf::from("/tmp/app"),
                arguments: vec![String::from("--count"), String::from("4")],
                environment: Vec::new(),
                working_directory: PathBuf::from("/tmp/project"),
            })
        );
    }

    #[test]
    fn an_explicit_session_replaces_the_profile_session_type() {
        let contents = "gdb=gdb\n[profile attached]\nattach=42\nexecutable=/tmp/app\n";

        let configuration = resolve(
            &[
                "fgdb",
                "--profile",
                "attached",
                "--remote",
                "localhost:1234",
            ],
            contents,
        );

        assert_eq!(
            configuration.initial_session(),
            Some(DebugSession::Remote {
                endpoint: String::from("localhost:1234"),
                executable: Some(PathBuf::from("/tmp/app")),
                extended: false,
                remote_executable: None,
            })
        );
    }

    #[test]
    fn source_breakpoint_relocation_defaults_on_and_obeys_configuration_layers() {
        for (contents, profile, environment, expected) in [
            ("", None, None, true),
            (super::DEFAULT_CONFIG, None, None, true),
            ("breakpoint_auto_relocate=false\n", None, None, false),
            (
                "breakpoint_auto_relocate=true\n[profile exact]\nbreakpoint_auto_relocate=false\n",
                Some("exact"),
                None,
                false,
            ),
            ("breakpoint_auto_relocate=false\n", None, Some(true), true),
            ("", None, Some(false), false),
        ] {
            let mut cli = Cli::try_parse_from(["fgdb"]).unwrap();
            cli.profile = profile.map(str::to_owned);
            let loaded =
                loaded_config_from_contents(PathBuf::from("/tmp/fgdb.conf"), contents, false);
            let overrides = EnvironmentOverrides {
                layer: ConfigLayer {
                    breakpoint_auto_relocate: environment,
                    ..ConfigLayer::default()
                },
                ..EnvironmentOverrides::default()
            };
            let configuration =
                resolve_launch_config(cli, &loaded, overrides, PathBuf::from("/current")).unwrap();
            assert_eq!(configuration.breakpoint_auto_relocate, expected);
            assert!(configuration.configuration_report().issues().is_empty());

            assert_eq!(
                configuration
                    .configuration_report()
                    .effective()
                    .iter()
                    .find(|entry| entry.name() == "breakpoint_auto_relocate")
                    .unwrap()
                    .value(),
                expected.to_string(),
            );
        }

        let configuration = resolve(&["fgdb"], "# invalid\nbreakpoint_auto_relocate=perhaps\n");
        assert!(configuration.breakpoint_auto_relocate);
        assert_eq!(configuration.configuration_report().issues().len(), 1);
        assert_eq!(
            configuration.configuration_report().issues()[0].location(),
            "/tmp/config.conf:2",
        );
    }

    #[test]
    fn config_validation_rejects_unknown_duplicate_and_conflicting_settings() {
        assert!(parse_user_config("unknown=value\n").is_err());
        assert!(parse_user_config("gdb=gdb\ngdb_executable=/usr/bin/gdb\n").is_err());

        let config =
            parse_user_config("gdb=gdb\n[profile broken]\nattach=12\nremote=localhost:1234\n")
                .unwrap();

        assert!(validate_file_config(&config).is_err());
    }

    #[test]
    fn reports_every_parse_problem_with_its_file_and_line() {
        let loaded = loaded_config_from_contents(
            PathBuf::from("/tmp/fgdb.conf"),
            "gdb=/usr/bin/gdb\nunknown=value\nsafe_mode=perhaps\nattach=0\n",
            false,
        );

        assert_eq!(loaded.issues.len(), 3);
        assert_eq!(loaded.issues[0].location(), "/tmp/fgdb.conf:2");
        assert_eq!(loaded.issues[1].location(), "/tmp/fgdb.conf:3");
        assert_eq!(loaded.issues[2].location(), "/tmp/fgdb.conf:4");
        assert!(loaded.issues[0].message().contains("Unknown setting"));

        assert_eq!(
            loaded.config.defaults.gdb_executable.as_deref(),
            Some("/usr/bin/gdb")
        );

        assert_eq!(loaded.config.defaults.safe_mode, None);
        assert_eq!(loaded.config.defaults.attach, None);
    }

    #[test]
    fn file_failures_fall_back_to_defaults_and_remain_visible() {
        let loaded = fallback_loaded_config(
            PathBuf::from("/protected/fgdb/config.conf"),
            String::from("Could not read the configuration: Permission denied"),
        );

        assert!(loaded.loaded_paths.is_empty());
        assert_eq!(loaded.issues.len(), 1);
        assert_eq!(loaded.issues[0].location(), "/protected/fgdb/config.conf");
        assert!(loaded.issues[0].message().contains("Permission denied"));

        assert_eq!(
            loaded.config.defaults.gdb_executable.as_deref(),
            Some("gdb")
        );
    }

    #[test]
    fn invalid_file_values_are_excluded_from_the_effective_configuration() {
        let cli = Cli::try_parse_from(["fgdb"]).unwrap();

        let loaded = loaded_config_from_contents(
            PathBuf::from("/tmp/fgdb.conf"),
            "gdb=/usr/bin/gdb\ngdb_args='unterminated\nattach=41\nremote=localhost:1234\n",
            false,
        );

        let configuration = resolve_launch_config(
            cli,
            &loaded,
            EnvironmentOverrides::default(),
            PathBuf::from("/current"),
        )
        .unwrap();
        assert!(configuration.gdb_startup_arguments.is_empty());
        assert_eq!(configuration.initial_session(), None);
        assert_eq!(configuration.configuration_report().issues().len(), 2);

        assert_eq!(
            configuration
                .configuration_report()
                .effective()
                .iter()
                .find(|entry| entry.name() == "gdb")
                .map(super::EffectiveConfigurationEntry::value),
            Some("/usr/bin/gdb")
        );
    }

    #[test]
    fn reads_existing_global_configuration_aliases() {
        assert_eq!(
            parse_user_config(
                "# fgdb\ngdb=/usr/bin/gdb\ngdb_args=-ex init-gef-special\ngef_context=show\nsource_path='/src/one:/src/two'\n"
            )
            .unwrap(),
            FileConfig {
                defaults: ConfigLayer {
                    gdb_executable: Some(String::from("/usr/bin/gdb")),
                    gdb_startup_arguments: Some(String::from("-ex init-gef-special")),
                    gef_context_visible: Some(true),
                    source_paths: Some(vec![PathBuf::from("/src/one"), PathBuf::from("/src/two")]),
                    ..ConfigLayer::default()
                },
                ..FileConfig::default()
            }
        );
    }

    #[test]
    fn resolves_and_deduplicates_pretty_printer_scripts() {
        let root =
            std::env::temp_dir().join(format!("fgdb-config-printers-{}", std::process::id()));
        let script = root.join("printers.py");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(&script, "# test printer\n").unwrap();

        let (paths, errors) =
            resolve_pretty_printer_paths(vec![PathBuf::from("printers.py"), script.clone()], &root);

        assert!(errors.is_empty());
        assert_eq!(paths, [script.canonicalize().unwrap()]);

        let (_, errors) = resolve_pretty_printer_paths(vec![PathBuf::from("missing.py")], &root);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].0, PathBuf::from("missing.py"));

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reports_invalid_pretty_printer_paths_at_their_configuration_line() {
        let configuration = resolve(
            &["fgdb"],
            "gdb=gdb\npretty_printer_path=missing-printer.py\n",
        );
        let report = configuration.configuration_report();

        assert!(configuration.pretty_printer_paths.is_empty());
        assert!(report.issues().iter().any(|issue| {
            issue.location() == "/tmp/config.conf:2"
                && issue.message().contains("missing-printer.py")
        }));
    }

    #[test]
    fn configured_pretty_printer_scripts_are_quoted_as_startup_commands() {
        let root = std::env::temp_dir().join(format!(
            "fgdb-config-printer-command-{}",
            std::process::id()
        ));
        let script = root.join("user printer.py");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(&script, "# test printer\n").unwrap();
        let contents = format!("gdb=gdb\npretty_printer_path={}\n", script.display());
        let configuration = resolve(&["fgdb"], &contents);
        let expected = format!(
            "source {}",
            crate::debugger::gdb_cli_string(script.canonicalize().unwrap().to_str().unwrap())
                .unwrap()
        );

        assert!(
            configuration
                .gdb_arguments()
                .windows(2)
                .any(|arguments| arguments == ["-iex", expected.as_str()])
        );

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn help_and_version_are_terminal_actions() {
        for argument in ["--help", "--version"] {
            let error = Cli::try_parse_from(["fgdb", argument]).unwrap_err();

            assert!(matches!(
                error.kind(),
                clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
            ));
        }
    }

    #[test]
    fn unknown_frontend_options_fail_but_inferior_options_are_preserved() {
        assert!(Cli::try_parse_from(["fgdb", "--unknown"]).is_err());
        let cli = Cli::try_parse_from(["fgdb", "/tmp/app", "--unknown"]).unwrap();
        assert_eq!(cli.target.as_deref(), Some("/tmp/app"));
        assert_eq!(cli.target_arguments, ["--unknown"]);
    }
}
