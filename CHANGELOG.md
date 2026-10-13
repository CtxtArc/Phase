# Changelog

All notable changes to PHASE are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project
uses [Semantic Versioning](https://semver.org/).

## [1.3.0] — 2026-10-13

Evidence that the static guarantees hold: a runtime-checked build mode
and a differential fuzzer that runs thousands of random accepted programs
through it. The fuzzer found two soundness holes in the analyzer and a
codegen bug, all fixed here. Buffer types are now checked at call sites —
closing a real overflow — and parameters may be length-generic.

After the fixes, 6,500 freshly fuzzed programs (2,973 accepted and run)
produced no violations, and the fuzzer was validated by putting fixed
analyzer bugs back: it caught them (spec §6.7).

As in 1.0, the fixes mean **some programs 1.2 accepted are now
rejected**, each for a real bug: a buffer left hardware-owned at the end
of a block, `swap` with a buffer from an inner block, and buffer arguments
of the wrong element type or length. The fix for each is in the
diagnostic.

### Added

- **`phase build --checked`** (spec §6.7): instruments the generated C to
  verify every ownership rule on the path the program actually takes —
  a shadow state per buffer storage (`runtime/phase_check.c`), checked at
  every buffer creation, move, sync, borrow, use/release of a borrow,
  destroy, swap, call argument (domain, access mode, borrows, aliasing),
  extern domain change, parameter exit, and scope exit. A violation
  prints `PHASE runtime check failed: …` and exits with status 70.
  Independent of the analyzer; borrow rules apply per call frame.
  Writes `<stem>.checked.gen.c` and the binary `<stem>-checked`.
- **`phase build --stub-externs`**: generates placeholder bodies for a
  program's `extern fn`s, with scalar results from a PRNG seeded by
  `PHASE_STUB_SEED`, so a program can be tried without writing C.
- **`phase-fuzz`** (new crate `crates/fuzz`): generates random programs,
  builds every one `phase check` accepts without warnings in checked mode
  with stub externs, and runs it with several seeds. Every past finding
  lives in `crates/fuzz/regressions/` and is replayed by the tests; CI
  fuzzes 1,500 fresh programs per run and uploads any findings.
- **Length-generic buffer parameters**: `buffer<T, _>` accepts any
  length; the real length (the `_len` argument every buffer parameter
  already has in C) is passed on to callees, used by `sync`, and carried
  by borrows. A run-time length only fits another `_` parameter.
- `examples/multi_rate.phase` (+ runtime): one set of generic helpers for
  a 64- and a 256-sample capture.
- Bug gallery #25 `dma_into_dead_block`, #26 `swap_across_blocks`, #27
  `buffer_length_mismatch`, #28 `buffer_element_mismatch`, #29
  `runtime_length_to_fixed`, with CWE mappings.
- Spec §6.7 (runtime-checked builds and fuzzing); §3.3 documents buffer
  type checking and `_` lengths.

### Fixed

- **A buffer declared inside an `if`/`while` block could be left owned by
  the DMA engine when the block ended** — DMA into reused stack memory.
  Block ends are now checked like function returns. *(Found by the
  fuzzer.)*
- **`swap` with a buffer from an inner block left the outer name pointing
  at storage that dies with the block.** `swap` now requires both buffers
  to be declared in the same block. *(Found by the fuzzer.)*
- **Buffer types were never compared at call sites.** A 4-element buffer
  could be passed where 16 were declared — and the callee then told its
  kernels the length was 16: a buffer overflow. A `u8` buffer was accepted
  where `f32` samples were required. Element type and length are now
  checked for every buffer argument (and a buffer can't stand in for a
  scalar or entity, or one entity type for another).
- **An early `return;` in `main`** was emitted as a bare `return;` in C's
  `int main`, leaving the exit status undefined. It now returns 0.
  *(Found by the fuzzer.)*

### Changed

- Internal crate APIs: `TypeExpr::Buffer`'s length is a `BufferLen`
  (`Fixed(n)` or `Any`); `PirType::Buffer`'s is an `Option<u64>`;
  `PirFnSig` gains `param_access`, `param_domains`, and
  `param_domains_after`; `phase_codegen_c` gains `generate_checked` and
  `generate_stubs`. Normal (unchecked) generated C is unchanged.

## [1.2.0] — 2026-10-12

Parameters can say whether a function reads or writes what it's given,
which makes the aliasing and borrow rules exact and closes a hole in
them; and `swap` makes ping-pong double buffering — the standard way to
stream from a DMA engine — expressible and checked.

Every program 1.1 accepted is still accepted; at most it now gets a
warning.

### Added

- **Access modes** (spec §3.12): `name: read T` / `name: write T`.
  - The same buffer may appear several times in one call if every use is
    read-only: `cmp(x, x)` is fine, `fir_filter(x, x)` still isn't.
  - While only read borrows are live, the owner may be passed to `read`
    parameters (and its fields read).
  - Inside a PHASE function a `read` parameter can't be moved, synced,
    destroyed, write-borrowed, or passed to a `write` parameter.
  - A `read` typestate parameter checks the state without consuming the
    value: `inspect(v); handle(v);`.
  - `read` buffer parameters are `const` pointers in the generated C, so
    the C compiler rejects an extern body that writes through one.
- **Warnings.** Diagnostics now have a severity. Warnings use the same
  renderer (`warning:` in yellow; `warning` in `--error-format=short`),
  never change the exit status, and are counted on `check`'s success
  line. Library: `Severity`, `Report`, and `analyze_full`; `analyze` is
  unchanged and still returns only errors.
- **`swap a, b;`** (spec §3.13): exchanges the storage two buffer names
  refer to, with everything the checker knows about it. A ping-pong loop
  ends each iteration with a `swap`, which restores the loop invariant, so
  the single-pass loop check stays sound. Swapped buffers are emitted as
  storage plus a pointer, and `swap` exchanges pointers — no copying.
- `examples/ping_pong.phase` (+ runtime): seven frames processed in order
  from alternating buffers, each next capture started before the CPU
  works on the previous one. Builds and runs; an end-to-end test checks
  the order and the alternation.
- Bug gallery #20 `write_through_read_borrow`, #21
  `read_param_handed_to_writer`, #22 `refill_while_processing`, #23
  `ping_pong_without_swap`, #24 `swap_while_borrowed`, with CWE mappings.
- Spec §4.3 now describes how loops are checked (the loop invariant).

### Fixed

- **A read borrow could be passed to a parameter that writes**, so a
  "read" borrow didn't actually prevent writes. Passing read-only data to
  a `write` parameter is now an error; to a parameter with no mode, a
  warning (that parameter may write, but 1.1 code had no way to say
  otherwise). Expect the warning to become an error in 2.0.

### Changed

- The examples' externs are annotated `read`/`write`, and their
  hand-written C uses `const` to match the generated prototypes.
- Loop invariant errors say what happened: "entity 'ping' is @RAM at the
  end of the `while` body but @DMA when the loop is entered", instead of
  the branch-merge wording.
- `swap` is now a keyword.
- Internal crate APIs: `Param` gains `access`; `Stmt::Swap` and
  `PirInst::Swap`; `PirFnSig` gains `read_only`; `AnalysisError` gains
  `severity`.

## [1.1.0] — 2026-10-12

Errors now show where they happen and why, and function signatures can
declare that a call moves a buffer to or from hardware — so starting a DMA
transfer and waiting for it can live in separate functions, with CPU work
in between, and still be checked end to end.

Every program 1.0 accepted is still accepted.

### Added

- **Source-located diagnostics.** Every error has a location — the
  statement that breaks the rule, with the entity's name underlined — and
  notes pointing at the cause: where the entity was declared, moved,
  synced, borrowed, released, destroyed, or consumed; where each branch of
  an `if` left it; which call changed its domain. Rendered rustc-style:

  ```
  error: entity 'frame' is in domain @DMA but 'report' expects @RAM for this argument
    --> examples/bug_gallery/read_while_capture_in_flight.phase:19:12
     |
  19 |     report(frame);
     |            ^^^^^
  note: 'frame' is moved to @DMA by 'start_capture' here
    --> examples/bug_gallery/read_while_capture_in_flight.phase:16:5
     |
  16 |     start_capture(frame);
     |     ---------------------
  ```

  Parse errors use the same renderer.
- `--error-format=short` prints one `file:line:col: error|note: message`
  line per error and note, for editors and scripts.
- `--color=auto|always|never`; `auto` (the default) colors only a
  terminal and respects `NO_COLOR`.
- **Domain changes in signatures** (spec §3.11). A parameter may be
  declared `name: T @FROM -> @TO`:
  - the signature's change must be a legal transition (§2.4);
  - at a call the argument must be in `@FROM` and owned (not a borrow),
    and is in `@TO` afterwards;
  - a PHASE function's body must leave the parameter in `@TO` at every
    `return`.
- `examples/overlapped_capture.phase` (+ runtime): a capture started by
  `start_capture(dst: .. @RAM -> @DMA)`, a window computed while it runs,
  and a PHASE `wait_capture(b: .. @DMA -> @RAM)` helper. Builds and runs.
- Bug gallery #17 `broken_sync_promise`, #18
  `illegal_signature_transition`, #19 `read_while_capture_in_flight`, with
  CWE mappings.
- Tests: renderer unit tests, analyzer location and domain-change tests,
  parser and PIR tests, and CLI tests for both error formats, color, flag
  validation, parse-error rendering, and building and running the new
  example.

### Fixed

- Inside a helper function, `sync` of a buffer parameter passed the C
  pointer's size (8 bytes) to the runtime wait instead of the buffer's
  size. Only the simulated wait's reported byte count was affected.

### Changed

- The default (human) error output is multi-line. Scripts that parsed the
  old one-line `path: error: message` form should use
  `--error-format=short` (whose lines are now `path:line:col: error: ...`).
  The final `<path>: N errors found` line is unchanged.
- Internal crate APIs: `phase_ast::Span` (now with an end position)
  replaces `phase_lexer::Span` (re-exported); block statements are
  `Spanned<Stmt>`; `FnDecl`/`ExternFnDecl` gain `name_span`, `Param` gains
  `span` and `domain_after`; `AnalysisError` gains `span`, `subject`, and
  `notes`.
- The bug-gallery transcript script passes `--color=never`.

## [1.0.0] — 2026-10-12

The first stable release. An audit of the v0.1 analyzer found programs
it accepted that broke the very rules the bug gallery documents; 1.0
closes all of them, states the resulting guarantees precisely
(`phase_specification.md` §4.4), and pins them with tests.

Because the checker is stricter, **some programs v0.1 accepted are now
rejected**, each one for a real bug. The fix for each is listed below.

### Fixed — soundness

- **Typestate could be forged with an annotation.**
  `let p: Packet<Validated> = receive_packet();` was accepted; the
  annotation overrode the call's declared return state. An annotation is
  now checked against its initializer. A new value may only be created
  directly (struct literal or no initializer) in the machine's *initial*
  state. *Fix:* call the transition functions.
- **Typestate values could be reused.** After `decode(p1)`, `p1` was
  still usable. Typestate values are now linear: passing one to a
  state-requiring parameter, or returning it, consumes it. Consumption on
  one branch of an `if` makes the value unusable after the join, and
  consuming an outer value inside a `while` body is an error.
- **A buffer could be destroyed while `@DMA`/`@DEVICE` owned it.**
  *Fix:* `sync` before `destroy`.
- **Functions could return while hardware owned a local buffer**, with
  borrows still live, or with a parameter left in a different domain
  than it arrived in. These are now checked at every `return`, including
  the implicit one at the end of a body. Parameters cannot be destroyed.
- **The owner of a borrowed buffer could still be used directly**, so a
  write borrow wasn't exclusive. While any borrow is live, the owner is
  reachable only through its borrows.
- **Released borrow handles could still be used**, and handles could be
  moved, synced, destroyed, or re-borrowed. All rejected now.
- **A `@DMA`/`@DEVICE`-owned buffer could be borrowed**, bypassing `sync`.
- **The same buffer could be passed twice to one call** (e.g. as a
  kernel's input and output). Now allowed only when every occurrence is a
  read borrow.
- **Names could be redeclared**, hiding a live (even DMA-owned) entity,
  and `let y = x;` could copy an entity into an untracked alias. Both are
  rejected.
- **Unknown names in conditions passed `check`** and only failed later in
  `cc`. Identifiers in conditions and assignments must now exist and be
  scalars. Call arity and duplicate function names are also checked.
- A branch ending in `return` no longer takes part in the join after an
  `if` (previously it could cause spurious disagreement errors or hide
  real ones); statements after `return` are reported as unreachable.

### Added

- Bug gallery entries #8–#16: `forged_typestate`, `stale_typestate_reuse`,
  `destroy_while_dma`, `dma_escapes_function`, `use_owner_while_borrowed`,
  `use_after_release`, `aliased_arguments`, `borrow_unsynced_dma`,
  `shadowed_dma_buffer`, with CWE mappings in `docs/security_mapping.md`.
- The gallery test now asserts each file is rejected with the exact error
  in its `// Expected:` header.
- 46 analyzer regression tests, each rule paired with a program it must
  keep accepting.
- `phase --version` / `-V` and `phase --help` / `-h`.
- CLI integration tests that drive the real `phase` binary.
- GitHub Actions CI: `cargo fmt --check`, `clippy -D warnings`, the full
  test suite with the ARM cross-compiler and QEMU installed
  (`PHASE_REQUIRE_HW_TOOLS=1` turns the hardware test's skip into a
  failure), the `-O2` volatile proof demo, and a staleness check on the
  bug-gallery transcript.

### Changed

- `phase build` no longer has to run from the repository root. It finds
  the nearest `runtime/` at or above the source file (override:
  `PHASE_RUNTIME_DIR`) and writes to the `build/` beside it.
- Identical diagnostics are reported once, and a rejected `borrow` no
  longer produces a follow-on error at its `release`.
- Loop-related merge errors now explain that a `while` body must leave
  every entity as it found it.
- All crates are versioned together via `[workspace.package]`.
- The specification is updated to 1.0: §4.4 states the guarantees and the
  `extern fn` trust boundary, and stale v0.1 text (crate layout, CLI,
  runtime description, domain count, pipeline) now matches the code.
- The README no longer carries a hand-maintained test count.
- `demos/m6_volatile_proof/build/*.s` are no longer tracked (they are
  toolchain-dependent build output).

## [0.1.0]

The original milestone series, M1–M11: lexer, parser, domain/ownership/
borrow analysis, typestate, `if`/`while` merging, PIR and C codegen with a
simulated runtime, `volatile` MMIO with an `-O2` proof, real control-flow
codegen, scalar assignment, a real ARM Cortex-M3 runtime validated under
QEMU, and `volatile_read` in loop conditions.
