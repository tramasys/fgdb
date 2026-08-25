# C++ debugger target

Build the deliberately debugger-friendly target with:

```bash
make -C examples
```

Then launch it in fgdb:

```bash
FGDB_GDB=/usr/bin/gdb \
FGDB_GDB_ARGS='-ex init-gef-special' \
cargo run -- target/debug-fixtures/cpp-debug-target
```

Useful exercises:

- Break at `debugger_checkpoint` to inspect every integer family, nested structs, an optional, a variant, STL containers, smart pointers, a cyclic pointer graph, and a 1.3 MB heap allocation.
- Add `watch watched_value`, or use the Watchpoints UI, then continue through `update_watch_value`.
- Add the conditional breakpoint `break update_watch_value if iteration == 5`.
- Inspect the main and worker threads while stopped at `debugger_checkpoint`.
- Continue to `SIGUSR1` to exercise signal handling. Pass `--no-signal` to suppress it.
- Use `catch throw` to stop on the intentional C++ exception.
- Step into `malloc`, `memset`, `sort`, `free`, or the local no-inline functions.
- Add a memory watch for `large_allocation` while stopped in `main` or `debugger_checkpoint`.

The default flags intentionally disable optimization and inlining, preserve frame pointers, and include maximal debug information. Override `CXX` or `CXXFLAGS` when testing optimized code or Clang-generated DWARF.
