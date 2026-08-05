# PHASE: Physical Hardware Assurance & Safety Engine 

**A domain-specific compiler that mathematically proves memory safety for hardware pipelines (DMA, MMIO, and zero-copy devices).**

<p align="center">
  <img src="https://img.shields.io/badge/Status-M7_Complete-success" alt="Status">
  <img src="https://img.shields.io/badge/License-Apache_2.0-blue" alt="License">
  <img src="https://img.shields.io/badge/Language-Rust-orange" alt="Language">
</p>

## The Problem: Hardware Moves Fast, Software is Blind

In modern embedded and systems engineering, hardware components (ADCs, DACs, NICs) use DMA engines to stream massive amounts of data directly into memory, bypassing the CPU entirely. 

Standard software languages (like C or C++) are blind to this physical reality. If a programmer accidentally writes code that reads a DMA buffer before the hardware finishes writing to it, the compiler happily builds it. On real silicon, this creates devastating race conditions, corrupted math, and unpatchable zero-day vulnerabilities. 

## The Solution

PHASE is a constraint language that treats physical hardware boundaries as mathematical types. It acts as a strict **Design Rule Check (DRC)** for your memory pipelines.

You write your heavy DSP or routing math in standard C/C++, and you write your pipeline coordination rules in PHASE. The PHASE compiler statically proves that:
1. **Domain Safety:** The CPU never touches a buffer owned by the hardware (`@DMA`, `@DEVICE`) without explicit synchronization.
2. **Typestate Enforcement:** Data never skips a step in the signal/packet chain (e.g., parsing a packet before the firewall validation step).
3. **Volatile MMIO:** Hardware control registers (`@MMIO`) are never optimized away or reordered by the C compiler.

Once the safety proofs pass, PHASE translates your pipeline into **blisteringly fast, zero-copy, raw C code** with zero runtime overhead. All safety constraints are erased at compile-time.

---

## The Bug Gallery: Preventing Real-World CVEs

Every rule PHASE enforces maps directly to a real-world, recurring bug class. The `examples/bug_gallery/` directory contains deliberately broken code that PHASE catches at compile-time. 

*   **[CWE-362/367] The DMA Race Condition:** Fails compilation if you read a `@DMA` buffer before calling `sync()`. (Prevents vulnerabilities like CVE-2025-38095).
*   **[CWE-416] Use-After-Free:** Fails compilation if you move or destroy a buffer while a C math function is still borrowing it (e.g., Linux `dmam_free_coherent` races like CVE-2024-43856).
*   **[CWE-696] Typestate Skipping:** Fails compilation if you try to pass a `Packet<Received>` to a function that requires `Packet<Decoded>`. (Prevents logic bypasses like Apple's CVE-2014-1266 "goto fail").
*   **[CWE-758] Silent MMIO Optimization:** Guarantees `@MMIO` registers are lowered to `volatile` C globals, preventing the compiler from silently deleting critical hardware writes.

> **See the exact compiler output:** Read [`docs/bug_gallery_transcript.md`](docs/bug_gallery_transcript.md) or run `./scripts/capture_bug_gallery.sh` to see PHASE reject these bugs in real-time. See [`docs/security_mapping.md`](docs/security_mapping.md) for a deep dive into the real-world CVEs.

---

## Architecture & Repository Layout

PHASE is a fully functional compiler frontend written in Rust, leveraging a custom Phase Intermediate Representation (PIR) to preserve physical entity histories before lowering to C.

```text
crates/
  ast/         AST node definitions
  lexer/       Hand-written lexer (Source -> Tokens)
  parser/      Hand-written recursive-descent parser (Tokens -> AST)
  analysis/    Domain, ownership, typestate, branch-merge, and MMIO analyzers
  pir/         Phase Intermediate Representation (PIR) builder
  codegen_c/   Lowers PIR to raw C source code
  driver/      CLI binary (`phase`)
runtime/
  phase_runtime.c          Simulated hardware engines (generic DMA/Device waits)
  *_extern.c               C implementations of external math kernels
examples/
  sdr_session.phase        Full demo: MMIO setup + DMA capture + Typestate + C Math
  bug_gallery/             One deliberately broken file per bug class
demos/
  m6_volatile_proof/       Before/after proof showing C compiler dead-store elimination

```

## Getting Started: Building & Testing

**Requirements:** `cargo` (Rust) and a standard C compiler (`cc`).

The test suite includes 135+ tests across the workspace, including end-to-end integration tests that invoke the C compiler and inspect the generated assembly to prove optimization safety.

```bash
# Run the full test suite (including the bug-gallery regression sweep)
cargo test --workspace

# Check a file for safety without generating C code
cargo run --bin phase -- check examples/sdr_session.phase

# Build an executable (translates to C, links runtime, and compiles)
# Note: MUST be run from the project root directory
cargo run --bin phase -- build examples/sdr_session.phase
cargo run --bin phase -- build examples/mmio_registers.phase

# Run the generated binaries
./build/sdr_session

# Run the MMIO Volatile Proof Demo
./demos/m6_volatile_proof/run_demo.sh

```

## What's Next

The roadmap's core milestones (M1-M7) are complete. From here, natural next directions (not yet scoped as formal milestones) include:

* Lowering `if`/`while` to real C branches (closing M5's documented scope cut).
* Supporting real hardware targets instead of the simulated C runtime (spec §5.3).
* An LLVM backend (spec §6.2's documented non-goal for v0.1).

For the complete language specification, theoretical domain model, and future roadmap, see [`phase_specification.md`](phase_specification.md).

