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
| `c-thread-target` | `c_threads_checkpoint` | Named pthreads in futex/poll waits, per-thread signal masks, a pending signal and atomic values |
| `c-process-target` | `c_process_checkpoint` | Fork catchpoints and a live parent/child/grandchild hierarchy |
| `cpp-debug-target` | `debugger_checkpoint` | Integer families, STL containers, structs, pointer cycles, watchpoints, threads, signals and exceptions |
| `cpp-object-target` | `cpp_objects_checkpoint` | Virtual dispatch, multiple inheritance, templates, smart pointers, variants, lambdas and nested exceptions |
| `rust-debug-target` | `rust_debugger_checkpoint` | Rust enums, `Option`, `Result`, collections, trait objects, `Rc` cycles, primitives, Unicode and a named worker thread |
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

At each stop, inspect **Kernel → Changes** for deltas and **Kernel → Maps** for RSS/PSS, NUMA placement, VM flags and pagemap samples. The fixture resets its own soft-dirty tracking through `/proc/self/clear_refs`; failure to do so is intentionally non-fatal on restricted systems.

The default targets use `-O0`, full debug information, frame pointers and disabled C/C++ inlining. The `-o2` variants deliberately retain optimization. Override `CC`, `CXX`, `RUSTC`, or the corresponding flags to compare toolchains and DWARF versions.
