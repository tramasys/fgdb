<p align="center">
  <img src="assets/icons/dev.fgdb.Fgdb.png" width="256" alt="fgdb raven logo">
</p>

<h1 align="center">fgdb</h1>

## Command line

```text
fgdb [OPTIONS] [EXECUTABLE] [ARGUMENT]...
```

Common startup forms:

```sh
fgdb ./program --flag "argument with spaces"
fgdb --attach 1234
fgdb --attach 1234 --executable ./program
fgdb --core ./core.1234 --executable ./program
fgdb --remote localhost:1234 --executable ./program
fgdb --working-directory ./project ./build/program
fgdb --profile local
fgdb --safe-mode ./program
fgdb --check-config
```

Run `fgdb --help` for the complete option list. fgdb options must appear before the positional executable. Everything after the executable is passed to the inferior unchanged.

## Configuration profiles

fgdb reads `$XDG_CONFIG_HOME/fgdb/config.conf`, or `~/.config/fgdb/config.conf` when `XDG_CONFIG_HOME` is unset. Environment variables override file settings, named profiles override global file settings, and command-line options have the highest priority.

```ini
gdb=/usr/bin/gdb
gdb_args=-ex init-gef-special
source_path=/workspace/src:/workspace/generated
gef_context=hide
safe_mode=false

[profile local]
executable=/workspace/build/program
arguments=--server localhost --port 9000
working_directory=/workspace

[profile crash]
executable=/workspace/build/program
core=/tmp/core.program

[profile device]
executable=/workspace/build/program
remote=localhost:1234
```

Profiles also accept `attach=PID`. A profile can override `gdb`, `gdb_args`, `source_path`, `gef_context`, `safe_mode`, and `working_directory`. `FGDB_PROFILE`, `FGDB_SAFE_MODE`, and `FGDB_WORKING_DIRECTORY` provide environment equivalents for the new global options.

Safe mode passes `--nx` to GDB and ignores configured GDB startup arguments for that invocation.

The Session menu includes Configuration diagnostics with the loaded file list, file and line specific errors, and the effective merged settings. Invalid file entries are ignored safely while valid settings remain active. Use Open active config to edit the selected file with the desktop editor.
