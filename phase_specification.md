# PHASE — Complete Specification

**A physical-domain safety language for signal, RF, and embedded pipelines**

Version 1.1

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
- Not tied to real silicon. The default target is simulated hardware
  running on the host, so the project is fully testable without an SDR or a
  microcontroller; a real ARM Cortex-M3 runtime exists alongside it (§5.3).

### 1.4 Why this is a good portfolio compiler project

- It has a genuine, novel static-analysis core (a domain-aware borrow
  checker) — not a rehash of a textbook toy language.
- It maps directly onto a real, nameable bug class (embedded/driver
  use-after-free and TOCTOU bugs), which gives the project a concrete
  security narrative, not just "look, a compiler."
- It is scoped so every milestone produces something runnable and
  demoable, rather than requiring the whole system to exist before anything
  works.
- Compiling to readable C (not LLVM) for v1 keeps 100% of the engineering
  effort on the actually-novel part of the project (the physical analysis)
  instead of backend plumbing.

---

<a name="part-2-physical-domain-model"></a>
## Part 2: Physical Domain Model

### 2.1 Domains (v1 fixed set)

PHASE v1 supports exactly five domains. This is intentionally small — the
whole point is depth, not breadth.

| Domain    | Meaning                                             | CPU-readable by default? |
|-----------|------------------------------------------------------|---------------------------|
| `@RAM`    | Ordinary host memory                                  | yes                       |
| `@STACK`  | Function-local memory                                 | yes                       |
| `@DMA`    | Memory owned/transferred by a DMA engine              | no — requires `sync`      |
| `@MMIO`   | Memory-mapped hardware register                       | yes, but every access is an observable side effect |
| `@DEVICE` | Generic external domain (ADC, DAC, radio front end, accelerator, network interface) | no — requires `sync` or explicit handoff |

`@DEVICE` is deliberately generic in v1. A real ADC, a real GPU, and a real
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

### 2.4 Legal transitions (v1 table)

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
X to Y."** This table is intentionally a flat, hardcoded structure in v1
(see §6.6) — not a plugin system yet. Making it pluggable is future work.

### 2.5 Costs and observability

Every transition has a declared cost class used only for diagnostics/tracing
in v1 (not for optimization yet):

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

A parameter's domain is what the argument must be in when the function is
called; unless the parameter declares a change (§3.11), the function hands
it back in that same domain.

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

### 3.11 Domain changes in signatures

```phase
extern fn start_capture(dst: buffer<Sample, 256> @RAM -> @DMA);

fn wait_capture(b: buffer<Sample, 256> @DMA -> @RAM) {
    sync(b);
}

fn main() {
    buffer<Sample, 256> frame @RAM;
    start_capture(frame);   // frame is now @DMA
    make_window(window);    // CPU work while the transfer runs
    wait_capture(frame);    // frame is @RAM again
}
```

A parameter written `name: T @FROM -> @TO` declares that the function
changes the argument's domain:

- **Signature.** `FROM -> TO` must be a legal transition (§2.4): a `move`
  (`@STACK -> @RAM`, `@RAM -> @DMA`, `@RAM -> @DEVICE`) or a `sync`
  (`@DMA -> @RAM`, `@DEVICE -> @RAM`). A change that changes nothing
  (`@RAM -> @RAM`) is rejected.
- **Call site.** The argument must be in `FROM` and must be an owner, not
  a borrow (a borrow cannot hand the storage it views to hardware). After
  the call the argument is in `TO`, exactly as if the caller had written the
  `move`/`sync` itself.
- **Body** (for a PHASE `fn`). The parameter starts in `FROM` and must be
  in `TO` at every `return`, explicit or falling off the end.
- **`extern fn`.** The change is part of the trusted contract (§4.4): an
  extern declared `@RAM -> @DMA` is trusted to have started a transfer, one
  declared `@DMA -> @RAM` to have waited for it.

This is what lets a pipeline split "start the transfer" and "wait for it"
into separate functions with CPU work in between — the overlap DMA exists
for — while the checker still tracks the buffer through both calls. See
`examples/overlapped_capture.phase`.

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

### 4.4 What the type system guarantees

For every program `phase check` accepts, within a function body:

- **Domains.** No use, borrow, field read, or `destroy` of a `@DMA`/
  `@DEVICE`-owned entity by the host without a `sync`; every `move`/`sync`
  follows §2.4.
- **Borrows.** Many readers or one writer, never both. While any borrow
  of an entity is live, the entity is reachable only through its borrows
  (the owner cannot be used, moved, synced, or destroyed). A released
  handle is dead. Handles cannot be moved, synced, destroyed, or
  re-borrowed.
- **Aliasing.** One storage location appears at most once among a call's
  arguments, unless every occurrence is a read borrow. Entities cannot be
  copied with `let`, and names cannot be redeclared, so one location never
  has two independently tracked names.
- **Lifetimes.** No access after `destroy`. At every `return` (explicit or
  falling off the end): no local is still `@DMA`/`@DEVICE`-owned, no borrow
  is live, and every parameter is in the domain its signature promises —
  the one it arrived in, or the `@TO` of a declared change (§3.11).
  Parameters cannot be destroyed (the caller owns them).
- **Typestate.** A value's state is established only by a function's
  declared return state, or — for a newly constructed value — the
  machine's initial state; a type annotation is checked against that and
  never trusted on its own. Typestate values are linear: passing one to a
  state-requiring parameter, or returning it, consumes it.
- **Control flow.** At an `if` join every incoming path agrees on each
  pre-existing entity's domain, typestate, destroyed-ness, and borrows
  (paths that `return` do not reach the join; a value consumed on one path
  only is unusable after it). A `while` body leaves every pre-existing
  entity exactly as it found it, so the single-pass check is a loop
  invariant and holds for any iteration count.
- **MMIO.** Every `volatile_read`/`volatile_write` targets an `@MMIO`
  register reference and is lowered to a `volatile` access, so the C
  compiler cannot eliminate or reorder it.

**Trust boundary.** `extern fn` bodies are not analyzed (§3.9): PHASE
trusts that a kernel does only what its signature says — touches only the
buffers it is passed, does not retain them past the call, and really
performs the state transition its return type claims. A buggy or
malicious extern can violate any guarantee above. Likewise the analysis is
per-function and assumes the hardware honors the ownership model (e.g.
that a transfer is complete when `sync` returns).

---

<a name="part-5-runtime--simulated-hardware"></a>
## Part 5: Runtime & Simulated Hardware

### 5.1 Why simulated hardware

`phase build` targets host execution by default so the whole project is
testable and demoable without real RF/embedded hardware. Every "device" is a small C
runtime component with realistic *timing and ownership* behavior, not real
signal behavior.

### 5.2 Simulated components

- `runtime/phase_runtime.c` — `sim_dma_wait` / `sim_device_wait`, the two
  functions every `sync` lowers to. Everything shares one address space;
  what is simulated is the *timing*: capture/playback externs record a
  transfer delay, and the wait functions block until it has elapsed, so
  `sync` has observable sequencing behavior.
- `runtime/<program>_extern.c` — hand-written bodies for a program's
  `extern fn`s, e.g. a capture stand-in that fills a buffer with a
  synthetic signal (sine + deterministic noise) so the demo pipeline has
  something real to filter.
- `@MMIO` registers — generated as `volatile` globals; demo externs that
  play the hardware's role read and write the same globals, which is how
  `examples/mmio_poll.phase` observes a register changing under it.

### 5.3 Real hardware

The runtime interface (`sim_dma_wait`, `sim_device_wait`, and `volatile`
register globals) is written so a real backend (Linux DMA-API, an SDR
driver, a microcontroller HAL) can implement it without changing the
compiler. `runtime/hw/` is one such backend: an ARM Cortex-M3 (MPS2 AN385)
startup file, linker script, register map, and wait functions that poll a
real peripheral register, validated under QEMU (see M10 in §9).
`phase build` links the simulated runtime; the hardware build is the
separate `scripts/build_hw_demo.sh` step.

---

<a name="part-6-compiler-architecture"></a>
## Part 6: Compiler Architecture

### 6.1 Pipeline

```
Source -> Lexer -> Parser -> AST
       -> Analyzer (names, domains, ownership, typestate, lifetimes, merges)
       -> PIR Generator -> C Codegen -> cc -> Binary
```

### 6.2 Why C, not LLVM, for v1

Compiling to readable C keeps 100% of engineering time on the analyzer (the
actually novel part). LLVM backend is explicitly deferred (§10).

### 6.3 Crate layout (Rust workspace)

```
phase/
  crates/
    ast/            -- AST type definitions (shared)
    lexer/          -- tokens
    parser/         -- AST construction
    analysis/       -- name, domain, ownership, typestate, lifetime and
                       control-flow analyzers (one pass over each fn body)
    pir/            -- Phase IR data structures + builder
    codegen_c/      -- PIR -> C source
    driver/         -- the `phase` CLI
  runtime/
    phase_runtime.c / .h   -- sim_dma_wait / sim_device_wait (host)
    <program>_extern.c     -- hand-written bodies for a program's extern fns
    hw/                    -- ARM Cortex-M3 backend (§5.3)
  examples/
    *.phase                -- programs that pass analysis
    bug_gallery/*.phase    -- one rejected program per bug class (§8)
```

Name resolution and type checking happen inside `analysis` rather than a
separate `sema` crate: the language has no user-defined generics or
overloading, so a separate pass would only duplicate the symbol tables the
analyzer already builds.

### 6.4 CLI

```
phase [options] check <file>   -- run all analysis passes, report errors, no codegen
phase [options] build <file>   -- full pipeline, emits a native binary
phase [options] --dump-tokens <file>
phase [options] --dump-ast <file>
phase [options] --dump-pir <file>
phase --version | --help

options:
  --error-format=human|short   (default: human, see §6.5)
  --color=auto|always|never    (default: auto -- respects NO_COLOR, and
                                only colors a terminal)
```

`build` locates the runtime by walking up from the source file to the
nearest `runtime/` directory (override: `PHASE_RUNTIME_DIR`) and writes
into a `build/` directory beside it. Exit status is 0 on success and 1 on
any error.

### 6.5 Diagnostics

Every token carries a source span (start and end, line and column). The
parser attaches spans to every statement, function name, and parameter,
and the analyzer reports each violation with:

- a **primary location** — the statement that breaks the rule (an error
  inside a branch keeps the inner statement; a disagreement at a join
  points at the `if`/`while`; falling off the end of a function points at
  its closing `}`), narrowed to the entity's name where it appears there;
- **notes** at the locations that explain it — where the entity was
  declared, moved, synced, borrowed, released, destroyed, or consumed;
  where each branch left it; where a typestate value got its state; which
  call changed its domain.

`human` format renders each as a source snippet:

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

`short` format prints one `file:line:col: error|note: message` line per
error and note, for editors and scripts. Parse errors use the same
renderer.

### 6.6 Transition table representation

v1 keeps the domain-transition table (§2.4) as a hardcoded Rust `match` /
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

`--dump-pir` prints the lowered PIR for a program that passed analysis.
*Why* a program was rejected is shown by the diagnostics themselves
(§6.5), whose notes trace the entity's history up to the violation.

---

<a name="part-8-bug-gallery--what-the-compiler-must-catch"></a>
## Part 8: Bug Gallery — What the Compiler Must Catch

Each entry below is one deliberately-broken example program plus the exact
compile error PHASE must produce. This doubles as the acceptance-test suite
for the analyzer and as the security narrative for the portfolio write-up.

| # | Program pattern | Real-world analogue | Compiler must report |
|---|---|---|---|
| 1 | CPU reads a `@DMA` buffer with no prior `sync` | Reading a transfer buffer before DMA completion | "entity `X` is in domain @DMA but `f` expects @RAM" |
| 2 | `move` or `destroy` while a `borrow` is live | Freeing a buffer another part of the code still references | "entity `X` has a live borrow; cannot move/destroy" |
| 3 | Two concurrent `write` borrows | Data race on a shared buffer | "entity `X` already has an exclusive write borrow" |
| 4 | Access after `destroy` | Use-after-free | "entity `X` was destroyed; cannot use as argument to `f`" |
| 5 | Calling a function requiring `Packet<Decoded>` with a `Packet<Received>` | Parsing/validating a packet in the wrong order (TOCTOU) | "expected state Decoded, found Received" |
| 6 | Branch merges with mismatched domain/state on the two paths | Missing sync on an error path | "entity `X` disagrees on domain/state across incoming branches" |
| 7 | Reordering/eliding a `volatile_read`/`volatile_write` sequence | Compiler "optimizing away" a hardware side effect | (verified negatively — codegen test asserts both accesses appear in emitted C, in order) |
| 8 | Annotating a value with a state it was never given | Asserting verification instead of performing it | "`p` is declared as Packet<Validated> but `f` returns Packet<Received>" |
| 9 | Reusing a typestate value after passing it on | Double-processing / replay | "value `p` was already consumed (passed to `f`)" |
| 10 | `destroy` while `@DMA`/`@DEVICE`-owned | Freeing memory a device is still writing | "entity `X` is still owned by @DMA; sync(X) before destroying it" |
| 11 | Returning while a local is `@DMA`/`@DEVICE`-owned | DMA into an out-of-scope buffer | "entity `X` is still owned by @DMA when `f` returns" |
| 12 | Using the owner while it is borrowed | Bypassing exclusive access | "entity `X` is borrowed by `v`; it cannot be used directly" |
| 13 | Using a borrow after `release` | Dangling reference | "borrow `v` of `X` was already released" |
| 14 | Same buffer passed twice to one call | In-place call of a non-aliasing kernel | "entity `X` is passed to `f` more than once" |
| 15 | Borrowing a `@DMA`-owned buffer | CPU access before transfer completion | "entity `X` is owned by @DMA; sync(X) before borrowing it" |
| 16 | Redeclaring a name over a live entity | Losing track of an in-flight buffer | "`X` is already declared in fn `f`" |
| 17 | A helper declared `@DMA -> @RAM` that never syncs | A "wait" function that doesn't wait | "parameter `b` must be in @RAM when `f` returns (its signature says @DMA -> @RAM)" |
| 18 | A signature claiming an illegal domain change | A trusted kernel promising an impossible hand-off | "no legal transition from @DMA to @DEVICE in the signature of `f`" |
| 19 | Using a buffer between start-transfer and wait calls | Use-before-sync spread across functions | "entity `X` is in domain @DMA but `f` expects @RAM" (note: moved to @DMA by `start`) |

Every file in `examples/bug_gallery/` carries its expected error in an
`// Expected: "..."` header, and a test asserts the analyzer produces it.

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

**M11 — `volatile_read` in conditions/assignment RHS (real MMIO polling loops)**
Closes the gap directly: `PirExpr` gains a `VolatileRead` variant, so
`volatile_read(REG @MMIO)` can now appear inside an `if`/`while`
condition or an assignment right-hand side -- e.g.
`while volatile_read(READY @MMIO) == 0 { .. }`, a real hardware polling
loop expressed entirely in PHASE source instead of requiring the poll
itself to be hand-written in the runtime. Deliberately restricted to this
one builtin, not general `extern fn` calls in a condition -- see the
`PirExpr::VolatileRead` doc comment for why. Output:
`examples/mmio_poll.phase` builds and runs, polling a simulated-hardware
register across exactly 3 iterations before terminating, with the loop
condition itself 100% compiler-generated code; a companion driver test
recompiles the generated C at `-O2` and inspects the assembly to confirm
the register read genuinely survives optimization (two separate reads,
not one hoisted out of the loop) rather than trusting `volatile`
by convention alone.

Building this milestone surfaced and fixed two real latent bugs from
earlier milestones, both now covered by regression tests: (1) PIR's
`collect_mmio_registers` only scanned top-level instructions, never
recursing into `if`/`while` bodies -- a gap since M8 that nothing had
exercised until a register was referenced only from inside a loop
condition; (2) the M2/M6 analyzer's `check_expr` never recursed into
`Expr::Binary`, so `volatile_read(...) == 0` (a comparison, not a bare
call) never actually got its domain/arity validation run -- confirmed via
`docs/bug_gallery_transcript.md` regeneration that this fix doesn't
change any existing accept/reject outcome.

**v1.0 — Soundness hardening**
An audit of the M1–M11 analyzer found accepted programs that violated the
very rules the bug gallery documents (a typestate forged by annotation,
reuse of consumed values, `destroy` of a DMA-owned buffer, hardware-owned
buffers escaping a function, owner use during a write borrow, use after
`release`, aliased arguments, borrowing unsynced DMA, shadowing, and
unchecked names in conditions). v1.0 closes each one (§4.4 states the
resulting guarantees), adds bug-gallery entries #8–#16, and pins each
gallery file to its documented error in the test suite. It also makes
`phase build` independent of the working directory and adds CI.

**v1.1 — Diagnostics and domain changes in signatures**
Errors now carry source locations: spans on every token, statement,
function name, and parameter; analyzer errors with a primary location and
notes that trace the cause; rustc-style rendering with a `short` format for
tools (§6.5). And a parameter may declare `@FROM -> @TO` (§3.11), so
starting and finishing a transfer can live in separate functions with CPU
work in between, checked across both calls. Output:
`examples/overlapped_capture.phase` builds and runs with a window computed
while the capture is in flight; bug-gallery entries #17–#19.

**M12 — LLVM backend** *(not yet started)*
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
- No LLVM backend (documented future work, M12).
- `phase build` targets the host simulation; the real-hardware runtime
  (§5.3) is a separate build step, not a CLI target switch.
- No interprocedural analysis beyond signatures: each function is checked
  against the signatures of the functions it calls — including any
  declared domain changes (§3.11) — and `extern fn` bodies are trusted
  (§4.4).
- No GPU kernel compilation — accelerators are just another `@DEVICE`.
- No garbage collector; no automatic parallelism; no distributed/NUMA
  domains.
- No plugin/config-file-driven hardware description in v1 — the
  transition table is fixed and hardcoded (§6.6), matching v1 scope.
- Not attempting to be a general-purpose language: `if`/`while`/functions
  exist only to the extent the pipeline-safety demos need them.
