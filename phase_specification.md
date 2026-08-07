# PHASE — Complete Specification

**A physical-domain safety language for signal, RF, and embedded pipelines**

Version 0.1 — Draft

---

## Table of Contents

1. [Project Definition & Scope](#part-1-project-definition--scope)
2. [Physical Domain Model](#part-2-physical-domain-model)
3. [Language Reference](#part-3-language-reference)
4. [Type System & Static Analysis](#part-4-type-system--static-analysis)
5. [Runtime & Simulated Hardware](#part-5-runtime--simulated-hardware)
6. [Compiler Architecture](#part-6-compiler-architecture)
7. [PIR — Phase Intermediate Representation](#part-7-pir--phase-intermediate-representation)
8. [Bug Gallery — What the Compiler Must Catch](#part-8-bug-gallery--what-the-compiler-must-catch)
9. [Implementation Roadmap & Milestones](#part-9-implementation-roadmap--milestones)
10. [Non-Goals](#part-10-non-goals)

---

<a name="part-1-project-definition--scope"></a>
## Part 1: Project Definition & Scope

### 1.1 One-sentence definition

PHASE is a small, standalone, compiled language whose type system proves that
buffers moving between physically distinct execution domains (ADC/DAC front
ends, DMA engines, MMIO-mapped devices, host RAM) are never accessed by the
wrong domain at the wrong time.

### 1.2 The problem it solves

In embedded, RF, and driver-adjacent code, a large and recurring class of bugs
and CVEs comes from exactly one pattern: **a buffer is read or written by a
domain that does not currently own it.**

Examples of this pattern in the wild:

- CPU reads a DMA buffer before the transfer engine has finished writing it.
- A device is handed a buffer, the buffer is freed or reused on the host
  side before the device is done with it (use-after-free across a domain
  boundary).
- An MMIO register is read/written without accounting for side effects,
  and the compiler "optimizes away" an access that mattered.
- A packet is parsed/decoded before the underlying transfer/validation step
  has actually completed (TOCTOU across a boundary).

C does not track any of this. Rust's borrow checker tracks aliasing *within
one execution domain* but has no concept of "this memory is currently owned
by a DMA engine." PHASE's entire reason to exist is to make this specific bug
class a compile-time error instead of a runtime incident.

### 1.3 What PHASE is NOT trying to be

- Not a general-purpose systems language (no ambition to replace C/Rust/Zig).
- Not a DSP language. PHASE does not know what an FFT or FIR filter is — it
  calls out to existing, trusted C/Rust kernels for the actual math and only
  reasons about *when* it is legal to touch the buffers those kernels operate
  on.
- Not a GPU/FPGA compiler. Accelerators are modeled as a generic `DEVICE`
  domain with the same handoff rules as everything else; specialized
  GPU/FPGA support is explicitly future work.
- Not targeting real silicon in v0.1. The first target is simulated hardware
  running on the host, so the project is fully testable without an SDR or a
  microcontroller.

### 1.4 Why this is a good portfolio compiler project

- It has a genuine, novel static-analysis core (a domain-aware borrow
  checker) — not a rehash of a textbook toy language.
- It maps directly onto a real, nameable bug class (embedded/driver
  use-after-free and TOCTOU bugs), which gives the project a concrete
  security narrative, not just "look, a compiler."
- It is scoped so every milestone produces something runnable and
  demoable, rather than requiring the whole system to exist before anything
  works.
- Compiling to readable C (not LLVM) for v0.1 keeps 100% of the engineering
  effort on the actually-novel part of the project (the physical analysis)
  instead of backend plumbing.

---

<a name="part-2-physical-domain-model"></a>
## Part 2: Physical Domain Model

### 2.1 Domains (v0.1 fixed set)

PHASE v0.1 supports exactly four domains. This is intentionally small — the
whole point is depth, not breadth.

| Domain    | Meaning                                             | CPU-readable by default? |
|-----------|------------------------------------------------------|---------------------------|
| `@RAM`    | Ordinary host memory                                  | yes                       |
| `@STACK`  | Function-local memory                                 | yes                       |
| `@DMA`    | Memory owned/transferred by a DMA engine              | no — requires `sync`      |
| `@MMIO`   | Memory-mapped hardware register                       | yes, but every access is an observable side effect |
| `@DEVICE` | Generic external domain (ADC, DAC, radio front end, accelerator, network interface) | no — requires `sync` or explicit handoff |

`@DEVICE` is deliberately generic in v0.1. A real ADC, a real GPU, and a real
network card all behave the same way from the *safety* point of view: you
hand them a buffer, they own it for a while, you get it back after a
synchronization point. Specializing `@DEVICE` into named subtypes
(`@GPU`, `@RADIO`, `@NIC`) is future work and does not require changing the
core analysis.

### 2.2 Entity model

Every value that occupies a domain is an **entity**:

```
Entity =
    Identity        (unique id)
    + Type           (element type, e.g. i16, Sample<f32>)
    + Domain         (@RAM, @DMA, @MMIO, @DEVICE)
    + Ownership State (Owned, Borrowed, DeviceOwned, Destroyed)
    + History        (sequence of transitions it has undergone)
```

### 2.3 Ownership states

```
Owned          -- normal, accessible per domain rules
Borrowed(R|W)  -- temporarily lent out; original owner cannot conflict
DeviceOwned    -- handed to @DMA or @DEVICE; host cannot access until synced
Destroyed      -- lifetime ended; any further access is an error
```

### 2.4 Legal transitions (v0.1 table)

```
@STACK  --move-->        @RAM
@RAM    --move-->        @DMA        (buffer becomes DeviceOwned)
@DMA    --sync-->        @RAM        (buffer becomes Owned again, CPU-visible)
@RAM    --move-->        @DEVICE     (buffer becomes DeviceOwned)
@DEVICE --sync-->        @RAM        (buffer becomes Owned again)
@RAM    --borrow(R|W)--> @RAM        (temporary, no domain change)
@MMIO   --volatile_read-->  @RAM     (value only, not the register itself)
@RAM    --volatile_write--> @MMIO
```

Everything not in this table is a compile error: **"no legal transition from
X to Y."** This table is intentionally a flat, hardcoded structure in v0.1
(see §6.5) — not a plugin system yet. Making it pluggable is future work.

### 2.5 Costs and observability

Every transition has a declared cost class used only for diagnostics/tracing
in v0.1 (not for optimization yet):

```
move            -- cheap, same-domain-family
sync            -- potentially blocking, waits on hardware
volatile access -- always observable, never eliminated by codegen
```

---

<a name="part-3-language-reference"></a>
## Part 3: Language Reference

### 3.1 Lexical elements

Identifiers, integer/float literals, string literals, comments (`//`, `/* */`).
Keywords: `entity`, `fn`, `let`, `move`, `sync`, `borrow`, `release`, `destroy`,
`domain`, `state`, `if`, `else`, `while`, `return`, `struct`, `extern`.

### 3.2 Entities and domains

```phase
entity Sample : @RAM {
    value: f32
}

fn main() {
    let s: Sample @RAM = Sample { value: 0.0 };
}
```

### 3.3 Buffers

```phase
buffer<Sample, 1024> radio_buf @DMA;
```

A `buffer<T, N>` is the core aggregate type — a fixed-size, domain-tagged
array of entities. This is the primary object DMA/DEVICE code manipulates.

### 3.4 move

```phase
move radio_buf -> @DMA;
```

Transfers ownership of `radio_buf` from its current domain to `@DMA`. After
this line, `radio_buf` is `DeviceOwned` and any CPU access is a compile
error until it is synced back.

### 3.5 sync

```phase
sync(radio_buf);
```

Makes a `DeviceOwned` entity `Owned` and CPU-visible again. Lowers to a
runtime wait on the simulated (or real) transfer engine.

### 3.6 borrow / release

```phase
let view = borrow radio_buf read;
// ... read-only access ...
release view;
```

Multiple concurrent `read` borrows are legal. A `write` borrow must be
exclusive (no other borrows of any kind may be live). Attempting a `move` or
`destroy` while a borrow is live is a compile error.

### 3.7 volatile MMIO access

```phase
let status: u32 = volatile_read(UART_STATUS @MMIO);
volatile_write(UART_CONTROL @MMIO, 0x1);
```

Volatile accesses are never eliminated, reordered across each other, or
cached by codegen, regardless of optimization level.

### 3.8 Typestate (protocol states)

```phase
state Packet {
    Received -> Decoded -> Validated
}

fn decode(p: Packet<Received>) -> Packet<Decoded> { ... }
```

A function that requires `Packet<Decoded>` cannot be called with a
`Packet<Received>` — this is checked the same way a domain mismatch is: at
compile time, by the state analyzer described in §4.

### 3.9 Functions and extern kernels

```phase
extern fn fir_filter(input: buffer<Sample, 1024> @RAM,
                      output: buffer<Sample, 1024> @RAM);
```

`extern fn` declares a trusted, opaque operation (typically a C function)
that PHASE does not analyze internally — it only checks that the domains of
the arguments match the declared signature at the call site. This is how DSP
math re-enters the pipeline without PHASE needing to understand it.

### 3.10 Example — a minimal end-to-end pipeline

```phase
extern fn fir_filter(in_: buffer<Sample, 1024> @RAM,
                      out: buffer<Sample, 1024> @RAM);

fn main() {
    buffer<Sample, 1024> raw @DMA;
    device_capture(raw);       // simulated ADC fills the DMA buffer
    sync(raw);                 // wait for capture to finish; entity is now @RAM

    buffer<Sample, 1024> filtered @RAM;
    fir_filter(raw, filtered);

    move filtered -> @DEVICE;  // hand to simulated DAC
    device_playback(filtered);
    sync(filtered);
}
```

---

<a name="part-4-type-system--static-analysis"></a>
## Part 4: Type System & Static Analysis

### 4.1 The extended type

```
PType = { BaseType, Domain, OwnershipState, TypestateLabel? }
```

Two values are only interchangeable if all four components match (or the
mismatch is resolved by an explicit `move`/`sync`/state-transition function).

### 4.2 Analysis passes (matches compiler pipeline in Part 6)

1. **Domain checker** — every access is checked against the entity's current
   domain and the legal-transition table (§2.4).
2. **Ownership/borrow checker** — enforces: many-readers/one-writer; no
   access while `DeviceOwned`; no `move`/`destroy` while borrowed.
3. **State checker (typestate)** — enforces declared state machines (§3.8);
   a call requiring state `S` on an entity currently in state `S'` is
   rejected unless a transition function `S' -> S` exists and was invoked.
4. **Lifetime checker** — no use after `destroy`; no entity escapes the
   scope that owns it without an explicit `move`.

### 4.3 Control flow and merging

At a branch/join point, an entity may have different domains/states on
different incoming paths (e.g. synced on one branch, not on the other). The
merge point requires all incoming states to agree; if they don't, this is a
compile error requiring the programmer to reconcile it (e.g. insert a `sync`
on the branch that's missing one). This is the direct analogue of KAIROS's
`ENTITY_PHI` (Part 6, §10 of the KAIROS doc) and is the single hardest piece
of the analyzer — flagged explicitly as its own milestone (§9, Milestone 4).

### 4.4 What the type system guarantees (design goal)

No use of a `DeviceOwned` buffer by the host without a `sync`; no write-borrow
that overlaps a read-borrow or another write-borrow; no access after
`destroy`; no call requiring typestate `S` on a value not proven to be in
state `S`; no elimination/reordering of `@MMIO` accesses.

---

<a name="part-5-runtime--simulated-hardware"></a>
## Part 5: Runtime & Simulated Hardware

### 5.1 Why simulated hardware

v0.1 targets host-only execution so the whole project is testable and
demoable without real RF/embedded hardware. Every "device" is a small C
runtime component with realistic *timing and ownership* behavior, not real
signal behavior.

### 5.2 Simulated components (v0.1)

- `sim_dma` — models a transfer engine: `start_transfer`, background delay,
  `is_done`, `wait`. Used to back `move -> @DMA` / `sync`.
- `sim_adc` / `sim_dac` — generic `@DEVICE` capture/playback stand-ins;
  `sim_adc` can optionally fill a buffer with a synthetic signal (sine +
  noise) so the demo pipeline has something real to filter.
- `sim_mmio` — a fake register file in a struct, with a hook to log every
  volatile access (used to prove volatile accesses aren't dropped by
  codegen).

### 5.3 Real hardware (future work, not v0.1)

The runtime interface (`start_transfer`, `wait`, `volatile_read/write`) is
written so a real backend (Linux DMA-API, an SDR driver, a microcontroller
HAL) could implement the same interface later without changing the compiler.
Not attempted in v0.1.

---

<a name="part-6-compiler-architecture"></a>
## Part 6: Compiler Architecture

### 6.1 Pipeline

```
Source -> Lexer -> Parser -> AST -> Name Resolution -> Type Checker
       -> Domain/Ownership/State/Lifetime Analyzer -> PIR Generator
       -> PIR Optimizer (minimal, v0.1) -> C Codegen -> cc -> Binary
```

### 6.2 Why C, not LLVM, for v0.1

Compiling to readable C keeps 100% of engineering time on the analyzer (the
actually novel part). LLVM backend is explicitly deferred (§10).

### 6.3 Crate layout (Rust workspace)

```
phase/
  crates/
    lexer/          -- tokens
    parser/         -- AST construction
    ast/            -- AST type definitions (shared)
    sema/            -- name resolution + type checker
    analysis/        -- domain, ownership, state, lifetime analyzers
    pir/             -- Phase IR data structures + builder
    codegen_c/        -- PIR -> C source
    driver/          -- CLI: phase build, phase check, phase --dump-pir
  runtime/
    sim_dma.c / .h
    sim_adc.c / .h
    sim_dac.c / .h
    sim_mmio.c / .h
  examples/
    radio_pipeline.phase
    bug_gallery/
      use_before_sync.phase
      write_borrow_conflict.phase
      use_after_destroy.phase
      missing_state_transition.phase
```

### 6.4 CLI (v0.1)

```
phase check <file>        -- run all analysis passes, report errors, no codegen
phase build <file>        -- full pipeline, emits a native binary
phase --dump-ast <file>
phase --dump-pir <file>
phase --trace-entity <name> <file>   -- print full history of one entity
```

### 6.5 Transition table representation

v0.1 keeps the domain-transition table (§2.4) as a hardcoded Rust `match` /
static table inside `analysis`, not a plugin/config file. This is a
deliberate scope cut versus KAIROS's `.hardware` file idea — making it
data-driven is easy future work once the core analyzer is proven correct.

---

<a name="part-7-pir--phase-intermediate-representation"></a>
## Part 7: PIR — Phase Intermediate Representation

### 7.1 Purpose

PIR is the internal representation between the analyzer and C codegen. It
preserves everything C cannot express natively: entity identity, domain,
ownership state, and history.

### 7.2 Core instructions

```
ENTITY_CREATE   id, type, domain
ENTITY_MOVE     id, from_domain, to_domain
ENTITY_SYNC     id
ENTITY_BORROW   id, mode(R|W)
ENTITY_RELEASE  id
ENTITY_TRANSFORM id, from_state, to_state
ENTITY_DESTROY  id
ENTITY_CALL     fn, args[]           -- extern fn calls (DSP kernels)
ENTITY_PHI      id, [ (block, domain, state) ... ]   -- merge point
```

### 7.3 Verification (must hold before codegen)

1. Every entity has exactly one domain at every program point.
2. Every `ENTITY_MOVE`/`ENTITY_SYNC` corresponds to a legal transition.
3. No `ENTITY_*` access occurs after that entity's `ENTITY_DESTROY`.
4. No conflicting borrows are live simultaneously.
5. Every `ENTITY_PHI` has agreeing domain/state across all incoming edges.

### 7.4 Lowering to C

```
@RAM     -> plain pointer
@DMA     -> pointer + call to sim_dma_wait() at each ENTITY_SYNC
@MMIO    -> volatile pointer, direct volatile read/write, never cached
@DEVICE  -> pointer + call to the relevant sim_* function
```

### 7.5 Debug output

`--dump-pir` prints each entity's full instruction history — the direct
analogue of KAIROS's entity history tracking, and the main artifact used in
the bug-gallery demos (§8) to show *why* something was rejected.

---

<a name="part-8-bug-gallery--what-the-compiler-must-catch"></a>
## Part 8: Bug Gallery — What the Compiler Must Catch

Each entry below is one deliberately-broken example program plus the exact
compile error PHASE must produce. This doubles as the acceptance-test suite
for the analyzer and as the security narrative for the portfolio write-up.

| # | Program pattern | Real-world analogue | Compiler must report |
|---|---|---|---|
| 1 | CPU reads a `@DMA` buffer with no prior `sync` | Reading a transfer buffer before DMA completion | "entity `X` is DeviceOwned; access requires sync" |
| 2 | `move` or `destroy` while a `borrow` is live | Freeing a buffer another part of the code still references | "entity `X` has a live borrow; cannot move/destroy" |
| 3 | Two concurrent `write` borrows | Data race on a shared buffer | "entity `X` already has an exclusive write borrow" |
| 4 | Access after `destroy` | Use-after-free | "entity `X` was destroyed at line N" |
| 5 | Calling a function requiring `Packet<Decoded>` with a `Packet<Received>` | Parsing/validating a packet in the wrong order (TOCTOU) | "expected state Decoded, found Received" |
| 6 | Branch merges with mismatched domain/state on the two paths | Missing sync on an error path | "entity `X` disagrees on domain/state across incoming branches" |
| 7 | Reordering/eliding a `volatile_read`/`volatile_write` sequence | Compiler "optimizing away" a hardware side effect | (verified negatively — codegen test asserts both accesses appear in emitted C, in order) |

---

<a name="part-9-implementation-roadmap--milestones"></a>
## Part 9: Implementation Roadmap & Milestones

Each milestone below produces something runnable/testable — no milestone
depends on the whole system existing first.

**M1 — Lexer + Parser + AST**
Grammar for entities, domains, `move`/`sync`/`borrow`, functions. Output:
`phase --dump-ast` works on `examples/radio_pipeline.phase` (straight-line,
no branches yet).

**M2 — Type checker + fixed transition table, straight-line programs only**
No control flow. Domain checker + ownership checker for sequential code.
Output: `phase check` correctly accepts the happy-path example and rejects
bug-gallery items #1–#4.

**M3 — State analyzer (typestate)**
Adds `state`/typestate checking. Output: rejects bug-gallery item #5.

**M4 — Control flow + ENTITY_PHI / merge checking**
`if`/`while` support in AST, parser, and analyzer. This is the hardest
milestone — budget the most time here. Output: rejects bug-gallery item #6.

**M5 — PIR generation + C codegen + sim runtime**
Full pipeline to a running binary against `sim_dma`/`sim_adc`/`sim_dac`.
Output: `phase build` on the happy-path pipeline produces a binary that
actually runs the simulated capture → filter → playback sequence.

**M6 — MMIO + volatile semantics**
Output: rejects/accepts appropriately, and a codegen test proves volatile
ordering is preserved (bug-gallery item #7).

**M7 — Demo + write-up**
Full `radio_pipeline.phase` demo (synthetic signal in, FIR filter via
`extern fn`, synthetic signal out) plus the full bug gallery with real
compiler error output captured, plus a short doc mapping each caught bug
class to a real-world CVE/incident category.

**M8 — Real control-flow codegen**
Closes M5's documented scope cut: `PirInst::If`/`PirInst::While` lower
`if`/`while` to real, correctly-indented, arbitrarily-nested C `if`/
`while` statements (`phase_pir`/`phase_codegen_c`), instead of
`phase build` rejecting any body that uses them. Output:
`examples/branching_pipeline.phase` builds with `cc` and a driver
end-to-end test runs *both* branches of a real conditional, selected at
runtime (`PHASE_DEMO_USE_DMA`) -- not just that the M4 analyzer accepts
the branching source. Condition expressions are deliberately scoped to
`PirExpr` (identifiers, literals, binary comparisons/arithmetic) rather
than the full `Expr` grammar; see the Honest Scope note on assignment
below for why a loop can't yet mutate its own condition.

**M9 — Assignment + richer expressions**
The language had no assignment statement -- a `let`-bound local never
changed value after it was declared, so a `while` loop's condition could
never change between iterations. M9 adds `name = expr;`
(`Stmt::Assign`/`PirInst::Assign`), scoped to plain scalar locals only
(domain-tracked entities/buffers keep using `move`/`sync`/`borrow`/
`destroy`, not assignment -- reassigning through them would bypass those
checks). Output: `examples/counting_loop.phase` builds and a driver
end-to-end test runs a real `while n < 5 { .. n = n + 1; }` loop and
confirms it prints exactly `n=0` through `n=4`, not zero times, not
forever. Condition/RHS expressions are still the same `PirExpr` subset
from M8 (identifiers, literals, binary comparisons/arithmetic) -- no
calls yet, so a condition can't directly poll `volatile_read(...)`.

**M10 — Real hardware target**
Replaces `runtime/phase_runtime.c`'s two simulated wait functions
(`sim_dma_wait`/`sim_device_wait`) with `runtime/hw/phase_runtime_hw.c`,
a real busy-poll of a real, documented hardware register -- on a real
ARM Cortex-M3 target (the MPS2 AN385 board), not the host's simulated
address space. By design this only touches the runtime layer: the
compiler, `phase_codegen_c`'s output, and the PHASE source
(`examples/hw_uart_echo.phase`) are all byte-for-byte the same as every
other example. Output: `./scripts/build_hw_demo.sh` cross-compiles with
`arm-none-eabi-gcc` (`-mcpu=cortex-m3 -mthumb -ffreestanding -nostdlib`)
against a real linker script/vector table (`runtime/hw/`) and runs the
resulting ARM binary under `qemu-system-arm -M mps2-an385` -- QEMU's
cycle-accurate model of that real board, the standard way embedded
engineers validate firmware without physical silicon on the bench.
Verified three ways: (1) a driver end-to-end test cross-compiles and
actually runs the binary under QEMU, asserting on its real UART0 output;
(2) `arm-none-eabi-objdump` disassembly confirms `sim_device_wait` really
compiles to `mov r2, #0x40004000` / `ldr`/`tst`/`bne` -- an actual
register poll, not a stand-in; (3) the CMSDK UART register map
(`runtime/hw/mps2an385.h`) is cross-checked against QEMU's own board
model source, not guessed. Honest limit: the MPS2 AN385 QEMU model has
no real DMA controller, so both wait functions poll the one real
peripheral this board exposes (the UART) -- a target with an actual DMA
controller would poll that controller's own completion register the
same way, same pattern, different register.

**M11 — LLVM backend** *(not yet started, documented non-goal for v0.1)*
An alternative to `codegen_c` that lowers PIR straight to LLVM IR instead
of C, avoiding a dependency on an external `cc`. See §10 for why this was
deliberately deferred rather than attempted first.

**Suggested testing approach throughout:** every milestone gets a
`tests/` directory of `.phase` files with an expected-output (`.stdout`/
`.stderr`) golden file, run via `cargo test` -- both accept-cases and the
bug-gallery reject-cases, so regressions in the analyzer are caught
immediately as the language grows.

---

<a name="part-10-non-goals"></a>
## Part 10: Non-Goals

- Not a replacement for C, Rust, or CUDA.
- No LLVM backend in v0.1 (documented future work).
- No real hardware target in v0.1 (documented future work).
- No GPU kernel compilation — accelerators are just another `@DEVICE`.
- No garbage collector; no automatic parallelism; no distributed/NUMA
  domains.
- No plugin/config-file-driven hardware description in v0.1 — the
  transition table is fixed and hardcoded (§6.5), matching v0.1 scope.
- Not attempting to be a general-purpose language: `if`/`while`/functions
  exist only to the extent the pipeline-safety demos need them.
