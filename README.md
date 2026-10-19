<p align="center">
  <img src="docs/assets/phase-logo.png" alt="PHASE logo" width="200">
</p>

# PHASE: Physical Hardware Assurance & Safety Engine

**A compiler that statically checks memory safety for hardware pipelines — DMA, MMIO, and zero-copy devices — before the code ever runs.**

<p align="center">
  <img src="https://img.shields.io/badge/version-3.0.0-success" alt="Version 3.0.0">
  <img src="https://img.shields.io/badge/License-Apache_2.0-blue" alt="License">
  <img src="https://img.shields.io/badge/Language-Rust-orange" alt="Language">
</p>

**New here? [PHASE in 15 minutes](docs/tutorial/README.md)** — from a
first buffer to circular DMA, packet protocols, hardware registers and
interrupts, with every program ready to compile.

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
   (a read borrow, a `read` parameter) is never handed to code that writes
   or doesn't say it only reads;
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
   as it found it — with `swap` and `rotate` to trade buffers for
   ping-pong and triple-buffered DMA, and `split`/`join` to give the
   halves of a circular DMA buffer their own ownership.
6. **Register safety** — a peripheral's register map is declared once
   (`device Uart0 @ 0x40004000 { .. }`), and every access is checked
   against it: no misspelled register or field, no write to a read-only
   register or read of a write-only one, no value — literal or computed
   constant — wider than its field, no index past the end of a register
   array, and no field write that clears a write-1-to-clear flag it
   didn't mean to. A DMA engine gets a buffer's address only through a
   register declared `address`, and writing it *is* the hand-off:
   `volatile_write(Dma0.CDEST[0], address_of(lo))` gives `lo` to the
   engine, checked like `move`. Register accesses are `volatile`, so the
   C compiler never reorders or eliminates them.
7. **Interrupt safety** — an `interrupt fn` handler can't lose a register
   write to the code it interrupts: a field write (read-modify-write) to a
   register a handler also writes must be inside `critical { }`. Nothing a
   handler runs can `wait` for an event, which would never come, and
   nothing waits for an event no code signals. A handler can hand `main`
   a buffer: `wait half_done for lo;` takes back the half of a circular
   DMA buffer the engine just finished — and only that half.

The precise rules, and the trust boundary they hold within, are in
[`phase_specification.md`](phase_specification.md) §4.4.

## Evidence, not just claims

A static checker is only as good as its rules, so PHASE also checks
itself at run time:

```
$ phase build --checked --stub-externs program.phase   # instrumented build
$ cargo run --release -p phase_fuzz -- --count 1000     # differential fuzzing
1000 programs generated, 437 accepted by `phase check` (0 more only with warnings, not run), 0 soundness violations, 0 pipeline failures
```

- **`phase build --checked`** emits C that verifies every ownership rule
  on the path the program actually takes — a shadow state per buffer
  (size, domain, borrows, split) and per typestate value (state,
  consumed), checked at every move, sync, borrow, call, swap, rotate,
  split, join and scope exit — and every register access against its
  declared map, every `wait` against where it may run, and every field
  write against the interrupt handlers (it runs them between the read and
  the write, and catches the write they'd lose). It exits with status 70
  on any violation, and never reasons about branches or loops, so it is
  independent of the analyzer.
- **`phase-fuzz`** generates thousands of random programs — three buffer
  types with fixed and generic lengths, typestate values, borrows,
  rotations, nested splits, and a device with interrupt handlers, events,
  waits, buffer hand-offs and critical sections — builds every one the
  analyzer accepts in checked mode, and runs it down different paths. An
  accepted program that trips a runtime check is an analyzer soundness
  bug.

Its first runs found four real bugs — two analyzer holes (a buffer left
DMA-owned at the end of an `if` block, and `swap` across blocks leaving a
dangling name), a codegen bug, and a false alarm in the checker itself —
all fixed in 1.3 and replayed on every test run; in 1.4 it found two
more false alarms in the checker's new `split` support before release.
After the last fix, 6,000 freshly generated programs — with every buffer
type, typestate values, rotations and splits — were fuzzed (2,619
accepted and run) with no violations, and CI fuzzes a fresh batch on
every push.

And the fuzzer demonstrably finds bugs when they exist: with one of the
fixed analyzer holes deliberately put back, it reported 8 violations in
the same first 1,000 programs that the real analyzer passes cleanly — and
every rule family added since (buffer types, typestate consumption,
forged states, split and join, and in 2.3 registers and interrupts) was
checked the same way, one analyzer check removed at a time, and caught. Details, including the cases that
were *not* caught until the generator was fixed, in
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

Hardware registers get the same treatment. Declare the register map once
and the compiler holds every access to it:

```phase
device Uart0 @ 0x40004000 {
    DATA:  read write u32 @ 0x00;
    STATE: read u32 @ 0x04 { TXFULL: bit 0, RXFULL: bit 1 }
    CTRL:  read write u32 @ 0x08 = 0 { TX_EN: bit 0, RX_EN: bit 1 }
}

fn send_byte(c: u32) {
    while volatile_read(Uart0.STATE.TXFULL) == 1 {
    }
    volatile_write(Uart0.DATA, c);
}
```

```
$ phase check examples/bug_gallery/field_value_too_wide.phase
error: 16 does not fit in 'Timer.CTRL.PRESCALE' (4 bits, at most 15)
  --> examples/bug_gallery/field_value_too_wide.phase:14:5
   |
14 |     volatile_write(Timer.CTRL.PRESCALE, 16);
   |     ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
note: 'Timer.CTRL.PRESCALE' is declared here
  --> examples/bug_gallery/field_value_too_wide.phase:8:39
   |
 8 |     CTRL: read write u32 @ 0x04 = 0 { PRESCALE: bits 0..=3, ENABLE: bit 4 }
   |                                       --------
```

A chip's identical peripherals share one `device type`
(`device Uart1: CmsdkUart @ 0x40005000;`), and a board's devices live in
a file programs `import` — like
[`examples/devices/mps2_an385.phase`](examples/devices/mps2_an385.phase),
whose addresses were each checked on QEMU's model of the board. Two
devices at one address are an error.

`phase build --target=mps2-an385 --run examples/uart_hello.phase` compiles
a UART driver written entirely in PHASE for an ARM Cortex-M3 board and
runs it on QEMU's model of it — `Hello from PHASE` comes out of the real
UART0 data register. `--target=mps2-an505` builds for a Cortex-M33 board
with real DMA controllers: `examples/dma_stream.phase` streams ten blocks
through its PL081, each half of the buffer handed to `main` by the
controller's interrupt.

PHASE also runs on real boards. `--target=nucleo-f103rb` and
`--target=nucleo-f401re` build for ST's NUCLEO-F103RB and NUCLEO-F401RE;
`--run` flashes the board (with `probe-rs`, OpenOCD or `st-flash`) and
shows its console from the ST-LINK's USB serial port.
`phase build --target=nucleo-f103rb --run examples/nucleo_f103rb_blink.phase`
blinks LD2 from a timer interrupt and prints `tick 1` .. `tick 16`; that
program has run on a NUCLEO-F103RB. The F401RE target is checked against
ST's own CMSIS header and its blink program runs on QEMU's STM32F405
model; its DMA and ADC programs have yet to run on the chip
([`docs/nucleo.md`](docs/nucleo.md)). `phase emit-c` writes the checked C
for use inside an existing STM32 project
([`docs/embedding.md`](docs/embedding.md)), and `phase import-svd` makes
a board file from a chip's CMSIS-SVD description
([`docs/svd_import.md`](docs/svd_import.md)).

Interrupts are part of the language too — a handler bound to its IRQ,
an event it signals, and `main` sleeping until it does:

```phase
event tick;

interrupt fn on_timer0() @ irq 8 {
    volatile_write(Timer0.INT.PENDING, 1);
    signal tick;
}
```

`examples/timer_ticks.phase` takes five real timer interrupts on QEMU's
board model, and a field write a handler could split in the middle is a
compile error:

```
$ phase check examples/bug_gallery/interrupted_field_write.phase
error: writing field 'Timer0.CTRL.ENABLE' reads 'Timer0.CTRL', changes the field and writes it back -- and interrupt handler 'on_timer0' (IRQ 8) also writes 'Timer0.CTRL': if it runs in between, its write is lost; do this inside `critical { }`
  --> examples/bug_gallery/interrupted_field_write.phase:25:5
   |
25 |     volatile_write(Timer0.CTRL.ENABLE, 1);
   |     ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
note: 'on_timer0' writes 'Timer0.CTRL' here
  --> examples/bug_gallery/interrupted_field_write.phase:18:5
   |
18 |     volatile_write(Timer0.CTRL.IRQ_EN, 0);   // one shot
   |     --------------------------------------
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
- **[CWE-362] Writing through read-only views** — [`write_through_read_borrow.phase`](examples/bug_gallery/write_through_read_borrow.phase), [`read_param_handed_to_writer.phase`](examples/bug_gallery/read_param_handed_to_writer.phase), [`read_only_to_unannotated_param.phase`](examples/bug_gallery/read_only_to_unannotated_param.phase): a read borrow or a `read` parameter handed to a kernel that writes, or to one that never said it only reads.
- **[CWE-362/367] Broken double buffering** — [`refill_while_processing.phase`](examples/bug_gallery/refill_while_processing.phase), [`ping_pong_without_swap.phase`](examples/bug_gallery/ping_pong_without_swap.phase), [`swap_while_borrowed.phase`](examples/bug_gallery/swap_while_borrowed.phase): refilling the buffer the CPU is processing (a torn frame), forgetting to trade buffers, or swapping one out from under a live view.
- **[CWE-362/367] Ring buffers and circular DMA** — [`process_half_in_flight.phase`](examples/bug_gallery/process_half_in_flight.phase), [`use_ring_while_split.phase`](examples/bug_gallery/use_ring_while_split.phase), [`join_with_half_in_flight.phase`](examples/bug_gallery/join_with_half_in_flight.phase), [`rotate_wrong_direction.phase`](examples/bug_gallery/rotate_wrong_direction.phase), [`wrong_half_event.phase`](examples/bug_gallery/wrong_half_event.phase): processing the half the engine is still filling (the classic off-by-one-half in circular ADC drivers), using the whole buffer while half of it is in flight, treating it as whole again with one half still streaming, rotating a triple-buffer ring the wrong way, or taking the wrong half on a half-transfer interrupt.
- **[CWE-684/908/197] Hardware register misuse** — [`write_read_only_register.phase`](examples/bug_gallery/write_read_only_register.phase), [`read_write_only_register.phase`](examples/bug_gallery/read_write_only_register.phase), [`field_write_on_write_only_register.phase`](examples/bug_gallery/field_write_on_write_only_register.phase), [`field_value_too_wide.phase`](examples/bug_gallery/field_value_too_wide.phase), [`overlapping_registers.phase`](examples/bug_gallery/overlapping_registers.phase), [`overlapping_devices.phase`](examples/bug_gallery/overlapping_devices.phase), [`constant_too_wide.phase`](examples/bug_gallery/constant_too_wide.phase), [`field_write_clears_flag.phase`](examples/bug_gallery/field_write_clears_flag.phase), [`register_index_out_of_range.phase`](examples/bug_gallery/register_index_out_of_range.phase): clearing a flag through a read-only status register, trusting what a write-only register reads back as, `CMD |= START` on a write-only register, a prescaler value — literal or named — spilling into the enable bit, copy-pasted register maps with two registers or two whole devices at one address, `INTSTATUS |= TX` clearing a pending RX interrupt, and DMA stream 8 on an 8-stream controller.
- **[CWE-362/833] Interrupt races and deadlocks** — [`interrupted_field_write.phase`](examples/bug_gallery/interrupted_field_write.phase), [`wait_in_handler.phase`](examples/bug_gallery/wait_in_handler.phase), [`wait_never_signalled.phase`](examples/bug_gallery/wait_never_signalled.phase): a one-shot timer that keeps firing because `main`'s read-modify-write of its control register overwrote the handler's, a handler that waits for an interrupt that can't run until it returns, and a `main` that waits for an event a refactor stopped signalling.
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
  → analysis      names, domains, ownership/borrows, lifetimes, typestate, branch/loop merges, device registers, interrupts
  → pir           AST -> a flat per-function intermediate representation
  → codegen_c     PIR -> readable C
  → cc            real C compiler -> a real binary (or arm-none-eabi-gcc -> a Cortex-M3 .elf)
```

```text
crates/
  ast/         AST node definitions
  lexer/       hand-written lexer (source -> tokens)
  parser/      hand-written recursive-descent parser (tokens -> AST)
  analysis/    the safety analyzer (all checks above)
  pir/         Phase Intermediate Representation builder
  codegen_c/   lowers PIR to raw C source
  driver/      the `phase` CLI, including the language server (`phase lsp`)
  fuzz/        phase-fuzz: differential fuzzer (+ regressions/, every past finding)
  svd/         `phase import-svd`: CMSIS-SVD to board files

runtime/
  phase_runtime.h/.c   generic sim_dma_wait/sim_device_wait, always linked
  phase_check.h/.c     runtime ownership checker, linked by `phase build --checked`
  *_extern.c           one per demo program, implementing that program's extern fns
  hw/                  bare-metal ARM backend for --target=mps2-an385 / mps2-an505 /
                       nucleo-f401re / nucleo-f103rb (startup, linker script, board
                       header, real sim_dma_wait/sim_device_wait, memset/memcpy)

editors/
  vscode/              TextMate grammar + an extension that starts `phase lsp`
  vim/                 syntax and filetype detection
  README.md            setup for VS Code, Neovim, Vim, Helix and Emacs

examples/
  *.phase              programs that pass analysis (sdr_session.phase is the fullest)
  devices/             board support: device types and devices to `import`
  embedding/           PHASE as one layer inside a vendor (STM32) project
  bug_gallery/         one deliberately-broken file per bug class

demos/
  m6_volatile_proof/   before/after proof that a C compiler really does delete non-volatile writes

docs/
  tutorial/                   PHASE in 15 minutes: the programs, and the page generated from them
  bug_gallery_transcript.md   generated: real compiler output for every example
  security_mapping.md         the CWE/CVE write-up
  nucleo.md                   running PHASE on the NUCLEO-F103RB / F401RE boards
  embedding.md, boundary.md   PHASE inside another project, and what the C side must promise
  svd_import.md               board files from CMSIS-SVD
  real_boards.md              what real boards still need

scripts/
  capture_bug_gallery.sh      regenerates the transcript from real compiler output
  render_tutorial.py          regenerates docs/tutorial/README.md by running its programs
  build_hw_demo.sh            runs the hardware examples on QEMU (--target=<board> --run)
  check_against_cmsis.sh      checks the NUCLEO-F401RE board file against ST's CMSIS header
```

## Building and testing

**Requirements:** a recent stable Rust toolchain and a C compiler (`cc`).
`--target=mps2-an385` additionally needs `arm-none-eabi-gcc` and
`qemu-system-arm` (`sudo apt-get install --no-install-recommends
gcc-arm-none-eabi qemu-system-arm`) — everything else works without them,
and the one test that needs them
(`hardware_examples_run_on_the_mps2_boards_under_qemu`) skips itself
with a clear message if they're missing. (CI sets
`PHASE_REQUIRE_HW_TOOLS=1`, which turns that skip into a failure.) The
NUCLEO targets build with the same `arm-none-eabi-gcc`; flashing one with
`--run` also needs `probe-rs`, OpenOCD or `st-flash`, and access to the
board's serial port ([`docs/nucleo.md`](docs/nucleo.md)).

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
a fuzzing pass that replays every past finding and tries a fresh batch;
and a full language-server session over pipes.
CI ([`.github/workflows/ci.yml`](.github/workflows/ci.yml)) also runs
`cargo fmt --check`, `clippy -D warnings`, the volatile proof demo, 1,500
freshly fuzzed programs, the editor grammar against every example, and a
staleness check on the transcript.

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

# Cross-compile for the ARM MPS2 AN385 board (Cortex-M3) and run it on QEMU
phase build --target=mps2-an385 --run examples/uart_hello.phase

# Build for a NUCLEO board, flash it and show its console (needs the board)
phase build --target=nucleo-f103rb --run examples/nucleo_f103rb_blink.phase

# The checked C for another project's build; a board file from an SVD
phase emit-c --out-dir=generated examples/nucleo_adc_dma.phase
phase import-svd --only=GPIOA,USART2 STM32F401.svd

# The language server your editor talks to (see editors/README.md)
phase lsp

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

`examples/triple_buffer.phase`: triple buffering — two captures always in
flight while the CPU processes a third, with `rotate a, b, c;` advancing
the ring. Eight frames, processed in order across the three buffers.

`examples/circular_dma.phase`: a DMA engine in circular mode over one
buffer with half-transfer interrupts, the way most microcontroller ADC
drivers stream. `split ring into lo, hi;` gives each half its own
ownership, so the checker knows which half the CPU may touch, and the
interrupt handler hands each half to `main` as the engine finishes it
(`wait half_done for lo;`); after `join lo, hi into ring;` the whole
buffer is the CPU's again. A host build runs the real handler against a
simulated DMA controller.

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

`examples/uart_hello.phase`: a UART driver written entirely in PHASE
against the board's declared register map — baud divisor, transmitter
enable, and a wait on the TX-full flag before each byte. Built with
`--target=mps2-an385 --run`, it prints `Hello from PHASE` on QEMU's model
of the MPS2 AN385 board; a host build runs the same code against
simulated registers.

`examples/timer_ticks.phase`: a timer interrupt handler that acknowledges
each interrupt and signals an event, and a `main` that sleeps in
`wait tick` and prints a dot per tick — five real interrupts on QEMU's
board model.

`examples/dma_stream.phase`: real DMA, programmed entirely in PHASE. Built
with `--target=mps2-an505 --run`, the AN505's PL081 controller copies a
fresh block into each half of a buffer in turn — the program hands it
both buffers by writing their addresses (`address_of`) — its
transfer-complete interrupt hands them back
(`wait lo_done for src_lo, lo`), and `main` checks every word before
starting the next copy: ten blocks, each a real (emulated) transfer. A
host build runs the same program against a simulated PL081.

`examples/hw_uart_echo.phase`: a buffer handed to the UART as a device,
built for the same board — its `sync()` is a real register poll on real
(emulated) hardware, not the host's simulated address space.

`examples/nucleo_f103rb_blink.phase` and `examples/nucleo_blink.phase`:
a TIM2 interrupt blinks the board's LED (LD2) and counts the blinks on
its console, for the NUCLEO-F103RB and NUCLEO-F401RE. The F103RB one has
run on the board. `examples/nucleo_dma_copy.phase` (DMA2 memory to memory)
and `examples/nucleo_adc_dma.phase` (ADC1 into a buffer through DMA2,
taken back by `sync` on the stream's own enable bit) are the F401RE's DMA
programs, and have yet to run on the chip. `examples/dma_sync.phase`
shows the same `sync` on QEMU's PL081.

`examples/mmio_poll.phase`: a real
`while volatile_read(READY @MMIO) == 0 { .. }` polling loop, with the
poll itself expressed in PHASE source rather than hand-written in the
runtime — it polls simulated hardware state across exactly 3 iterations
before terminating.

## Honest scope

PHASE is a focused, research-grade compiler: its guarantees are
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
  constants, comparisons, arithmetic and bit operations, and a direct
  `volatile_read` of a register) — not the full `Expr` grammar. There are
  no arrays to index other than registers, and a register array's index
  must be a constant. A condition using any other call is a
  clear `UnsupportedExpr` error, not a silent guess: general `extern fn`
  calls inside a condition would be re-invoked every loop iteration with
  much less clear ordering/purity guarantees than a register read.
- **Every program with an `extern fn` needs a matching hand-written
  runtime file**, `runtime/<stem>_extern.c` (e.g. `sdr_session.phase` →
  `runtime/sdr_session_extern.c`). The compiler only ever generates a
  *prototype* for `extern fn`, never a body. `phase build` fails early
  with a clear message naming the missing file — or, with
  `--stub-externs`, generates placeholders to try the program without one.
- **The runtime checker and the fuzzer cover buffers, typestate,
  registers and interrupts.** Plain struct entities are checked statically
  but not at run time, and the fuzzer uses three buffer types, one
  typestate machine and one device. Their silence is strong evidence for
  the rules they exercise, not a proof of everything.
- **Four hardware targets, one proven on silicon.**
  `--target=mps2-an385` (ARM Cortex-M3) and `--target=mps2-an505`
  (Cortex-M33 with PL081 DMA) are validated on QEMU's models of the
  boards. `--target=nucleo-f103rb` (ST NUCLEO-F103RB, Cortex-M3) is the
  one that has run on a real board: its blink program (TIM2 interrupts,
  GPIO and the ST-LINK console) was flashed and run on the chip, and it
  has no DMA support yet. `--target=nucleo-f401re` (NUCLEO-F401RE) has a
  board file checked against ST's header and its blink program run on
  QEMU's STM32F405 model, but its DMA and ADC programs have yet to run on
  the chip. Another board needs its own startup code, linker script and
  wait functions under `runtime/`. `sync` waits on the device itself only for a buffer
  handed over through a register that says when the device is done
  (`address until CR.EN == 0`, since 3.0); one handed over by a C extern
  still waits on a stand-in, and `wait E for b` relies on the program's
  word that `E` means `b` is done. What else real boards need is in
  [`docs/real_boards.md`](docs/real_boards.md).
- **Interrupts are modeled simply.** One core, one priority level
  (handlers don't preempt each other), and events are flags, not
  counters: two signals before a `wait` count as one. A handler hands
  `main` a buffer through an event (`wait E for b`), not directly:
  handlers have no parameters and there are no globals. That an event
  gives back the right buffer is checked within a function, by name — an
  event that hands back `lo` in one place and `hi` in another is caught,
  but consistently swapped events are not. A host build has no interrupt
  sources: each `wait` runs every handler once (after an optional C hook
  that plays the hardware).
- **Imports share one namespace.** `import` brings in every item of a
  file; there are no modules, qualified names or private items, so two
  files can't both declare a `send`.
- **The language server has no completion or rename.** It reports
  diagnostics and symbols, shows a local's owner and history on hover,
  and jumps to declarations.
- **No LLVM backend.** Compiling to readable C was a deliberate choice so
  the engineering effort goes into the analysis passes, not backend
  plumbing.
- **Loops are checked by invariant, not by iteration.** A `while` body
  must leave every pre-existing entity exactly as it found it. That makes
  the single-pass check sound for any iteration count; a body that rotates
  buffers between roles restores the invariant with `swap` or `rotate`.
  Patterns that need more state than that (say, a ring of N buffers
  advanced by a run-time index) aren't expressible yet.
- **`split` cuts into equal, fixed slices.** A buffer with a run-time
  length (`buffer<T, _>`) or a parameter can't be split, and parts can't
  be sized unequally.
- **Unannotated parameters are assumed to write.** A buffer parameter
  without `read`/`write` may be given data the caller may write, never
  read-only data (an error since 2.0). Annotate your externs to make the
  aliasing and borrow rules exact.

Full rationale for all of these is in
[`phase_specification.md`](phase_specification.md) §4.4 and §10.

## What's next

2.4 runs real (emulated) DMA on a second board, the Cortex-M33 MPS2
AN505, and adds what programming it took: constants and bit operations,
register arrays, flags with read and write side effects, and DMA
addresses written from PHASE as checked hand-offs. 2.3 let interrupts
hand buffers to `main`, fuzzed register and interrupt code, and added
hover and go-to-definition; 2.2 brought interrupts, 2.1 imports and
device types, 2.0 register maps, a Cortex-M3 target and the language
server (see [`CHANGELOG.md`](CHANGELOG.md)).

3.0 is the real-board release ([`CHANGELOG.md`](CHANGELOG.md)): a
NUCLEO-F103RB target whose blink program has run on the board, a
NUCLEO-F401RE target with a board file checked against ST's own header,
`sync` that waits on the DMA stream itself, an ADC-to-memory DMA driver
written in PHASE down to the hand-off, board files imported from
CMSIS-SVD, and `phase emit-c` for using PHASE as one checked layer inside
an existing STM32 project ([`docs/embedding.md`](docs/embedding.md),
[`docs/boundary.md`](docs/boundary.md)). What ran on silicon is the
F103RB blink program; the F401RE's three programs are written and
QEMU-tested, and still to run on a board
([`docs/nucleo.md`](docs/nucleo.md)). After that:

- **DMA on silicon**: a DMA1 model for the F103RB, or the F401RE
  programs on a real F401RE.
- **The VS Code extension**, packaged as a `.vsix` and tried installed.
- **Per-engine domains, static buffers, memory regions and caches** — the
  rest of [`docs/real_boards.md`](docs/real_boards.md).
- **M12 — An LLVM backend**, removing the dependency on an external `cc`
  ([`phase_specification.md`](phase_specification.md) §9).

For the complete language specification, domain model, type system, and
full milestone roadmap, see
[`phase_specification.md`](phase_specification.md).

## License

Apache-2.0 — see [`LICENSE`](LICENSE).
