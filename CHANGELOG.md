# Changelog

All notable changes to PHASE are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project
uses [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- **`--target=mps2-an505`**: the MPS2 AN505 board (Cortex-M33 with
  TrustZone, run in the Secure state), on `qemu-system-arm -M mps2-an505`.
  `examples/devices/mps2_an505.phase` declares its UARTs, NVIC and four
  PL081 DMA controllers.
- `examples/dma_stream.phase`: ten blocks through a real (emulated) PL081
  DMA controller, each half of a split buffer handed to `main` by the
  controller's transfer-complete interrupt; part of the QEMU hardware
  test. The same program runs on the host against a simulated PL081.
- The vector table covers 128 IRQs.

### Changed

- A checked build lets the hardware move on too (`phase_sim_hardware()`)
  when it runs the interrupt handlers in the middle of a field write, as
  it does when `wait` sleeps.

### Fixed

- The CLI tests of checked builds ran the unchecked binary; they now run
  the checked one.

## [2.3.0] — 2026-10-16

Interrupts hand over buffers: `wait half_done for lo;` lets the interrupt
that signals an event give `main` the buffer the hardware just finished —
checked like `sync`, and an event always gives back the same buffer
within a function — so circular DMA runs off its interrupt instead of a
blocking call. Register and interrupt code is now fuzzed, and the
language server explains who owns a buffer on hover.

Every program 2.2 accepted is still accepted, unless it waits for an
event nothing signals — which would hang.

Each of the analyzer's ten register and interrupt checks, removed in turn,
was caught by the fuzzer within 1,500 programs; with the real analyzer,
5,000 freshly fuzzed programs (1,925 accepted and run in checked mode,
including 1,000 at `--chaos 0.3`) produced no soundness violations.

### Added

- **`wait E for b;`** (spec §3.16): waits for `E`, then hands `b` back to
  the CPU. `b` must be `@DMA`/`@DEVICE`-owned, as for `sync`; within a
  function, every `wait E for ..` names the same buffer (bug gallery
  #43).
- **A `wait` on an event no reachable code signals is an error** (#44).
- **Host builds can play the hardware**: before running the handlers, a
  `wait` calls `phase_sim_hardware()` if the program's C side defines it.
- `examples/circular_dma.phase` now declares a DMA controller and its
  interrupt handler, and runs it against a simulated engine.
- **Checked builds verify registers and interrupts at run time** (spec
  §6.7): register access modes, written values (including computed
  ones), `wait` in handlers or `critical`, waits no handler satisfies,
  `return` inside `critical`, and an event handing back two buffers. A
  field write outside `critical` runs the handlers between its read and
  its write, and reports a handler write to the same register as the
  lost write it would be.
- **The fuzzer generates register and interrupt code**: a device with
  four registers of different modes and widths, interrupt handlers that
  write registers and signal events, and register reads and writes,
  waits, buffer hand-offs, signals and critical sections in the code they
  interrupt. Each of the analyzer's register and interrupt checks was
  removed in turn to confirm the fuzzer catches it (spec §6.7).
- **Language server: hover and go-to-definition** (spec §6.8). Hovering a
  local shows its type, owner, borrows and split state at that line and
  the statements that changed it; hovering a function, device, register,
  field or event shows its declaration (an event: who signals and waits
  for it). Definitions are found across imports.
- Tutorial: interrupts that hand over buffers. Grammar and Vim syntax for
  `wait E for b`.

### Changed

- Internal crate APIs: `Stmt::Wait` and `PirInst::Wait` carry the buffer
  handed over; `RegRef` carries the register's access; `phase_analysis::trace`.

## [2.2.0] — 2026-10-16

Interrupts. A handler is an `interrupt fn` bound to its IRQ; it tells the
rest of the program something happened with an `event`, which `main`
waits for; `critical { }` keeps interrupts out of a block. The analyzer
follows the call graph from every handler, so a register write a handler
could lose and a `wait` that could never return are compile errors.

Every program 2.1 accepted is still accepted: the new words are keywords
only where they start an item or a statement.

After the changes, 4,000 freshly fuzzed programs (1,816 accepted and run
in checked mode) produced no soundness violations. The fuzzer does not
generate interrupts; they are covered by the analyzer, codegen and
end-to-end tests, including real timer interrupts on QEMU.

### Added

- **`interrupt fn NAME() @ irq N`, `event E;`, `signal E;`, `wait E;`,
  `critical { .. }`** (spec §3.16). Handlers take nothing, return
  nothing, are never called directly, and each IRQ (0-239) has one.
- **Checks** (whole program, through calls): a field write in thread
  code to a register an interrupt handler writes must be inside
  `critical` (bug gallery #41); `wait` in handler code (#42) or inside
  `critical` is an error, and so is `return` inside `critical`.
- **Code generation**: events are `volatile` flags; `critical` saves
  PRIMASK and disables interrupts; `wait` tests and clears its event with
  interrupts off and sleeps with `wfi`, so no signal is lost; handlers
  become the vector table's `IRQn_Handler`. A host build runs every
  handler once per `wait`, and stops a `wait` nothing signals with a
  message.
- `runtime/hw/startup.c`: the vector table covers the board's 32 IRQs as
  weak aliases.
- Board file: the NVIC, and the IRQ numbers of Uart0 transmit (1),
  Timer0 (8) and Timer1 (9), each checked on QEMU's model by taking it.
- `examples/timer_ticks.phase`: five real timer interrupts on QEMU's board
  model, part of the hardware test.
- Tutorial step 9, interrupts; grammar and Vim syntax for the new words.

### Changed

- Internal crate APIs: `FnDecl::irq`, `Item::Event`, `Stmt::Signal`,
  `Stmt::Wait`, `Stmt::Critical`; `PirInst::Signal`/`Wait`/`Critical`,
  `PirFn::irq`, `PirProgram::events`.

## [2.1.0] — 2026-10-13

Programs can span several files, and a chip's identical peripherals share
one register layout — so a board's devices are declared once, in a file
every program imports, and checked against each other. Plus a tutorial.

Every program 2.0 accepted is still accepted, unless two of its devices
overlap or a device's base address misaligns its registers (each a real
bug), or it uses `import` as the first word of an item.

After the changes, 4,000 freshly fuzzed programs (1,740 accepted and run
in checked mode) produced no soundness violations.

### Added

- **`import "path.phase";`** (spec §3.15): every item of that file becomes
  part of the program; paths are relative to the importing file, each
  file is loaded once (cycles are harmless), and there is one namespace.
  `main` must be in the file being compiled. Every source location now
  records its file: errors and notes are shown in the file they point
  into, in both output formats, and a missing import is reported at the
  `import` that names it. `phase check` counts the files.
- **`phase lsp` and imports**: imported files are read from open
  documents first, then from disk; a problem inside one is shown on the
  `import` line that brought it in, linked to the real location.
- **Device types** (spec §3.7): `device type CmsdkUart { .. }` declares a
  register layout once; `device Uart1: CmsdkUart @ 0x40005000;` is a
  device with it. In C, one struct per type.
- **Devices may not overlap** in the address space, and a device's base
  address must keep its registers aligned (bug gallery #40).
- `examples/devices/mps2_an385.phase`: board support for the MPS2 AN385 —
  CMSDK UART and timer types, all five UARTs and both timers, each
  address checked on QEMU's model of the board. `examples/uart_hello.phase`
  imports it.
- Editor grammar and Vim syntax for `import`, `device type` and device
  instances; the Neovim/LazyVim setup in `editors/README.md`, as tested.
- **[PHASE in 15 minutes](docs/tutorial/README.md)**: a tutorial from a
  first buffer to DMA across calls, borrows, ping-pong streaming,
  circular DMA, typestate, hardware registers and board files. Its 22 programs are in
  `docs/tutorial/`; the page is generated from them by
  `scripts/render_tutorial.py`, which runs every command it shows, and CI
  fails if it no longer matches the compiler.
- `--stub-externs` builds print each extern call, its arguments, and the
  result the stub returns.

### Changed

- `editors/README.md` marks the VS Code extension as not yet tested in
  VS Code.
- Internal crate APIs: `Span` has a `file`; `Item::Import` and
  `Item::DeviceType`; `DeviceDecl::of_type`; `phase_parser::load`;
  `PirDevice::of_type` and `PirProgram::device_types`; the diagnostics
  renderer takes a list of source files.

### Fixed

- Generated C no longer warns about a `let` whose value is never used, or
  (with GCC) about a buffer only ever filled by hardware.
- Bug gallery #1 had a second, unrelated error (a missing `sync` at the
  end of `main`).
- Stray `[stub]` output from one CLI test leaked into `cargo test`'s
  output.

## [2.0.0] — 2026-10-13

The hardware path becomes a compiler feature: a peripheral's register map
is declared in PHASE and every access is checked against it, and
`phase build --target=mps2-an385 --run` builds for an ARM Cortex-M3 board
and runs on QEMU. The compiler's diagnostics come to the editor through a
built-in language server. And the change promised in 1.2 lands, which is
why this is 2.0.

After the changes, 4,000 freshly fuzzed programs (1,710 accepted and run
in checked mode) produced no soundness violations. The fuzzer does not
generate device registers; those are covered by the analyzer, codegen and
end-to-end tests, including the run on QEMU.

### Breaking

- **Read-only data passed to a parameter with no access mode is an
  error** (it was a warning since 1.2). A read borrow or a `read`
  parameter may only go to `read` parameters; a parameter with neither
  `read` nor `write` may write. Fix: mark the parameter `read` if the
  function only reads it. Every program 1.4 accepted *without warnings*
  is still accepted. (Bug gallery #39.)
- `device` is now a keyword at the start of a top-level item, and a local
  cannot have the name of a declared device. `device`, `bit` and `bits`
  remain usable as names everywhere else.
- Internal crate APIs: `Item::Device`; `CallArg::DeviceRegister` and
  `PirExpr::DeviceRead`; `PirProgram::devices`; `TokenKind::DotDotEq`.

### Added

- **`device` register maps** (spec §3.7):

      device Uart0 @ 0x40004000 {
          DATA:  read write u32 @ 0x00;
          STATE: read u32 @ 0x04 { TXFULL: bit 0, RXFULL: bit 1 }
          CTRL:  read write u32 @ 0x08 = 0 { TX_EN: bit 0, BAUD: bits 4..=11 }
      }

  `volatile_read(Uart0.STATE.TXFULL)` reads a field;
  `volatile_write(Uart0.CTRL.TX_EN, 1)` writes one and keeps the other
  bits. Checked: the map (sizes, alignment, no overlapping registers or
  fields, reset values that fit, unique names) and every access (real
  register and field names, read only readable registers, write only
  writable ones, field writes only where the register can also be read,
  literals that fit). In C, a device is a struct of `volatile` members at
  the declared offsets (`_Static_assert`ed), at its real address on a
  hardware target and a simulated block, starting from the reset values,
  in a host build; `Pos`/`Msk` macros serve hand-written C. The
  `REG @MMIO` form still works.
- **`phase build --target=mps2-an385`** (spec §5.3): cross-compiles for the
  ARM MPS2 AN385 board (Cortex-M3) with `arm-none-eabi-gcc` into
  `build/<stem>.elf`; externs come from `runtime/hw/<stem>_extern.c` or
  `runtime/<stem>_extern.c`. **`--run`** runs the result: the host binary,
  or the .elf on `qemu-system-arm`, which now exits cleanly through
  semihosting when `main` returns. `PHASE_ARM_CC` and `PHASE_QEMU`
  override the tools. `scripts/build_hw_demo.sh` just runs this now.
- **`phase lsp`** (spec §6.8): a Language Server Protocol server built into
  the compiler. It publishes the same diagnostics as `phase check` — same
  underlines, notes as related information — on every change, and lists
  functions and devices as document symbols.
- **`editors/`**: a TextMate grammar in a VS Code extension that starts
  `phase lsp`, a Vim syntax file, and setup notes for Neovim, Helix and
  Emacs. CI checks the grammar's scopes against every example.
- Hex integer literals (`0x4000_4000`).
- `examples/uart_hello.phase`: a UART driver written entirely in PHASE;
  on QEMU's board model it prints "Hello from PHASE" from the real UART0
  data register.
- Bug gallery #34 `write_read_only_register`, #35
  `read_write_only_register`, #36 `field_write_on_write_only_register`,
  #37 `field_value_too_wide`, #38 `overlapping_registers`, #39
  `read_only_to_unannotated_param`, with CWE mappings.
- `runtime/hw/libc_min.c`: `memset`/`memcpy`/`memmove`/`memcmp`, which GCC
  may call even in freestanding code.

### Changed

- The error for a `volatile_read`/`volatile_write` whose first argument
  is not a register now names both forms (`Uart0.STATE`, `REG @MMIO`).
- Spec: QEMU's MPS2 model is described as a model of the board's
  peripherals and memory map, not as cycle-accurate (it isn't).

## [1.4.0] — 2026-10-13

Ring buffers: `rotate` for triple buffering and `split`/`join` for
circular DMA with half-transfer interrupts. And the evidence from 1.3 now
reaches the whole type system: the runtime checker verifies buffer sizes,
typestate and splits, and the fuzzer generates them — each rule family
validated by putting an analyzer bug back and watching the fuzzer catch
it (spec §6.7).

Every program 1.3 accepted is still accepted, unless it uses `rotate`,
`split`, `join` or `into` as a name: they are keywords now.

### Added

- **`rotate a, b, c;`** (spec §3.14): each name takes the next one's
  storage, with everything the checker knows about it; `swap` is the
  two-name case and keeps its messages. Same rules as `swap`.
- **`split buf into p0, p1, …;` and `join p0, p1, … into buf;`** (spec
  §3.14): equal, consecutive slices of a fixed-length owned local become
  separately tracked buffers in its domain; the whole is unusable until
  the same parts — each still holding its own slice, all in one domain —
  are joined back. Parts carry the block-end and exit obligations, and a
  split must agree across merges and be undone within a loop body.
- **Runtime checker** (`--checked`): every buffer argument is checked to
  really have the bytes the callee is told it has; typestate values are
  tracked (state from the callee's declared return state, never an
  annotation; consumption; a direct construction must be in the initial
  state); `rotate`, `split` and `join` are checked (slices in place, one
  domain, no whole-buffer use while split).
- **Fuzzer**: three buffer types with fixed and `_` lengths across 21
  extern operations and 10 helper parameter shapes; a typestate machine
  whose values are created, advanced, inspected and consumed; `rotate`,
  `split` (into 2 or 4, nested) and `join`. Deviations for rare rules are
  targeted (forged states then used as claimed, reuse of consumed values,
  joining right after unsettling a part). `phase-fuzz --print SEED` prints
  one generated program.
- `examples/triple_buffer.phase` and `examples/circular_dma.phase` (+
  runtimes), with end-to-end tests that check frame and block order in
  normal and checked builds.
- Bug gallery #30 `process_half_in_flight`, #31 `use_ring_while_split`,
  #32 `join_with_half_in_flight`, #33 `rotate_wrong_direction`, with CWE
  mappings.

### Fixed

- **A borrow of a typestate value passed to a by-value parameter** emitted
  the pointer instead of the value (`peek(r)` for `peek(Pkt p)`), so an
  accepted program failed to compile in C. It now passes `*r`.
  *(Found by the new runtime-checker tests.)*
- Two false alarms in the runtime checker's `split` support, both found
  by the fuzzer before release: a buffer, its first part and that part's
  own first part share one address, and a buffer split under a name
  `rotate` had moved ends under its declared name. Split records are now
  matched by name and call frame. Both programs are in
  `crates/fuzz/regressions/`.

### Changed

- Internal crate APIs: `Stmt` gains `Rotate`, `Split` and `Join`;
  `PirInst` gains `Rotate`, `Split`, `Join` and `TypestateInit`;
  `PirFnSig` gains `param_states` and `return_state`. Normal (unchecked)
  generated C is unchanged for every 1.3 program.

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
