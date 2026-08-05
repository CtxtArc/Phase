# PHASE

A physical-domain safety language for signal/RF/embedded pipelines.
Full design rationale and roadmap: see `phase_specification.md`.

## Status: M6 — MMIO + Volatile Semantics (done)

Bug-gallery item #7: `volatile_read`/`volatile_write` accesses must never
be reordered or eliminated by codegen. Rather than just asserting this,
it's verified against a real optimizing compiler at the assembly level.

```
$ cargo run --bin phase -- build examples/mmio_registers.phase
OK: built build/mmio_registers
```

- **Analyzer**: `volatile_read(REG @MMIO)` / `volatile_write(REG @MMIO,
  value)` now get real argument-shape validation — correct arity, and the
  register argument must actually be an `@MMIO` domain reference, not an
  arbitrary expression. (The old code had a comment claiming the parser
  already enforced this; it didn't — a real gap, closed here.)
- **PIR**: a new `CallArg::MmioRegister` variant, plus whole-program
  collection of every distinct register name used. This closed another
  real gap: PIR previously had no case for `DomainRef` arguments at all,
  so `volatile_read`/`volatile_write` calls couldn't be lowered — found by
  writing the M6 tests, not by inspection.
- **Codegen**: each distinct register becomes one `volatile uint32_t`
  global (`extern` in the header, defined once in source).
  `volatile_read`/`volatile_write` compile to a **direct read or
  assignment**, not a function call — so the actual guarantee is enforced
  by the C standard's own `volatile` semantics, not by anything this
  compiler has to independently prove.
- **The real proof, not just an assertion**: `examples/mmio_registers.phase`
  writes the same register twice in a row
  (`UART_CONTROL = 1; UART_CONTROL = 2;`). Under ordinary (non-volatile)
  optimization, a compiler is free to eliminate the first store as dead,
  since it's immediately overwritten.
  `crates/driver/tests/mmio_volatile.rs` compiles this exact program at
  `-O2 -S` and inspects the real generated assembly:
  ```
  movl	$1, UART_CONTROL(%rip)
  movl	$2, UART_CONTROL(%rip)
  ```
  Two separate stores survive. If `volatile` weren't correctly applied,
  GCC would have collapsed these into one — this test would have caught
  it. That's the textbook demonstration of what `volatile` is for,
  verified against a real toolchain rather than asserted in a comment.
- **A program with no `extern fn` (like this one) now builds standalone**,
  without linking `runtime/phase_runtime.c` at all — that file is
  hardcoded to `radio_pipeline.gen.h`'s specific symbols (spec §5.3: it's
  program-specific demo glue, not a generic runtime), so linking it
  against an unrelated program would fail. `phase build` now only links
  it when a program actually declares an `extern fn`.

## Layout

```
crates/
  ast/         AST node definitions (no logic)
  lexer/       hand-written lexer, source -> Vec<Token>
  parser/      hand-written recursive-descent parser, tokens -> AST
  analysis/    M2 domain/ownership/borrow + M3 typestate + M4 branch-merge + M6 MMIO analyzer
  pir/         M5: AST -> PIR (straight-line bodies only) + M6 MMIO registers
  codegen_c/   M5: PIR -> C source + M6 volatile globals
  driver/      CLI binary ("phase")
runtime/
  phase_runtime.h/.c   demo-specific extern fn implementations + sim wait functions
examples/
  radio_pipeline.phase        the DMA/device pipeline example -- `phase build` target
  packet_pipeline.phase       the typestate example from the spec
  branching_pipeline.phase    branch-consistent domain handling (M4; check-only, not build)
  mmio_registers.phase        volatile MMIO register access (M6) -- builds standalone
  bug_gallery/                one deliberately-broken file per bug class (#1-#6, plus MMIO validation)
phase_specification.md        full language spec, roadmap, bug gallery
```

## Building & testing

```
cargo build --workspace
cargo test --workspace
```

Tests are plain `cargo test` — no custom test runner. 132 tests total
across the workspace, including two real end-to-end tests that invoke a C
compiler and inspect its actual output: one runs the compiled
`radio_pipeline` binary and checks real stdout, the other compiles at
`-O2` and inspects the real generated assembly. Each crate has unit tests
in `src/lib.rs`; the driver crate has integration tests in
`crates/driver/tests/` that run real files under `examples/` through the
real pipeline end to end.

## CLI

**`phase build` and any test that invokes `cc` must be run from the
`phase/` project root** — it resolves `runtime/phase_runtime.c` (when
needed) and writes output to `build/` relative to the current directory.

```
cargo run --bin phase -- check examples/radio_pipeline.phase
cargo run --bin phase -- --dump-pir examples/mmio_registers.phase
cargo run --bin phase -- build examples/radio_pipeline.phase
cargo run --bin phase -- build examples/mmio_registers.phase
./build/radio_pipeline
./build/mmio_registers
```

## Next milestone (M7)

Per the roadmap, the core milestones (M1-M6) are now complete. M7 is the
demo + write-up milestone: a slightly richer worked example, the full bug
gallery with captured compiler output, and a short doc mapping each caught
bug class to a real CVE category — the portfolio-facing wrap-up rather
than new compiler features.





