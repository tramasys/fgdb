# fgdb

A native GDB frontend built with Rust, GTK4, GtkSourceView, and VTE. It keeps a real interactive GDB/GEF terminal while a second GDB/MI interface drives native debugger controls and state panels.

## Current prototype

- Native dark GTK4 interface
- Real PTY-backed GDB terminal; normal `.gdbinit` and GEF configuration are preserved
- Separate GDB/MI 2 channel connected to the same GDB process
- Run, pause, source/instruction stepping, finish, and a GEF-backed Run Until menu, with F5/F6/F10/F11 shortcuts
- Syntax-highlighted, reorderable source tabs with per-file scroll and cursor state
- Multi-file picker, DevTools-style clickable breakpoint line numbers with enabled/disabled states, and Ctrl-click source-symbol navigation with link highlighting and definition ranking through GDB debug info
- Automatic initial-source detection from executable debug information, source-tab navigation, and a current-instruction arrow when GDB stops
- Live call stack, detailed threads, loaded-module/symbol status, typed expandable variable trees with explicit loading/error rows, compact signal and C++/Rust/process catchpoint controls, instruction context, conditional breakpoints, hardware watchpoints, and category-safe bulk deletion controls
- Branch/call classification, guessed x86-64 call arguments, and memory previews for the current instruction
- EFLAGS/RFLAGS-based x86 branch prediction and common Linux x86-64 syscall/ABI argument decoding
- A categorized GEF tools menu for `xinfo`, `telescope`, `dt`, syscall arguments, future calls, memory maps, binary protections, unwind data, TLS, stack canaries, GOT/PLT, deeper heap inspection, errno, fork behavior, file descriptors, and the ELF auxiliary vector; results stay in the real terminal session
- Truly lazy expandable locals with full-name click targets, keyboard expansion, and no child queries until a value is opened
- Grouped GEF-style general, segment, vector, and floating-point registers with symbols, pointer chains, ASCII hints, loop detection, and decoded flags
- Click-to-edit scalar registers, a bitwise flags editor, and typed per-lane SIMD editors for integer and floating-point views
- Scoped, bounded automatic value previews that avoid scanning large strings or arrays while preserving the terminal's GDB/GEF print settings
- Persistent memory watches with independently selectable address, raw-value, and decoded-value columns, byte/word/pointer views, and a native `/proc` memory map
- Native GEF-style context for code, source, stack memory, threads, and trace/backtrace data
- Draggable workspace columns, a vertically resizable locals/instructions split, remembered window size and pane positions across launches, a collapsible session panel, and a terminal visibility toggle
- Resizable and reorderable instruction columns for full addresses, opcodes, operands, bytes, and symbols
- Centralized theme tokens and terminal palette
- Optional Vulkan-backed GTK renderer

The debugger panels refresh after stop and selection notifications, including actions entered directly in the embedded GEF terminal.
While the inferior is running, stop-specific panels are dimmed and editing is disabled so values from the previous stop cannot be mistaken for live state. The bottom status bar keeps command progress and actionable errors visible without requiring the Context page to remain open.

Default execution shortcuts are **F5** for Run/Continue, **F6** for Pause, **F10** for Next, **F11** for Step, **Ctrl+F10/F11** for instruction-level next/step, and **Shift+F11** for Finish. Double-click an instruction to toggle its address breakpoint. Press **Escape** to dismiss value, register, flag, and breakpoint-condition editors.

Source files are resolved through GDB's `fullname`, the current working directory, Rust's active sysroot sources, `/usr/src/debug`, and the debuginfod cache. Extra source roots can be supplied as a platform path list:

```bash
FGDB_SOURCE_PATH=/path/to/glibc:/path/to/other/sources cargo run -- /path/to/program
```

Library source still requires matching debug information and source files. For Rust standard-library stepping, install the source component with `rustup component add rust-src`. On distributions using separate debug-source packages, install the matching package or enable GDB debuginfod.

If your GDB configuration uses `set auto-solib-add off`, press **Load libs** after the program's first stop and before stepping into glibc or another shared library. This runs GDB's `sharedlibrary` command without changing your persistent configuration.

## Requirements

- Rust 1.98.0 (selected automatically by `rust-toolchain.toml`)
- GTK 4.22 or newer
- GtkSourceView 5.18 or newer
- VTE for GTK4 0.84 or newer
- GDB with the `new-ui` command and MI2 support

On Arch Linux:

```bash
sudo pacman -S --needed gtk4 gtksourceview5 vte4 gdb
```

## Run

Start without a target:

```bash
cargo run
```

Start GDB with a target and its arguments:

```bash
cargo run -- /path/to/program argument-one argument-two
```

Use a custom GDB build—such as the one configured for your GEF installation:

```bash
FGDB_GDB=/path/to/custom/gdb cargo run -- /path/to/program
```

Add startup arguments with shell-style quoting. For a shell alias such as
`gdbgs='gdb -q -ex init-gef-special'`, use the underlying executable and arguments directly:

```bash
FGDB_GDB=/usr/bin/gdb \
FGDB_GDB_ARGS='-ex init-gef-special' \
cargo run -- /path/to/program
```

The application already supplies GDB's `--quiet` option, equivalent to `-q`. Shell aliases are not available to child-process spawning, so the alias itself should not be placed in `FGDB_GDB`. The old `GDB_UI_GDB` and `GDB_UI_GDB_ARGS` names remain supported as compatibility fallbacks.

Ask GTK to use its Vulkan renderer:

```bash
GSK_RENDERER=vulkan cargo run --release -- /path/to/program
```

Renderer selection is intentionally not forced. GTK can choose the best available renderer by default, while Vulkan remains available to users and packagers whose GTK build and graphics driver support it.

## Architecture

```text
GTK application
├── GtkSourceView and native debugger panels
├── VTE terminal ── console PTY ──┐
└── MI client ───── MI PTY ───────┴── one GDB process ── inferior
```

GDB starts in its normal console interpreter inside VTE. Once it is running, the application sends `new-ui mi2 <pty>` through the console and uses that secondary channel for machine-readable commands and asynchronous state notifications.

The Rust code is split by responsibility: `app.rs` coordinates debugger events, `debugger/mi.rs` owns the MI transport and parser, `debugger/model.rs` converts MI records into UI models, `debugger/context.rs` derives GEF-style memory context, `source.rs` resolves debug-info paths, and `ui.rs` builds and updates GTK widgets.

Theme colors live in `src/theme.rs`; widgets consume semantic GTK CSS colors rather than embedding their own palette. A future theme loader can replace the built-in `Theme` value without restructuring the UI.
