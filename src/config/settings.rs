//! Editable preferences share the launch parser and preserve untouched config text.

use super::*;

#[derive(Clone, Copy)]
pub(crate) struct Choice {
    pub value: &'static str,
    pub label: &'static str,
}

macro_rules! choices {
    ($(#[$attribute:meta])* $name:ident { $($(#[$variant_attribute:meta])* $variant:ident => ($value:literal, $label:literal)),+ $(,)? }) => {
        $(#[$attribute])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub(crate) enum $name {
            $($(#[$variant_attribute])* $variant),+
        }

        impl $name {
            pub const CHOICES: &'static [Choice] = &[
                $(Choice { value: $value, label: $label }),+
            ];

            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $value),+
                }
            }

            pub fn parse(value: &str) -> Result<Self, String> {
                match value {
                    $($value => Ok(Self::$variant),)+
                    _ => Err(format!(
                        "Invalid choice '{value}'. Expected {}",
                        Self::CHOICES.iter()
                            .map(|choice| choice.value)
                            .collect::<Vec<_>>()
                            .join(", "),
                    )),
                }
            }
        }
    };
}

choices!(IntegerDisplay {
    Automatic => ("automatic", "Automatic"),
    Hexadecimal => ("hexadecimal", "Hexadecimal"),
    Decimal => ("decimal", "Decimal"),
    Both => ("both", "Hex + decimal"),
});

choices!(AssemblySyntax {
    Gdb => ("gdb", "GDB default"),
    Intel => ("intel", "Intel"),
    Att => ("att", "AT&T"),
});

choices!(CursorShape {
    Block => ("block", "Block"),
    Ibeam => ("ibeam", "I-beam"),
    Underline => ("underline", "Underline"),
});

choices!(CursorBlink {
    System => ("system", "System default"),
    On => ("on", "On"),
    Off => ("off", "Off"),
});

choices!(#[derive(Default)] SymbolDownloads {
    #[default]
    Gdb => ("gdb", "Follow GDB setting"),
    Ask => ("ask", "Ask before downloading"),
    On => ("on", "Allow on explicit symbol requests"),
    Off => ("off", "Local files only"),
});

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Preferences {
    pub restore_panel_windows: bool,
    pub symbol_downloads: SymbolDownloads,
    pub debug_file_directories: String,
    pub keybindings: keybindings::Bindings,
    pub source_font: String,
    pub source_tab_width: u32,
    pub source_wrap: bool,
    pub source_highlight_line: bool,
    pub source_auto_reload: bool,
    pub source_editor: String,
    pub terminal_font: String,
    pub terminal_scrollback: u32,
    pub integer_display: IntegerDisplay,
    pub instruction_bytes: bool,
    pub instruction_symbols: bool,
    pub instruction_source: bool,
    pub assembly_syntax: AssemblySyntax,
    pub terminal_cursor_shape: CursorShape,
    pub terminal_cursor_blink: CursorBlink,
    pub terminal_scroll_output: bool,
    pub terminal_scroll_typing: bool,
    pub log_info: bool,
    pub log_warnings: bool,
    pub log_errors: bool,
    pub log_follow: bool,
    pub log_wrap: bool,
}

impl Preferences {
    pub(super) fn entries(&self) -> Vec<EffectiveConfigurationEntry> {
        self.values()
            .into_iter()
            .map(|(key, value)| EffectiveConfigurationEntry::new(key, value))
            .collect()
    }

    fn values(&self) -> Values {
        let mut entries: Values = [
            (
                "restore_panel_windows",
                self.restore_panel_windows.to_string(),
            ),
            ("symbol_downloads", self.symbol_downloads.as_str().into()),
            (
                "debug_file_directories",
                self.debug_file_directories.clone(),
            ),
            ("source_font", self.source_font.clone()),
            ("source_tab_width", self.source_tab_width.to_string()),
            ("source_wrap", self.source_wrap.to_string()),
            ("source_auto_reload", self.source_auto_reload.to_string()),
            ("source_editor", self.source_editor.clone()),
            (
                "source_highlight_line",
                self.source_highlight_line.to_string(),
            ),
            ("terminal_font", self.terminal_font.clone()),
            ("terminal_scrollback", self.terminal_scrollback.to_string()),
            ("integer_display", self.integer_display.as_str().into()),
            ("instruction_bytes", self.instruction_bytes.to_string()),
            ("instruction_symbols", self.instruction_symbols.to_string()),
            ("instruction_source", self.instruction_source.to_string()),
            ("assembly_syntax", self.assembly_syntax.as_str().into()),
            (
                "terminal_cursor_shape",
                self.terminal_cursor_shape.as_str().into(),
            ),
            (
                "terminal_cursor_blink",
                self.terminal_cursor_blink.as_str().into(),
            ),
            (
                "terminal_scroll_output",
                self.terminal_scroll_output.to_string(),
            ),
            (
                "terminal_scroll_typing",
                self.terminal_scroll_typing.to_string(),
            ),
            ("log_info", self.log_info.to_string()),
            ("log_warnings", self.log_warnings.to_string()),
            ("log_errors", self.log_errors.to_string()),
            ("log_follow", self.log_follow.to_string()),
            ("log_wrap", self.log_wrap.to_string()),
        ]
        .into_iter()
        .collect();

        entries.extend(
            keybindings::Action::ALL
                .iter()
                .map(|&action| (action.config_key(), self.keybindings.value(action))),
        );

        entries
    }

    pub(super) fn from_layer(layer: &ConfigLayer) -> Self {
        Self {
            restore_panel_windows: layer.restore_panel_windows.unwrap_or(true),
            symbol_downloads: layer.symbol_downloads.unwrap_or_default(),
            debug_file_directories: layer.debug_file_directories.clone().unwrap_or_default(),
            keybindings: keybindings::Bindings::from_overrides(&layer.keybindings),
            source_font: layer
                .source_font
                .clone()
                .unwrap_or_else(|| "Monospace 9".into()),
            source_tab_width: layer.source_tab_width.unwrap_or(4),
            source_wrap: layer.source_wrap.unwrap_or(false),
            source_highlight_line: layer.source_highlight_line.unwrap_or(true),
            source_auto_reload: layer.source_auto_reload.unwrap_or(false),
            source_editor: layer.source_editor.clone().unwrap_or_default(),
            terminal_font: layer
                .terminal_font
                .clone()
                .unwrap_or_else(|| "Monospace 9.5".into()),
            terminal_scrollback: layer.terminal_scrollback.unwrap_or(20_000),
            integer_display: layer.integer_display.unwrap_or(IntegerDisplay::Automatic),
            instruction_bytes: layer.instruction_bytes.unwrap_or(true),
            instruction_symbols: layer.instruction_symbols.unwrap_or(true),
            instruction_source: layer.instruction_source.unwrap_or(false),
            assembly_syntax: layer.assembly_syntax.unwrap_or(AssemblySyntax::Gdb),
            terminal_cursor_shape: layer.terminal_cursor_shape.unwrap_or(CursorShape::Block),
            terminal_cursor_blink: layer.terminal_cursor_blink.unwrap_or(CursorBlink::On),
            terminal_scroll_output: layer.terminal_scroll_output.unwrap_or(false),
            terminal_scroll_typing: layer.terminal_scroll_typing.unwrap_or(true),
            log_info: layer.log_info.unwrap_or(true),
            log_warnings: layer.log_warnings.unwrap_or(true),
            log_errors: layer.log_errors.unwrap_or(true),
            log_follow: layer.log_follow.unwrap_or(true),
            log_wrap: layer.log_wrap.unwrap_or(true),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct LiveSettings {
    profile: Option<String>,
    overrides: ConfigLayer,
}

impl LiveSettings {
    pub(super) fn new(profile: Option<String>, overrides: ConfigLayer) -> Self {
        Self { profile, overrides }
    }

    pub(crate) fn resolve(&self, document: &Document) -> Result<(Preferences, bool), String> {
        if !document.issues.is_empty() {
            return Err(String::from(
                "Fix configuration issues before applying live preferences",
            ));
        }

        let mut layer = document.layer(self.profile.as_deref())?;
        layer.overlay(&self.overrides);

        Ok((
            Preferences::from_layer(&layer),
            layer.breakpoint_auto_relocate.unwrap_or(true),
        ))
    }
}

#[derive(Clone, Copy)]
pub(crate) enum Control {
    Text,
    Font,
    Toggle,
    Number { min: u32, max: u32 },
    Shortcut(keybindings::Action),
    Choice(&'static [Choice]),
}

#[derive(Clone, Copy)]
pub(crate) struct Setting {
    pub key: &'static str,
    pub title: &'static str,
    pub help: &'static str,
    pub page: &'static str,
    pub control: Control,
}

// The catalog supplies presentation metadata only. Validation stays in set_config_value.
pub(crate) const SETTINGS: &[Setting] = &[
    Setting {
        key: "restore_panel_windows",
        title: "Restore detached panels on startup",
        help: "Reopen detached panels on the next launch. Disabling this starts with all panels docked without moving windows in the current session",
        page: "Layout",
        control: Control::Toggle,
    },
    Setting {
        key: "source_font",
        title: "Source font",
        help: "Font family, style, and size for source code",
        page: "Appearance",
        control: Control::Font,
    },
    Setting {
        key: "source_tab_width",
        title: "Tab width",
        help: "Number of columns used to display a source tab",
        page: "Appearance",
        control: Control::Number { min: 1, max: 32 },
    },
    Setting {
        key: "source_wrap",
        title: "Wrap source lines",
        help: "Wrap long lines to the available source pane width",
        page: "Appearance",
        control: Control::Toggle,
    },
    Setting {
        key: "source_highlight_line",
        title: "Highlight cursor line",
        help: "Emphasize the source cursor line without changing the stopped-line marker",
        page: "Appearance",
        control: Control::Toggle,
    },
    Setting {
        key: "source_auto_reload",
        title: "Reload changed source",
        help: "Automatically reload externally edited source files. Otherwise show a Reload action. Reloading does not rebuild or replace the executable",
        page: "Appearance",
        control: Control::Toggle,
    },
    Setting {
        key: "source_editor",
        title: "External source editor",
        help: "Command with {file}, {line}, and {column} placeholders, for example code --goto {file}:{line}:{column}. Arguments are passed directly without a shell. Leave empty to use the system default editor",
        page: "Appearance",
        control: Control::Text,
    },
    Setting {
        key: "terminal_font",
        title: "Terminal font",
        help: "Font family, style, and size for the GDB terminal",
        page: "Terminal",
        control: Control::Font,
    },
    Setting {
        key: "terminal_scrollback",
        title: "Terminal history",
        help: "Maximum retained terminal lines. Reducing this discards older terminal output",
        page: "Terminal",
        control: Control::Number {
            min: 0,
            max: 1_000_000,
        },
    },
    Setting {
        key: "integer_display",
        title: "Integer format",
        help: "Presentation of plain integers in locals and watches. Automatic preserves debugger output with decimal hints. Pointers, enums, strings, and pretty-printer summaries remain unchanged",
        page: "Debugging",
        control: Control::Choice(IntegerDisplay::CHOICES),
    },
    Setting {
        key: "instruction_bytes",
        title: "Show instruction bytes",
        help: "Show the bytes column without changing the disassembly requests",
        page: "Instructions",
        control: Control::Toggle,
    },
    Setting {
        key: "instruction_symbols",
        title: "Show symbols",
        help: "Show the symbol column in the instruction table",
        page: "Instructions",
        control: Control::Toggle,
    },
    Setting {
        key: "instruction_source",
        title: "Show source annotations",
        help: "Default for the Source control. Fetches source annotations when the debugger can safely refresh disassembly",
        page: "Instructions",
        control: Control::Toggle,
    },
    Setting {
        key: "assembly_syntax",
        title: "Assembly syntax",
        help: "Preferred x86 syntax for new debugger backends. Existing sessions keep their toolbar choice",
        page: "Instructions",
        control: Control::Choice(AssemblySyntax::CHOICES),
    },
    Setting {
        key: "terminal_cursor_shape",
        title: "Cursor shape",
        help: "Shape of the terminal input cursor",
        page: "Terminal",
        control: Control::Choice(CursorShape::CHOICES),
    },
    Setting {
        key: "terminal_cursor_blink",
        title: "Cursor blinking",
        help: "Blink the input cursor or follow the desktop preference",
        page: "Terminal",
        control: Control::Choice(CursorBlink::CHOICES),
    },
    Setting {
        key: "terminal_scroll_output",
        title: "Scroll on output",
        help: "Scroll to the newest terminal output when it arrives",
        page: "Terminal",
        control: Control::Toggle,
    },
    Setting {
        key: "terminal_scroll_typing",
        title: "Scroll on typing",
        help: "Return to the terminal prompt when typing",
        page: "Terminal",
        control: Control::Toggle,
    },
    Setting {
        key: "log_info",
        title: "Show informational messages",
        help: "Default for the Info filter. Hidden messages remain in the bounded log history",
        page: "Log",
        control: Control::Toggle,
    },
    Setting {
        key: "log_warnings",
        title: "Show warnings",
        help: "Default for the Warnings filter. Hidden messages remain in the bounded log history",
        page: "Log",
        control: Control::Toggle,
    },
    Setting {
        key: "log_errors",
        title: "Show errors",
        help: "Default for the Errors filter. Hidden messages remain in the bounded log history",
        page: "Log",
        control: Control::Toggle,
    },
    Setting {
        key: "log_follow",
        title: "Follow new messages",
        help: "Initial Follow state. The toolbar can temporarily override it, and selecting text still pauses following",
        page: "Log",
        control: Control::Toggle,
    },
    Setting {
        key: "log_wrap",
        title: "Wrap long messages",
        help: "Wrap messages to the available width. Otherwise scroll horizontally to read long lines",
        page: "Log",
        control: Control::Toggle,
    },
    Setting {
        key: "breakpoint_auto_relocate",
        title: "Use next executable line",
        help: "Set source breakpoints at the next executable line when the clicked line has no code",
        page: "Debugging",
        control: Control::Toggle,
    },
    Setting {
        key: "symbol_downloads",
        title: "Debug information downloads",
        help: "Used only when explicitly resolving module symbols. Local loading never needs network consent",
        page: "Debugging",
        control: Control::Choice(SymbolDownloads::CHOICES),
    },
    Setting {
        key: "debug_file_directories",
        title: "Additional debug directories",
        help: "Absolute directories separated by ':', searched in addition to GDB's configured directories. Applies to the next symbol request",
        page: "Debugging",
        control: Control::Text,
    },
    Setting {
        key: "gdb",
        title: "GDB executable",
        help: "Executable name or full path. Takes effect on the next app launch",
        page: "Backend",
        control: Control::Text,
    },
    Setting {
        key: "gdb_args",
        title: "GDB arguments",
        help: "Shell-style quoting is supported. No shell is executed",
        page: "Backend",
        control: Control::Text,
    },
    Setting {
        key: "safe_mode",
        title: "Safe mode",
        help: "Ignore GDB init files, extra arguments, and startup printer scripts",
        page: "Backend",
        control: Control::Toggle,
    },
    Setting {
        key: "gef_context",
        title: "GEF terminal context",
        help: "Show GEF context output in the terminal when GEF is available",
        page: "Backend",
        control: Control::Toggle,
    },
    Setting {
        key: "source_path",
        title: "Source directories",
        help: "Search roots separated by ':' on Linux. Session-specific paths remain in Debug data",
        page: "Backend",
        control: Control::Text,
    },
    Setting {
        key: "pretty_printer_path",
        title: "Pretty-printer scripts",
        help: "Trusted Python scripts separated by ':' on Linux. Scripts execute with GDB's permissions",
        page: "Backend",
        control: Control::Text,
    },
    Setting {
        key: "rr",
        title: "rr executable",
        help: "Executable name or full path for rr replay",
        page: "Recording",
        control: Control::Text,
    },
    Setting {
        key: "record_full_limit",
        title: "Full instruction limit",
        help: "Default retained instructions for full recording",
        page: "Recording",
        control: Control::Number {
            min: 1,
            max: 10_000_000,
        },
    },
    Setting {
        key: "record_btrace_buffer_kib",
        title: "Trace buffer per thread (KiB)",
        help: "Default buffer size for branch-trace recording",
        page: "Recording",
        control: Control::Number {
            min: 4,
            max: 65_536,
        },
    },
];

pub(crate) fn settings() -> impl Iterator<Item = Setting> {
    SETTINGS
        .iter()
        .copied()
        .chain(keybindings::Action::ALL.iter().map(|&action| Setting {
            key: action.config_key(),
            title: action.title(),
            help: action.help(),
            page: "Keybindings",
            control: Control::Shortcut(action),
        }))
}

pub(crate) type Values = BTreeMap<&'static str, String>;

#[derive(Clone)]
pub(crate) struct Document {
    pub text: String,
    config: FileConfig,
    pub issues: Vec<ConfigurationIssue>,
}

impl Document {
    pub(crate) fn parse(text: String, path: &Path) -> Result<Self, String> {
        if text.len() > MAX_CONFIG_BYTES || text.contains('\0') {
            return Err(String::from(
                "Configuration must be valid text of at most 64 KiB without NUL bytes",
            ));
        }

        let mut parsed = parse_user_config_with_diagnostics(&text, path);
        collect_validation_issues(&parsed.config, &parsed.locations, path, &mut parsed.issues);

        Ok(Self {
            text,
            config: parsed.config,
            issues: parsed.issues,
        })
    }

    pub(crate) fn profiles(&self) -> impl Iterator<Item = &str> {
        self.config.profiles.keys().map(String::as_str)
    }

    fn layer(&self, profile: Option<&str>) -> Result<ConfigLayer, String> {
        let mut layer = self.config.defaults.clone();

        if let Some(profile) = profile {
            let overlay = self.config.profiles.get(profile).ok_or_else(|| {
                format!("Profile '{profile}' is no longer in the configuration file")
            })?;
            layer.overlay(overlay);
        }

        Ok(layer)
    }

    pub(crate) fn values(&self, profile: Option<&str>) -> Result<Values, String> {
        let layer = self.layer(profile)?;
        let preferences = Preferences::from_layer(&layer);
        let replay = ReplayConfig::default();
        let paths = |paths: Option<Vec<PathBuf>>| {
            env::join_paths(paths.unwrap_or_default())
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        };

        let mut values = BTreeMap::from([
            (
                "breakpoint_auto_relocate",
                layer.breakpoint_auto_relocate.unwrap_or(true).to_string(),
            ),
            ("gdb", layer.gdb_executable.unwrap_or_else(|| "gdb".into())),
            ("gdb_args", layer.gdb_startup_arguments.unwrap_or_default()),
            ("safe_mode", layer.safe_mode.unwrap_or(false).to_string()),
            (
                "gef_context",
                layer.gef_context_visible.unwrap_or(false).to_string(),
            ),
            ("source_path", paths(layer.source_paths)),
            ("pretty_printer_path", paths(layer.pretty_printer_paths)),
            ("rr", layer.rr_executable.unwrap_or(replay.rr_executable)),
            (
                "record_full_limit",
                layer
                    .record_full_limit
                    .unwrap_or(replay.full_instruction_limit)
                    .to_string(),
            ),
            (
                "record_btrace_buffer_kib",
                layer
                    .record_btrace_buffer_kib
                    .unwrap_or(replay.btrace_buffer_kib)
                    .to_string(),
            ),
        ]);

        values.extend(preferences.values());

        Ok(values)
    }

    pub(crate) fn patch(&self, profile: Option<&str>, changes: &Values) -> Result<String, String> {
        let mut layer = self.layer(profile)?;

        for (&key, value) in changes {
            if !settings().any(|setting| setting.key == key) || value.contains(['\0', '\n', '\r']) {
                return Err(format!("Invalid value for '{key}'"));
            }

            set_config_value(&mut layer, key, value)?;
        }

        // Shell quoting is checked even when safe mode currently masks the arguments.
        if let Some(arguments) = changes.get("gdb_args") {
            shell_words::split(arguments)
                .map_err(|error| format!("Invalid GDB arguments: {error}"))?;
        }

        let newline = if self.text.contains("\r\n") {
            "\r\n"
        } else {
            "\n"
        };

        let mut result = String::with_capacity(self.text.len() + 256);
        let mut remaining = changes.clone();
        let mut in_scope = profile.is_none();

        for raw in self.text.split_inclusive('\n') {
            let line = raw.trim();

            if line.starts_with('[') {
                if in_scope {
                    append_values(&mut result, &mut remaining, newline);
                }

                in_scope = parse_profile_header(line)
                    .ok()
                    .is_some_and(|name| Some(name.as_str()) == profile);
            }

            let key = line
                .split_once('=')
                .and_then(|(key, _)| canonical_config_key(key.trim()));

            if in_scope
                && let Some(key) = key
                && let Some(value) = changes.get(key)
            {
                // Collapse duplicate instances of an edited key instead of leaving an ineffective value.
                if remaining.remove(key).is_some() {
                    result.push_str(&format!("{key}={value}{newline}"));
                }
            } else {
                result.push_str(raw);
            }
        }

        append_values(&mut result, &mut remaining, newline);

        if changes
            .keys()
            .any(|key| keybindings::Action::from_config_key(key).is_some())
        {
            // Changing defaults can also create a conflict in an inheriting profile.
            let updated = parse_user_config_with_diagnostics(&result, Path::new("<settings>"));
            let profiles = std::iter::once(None).chain(updated.config.profiles.values().map(Some));

            for profile in profiles {
                let mut layer = updated.config.defaults.clone();

                if let Some(profile) = profile {
                    layer.overlay(profile);
                }

                if let Some((first, second)) =
                    keybindings::Bindings::from_overrides(&layer.keybindings).conflict()
                {
                    return Err(format!(
                        "'{}' and '{}' use the same shortcut. Choose another binding or disable one, including in inherited profiles",
                        first.title(),
                        second.title()
                    ));
                }
            }
        }

        if result.len() > MAX_CONFIG_BYTES {
            return Err(String::from(
                "Updated configuration exceeds the 64 KiB limit",
            ));
        }

        Ok(result)
    }
}

fn append_values(result: &mut String, values: &mut Values, newline: &str) {
    if !values.is_empty() && !result.is_empty() && !result.ends_with('\n') {
        result.push_str(newline);
    }

    for (key, value) in std::mem::take(values) {
        result.push_str(&format!("{key}={value}{newline}"));
    }
}

pub(super) fn validate_font(value: &str) -> Result<(), String> {
    const INVALID_FONT: &str =
        "Use a font family and size between 6 and 48, for example Monospace 10";

    // Reject malformed input before passing it to the native font parser.
    if value.len() > 256 || value.chars().any(char::is_control) {
        return Err(INVALID_FONT.into());
    }

    let font = gtk::pango::FontDescription::from_string(value);
    let size = f64::from(font.size()) / f64::from(gtk::pango::SCALE);

    if font.family().is_none() || !(6.0..=48.0).contains(&size) {
        return Err(INVALID_FONT.into());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(text: &str) -> Document {
        Document::parse(text.into(), Path::new("config.conf")).unwrap()
    }

    #[test]
    fn patches_only_edited_keys_in_the_selected_scope() {
        let original = "# Keep this\r\ngdb_executable=gdb\r\nfuture_setting=keep\r\n[profile local]\r\nsource_font=Monospace 11\r\narguments=--help\r\n[profile other]\r\nsource_font=Monospace 8";
        let config = document(original);
        let edits = Values::from([("source_font", "Monospace 12".into())]);
        let updated = config.patch(Some("local"), &edits).unwrap();
        assert_eq!(updated, original.replace("Monospace 11", "Monospace 12"));
        let updated = config.patch(None, &edits).unwrap();
        assert_eq!(
            updated,
            original.replace(
                "[profile local]",
                "source_font=Monospace 12\r\n[profile local]"
            )
        );

        assert!(config.patch(Some("missing"), &edits).is_err());

        let duplicate = document("gdb=gdb\ngdb_executable=old\n[broken]\nsource_font=Monospace 8");
        let edits = Values::from([
            ("gdb", "/usr/bin/gdb".into()),
            ("source_font", "Monospace 10".into()),
        ]);
        assert_eq!(
            duplicate.patch(None, &edits).unwrap(),
            "gdb=/usr/bin/gdb\nsource_font=Monospace 10\n[broken]\nsource_font=Monospace 8"
        );
    }

    #[test]
    fn catalog_round_trips_through_launch_validation_and_rejects_unsafe_edits() {
        let config = document("");
        let values = config.values(None).unwrap();
        assert_eq!(values.len(), settings().count());
        let all = document(&config.patch(None, &values).unwrap());
        assert!(all.issues.is_empty(), "{:?}", all.issues);
        assert_eq!(all.values(None).unwrap(), values);

        for (key, value) in [
            ("source_font", "Monospace 0"),
            ("source_font", "Monospace 49"),
            ("source_wrap", "maybe"),
            ("restore_panel_windows", "maybe"),
            ("integer_display", "binary"),
            ("assembly_syntax", "unknown"),
            ("terminal_cursor_shape", "circle"),
            ("terminal_cursor_blink", "true"),
            ("log_follow", "sometimes"),
            ("source_tab_width", "0"),
            ("terminal_scrollback", "1000001"),
            ("gdb_args", "'unterminated"),
            ("source_font", "Monospace 10\nsafe_mode=false"),
            ("gdb", "gdb\0"),
            ("unregistered", "value"),
        ] {
            assert!(
                config
                    .patch(None, &Values::from([(key, value.into())]))
                    .is_err(),
                "{key}={value}"
            );
        }

        assert!(
            Document::parse("x".repeat(MAX_CONFIG_BYTES + 1), Path::new("config.conf")).is_err()
        );

        for value in ["Monospace 10\0", "Monospace\t10", &"x".repeat(257)] {
            assert!(validate_font(value).is_err());
        }

        for setting in settings() {
            if let Control::Choice(choices) = setting.control {
                for choice in choices {
                    let edits = Values::from([(setting.key, choice.value.into())]);
                    let patched = document(&config.patch(None, &edits).unwrap());
                    assert!(patched.issues.is_empty());
                    assert_eq!(patched.values(None).unwrap()[setting.key], choice.value);
                }
            }
        }

        let inherited = document(
            "integer_display=decimal\ninstruction_bytes=false\nlog_info=false\nterminal_cursor_shape=ibeam\n[profile quiet]\nlog_follow=false\n",
        );
        let inherited_values = inherited.values(Some("quiet")).unwrap();
        assert_eq!(inherited_values["integer_display"], "decimal");
        assert_eq!(inherited_values["instruction_bytes"], "false");
        assert_eq!(inherited_values["log_info"], "false");
        assert_eq!(inherited_values["terminal_cursor_shape"], "ibeam");
        assert_eq!(inherited_values["log_follow"], "false");
    }

    #[test]
    fn live_preferences_preserve_profile_and_environment_precedence() {
        let config = document(
            "source_font=Monospace 9\nbreakpoint_auto_relocate=true\n[profile local]\nsource_font=Monospace 12\nbreakpoint_auto_relocate=false\n",
        );

        let mut overrides = ConfigLayer::default();
        set_config_value(&mut overrides, "source_font", "Monospace 14").unwrap();
        let resolver = LiveSettings::new(Some("local".into()), overrides);
        let (preferences, relocate) = resolver.resolve(&config).unwrap();
        assert_eq!(preferences.source_font, "Monospace 14");
        assert!(!relocate);
        assert!(
            resolver
                .resolve(&document("source_font=Monospace 9"))
                .is_err()
        );

        assert!(
            resolver
                .resolve(&document("[profile local]\nsource_tab_width=0"))
                .is_err()
        );
    }

    #[test]
    fn keybinding_edits_validate_inherited_profiles_and_live_reload() {
        let config = document("# Keep this\nkeybind_run=F8\n[profile local]\nkeybind_pause=F9\n");
        let edits = Values::from([("keybind_run", "F9".into())]);
        assert!(config.patch(None, &edits).is_err());

        let edits = Values::from([("keybind_run", "Disabled".into())]);
        let updated = document(&config.patch(Some("local"), &edits).unwrap());
        assert!(updated.issues.is_empty());
        assert_eq!(updated.values(None).unwrap()["keybind_run"], "F8");
        assert_eq!(
            updated.values(Some("local")).unwrap()["keybind_run"],
            "Disabled"
        );

        let resolver = LiveSettings::new(Some("local".into()), ConfigLayer::default());
        let (preferences, _) = resolver.resolve(&updated).unwrap();
        assert_eq!(
            preferences.keybindings.value(keybindings::Action::Run),
            "Disabled"
        );
        assert_eq!(
            preferences.keybindings.value(keybindings::Action::Pause),
            "F9"
        );
        let invalid = document("keybind_run=F6\n[profile local]\nsource_tab_width=4\n");
        assert!(!invalid.issues.is_empty());
        assert!(resolver.resolve(&invalid).is_err());
    }
}
