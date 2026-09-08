# Architecture and extension guide

fgdb is a single Rust binary with GTK presentation, a GDB/MI transport, and
bounded feature-specific inspection services. Prefer concrete modules and typed
requests to a general plugin framework. Sharing code is useful when the ownership,
lifetime, and failure policy are actually shared.

## Responsibility map

| Area | Responsibility | Keep out |
| --- | --- | --- |
| `app/` | Compose services, route events, coordinate sessions and asynchronous feature operations | Widget construction and duplicate instruction classification |
| `model/` | Authoritative execution state, selections, operation identities, stopped snapshots | GTK widget state as authorization |
| `debugger/` | MI protocol, transport, scoped requests, target types and instruction semantics | Application callbacks and GTK presentation helpers |
| `ui/` | Controls, rendering, row identity, user drafts and presentation caches | New copies of backend state machines |
| `ui/workspace/` | Panel identity, hosting, presentation, responsive allocation and navigation | Feature-specific data collection |
| `ui/components/`, `theme/` | Reusable controls and consistent interaction geometry | Feature-specific request policy |
| `kernel/`, `local_process/` | Verified target procfs inspection and local process discovery | Treating an arbitrary remote PID as a local process |
| `symbols/`, `language/`, `misc/`, `memory_search.rs`, `syscalls/` | Domain decoding, inspection policy and bounded collection | Independent widget trees or refresh loops for each caller |
| `config/`, `ui/layout/`, `investigation/` | Validated preferences, layout persistence and investigation files | One serializer that conflates their different compatibility and save policies |
| `background.rs`, `bounded.rs`, `performance.rs` | Worker admission, bounded I/O, cache/render budgets and diagnostics | Unbounded queues or silently truncated results |

These are responsibilities, not separate crates. Some UI control modules still
dispatch debugger commands, and `app` uses those control entry points. Preserve
their existing execution interlocks when extracting new controllers. Shared
domain types and callbacks are preferable to introducing a dependency from a
decoder or UI component into `app`.

## State and asynchronous work

- Authorize debugger actions through `DebuggerModel`, not a button's sensitivity
  or a render cache. Observed state, pending intent, selected inspection context,
  and the thread that owns the stop are distinct.
- Use `StopContext` for stopped requests. It carries transport epoch, refresh
  generation, inferior, thread, and frame. Scope MI commands explicitly and check
  freshness before dispatch and before publishing results.
- Invalidate work on execution, target changes, transport replacement, and symbol
  changes as appropriate. Reject obsolete replies rather than painting them over
  a newer selection. Use existing refresh gates to coalesce repeat requests.
- Keep GTK access on its main context. Use the bounded background pool for
  synchronous feature work, with appropriate priority and cancellation checks.
  Do not add a thread per row, mapping, or refresh.
- Release `RefCell` borrows before GTK calls that emit signals or invoke handlers.
  Logging, selection, and model publication can be reentrant.
- An asynchronous owner must own its cancellation and cleanup too. Subprocess
  capture owns both pipe readers, the wait, and its deadline in one future.
  Killing a child alone does not close pipes inherited by a descendant. GIO
  futures cancel their pending operation on drop, and GSubprocess reaps children.
  See the [GSubprocess lifecycle contract](https://docs.gtk.org/gio/class.Subprocess.html).

## Bounded work and stable presentation

- Keep limits at the owning layer, including input size, queue capacity, scan
  bytes, results, widget count, child pages, and retained history. Report partial,
  unreadable, skipped, stale, and unavailable outcomes distinctly.
- Keep source file identity validation even on cache hits. Reusing content is
  not permission to ignore replacement or modification of the underlying file.
- Reuse unchanged list rows and immutable snapshots. Compare the identities of
  actual rendered rows before updating in place. A capped list's layout can change
  when its selected item moves outside the visible prefix.
- Preserve drafts, selections, scroll position, expansion state, and terminal
  focus across data refreshes. Refresh optional details according to presentation
  policy, including detached panels, rather than a notebook index alone.
- Use weak references for cross-widget signal captures. Own subscriptions through
  `ui::lifecycle` where they need disconnection. Closing a window or menu must not
  leave a strong signal-reference cycle retaining its contents.
- Keep render caches in the UI and semantic caches in their domain modules.
  Prefer borrowing, shared immutable snapshots, and coalesced updates to copying
  a whole collection just to announce that it is unchanged.

## Extension points

### Panels and controls

Declare stable panel identity, persistence key, title, and refresh policy in
`ui/workspace/registry.rs`. Build a panel once and let workspace hosting reparent
it. Subscribe to presentation changes through the workspace API. Use existing
components, including shared subtab navigation, instead of importing builders
from another feature's view.

Stopped rendering is grouped under `ui/debug_state/`: locals and expansion,
disassembly and instruction flow, and stop-point controls. The parent coordinates
the remaining stopped views and shared refresh entry points. Keep a feature's
private rendering helpers alongside its methods.

### Languages and pretty printers

Add source-language policy to `language.rs` without assuming that a source
language is a GDB expression dialect. Add layout-aware adapters under
`language/printers/`, register them through `printers.py`, and include bundled
modules through the existing Python package mechanism. Reuse bounded child and
string readers, reject unsupported layouts, and retain raw GDB values as fallback.

User scripts go through `language::scripts::PrinterScript` and the existing
configuration/interactive loading flow. They execute in GDB and are trusted code,
not sandboxed plugins. Preserve native GDB printer registration, enable/disable
controls, user-printer precedence, and cache invalidation on objfile lifecycle
changes. Add representative fixtures for new layouts.

### Instruction semantics and allocators

Extend `debugger/instruction.rs` for instruction normalization, classification,
and direct targets. Until controls, Call ABI, and instruction presentation must
agree. Preserve the distinction between a direct destination, a register target,
and storage containing an indirect destination. Add syntax and architecture tests
before extending a predicate.

Allocator detection and inspection belong under `misc/allocator/`. A loaded
runtime is not necessarily the owner of the C allocation bindings. Keep detection
evidence separate from backend decoding capability. Use the shared symbol
resolver for missing debug data rather than adding allocator-specific downloads.

### Preferences and persistence

Use the existing preference types, parser, validation, configuration document
updates, and settings controls together. Preserve unrelated config text and
conflict/error reporting. Persistent preference changes and temporary inspection
choices are different. Layout keys must remain stable across panel reordering.

### Process metadata

`local_process::stat::Stat` is the shared borrowed `/proc/PID/stat` parser. Linux
command names are arbitrary bytes and may contain parentheses. Decode numeric
fields separately and render names lossily only at the presentation boundary.
Procfs identity and tracer checks remain in the verified inspection layer.

## Source hygiene

Keep modules organized by responsibility rather than an arbitrary line count.
Move large test bodies into child test modules when they obscure production code.
Use the narrowest practical visibility and avoid dependencies between unrelated
views. Separate multiline statements from the following statement with a blank
line, keep related single-line statements together, and do not use semicolons in
comments. Prefer regression tests for discovered failure modes over speculative
abstractions or unmeasured performance claims.
