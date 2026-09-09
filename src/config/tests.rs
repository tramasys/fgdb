use super::{
    Cli, ConfigLayer, DebugSession, EnvironmentOverrides, FileConfig, LaunchConfig, RustToolchain,
    fallback_loaded_config, loaded_config_from_contents, parse_user_config, resolve_launch_config,
    resolve_pretty_printer_paths, validate_file_config,
};
use clap::Parser;
use std::{path::PathBuf, sync::Arc};

fn resolve(arguments: &[&str], contents: &str) -> LaunchConfig {
    let cli = Cli::try_parse_from(arguments).unwrap();
    let loaded = loaded_config_from_contents(PathBuf::from("/tmp/config.conf"), contents, false);
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
        preferences: super::settings::Preferences::from_layer(&super::ConfigLayer::default()),
        live_settings: super::settings::LiveSettings::new(None, super::ConfigLayer::default()),
        replay: super::ReplayConfig::default(),
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
            "add-auto-load-safe-path /opt/rust/lib/rustlib/etc",
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
fn rr_sessions_and_recording_configuration_are_explicit_and_bounded() {
    let configuration = resolve(
        &["fgdb", "--rr", "trace with spaces"],
        "rr=/opt/rr/bin/rr\nrecord_full_limit=400000\nrecord_btrace_buffer_kib=128\n",
    );
    assert_eq!(
        configuration.initial_session(),
        Some(DebugSession::RrReplay {
            trace_directory: PathBuf::from("/current/trace with spaces"),
        })
    );
    assert_eq!(configuration.replay.rr_executable, "/opt/rr/bin/rr");
    assert_eq!(configuration.replay.full_instruction_limit, 400_000);
    assert_eq!(configuration.replay.btrace_buffer_kib, 128);
    assert!(configuration.needs_deferred_session_configuration());

    for args in [
        vec!["fgdb", "--rr", "trace", "--attach", "42"],
        vec!["fgdb", "--rr", "trace", "/tmp/program"],
        vec!["fgdb", "--rr", "trace", "--executable", "/tmp/program"],
    ] {
        let cli = Cli::try_parse_from(args).unwrap();
        let loaded = loaded_config_from_contents(PathBuf::from("/tmp/config.conf"), "", false);
        assert!(
            resolve_launch_config(
                cli,
                &loaded,
                EnvironmentOverrides::default(),
                PathBuf::from("/current")
            )
            .is_err()
        );
    }

    let configuration = resolve(
        &["fgdb"],
        "record_full_limit=0\nrecord_btrace_buffer_kib=4294967295\n",
    );
    assert_eq!(configuration.configuration_report().issues().len(), 2);
    assert_eq!(configuration.replay.full_instruction_limit, 200_000);
    assert_eq!(configuration.replay.btrace_buffer_kib, 64);
    assert!(super::set_config_value(&mut ConfigLayer::default(), "rr", "rr\0unexpected").is_err());
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
        let loaded = loaded_config_from_contents(PathBuf::from("/tmp/fgdb.conf"), contents, false);
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
        parse_user_config("gdb=gdb\n[profile broken]\nattach=12\nremote=localhost:1234\n").unwrap();

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
    let root = std::env::temp_dir().join(format!("fgdb-config-printers-{}", std::process::id()));
    let script = root.join("printers.py");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(&script, "# test printer\n").unwrap();

    let (paths, errors) =
        resolve_pretty_printer_paths(vec![PathBuf::from("printers.py"), script.clone()], &root);

    assert!(errors.is_empty());
    assert_eq!(paths, [script.canonicalize().unwrap()]);
    let validated = crate::language::scripts::PrinterScript::resolve(&script).unwrap();

    assert_eq!(
        validated.command(),
        format!("source {}", paths[0].display())
    );
    assert_eq!(validated.path(), paths[0]);

    let (_, errors) = resolve_pretty_printer_paths(vec![PathBuf::from("missing.py")], &root);
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].0, PathBuf::from("missing.py"));

    let invalid = root.join("line\nbreak.py");
    std::fs::write(&invalid, "# test printer\n").unwrap();

    let (paths, errors) = resolve_pretty_printer_paths(vec![root.clone(), invalid], &root);
    assert!(paths.is_empty());
    assert_eq!(errors.len(), 2);
    assert!(errors[0].1.contains("regular file"));
    assert!(errors[1].1.contains("unsupported characters"));

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
        issue.location() == "/tmp/config.conf:2" && issue.message().contains("missing-printer.py")
    }));
}

#[test]
fn configured_pretty_printer_scripts_use_literal_source_paths() {
    let root = std::env::temp_dir().join(format!(
        "fgdb-config-printer-command-{}",
        std::process::id()
    ));
    let script = root.join("user's \"printer\".py");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(&script, "# test printer\n").unwrap();
    let contents = format!("gdb=gdb\npretty_printer_path={}\n", script.display());
    let configuration = resolve(&["fgdb"], &contents);
    let expected = format!("source {}", script.canonicalize().unwrap().display());

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
