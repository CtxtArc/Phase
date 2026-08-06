# PHASE: Physical Hardware Assurance & Safety Engine

**A compiler that statically proves memory safety for hardware pipelines — DMA, MMIO, and zero-copy devices — before the code ever runs.**

<p align="center">
  <img src="https://img.shields.io/badge/Status-M9_Complete-success" alt="Status">
  <img src="https://img.shields.io/badge/Tests-158_passing-brightgreen" alt="Tests">
  <img src="https://img.shields.io/badge/License-Apache_2.0-blue" alt="License">
  <img src="https://img.shields.io/badge/Language-Rust-orange" alt="Language">
</p>

## The problem: hardware moves fast, software is blind

In embedded and systems engineering, hardware components (ADCs, DACs,
NICs) use DMA engines to stream data directly into memory, bypassing the
CPU entirely. Standard languages like C are blind to this: if code reads a
DMA buffer before the hardware finishes writing to it, or forgets that a
control register must be `volatile`, or skips a required validation step
before trusting a parsed packet, the compiler builds it anyway. On real
silicon, each of these is a race condition, a silently-corrupted signal,
or a security bypass — not a hypothetical, but a well-documented, recurring
class of real CVEs (see below).

## The solution

PHASE treats physical hardware boundaries as part of the type system. It's
a strict Design Rule Check for memory pipelines: every buffer's *physical
domain* — host RAM, a DMA engine, a memory-mapped register, an external
device — is tracked and checked at compile time, the same way Rust's
borrow checker tracks aliasing, just applied to hardware-shaped safety
instead.

You write your heavy DSP/routing math in ordinary C, and write the
pipeline's coordination rules in PHASE. The compiler statically proves:

1. **Domain safety** — the CPU never touches a buffer owned by hardware
   (`@DMA`, `@DEVICE`) without an explicit `sync()` first.
2. **Ownership/borrow safety** — a buffer is never moved or destroyed
   while something still borrows it; concurrent write borrows conflict.
3. **Typestate enforcement** — data never skips a required step in a
   protocol/validation chain (e.g. a packet reaching a "trusted" stage
   without ever passing through decode/validate).
4. **Branch-merge consistency** — two `if`/`else` paths can't leave a
   buffer in disagreeing physical domains.
5. **Volatile MMIO semantics** — hardware control registers are never
   silently reordered or eliminated by C compiler optimization.

Once these proofs pass, PHASE lowers the pipeline to plain, readable C.
Domain, borrow, and typestate checks are all compile-time-only and fully
erased — they add nothing to the generated code. (`sync()` is the one
exception: it's a real call into a small runtime that simulates hardware
transfer timing — see [Honest scope](#honest-scope) below.)

## See it catch a real bug, right now

```
$ cargo run --bin phase -- check examples/bug_gallery/use_before_sync.phase
error: entity 'raw' is in domain @DMA but 'fir_filter' expects @RAM
```

That's a real DMA buffer read before its transfer completed, rejected
before the program ever runs. And the compiler doesn't just *claim* its
volatile guarantee holds — `demos/m6_volatile_proof/run_demo.sh` compiles
the same register-write pattern by hand (no `volatile`) and with PHASE,
both at `-O2`, and inspects the real assembly:

```
naive C (no volatile):  1 store to UART_CONTROL survives   -- the "reset" write silently vanished
PHASE-generated C:       2 stores to UART_CONTROL survive   -- both writes preserved
```

## The bug gallery: preventing real-world CVE classes

Every rule PHASE enforces maps to a real, recurring bug class. The
[`examples/bug_gallery/`](examples/bug_gallery/) directory contains
deliberately broken code PHASE rejects at compile time:

- **[CWE-362/367] The DMA race condition** — [`use_before_sync.phase`](examples/bug_gallery/use_before_sync.phase) fails compilation for reading a `@DMA` buffer before `sync()`. This exact discipline is why Linux's `dma-buf` subsystem requires explicit sync ioctls (a 2025 kernel fix, CVE-2025-38095, was needed when that sync could still race).
- **[CWE-416] Use-after-free** — [`move_while_borrowed.phase`](examples/bug_gallery/move_while_borrowed.phase) / [`use_after_destroy.phase`](examples/bug_gallery/use_after_destroy.phase) fail compilation for moving, destroying, or using a buffer while something still borrows it — the same shape as CVE-2024-43856, a 2024 Linux kernel race in `dmam_free_coherent()`.
- **[CWE-696] Typestate skipping** — [`skipped_state_transition.phase`](examples/bug_gallery/skipped_state_transition.phase) fails compilation for passing a `Packet<Received>` where `Packet<Decoded>` was required — the same bug shape as Apple's 2014 "goto fail" (CVE-2014-1266), where a required verification step was silently skipped.
- **[CWE-758] Silent MMIO optimization** — `@MMIO` registers always lower to `volatile` C globals, so the C standard itself — not convention or code review — forbids the compiler from deleting or reordering the access. This is a well-documented, recurring class of embedded bugs (ARM's own compiler docs call it out directly).

**See the exact compiler output:** [`docs/bug_gallery_transcript.md`](docs/bug_gallery_transcript.md)
is regenerated straight from real `phase check` runs (`./scripts/capture_bug_gallery.sh`),
never hand-typed. [`docs/security_mapping.md`](docs/security_mapping.md) has
the full CWE/CVE write-up, with every citation verified against a real
source.

## Why a language, not a library

A library can check each buffer access in isolation, at the call site. It
can't answer whole-program questions before any input is ever processed:
does every branch agree on a buffer's domain? Is there a path where a
required validation step gets skipped? PHASE answers these by building a
real dependency graph over the whole program and analyzing it as a unit —
the same category of problem as a borrow checker, not something a
proc-macro or a lint pass can do from inside a host language. See
[`phase_specification.md`](phase_specification.md) §2 for the full
rationale.

## Architecture & repository layout

```
source (.phase)
  → lexer         hand-written, source -> tokens
  → parser        hand-written recursive descent, tokens -> AST
  → analysis       domain/ownership/borrow + typestate + branch-merge + MMIO checks
  → pir            AST -> a flat per-function intermediate representation
  → codegen_c      PIR -> readable C
  → cc             real C compiler -> a real binary
```

```text
crates/
  ast/         AST node definitions
  lexer/       hand-written lexer (source -> tokens)
  parser/      hand-written recursive-descent parser (tokens -> AST)
  analysis/    domain, ownership, typestate, branch-merge, and MMIO analyzers
  pir/         Phase Intermediate Representation builder
  codegen_c/   lowers PIR to raw C source
  driver/      the `phase` CLI

runtime/
  phase_runtime.h/.c   generic sim_dma_wait/sim_device_wait, always linked
  *_extern.c           one per demo program, implementing that program's extern fns

examples/
  sdr_session.phase    full demo: MMIO setup + DMA capture + typestate + real DSP math
  bug_gallery/         one deliberately-broken file per bug class

demos/
  m6_volatile_proof/   before/after proof that a C compiler really does delete non-volatile writes

docs/
  bug_gallery_transcript.md   generated: real compiler output for every example
  security_mapping.md         the CWE/CVE write-up

scripts/
  capture_bug_gallery.sh      regenerates the transcript from real compiler output
```

## Building and testing

**Requirements:** `cargo` (Rust) and a C compiler (`cc`).

```bash
cargo build --workspace
cargo test --workspace
```

135 tests, plain `cargo test`, no custom runner — including end-to-end
tests that invoke a real C compiler, run the resulting binaries, and
inspect real `-O2` assembly output, plus a blanket sweep asserting every
bug-gallery file is actually rejected.

**`phase build` and any test invoking `cc` must be run from this
directory** — it resolves `runtime/*.c` and writes to `build/`, both
relative paths:

```bash
# Check a file for safety without generating C
cargo run --bin phase -- check examples/sdr_session.phase

# Build & run — translates to C, links the runtime, compiles with cc
cargo run --bin phase -- build examples/sdr_session.phase
./build/sdr_session

cargo run --bin phase -- build examples/mmio_registers.phase
./build/mmio_registers

# The adversarial volatile proof (naive C vs. PHASE, both at -O2)
./demos/m6_volatile_proof/run_demo.sh

# Regenerate docs/bug_gallery_transcript.md from real compiler output
./scripts/capture_bug_gallery.sh
```

`examples/sdr_session.phase` is the fullest single demo — DMA/device
capture-filter-playback, a status packet that must pass through a full
`Received -> Decoded -> Validated` typestate chain, and MMIO register
control, composed in one program that actually builds and runs.

`examples/branching_pipeline.phase` is the M8 demo: a real `if`/`else`
that compiles to real C control flow and actually takes either branch at
runtime, picked by an extern call (`PHASE_DEMO_USE_DMA=0 ./build/branching_pipeline`
forces the `@DEVICE` path; unset or non-zero takes `@DMA`).

`examples/counting_loop.phase` is the M9 demo: a `while` loop with a real
`n = n + 1;` counter that runs exactly 5 iterations and stops on its own.

## Honest scope

This is a v0.1 research/portfolio compiler, not a production toolchain.
Limitations below are documented on purpose rather than left as silent
gaps someone has to discover the hard way:

- **Assignment is scoped to plain scalar locals.** `name = expr;` (M9)
  reassigns a `let`-bound scalar; domain-tracked entities/buffers still go
  through `move`/`sync`/`borrow`/`destroy` on purpose, so a `while` loop
  can now genuinely terminate (`examples/counting_loop.phase`), but
  there's still no way to reassign through an entity.
- **`if`/`while` conditions and assignment right-hand sides are a
  deliberately small subset of expressions** (`PirExpr`: identifiers,
  literals, binary comparisons/arithmetic) — not the full `Expr` grammar.
  A condition using a call (e.g. `volatile_read(...)`) is a clear
  `UnsupportedExpr` error, not a silent guess — so a genuine MMIO polling
  loop isn't expressible yet.
- **Every program with an `extern fn` needs a matching hand-written
  runtime file**, `runtime/<stem>_extern.c` (e.g. `sdr_session.phase` →
  `runtime/sdr_session_extern.c`). The compiler only ever generates a
  *prototype* for `extern fn` (it's a trusted, opaque operation by design
  — see spec §3.9), never a body. `phase build` fails early with a clear
  message naming the missing file if one isn't found; it doesn't attempt
  a doomed link.
- **No real hardware target.** Everything runs in one simulated address
  space on the host; porting to real hardware means replacing two runtime
  functions (`sim_dma_wait`/`sim_device_wait`), not touching the compiler.
- **No LLVM backend.** Compiling to readable C was a deliberate choice so
  the engineering effort goes into the analysis passes, not backend
  plumbing.
- **No loop fixed-point analysis.** `while` bodies are checked as "ran
  zero or one times," which is sound for the common case but doesn't
  compute a true fixed point across many iterations.

Full rationale for all of these is in
[`phase_specification.md`](phase_specification.md) §10 (Non-Goals).

## What's next

The roadmap's core milestones (M1–M9) are complete — `phase build` now
compiles real branching, looping, and self-terminating control flow to C.
Formal next milestones, in
[`phase_specification.md`](phase_specification.md) §9:

- **M10 — Real hardware target,** replacing the two simulated wait
  functions with an actual wait-for-completion on real silicon.
- **M11 — An LLVM backend** (§10's documented non-goal for v0.1).

For the complete language specification, domain model, type system, and
full milestone roadmap this was built against, see
[`phase_specification.md`](phase_specification.md).
