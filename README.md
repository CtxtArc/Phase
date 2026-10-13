# PHASE: Physical Hardware Assurance & Safety Engine

**A compiler that statically checks memory safety for hardware pipelines — DMA, MMIO, and zero-copy devices — before the code ever runs.**

<p align="center">
  <img src="https://img.shields.io/badge/version-1.3.0-success" alt="Version 1.3.0">
  <img src="https://img.shields.io/badge/License-Apache_2.0-blue" alt="License">
  <img src="https://img.shields.io/badge/Language-Rust-orange" alt="Language">
</p>

## The problem: hardware moves fast, software is blind

In embedded and systems engineering, hardware components (ADCs, DACs,
NICs) use DMA engines to stream data directly into memory, bypassing the
CPU entirely. Standard languages like C are blind to this: if code reads a
DMA buffer before the hardware finishes writing to it, frees a buffer a
device is still writing into, forgets that a control register must be
`volatile`, or skips a required validation step before trusting a parsed
packet, the compiler builds it anyway. On real silicon, each of these is a
race condition, a silently-corrupted signal, or a security bypass — not a
hypothetical, but a well-documented, recurring class of real CVEs (see
below).

## The solution

PHASE treats physical hardware boundaries as part of the type system. It's
a strict Design Rule Check for memory pipelines: every buffer's *physical
domain* — host RAM, a DMA engine, a memory-mapped register, an external
device — is tracked and checked at compile time, the same way Rust's
borrow checker tracks aliasing, just applied to hardware-shaped safety
instead.

You write your heavy DSP/routing math in ordinary C, and write the
pipeline's coordination rules in PHASE. For every program it accepts, the
compiler guarantees:

1. **Domain safety** — the CPU never reads, borrows, or frees a buffer
   owned by hardware (`@DMA`, `@DEVICE`) without an explicit `sync()` first.
2. **Ownership/borrow safety** — many readers or one writer; read-only data
   (a read borrow, a `read` parameter) is never handed to code that writes;
   while a buffer is borrowed it can't be written, moved, or freed;
   released borrows are dead; one buffer is never passed twice to a call
   unless every use is read-only.
3. **Lifetime and type safety** — nothing is used after `destroy`; no
   function returns, and no block ends, while hardware still owns one of
   its buffers or a borrow is still live; parameters end in the domain
   their signature promises; and every buffer argument has exactly the
   element type and length the callee expects (or the callee takes any
   length, `buffer<T, _>`, and is told the real one).
4. **Typestate enforcement** — data never skips a required step in a
   protocol/validation chain. A state comes only from a function that
   returns it — an annotation can't fake one — and each state value is
   used exactly once.
5. **Branch/loop consistency** — `if`/`else` paths can't leave a buffer in
   disagreeing physical domains, and a loop body must leave every buffer
   as it found it — with `swap` to rotate buffers for ping-pong DMA.
6. **Volatile MMIO semantics** — hardware control registers are never
   silently reordered or eliminated by C compiler optimization.

The precise rules, and the trust boundary they hold within, are in
[`phase_specification.md`](phase_specification.md) §4.4.

## Evidence, not just claims

A static checker is only as good as its rules, so PHASE also checks
itself at run time:

```
$ phase build --checked --stub-externs program.phase   # instrumented build
$ cargo run --release -p phase_fuzz -- --count 1000     # differential fuzzing
1000 programs generated, 499 accepted by `phase check` (0 more only with warnings, not run), 0 soundness violations, 0 pipeline failures
```

- **`phase build --checked`** emits C that verifies every ownership rule
  on the path the program actually takes — a shadow state per buffer,
  checked at every move, sync, borrow, call, swap and scope exit — and
  exits with status 70 on any violation. It never reasons about branches
  or loops, so it is independent of the analyzer.
- **`phase-fuzz`** generates thousands of random programs, builds every
  one the analyzer accepts in checked mode, and runs it down different
  paths. An accepted program that trips a runtime check is an analyzer
  soundness bug.

Its first runs found four real bugs — two analyzer holes (a buffer left
DMA-owned at the end of an `if` block, and `swap` across blocks leaving a
dangling name), a codegen bug, and a false alarm in the checker itself —
all fixed in 1.3 and replayed on every test run. Since the fixes, 6,500
freshly generated programs (2,973 of them accepted and run) have produced
no violations, and CI fuzzes a fresh batch on every push.

And the fuzzer demonstrably finds bugs when they exist: with one of the
fixed analyzer holes deliberately put back, it reported 8 violations in
the same first 1,000 programs that the real analyzer passes cleanly; with
the most basic rule disabled (a DMA-owned buffer passed where RAM is
expected), it caught that too. Details in
[`phase_specification.md`](phase_specification.md) §6.7.

Once these checks pass, PHASE lowers the pipeline to plain, readable C.
Domain, borrow, and typestate checks are all compile-time-only and fully
erased — they add nothing to the generated code. (`sync()` is the one
exception: it's a real call into a small runtime that simulates hardware
transfer timing — see [Honest scope](#honest-scope) below.)

## See it catch a real bug, right now

```
$ phase check examples/bug_gallery/read_while_capture_in_flight.phase
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

That's a buffer read while a DMA transfer is still writing into it —
started by one function call, meant to be finished by another — rejected
before the program ever runs, with the call that handed it to the hardware
pointed out. Annotations can't talk their way past the checker either:

```
$ phase check examples/bug_gallery/forged_typestate.phase
error: 'p' is declared as Packet<Validated> but 'receive_packet' returns Packet<Received>
  --> examples/bug_gallery/forged_typestate.phase:16:9
   |
16 |     let p: Packet<Validated> = receive_packet();
   |         ^
```

And the compiler doesn't just *claim* its volatile guarantee holds —
`demos/m6_volatile_proof/run_demo.sh` compiles the same register-write
pattern by hand (no `volatile`) and with PHASE, both at `-O2`, and inspects
the real assembly:

```
naive C (no volatile):  1 store to UART_CONTROL survives   -- the "reset" write silently vanished
PHASE-generated C:       2 stores to UART_CONTROL survive   -- both writes preserved
```

## The bug gallery: preventing real-world CVE classes

Every rule PHASE enforces maps to a real, recurring bug class. The
[`examples/bug_gallery/`](examples/bug_gallery/) directory contains one
deliberately broken program per rule, each rejected at compile time:

- **[CWE-362/367] The DMA race condition** — [`use_before_sync.phase`](examples/bug_gallery/use_before_sync.phase), [`borrow_unsynced_dma.phase`](examples/bug_gallery/borrow_unsynced_dma.phase): touching a `@DMA` buffer (directly or through a borrow) before `sync()`. This exact discipline is why Linux's `dma-buf` subsystem requires explicit sync ioctls (a 2025 kernel fix, CVE-2025-38095, was needed when that sync could still race).
- **[CWE-362/367] The same race across function calls** — [`read_while_capture_in_flight.phase`](examples/bug_gallery/read_while_capture_in_flight.phase), [`broken_sync_promise.phase`](examples/bug_gallery/broken_sync_promise.phase), [`illegal_signature_transition.phase`](examples/bug_gallery/illegal_signature_transition.phase): using a buffer between "start transfer" and "wait" calls, a wait helper that never waits, or a signature promising a hand-off the hardware can't do.
- **[CWE-416] Use-after-free, including by the device** — [`destroy_while_dma.phase`](examples/bug_gallery/destroy_while_dma.phase), [`dma_escapes_function.phase`](examples/bug_gallery/dma_escapes_function.phase), [`move_while_borrowed.phase`](examples/bug_gallery/move_while_borrowed.phase), [`use_after_destroy.phase`](examples/bug_gallery/use_after_destroy.phase), [`shadowed_dma_buffer.phase`](examples/bug_gallery/shadowed_dma_buffer.phase): freeing or abandoning a buffer while a DMA engine or a borrow still uses it — the same shape as CVE-2024-43856, a 2024 Linux kernel race in `dmam_free_coherent()`.
- **[CWE-362/672] Broken exclusivity** — [`write_borrow_conflict.phase`](examples/bug_gallery/write_borrow_conflict.phase), [`use_owner_while_borrowed.phase`](examples/bug_gallery/use_owner_while_borrowed.phase), [`use_after_release.phase`](examples/bug_gallery/use_after_release.phase): two writers at once, writing behind a borrow's back, or using a borrow after giving it up.
- **[CWE-696/672] Typestate skipping and replay** — [`skipped_state_transition.phase`](examples/bug_gallery/skipped_state_transition.phase), [`forged_typestate.phase`](examples/bug_gallery/forged_typestate.phase), [`stale_typestate_reuse.phase`](examples/bug_gallery/stale_typestate_reuse.phase): a packet reaching a "validated" stage without being validated, or being processed twice — the same bug shape as Apple's 2014 "goto fail" (CVE-2014-1266), where a required verification step was silently skipped.
- **[CWE-696/691] Inconsistent paths** — [`branch_domain_disagreement.phase`](examples/bug_gallery/branch_domain_disagreement.phase): two branches leaving a buffer in different domains.
- **[CWE-362] Writing through read-only views** — [`write_through_read_borrow.phase`](examples/bug_gallery/write_through_read_borrow.phase), [`read_param_handed_to_writer.phase`](examples/bug_gallery/read_param_handed_to_writer.phase): a read borrow or a `read` parameter handed to a kernel that writes.
- **[CWE-362/367] Broken double buffering** — [`refill_while_processing.phase`](examples/bug_gallery/refill_while_processing.phase), [`ping_pong_without_swap.phase`](examples/bug_gallery/ping_pong_without_swap.phase), [`swap_while_borrowed.phase`](examples/bug_gallery/swap_while_borrowed.phase): refilling the buffer the CPU is processing (a torn frame), forgetting to trade buffers, or swapping one out from under a live view.
- **[CWE-416/825] Found by the fuzzer** — [`dma_into_dead_block.phase`](examples/bug_gallery/dma_into_dead_block.phase), [`swap_across_blocks.phase`](examples/bug_gallery/swap_across_blocks.phase): a buffer abandoned to the DMA engine at the end of an `if` block, or swapped with one from an inner block and left dangling.
- **[CWE-787/843] Wrong buffer type** — [`buffer_length_mismatch.phase`](examples/bug_gallery/buffer_length_mismatch.phase), [`buffer_element_mismatch.phase`](examples/bug_gallery/buffer_element_mismatch.phase), [`runtime_length_to_fixed.phase`](examples/bug_gallery/runtime_length_to_fixed.phase): a shorter buffer where a longer one is required (a real overflow), bytes where samples are expected, or a run-time length where an exact one is assumed.
- **[CWE-758] Aliasing and silent MMIO optimization** — [`aliased_arguments.phase`](examples/bug_gallery/aliased_arguments.phase) passes one buffer as both input and output of a kernel; [`volatile_wrong_domain.phase`](examples/bug_gallery/volatile_wrong_domain.phase) points a register access at RAM. `@MMIO` registers always lower to `volatile` C globals, so the C standard itself — not convention or code review — forbids the compiler from deleting or reordering the access.

Each gallery file states its expected error in an `// Expected:` header,
and the test suite asserts the compiler produces exactly that error, so the
gallery can't drift from its documentation.

**See the exact compiler output:** [`docs/bug_gallery_transcript.md`](docs/bug_gallery_transcript.md)
is regenerated straight from real `phase check` runs (`./scripts/capture_bug_gallery.sh`),
never hand-typed, and CI fails if it is stale. [`docs/security_mapping.md`](docs/security_mapping.md)
has the full CWE/CVE write-up.

## Why a language, not a library

A library can check each buffer access in isolation, at the call site. It
can't answer whole-program questions before any input is ever processed:
does every branch agree on a buffer's domain? Is there a path where a
required validation step gets skipped, or a buffer is abandoned while a
device still owns it? PHASE answers these by tracking every entity through
each function's control flow and analyzing it as a unit — the same
category of problem as a borrow checker, not something a proc-macro or a
lint pass can do from inside a host language. See
[`phase_specification.md`](phase_specification.md) §2 for the full
rationale.

## Architecture & repository layout

```
source (.phase)
  → lexer         hand-written, source -> tokens
  → parser        hand-written recursive descent, tokens -> AST
  → analysis      names, domains, ownership/borrows, lifetimes, typestate, branch/loop merges, MMIO
  → pir           AST -> a flat per-function intermediate representation
  → codegen_c     PIR -> readable C
  → cc            real C compiler -> a real binary
```

```text
crates/
  ast/         AST node definitions
  lexer/       hand-written lexer (source -> tokens)
  parser/      hand-written recursive-descent parser (tokens -> AST)
  analysis/    the safety analyzer (all checks above)
  pir/         Phase Intermediate Representation builder
  codegen_c/   lowers PIR to raw C source
  driver/      the `phase` CLI
  fuzz/        phase-fuzz: differential fuzzer (+ regressions/, every past finding)

runtime/
  phase_runtime.h/.c   generic sim_dma_wait/sim_device_wait, always linked
  phase_check.h/.c     runtime ownership checker, linked by `phase build --checked`
  *_extern.c           one per demo program, implementing that program's extern fns
  hw/                  real ARM Cortex-M3 backend (startup, linker script,
                       real MMIO register map, real sim_dma_wait/sim_device_wait)

examples/
  *.phase              programs that pass analysis (sdr_session.phase is the fullest)
  bug_gallery/         one deliberately-broken file per bug class

demos/
  m6_volatile_proof/   before/after proof that a C compiler really does delete non-volatile writes

docs/
  bug_gallery_transcript.md   generated: real compiler output for every example
  security_mapping.md         the CWE/CVE write-up

scripts/
  capture_bug_gallery.sh      regenerates the transcript from real compiler output
  build_hw_demo.sh            cross-compile + run the real-hardware demo under QEMU
```

## Building and testing

**Requirements:** a recent stable Rust toolchain and a C compiler (`cc`).
The hardware demo additionally needs `arm-none-eabi-gcc` and
`qemu-system-arm` (`sudo apt-get install --no-install-recommends
gcc-arm-none-eabi qemu-system-arm`) — everything else works without them,
and the one test that needs them
(`hw_uart_echo_runs_on_a_real_arm_cortex_m3_target_under_qemu`) skips
itself with a clear message if they're missing. (CI sets
`PHASE_REQUIRE_HW_TOOLS=1`, which turns that skip into a failure.)

```bash
cargo build --release
cargo test --workspace
```

Plain `cargo test`, no custom runner — including end-to-end tests that
invoke a real C compiler, run the resulting binaries, and inspect real
`-O2` assembly output (including proving a `volatile_read` inside a
`while` condition survives optimization); a real cross-compile to ARM
Cortex-M3 run under QEMU; tests that drive the `phase` binary itself; a
sweep asserting every bug-gallery file is rejected with its documented
error; runtime-checker tests (each kind of violation, compiled without the
analyzer, must be caught; every example must run clean when checked); and
a fuzzing pass that replays every past finding and tries a fresh batch.
CI ([`.github/workflows/ci.yml`](.github/workflows/ci.yml)) also runs
`cargo fmt --check`, `clippy -D warnings`, the volatile proof demo, 1,500
freshly fuzzed programs, and a staleness check on the transcript.

```bash
# Check a file for safety without generating C (exit code 0 = safe, 1 = rejected)
phase check examples/sdr_session.phase

# One `file:line:col: error: ...` line per problem, for editors and scripts
phase --error-format=short check examples/bug_gallery/destroy_while_dma.phase

# Build & run — translates to C, links the runtime, compiles with cc
phase build examples/sdr_session.phase
./build/sdr_session

phase build examples/mmio_registers.phase
./build/mmio_registers

# The same program with every ownership rule checked at run time
phase build --checked examples/ping_pong.phase
./build/ping_pong-checked

# Try a program without writing any C: placeholder externs, paths picked
# by PHASE_STUB_SEED
phase build --stub-externs my_program.phase
PHASE_STUB_SEED=3 ./build/my_program

# Differential fuzzing (findings are saved to fuzz-findings/)
cargo run --release -p phase_fuzz -- --count 1000

# The adversarial volatile proof (naive C vs. PHASE, both at -O2)
./demos/m6_volatile_proof/run_demo.sh

# Regenerate docs/bug_gallery_transcript.md from real compiler output
./scripts/capture_bug_gallery.sh
```

(`phase` is `target/release/phase`, or use `cargo run --release --bin phase --`.)

`phase build` works from any directory: it finds the runtime by walking
up from the source file to the nearest `runtime/` directory (override with
`PHASE_RUNTIME_DIR=<dir>`) and writes the generated C and the binary into
the `build/` directory next to it.

`examples/sdr_session.phase` is the fullest single demo — DMA/device
capture-filter-playback, a status packet that must pass through a full
`Received -> Decoded -> Validated` typestate chain, and MMIO register
control, composed in one program that actually builds and runs.

`examples/ping_pong.phase`: ping-pong double buffering — the DMA engine
fills one buffer while the CPU processes the other, and `swap ping, pong;`
trades them each frame. Seven frames, processed in order from alternating
buffers, each next capture already in flight; the checker proves no frame
is touched before it's synced or refilled while being processed.

`examples/multi_rate.phase`: one set of helpers, written once with
length-generic parameters (`buffer<Sample, _>`), used for a 64- and a
256-sample capture — each kernel is told the real length — while a kernel
that needs exactly 64 still gets an exact, checked length.

`examples/overlapped_capture.phase`: one call starts a DMA capture
(`start_capture(dst: buffer<..> @RAM -> @DMA)`), the CPU computes a window
while it runs, and a PHASE helper declared `@DMA -> @RAM` waits for it —
the overlap DMA exists for, with the buffer tracked across both calls.

`examples/branching_pipeline.phase`: a real `if`/`else` that compiles to
real C control flow and actually takes either branch at runtime, picked by
an extern call (`PHASE_DEMO_USE_DMA=0 ./build/branching_pipeline` forces
the `@DEVICE` path; unset or non-zero takes `@DMA`).

`examples/counting_loop.phase`: a `while` loop with a real `n = n + 1;`
counter that runs exactly 5 iterations and stops on its own.

`examples/hw_uart_echo.phase`: the exact same compiler and PHASE source
shape, cross-compiled for a real ARM Cortex-M3 target and run on QEMU's
model of the real MPS2 AN385 board (`./scripts/build_hw_demo.sh`) — its
`sync()` is a real register poll on real (emulated) hardware, not the
host's simulated address space.

`examples/mmio_poll.phase`: a real
`while volatile_read(READY @MMIO) == 0 { .. }` polling loop, with the
poll itself expressed in PHASE source rather than hand-written in the
runtime — it polls simulated hardware state across exactly 3 iterations
before terminating.

## Honest scope

PHASE 1.x is a focused, research-grade compiler: its guarantees are
precise and tested, but its language is deliberately small. Limitations
below are documented on purpose rather than left as silent gaps someone
has to discover the hard way:

- **`extern fn` bodies are trusted.** PHASE checks every call against an
  extern's signature, but can't see inside the C. It assumes a kernel
  touches only the buffers it's given, doesn't keep them past the call,
  and really performs the state transition its return type claims (that's
  what makes `validate()` mean *validated*). A buggy extern can break any
  guarantee — the same contract as Rust's `unsafe` FFI.
- **Analysis is per-function.** Each body is checked against the
  signatures it calls. A signature can declare that a function moves an
  argument to another domain (`@RAM -> @DMA`); anything it doesn't
  declare, it must undo before returning.
- **Assignment is scoped to plain scalar locals.** `name = expr;`
  reassigns a `let`-bound scalar; entities and buffers go through
  `move`/`sync`/`borrow`/`destroy` on purpose, and can't be copied with
  `let` or shadowed.
- **`if`/`while` conditions and assignment right-hand sides are a
  deliberately small subset of expressions** (identifiers, literals,
  binary comparisons/arithmetic, and a direct `volatile_read(REG @MMIO)`)
  — not the full `Expr` grammar. A condition using any other call is a
  clear `UnsupportedExpr` error, not a silent guess: general `extern fn`
  calls inside a condition would be re-invoked every loop iteration with
  much less clear ordering/purity guarantees than a register read.
- **Every program with an `extern fn` needs a matching hand-written
  runtime file**, `runtime/<stem>_extern.c` (e.g. `sdr_session.phase` →
  `runtime/sdr_session_extern.c`). The compiler only ever generates a
  *prototype* for `extern fn`, never a body. `phase build` fails early
  with a clear message naming the missing file — or, with
  `--stub-externs`, generates placeholders to try the program without one.
- **The runtime checker and the fuzzer cover buffers.** Typestate and
  struct entities are checked statically but not at run time, and the
  fuzzer generates single-size `u8` buffers. Their silence is strong
  evidence for the buffer rules, not a proof of everything.
- **No real hardware target *by default*.** `phase build` links the
  simulated runtime; `examples/hw_uart_echo.phase` proves the swap to a
  real ARM Cortex-M3 target works (`./scripts/build_hw_demo.sh`, validated
  on QEMU's model of the real MPS2 AN385 board), but it's a separate
  cross-compile step, not a `phase build` flag.
- **No LLVM backend.** Compiling to readable C was a deliberate choice so
  the engineering effort goes into the analysis passes, not backend
  plumbing.
- **Loops are checked by invariant, not by iteration.** A `while` body
  must leave every pre-existing entity exactly as it found it. That makes
  the single-pass check sound for any iteration count; a body that rotates
  buffers between roles restores the invariant with `swap`. Patterns that
  need more state than that (say, a ring of three buffers advanced by
  index) aren't expressible yet.
- **Unannotated parameters are trusted to write.** A buffer parameter
  without `read`/`write` is assumed to possibly write. Passing read-only
  data to one is a warning rather than an error, so 1.1 programs keep
  compiling; annotate your externs to make it exact.

Full rationale for all of these is in
[`phase_specification.md`](phase_specification.md) §4.4 and §10.

## What's next

1.3 adds runtime-checked builds, differential fuzzing, and
length-generic, type-checked buffer parameters on top of 1.2's access
modes and double buffering (see [`CHANGELOG.md`](CHANGELOG.md)). Likely
next steps:

- **A tutorial** — "PHASE in 15 minutes", from a first buffer to a
  ping-pong pipeline.
- **Wider fuzzing** — typestate, entities, and varied buffer types and
  sizes in the generator and the runtime checker.
- **M12 — An LLVM backend**, removing the dependency on an external `cc`
  ([`phase_specification.md`](phase_specification.md) §9).
- **A real DMA controller** — the ping-pong pipeline on a board with one
  (e.g. an STM32 Nucleo), instead of QEMU's MPS2 model, which has none.

For the complete language specification, domain model, type system, and
full milestone roadmap, see
[`phase_specification.md`](phase_specification.md).

## License

Apache-2.0 — see [`LICENSE`](LICENSE).
