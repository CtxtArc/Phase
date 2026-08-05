# PHASE

A physical-domain safety language for signal/RF/embedded pipelines.
Full design rationale and roadmap: see `phase_specification.md`.

## Status: M7 — Demo & Write-Up (done)

The roadmap's core compiler milestones (M1-M6) are complete. M7 is the
portfolio-facing wrap-up: a richer combined example, the full bug gallery
captured as real compiler output, and a write-up connecting every caught
bug class to a real CWE category and, where one exists, a specific CVE.

- **`examples/sdr_session.phase`** — a fuller worked example exercising
  all three safety mechanisms together in one coherent program: DMA/device
  domain safety, typestate (a status packet must go
  `Received -> Decoded -> Validated`), and volatile MMIO register access
  (bringing the RF front end online before touching any buffers). **Builds
  and runs for real** (`phase build examples/sdr_session.phase`) — proves
  the features compose all the way down to an executable, not just in
  static analysis. Getting this to build surfaced a real gap: `Packet` only
  had a `state` declaration, never an `entity`, so PIR had no C
  representation for it — fixed by giving it a real (if minimal) field
  layout, since typestate itself is erased at runtime but the *value*
  still needs to exist.
- **The runtime convention generalized** from "one hardcoded demo" to a
  real pattern: `runtime/phase_runtime.c` is fully generic now (just
  `sim_dma_wait`/`sim_device_wait`, no program-specific `#include`) and is
  always linked; each program's actual `extern fn` bodies live in their
  own `runtime/<stem>_extern.c` (`radio_pipeline_extern.c`,
  `sdr_session_extern.c`), and `phase build` looks for the matching one
  automatically. A program with no `extern fn` at all (like
  `mmio_registers.phase`) still needs neither and builds standalone.
- **[`docs/bug_gallery_transcript.md`](docs/bug_gallery_transcript.md)** —
  every bug-gallery file plus the happy paths, with the *real, captured*
  `phase check` output for each — regenerate any time with
  `./scripts/capture_bug_gallery.sh`. Never hand-typed, so it can't drift
  from what the compiler actually says.
- **[`docs/security_mapping.md`](docs/security_mapping.md)** — maps each
  bug-gallery item to a MITRE CWE category, with real CVE/research
  precedent where a clean one exists (CVE-2014-1266 "goto fail" for the
  typestate-skip class, CVE-2024-43856 for the use-after-destroy class, a
  USENIX Security 2021 paper on unsafe DMA access detection for the
  missing-`sync` class, ARM's own compiler docs for the volatile/MMIO
  class). Every citation was verified against a real source before being
  written down, not pulled from memory.
- **A blanket regression test** (`every_bug_gallery_file_is_actually_rejected`)
  sweeps every file under `examples/bug_gallery/` and asserts each one
  fails analysis — independent of the per-file message tests, so a bug
  gallery file that accidentally started passing (or a new one added
  without actually being broken) gets caught even without a dedicated test.

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
  phase_runtime.h/.c          generic sim_dma_wait/sim_device_wait, always linked
  radio_pipeline_extern.c     radio_pipeline.phase's extern fn implementations
  sdr_session_extern.c        sdr_session.phase's extern fn implementations
examples/
  radio_pipeline.phase        the DMA/device pipeline example -- builds & runs
  packet_pipeline.phase       the typestate example from the spec (check-only)
  branching_pipeline.phase    branch-consistent domain handling (M4; check-only)
  mmio_registers.phase        volatile MMIO register access (M6) -- builds & runs standalone
  sdr_session.phase           M7: all three mechanisms combined -- builds & runs
  bug_gallery/                one deliberately-broken file per bug class (#1-#6, plus MMIO validation)
demos/
  m6_volatile_proof/           before/after proof that volatile prevents a real bug class
docs/
  bug_gallery_transcript.md    generated: real compiler output for every example
  security_mapping.md          CWE/CVE mapping for every bug class
scripts/
  capture_bug_gallery.sh       regenerates the transcript from real compiler output
phase_specification.md        full language spec, roadmap, bug gallery
```

## Building & testing

```
cargo build --workspace
cargo test --workspace
```

Tests are plain `cargo test` — no custom test runner. 135 tests total
across the workspace, including three real end-to-end tests that invoke a
C compiler and inspect its actual output: two run compiled binaries
(`radio_pipeline`, `sdr_session`) and check real stdout, one compiles at
`-O2` and inspects the real generated assembly. Each crate has unit tests
in `src/lib.rs`; the driver crate has integration tests in
`crates/driver/tests/` that run real files under `examples/` through the
real pipeline end to end, plus a blanket sweep asserting every bug-gallery
file is actually rejected.

## CLI

**`phase build` and any test that invokes `cc` must be run from the
`phase/` project root** — it resolves the relevant `runtime/*.c` files
(when needed) and writes output to `build/` relative to the current
directory.

```
cargo run --bin phase -- check examples/sdr_session.phase
cargo run --bin phase -- build examples/sdr_session.phase
cargo run --bin phase -- build examples/radio_pipeline.phase
cargo run --bin phase -- build examples/mmio_registers.phase
./build/sdr_session
./build/radio_pipeline
./build/mmio_registers
./demos/m6_volatile_proof/run_demo.sh
./scripts/capture_bug_gallery.sh
```

## What's next

The roadmap's core milestones (M1-M7) are complete. From here, natural
next directions (not yet scoped as formal milestones) include: lowering
`if`/`while` to real C branches (closing M5's documented scope cut),
supporting real hardware targets instead of the simulated runtime (spec
§5.3), and an LLVM backend (spec §6.2's documented non-goal for v0.1).






