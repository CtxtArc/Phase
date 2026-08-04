# PHASE

A physical-domain safety language for signal/RF/embedded pipelines.
Full design rationale and roadmap: see `phase_specification.md`.

## Status: M5 — PIR + C Codegen + Sim Runtime (done)

The first milestone that produces an actual running binary, not just a
static checker. `phase build examples/radio_pipeline.phase` now compiles a
real, working simulated capture → filter → playback pipeline you can run.

```
$ cargo run --bin phase -- build examples/radio_pipeline.phase
OK: built build/radio_pipeline
$ ./build/radio_pipeline
[sim] device_capture: filling 1024 samples
[sim] sim_dma_wait("raw"): waiting for DMA transfer to complete (4096 bytes)...
[sim] sim_dma_wait("raw"): DMA transfer complete
[sim] fir_filter: filtering 1024 -> 1024 samples
[sim] device_playback: streaming 1024 samples (min=-0.9455 max=0.9455 rms=0.6390)
[sim] sim_device_wait("filtered"): waiting for device transfer to complete (4096 bytes)...
[sim] sim_device_wait("filtered"): device transfer complete
```

- **`crates/pir`**: lowers an already-analyzed AST to PIR (spec §7) — a
  flat instruction list per function, with every type resolved to
  something codegen can turn directly into C. Typestate is erased here
  (compile-time-only, like Rust generics); domain is tracked just enough
  to know whether a `sync` should wait on a DMA or a device transfer.
- **`crates/codegen_c`**: PIR → C. Generates a header (struct typedefs +
  `extern fn` prototypes) and a source file per program. `buffer<T,N>`
  becomes a plain stack array; a buffer argument automatically gets a
  trailing `size_t ..._len` parameter carrying its compile-time-known
  length. `move` compiles to an explanatory comment, not a runtime
  operation — see the scope note below.
- **`runtime/phase_runtime.{h,c}`**: hand-written demo glue. `extern fn`
  only ever gets a *prototype* from the compiler (spec §3.9) — real
  implementations of `device_capture`/`fir_filter`/`device_playback` live
  here, including a real 5-tap FIR low-pass filter and a synthetic
  sine+noise signal generator, plus `sim_dma_wait`/`sim_device_wait`,
  which give `sync` genuine, observable timing rather than being a no-op.
- **Documented scope cut:** v0.1's simulation model keeps everything in
  one address space (spec §5.1) — there's no real separate DMA/device
  memory to copy between. So `move` has no runtime effect at all; only
  `sync`'s *timing* is simulated. Porting to real hardware later means
  replacing the bodies of two functions (`sim_dma_wait`/`sim_device_wait`),
  not touching the compiler.
- **Also a documented scope cut:** `phase build` only supports
  straight-line bodies (matching M1-M2 scope) — a body containing
  `if`/`while` is rejected by `phase_pir::build` with a clear
  `UnsupportedControlFlow` error rather than silently mis-compiled or
  panicking. M4's analyzer already proves such bodies are *safe*; this
  crate just doesn't yet know how to generate code for them. Lowering
  control flow to real C branches is future work.
- **Real end-to-end test, not just unit tests:** `cargo test` actually
  invokes `cc` (with `-Werror`, zero tolerance for warnings), runs the
  resulting binary, and asserts on its real stdout — trace ordering,
  and that the reported RMS is a genuine nonzero number computed from
  actual filtered signal data, not a stub. See
  `crates/driver/tests/build_and_run.rs`.

## Layout

```
crates/
  ast/         AST node definitions (no logic)
  lexer/       hand-written lexer, source -> Vec<Token>
  parser/      hand-written recursive-descent parser, tokens -> AST
  analysis/    M2 domain/ownership/borrow + M3 typestate + M4 branch-merge analyzer
  pir/         M5: AST -> PIR (straight-line bodies only)
  codegen_c/   M5: PIR -> C source
  driver/      CLI binary ("phase")
runtime/
  phase_runtime.h/.c   demo-specific extern fn implementations + sim wait functions
examples/
  radio_pipeline.phase        the DMA/device pipeline example -- `phase build` target
  packet_pipeline.phase       the typestate example from the spec
  branching_pipeline.phase    branch-consistent domain handling (M4; check-only, not build)
  bug_gallery/                one deliberately-broken file per bug class (#1-#6)
phase_specification.md        full language spec, roadmap, bug gallery
```

## Building & testing

```
cargo build --workspace
cargo test --workspace
```

Tests are plain `cargo test` — no custom test runner. 111 tests total
across the workspace, including a real end-to-end test that compiles
generated C with a C compiler and runs the resulting binary. Each crate
has unit tests in `src/lib.rs`; the driver crate has integration tests in
`crates/driver/tests/` that run real files under `examples/` through the
real pipeline end to end.

## CLI

**`phase build` and any test that invokes `cc` must be run from the
`phase/` project root** — it resolves `runtime/phase_runtime.c` and writes
output to `build/` relative to the current directory.

```
cargo run --bin phase -- check examples/radio_pipeline.phase
cargo run --bin phase -- --dump-pir examples/radio_pipeline.phase
cargo run --bin phase -- build examples/radio_pipeline.phase
./build/radio_pipeline
```

## Next milestone (M6)

MMIO + volatile semantics: prove `volatile_read`/`volatile_write` accesses
are never reordered or eliminated by codegen (bug-gallery item #7), plus a
codegen test asserting both accesses appear, in order, in the emitted C.




