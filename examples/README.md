# Debugger fixtures

Build every deliberately debugger-friendly binary with:

```bash
make -C examples
```

Launch one with, for example:

```bash
FGDB_GDB=/usr/bin/gdb \
FGDB_GDB_ARGS='-ex init-gef-special' \
cargo run -- target/debug-fixtures/c-page-target
```

| Binary | Primary breakpoint | Exercises |
| --- | --- | --- |
| `c-allocation-target` | `c_allocation_checkpoint` | Plain `malloc`/`calloc` allocations and a one-MiB anonymous mapping progressing from reserved to sparse, fully resident and reclaimed |
| `c-memory-target` | `c_memory_checkpoint` | Anonymous/shared mappings, a guard page, heap data, memfd, pipes, UNIX sockets, eventfd and epoll |
| `c-page-target` | `c_page_checkpoint` | Three page-lifecycle stops: sparse residency, soft-dirty pages, shared/private file pages, `mlock`, huge-page advice and reclaim |
| `c-tls-target` | `c_tls_checkpoint` | Per-thread initialized and zero-filled TLS, aggregate TLS objects, thread-pointer bases and distinct values across named pthreads |
| `c-thread-target` | `c_threads_checkpoint` | Named pthreads in futex/poll waits, per-thread signal masks, a pending signal and atomic values |
| `c-process-target` | `c_process_checkpoint` | Fork catchpoints and a live parent/child/grandchild hierarchy |
| `c-misc-auxv-target` | `c_misc_auxv_checkpoint` | Misc → Args / Env and Auxv with arguments, environment ranges, entry metadata, HWCAP and vDSO values |
| `c-misc-frame-target` | `c_misc_frame_checkpoint` | Misc → Call ABI with incoming integer/pointer slots at function entry and nested call/return boundaries |
| `c-misc-allocator-target` | `c_misc_allocator_checkpoint` | Misc → Allocator with brk allocations, a large allocator mapping and an explicit anonymous mapping |
| `c-misc-locks-target` | `c_misc_locks_checkpoint` | Misc → Locks with two waiters sharing one futex address and a third waiter on another address |
| `c-misc-core-target` | `c_misc_core_checkpoint` | Misc → Core dump with an intentional SIGSEGV, nested crash frames, heap state and anonymous mappings |
| `cpp-debug-target` | `debugger_checkpoint` | Integer families, STL containers, structs, pointer cycles, watchpoints, threads, signals and exceptions |
| `cpp-object-target` | `cpp_objects_checkpoint` | Virtual dispatch, multiple inheritance, templates, smart pointers, variants, lambdas and nested exceptions |
| `cpp-variable-viewer-target` | `variable_viewer_checkpoint` | Locals/arguments context actions, native and standard arrays, STL sequences, null-terminated lists and cyclic lists |
| `rust-debug-target` | `rust_debugger_checkpoint` | Rust enums, `Option`, `Result`, collections, trait objects, `Rc` cycles, primitives, Unicode and a named worker thread |
| `rust-variable-viewer-target` | `rust_types_ready` (then caller frame #1) | Rust locals and arguments containing `Vec`, `VecDeque`, linked lists, maps, sets, strings, slices, smart pointers, enums and nested user types |
| `cpp-debug-target-o2` | `debugger_checkpoint` | Optimized C++ stepping, inlined/optimized-out values and less direct source mappings |
| `rust-debug-target-o2` | `rust_debugger_checkpoint` | Optimized Rust DWARF, monomorphized frames and optimized-out values |

## Simple allocation fixture

Set `break c_allocation_checkpoint`, then continue through these five stops:

1. `reserved` has three ordinary heap allocations and an untouched one-MiB anonymous mapping.
2. `first-page` writes one mapping page and small portions of the heap allocations.
3. `sparse` writes one byte in every fourth mapping page.
4. `fully-used` fills the complete mapping and heap allocations.
5. `reclaimed` discards the middle half of the mapping with `MADV_DONTNEED`.

The checkpoint reports mapping residency using `mincore`. Compare its counter with **Kernel → Memory**, then use **Kernel → Maps** to inspect the exact anonymous mapping and its exclusive RSS.

## Page lifecycle fixture

Set `break c_page_checkpoint`, then continue through all three hits:

1. `reserved` has largely non-resident anonymous and file-backed mappings.
2. `populated` touches alternating anonymous pages, dirties shared and copy-on-write file pages, and attempts to lock one page.
3. `reclaimed` applies `MADV_DONTNEED` and exposes the resulting stop-to-stop RSS/PSS and page-state changes.

At each stop, inspect **Kernel → Changes** for deltas and **Kernel → Maps** for RSS/PSS, NUMA placement, VM flags and pagemap samples. The fixture resets its own soft-dirty tracking through `/proc/self/clear_refs`. Failure to do so is intentionally non-fatal on restricted systems.

The default targets use `-O0`, full debug information, frame pointers and disabled C/C++ inlining. The `-o2` variants deliberately retain optimization. Override `CC`, `CXX`, `RUSTC`, or the corresponding flags to compare toolchains and DWARF versions.

## Variable viewer fixture

Launch the focused locals/arguments fixture with:

```bash
cargo run -- target/debug-fixtures/cpp-variable-viewer-target
```

In the terminal, stop at the viewer checkpoint:

```gdb
break variable_viewer_checkpoint
run
```

Right-click these entries in Locals / Arguments:

- `native_values` tests the direct native-array viewer.
- `fixed_values` tests transparent `std::array` wrapper handling.
- `words` tests STL pretty-printer integration and the missing-printer fallback.
- `linear_head` tests a four-node list ending in null.
- `cycle_head` tests a three-node cycle and cycle detection.

For the equivalent Rust locals/arguments matrix, launch:

```bash
cargo run -- target/debug-fixtures/rust-variable-viewer-target
```

Set `break rust_types_ready`, run, then select caller frame `#1`. The caller
keeps every argument and local live after the marker, including `vector_arg`,
`deque_arg`, `local_vector`, `local_deque`, maps, sets, strings, slices,
`Option`, `Result`, `Box`, `Rc`, `Arc`, arrays, tuples and nested user types.

## Thread-local storage fixture

Set `break c_tls_checkpoint` and run to the `threads-ready` stop. The main thread and two named workers remain alive with different copies of the initialized TLS scalars, string buffer, aggregate object and 4 KiB zero-filled block. Switch GDB threads, then compare the variables with **Kernel → TLS** and the `$fs_base` or `$gs_base` register.

Continue once to the `main-mutated` stop to verify that the selected thread's live TLS values change without affecting either worker. The executable itself declares a `PT_TLS` template, so its module and named symbols also exercise the ELF metadata tables.

## Misc tab fixtures

Each new Misc view has a deliberately small target:

```bash
# Args / Env and Auxv: add arguments and an environment entry in the launcher.
FGDB_FIXTURE_MESSAGE='hello from envp' \
cargo run -- target/debug-fixtures/c-misc-auxv-target alpha 'two words'

# Call ABI: use `break *c_misc_frame_checkpoint` for the exact function entry,
# then instruction-step to the printf call and return boundary.
cargo run -- target/debug-fixtures/c-misc-frame-target

# Allocator mappings. Continue once for the post-MADV_DONTNEED stop.
cargo run -- target/debug-fixtures/c-misc-allocator-target

# Three futex waiters. Open Misc → Locks at the checkpoint.
cargo run -- target/debug-fixtures/c-misc-locks-target
```

Set the breakpoint named in the table before running each live target. For the Auxv fixture, configure the environment through fgdb's session launcher if you want the entry to appear in **Args / Env** as well as in the program's locals.

Generate a deterministic core file separately so it is not created during every normal fixture build:

```bash
make -C examples core
```

Then open these two files using **Session → New debug session → Core dump**:

```text
Executable: target/debug-fixtures/c-misc-core-target
Core dump:  target/debug-fixtures/c-misc-core-target.core
```

The core stops inside `raise(SIGSEGV)` with `core_crash_leaf`, `core_crash_middle`, `core_crash_outer` and `main` still unwindable. The Misc core view should show `NT_SIGINFO`, `NT_AUXV`, `NT_FILE`, the captured thread, mapped files and the crash signal.
