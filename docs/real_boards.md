# What PHASE needs to survive real boards

PHASE 2.3 runs on two QEMU boards: the MPS2 AN385 and the AN505, which has
a DMA controller. QEMU is a forgiving board. Its DMA finishes the instant
a channel starts, it has no caches or clocks to enable, it has a single
interrupt priority in practice, and no device ever fails. This is the
list of what has to get better before PHASE holds up on an STM32 Nucleo
or similar board: the plan for 3.0. Each item says what breaks today, and
roughly what would fix it.

Items marked **(v3 core)** are the ones a real driver can't do without.

## 1. Describing real hardware

- **Register maps from SVD files (v3 core).** A real MCU has hundreds
  of registers per peripheral family, and every vendor ships them as a
  CMSIS-SVD file. Writing board files by hand doesn't scale and invites
  typos. Needed: a `phase svd` command that turns an SVD file into a
  board file, so the declared map is the vendor's.
- **Arrays of registers and repeated blocks (v3 core).** An STM32F4 DMA
  controller has 8 streams with identical register sets, a GPIO port has
  16 identical pin fields, and the NVIC has 8 enable registers. Today each
  copy is declared separately: `examples/devices/mps2_an505.phase` spells
  out `C0CONTROL` and `C1CONTROL`. Needed: register arrays and clusters
  with a stride (`S[8] @ 0x10 + 0x18 * n { CR, NDTR, PAR, M0AR }`),
  indexed by a constant (`Dma2.S[0].CR`).
- **Bits with side effects (v3 core).** Real status registers have bits
  cleared by writing 1 (`w1c`), cleared by reading (`rc`), or cleared by
  writing 0 (`rc_w0`, the STM32 timers' `SR`). A read-modify-write on such
  a register silently clears every other pending flag. This is a classic
  lost-interrupt bug that PHASE can't see, because its maps only say
  read/write. Needed: these access kinds on fields, and an error for a
  field write that would clear the others. The same applies to registers
  whose read has a side effect (a data register that pops a FIFO).
- **Named field values.** `MODE: bits 0..=1` takes 0–3, but only some of
  those values mean something ("input", "output", "alternate", "analog").
  Needed: enumerated values on fields, and writes checked against them.
- **Clocks and power.** On an STM32 a peripheral's registers do nothing
  (or fault) until its clock is enabled in RCC. That is the most common
  first-day bug. Needed: a device can declare the enable it needs, and an
  access before it is an error. The typestate machinery already does this
  for values; a device would get a state.
- **Owning peripherals, pins and DMA streams.** Two drivers that both
  configure `USART2`, the same pin, or the same DMA stream conflict, and
  nothing catches it. Needed: devices (and pins, and channels) as owned
  resources, handed to the code that uses them like buffers are today.

## 2. Memory and DMA

- **Buffer addresses in PHASE (v3 core).** A DMA controller is started by
  writing a buffer's address into its address register. PHASE can't
  express that, so every example does it in a C extern: the most
  important step of the hand-off sits outside the checked code. Needed: a
  register field declared as an address, and `volatile_write(Dma2.S[0].M0AR,
  address_of(lo))` as the hand-off itself. `lo` would go to the
  hardware right there, and passing a buffer the CPU doesn't own would be
  an error.
- **One domain per engine (v3 core).** Today `@DMA` is one domain: two
  transfers on two streams look the same, and `sync(b)` doesn't know which
  engine to wait for. On real hardware it does, because the runtime's
  `sync` on the AN385 still polls the UART as a stand-in. Needed: domains
  parameterized by the engine (`@DMA(Dma2.S0)`), with `sync` and
  `wait E for b` tied to that engine's own completion flag.
- **Where a buffer lives (v3 core).** Not all memory is reachable by DMA.
  On an STM32F4 the CCM RAM isn't, and on an H7 the DTCM isn't reachable
  by DMA1/DMA2. A buffer on `main`'s stack lands wherever the stack is.
  Needed: memory regions in the board description, buffers placed in one
  (`buffer<u16, 256> adc @RAM in SRAM1`), and an error when a buffer goes
  to an engine that can't reach its region.
- **Caches (v3 core for Cortex-M7).** On an STM32F7/H7 the CPU sees stale
  cache lines after DMA writes, and DMA sees stale memory after CPU
  writes, unless the code cleans or invalidates the D-cache at each
  hand-off. PHASE's ownership model is exactly the information needed to
  do this: `move` to `@DMA` means *clean*, `sync` or `wait … for` means
  *invalidate*. Needed: emit the cache maintenance per target, and require
  buffers to be aligned to and sized in whole cache lines (32 bytes).
- **Alignment.** DMA transfers need aligned addresses for their width and
  burst size. Needed: alignment on buffers, checked against the transfer
  settings.
- **Buffers that outlive `main`'s frame.** Firmware keeps DMA buffers in
  static memory, both because `main` never returns and because large
  buffers overflow a small stack. PHASE has no static or global data.
  Needed: `static` buffers, owned by `main` as if declared there, placed in
  `.bss` (or a chosen region).
- **Circular DMA overruns.** In real circular mode the engine takes a half
  back by itself and keeps going. If `main` is still processing when the
  engine wraps, the half is overwritten mid-read. That's a timing bug the
  type system can't see. Needed: the overrun flag and a runtime check, or
  counting events (below) so a missed half is noticed rather than
  silently merged.
- **Peripheral-driven transfers.** The AN505's PL081 has no request lines
  wired in QEMU, so only memory-to-memory transfers were tested. ADC-to-
  memory and memory-to-UART transfers, paced by the peripheral, need a
  real board.

## 3. Interrupts and concurrency

- **Priorities and preemption (v3 core).** Real NVICs have priorities, and
  a higher-priority handler interrupts a lower one. PHASE assumes one
  level, so a field write in a low-priority handler that a high-priority
  one can split isn't caught. Needed: `@ irq N priority P`, and the
  lost-write rule applied between handlers of different priority.
- **Events that count.** An event is a flag: two half-transfers before a
  `wait` look like one, and a block is silently lost. Needed: counting
  events, or `wait` reporting missed signals, so overruns are visible.
- **Data shared with handlers.** Handlers have no parameters and there are
  no globals, so a UART receive handler has nowhere to put the byte.
  Needed: a checked shared structure between one handler and the code it
  interrupts, such as a single-producer/single-consumer queue or a value
  only accessed inside `critical`. Buffers owned by a handler would fit
  the same model.
- **Masking only some interrupts.** `critical` disables all of them
  (PRIMASK). Real code masks below a priority (BASEPRI) so urgent
  interrupts still run. Needed: `critical below P`.
- **Timeouts and errors.** A real `wait` needs a timeout (hardware hangs),
  and real DMA has transfer-error interrupts. After an error the buffer's
  state is unknown, which the ownership model must say something about.
  Needed: `wait E for b or timeout`, error events, and a defined owner
  after an abort.

## 4. The language itself

- **Bit operations and constants (v3 core).** Expressions have `+ - * /`
  and comparisons, but no `& | ^ << >> ~`, no `const`, and no enums. So
  register values can't be computed, and magic numbers like
  `volatile_write(Dma0.C0CONFIG, 0x8001)` end up in the code.
- **Arrays and indexing.** Buffers can only be handed around whole; PHASE
  code can't read `b[i]`, so all data processing lives in C. Needed:
  indexed access to CPU-owned buffers, bounds-checked, and `for` loops.
- **Modules.** Imports share one namespace; a vendor's register set plus a
  few drivers will collide. Needed: modules and qualified names.
- **Integer semantics.** Overflow, widths and conversions between `u8`,
  `u16` and `u32` need to be defined (and checked when a value meets a
  narrower register at run time, which checked builds do since 2.3).

## 5. Targets and tooling

- **A real target description (v3 core).** Each board today is hand-written
  C: startup, linker script and register header under `runtime/hw/`.
  Needed: a target file (CPU, memory regions, vector count, flash and RAM
  layout) and `--target=nucleo-f446re` built from it. Board support would
  then be data, not code.
- **Flashing and running on silicon (v3 core).** `--run` starts QEMU.
  Needed: flashing and running through a debug probe (probe-rs or
  OpenOCD), with the program's output over semihosting, RTT or the board's
  virtual COM port, and a hardware-in-the-loop test that CI can run when a
  board is attached.
- **Living next to vendor code.** Real projects keep the vendor's HAL or
  CMSIS headers for everything PHASE doesn't cover. The generated C
  defines device names as macros (`#define Uart0 …`) that can clash with
  CMSIS, and its handler names (`IRQ57_Handler`) aren't the vendor's
  (`DMA2_Stream0_IRQHandler`). Needed: prefixed generated names, and
  handlers that take the vendor's vector names.
- **Debugging PHASE, not the generated C.** `#line` directives in the
  generated C, so a debugger steps through `.phase` lines.
- **Size and stack.** Builds use `-O1` and put buffers on the stack.
  Needed: size-optimized builds, a stack-usage report, and a check that
  declared buffers fit the target's RAM.

## Order of work for 3.0

1. A target description and an STM32 Nucleo target that flashes and runs:
   nothing else can be validated without it.
2. SVD import, register arrays, and `w1c`/`rc` field semantics.
3. Bit operations and constants.
4. Buffer addresses in PHASE, domains per engine, and `sync` tied to the
   engine's real completion flag.
5. Memory regions, static buffers and alignment, plus cache maintenance
   if the board is a Cortex-M7.
6. Interrupt priorities and counting events.
7. Shared data with handlers, timeouts and error events.

Items 1–4 are enough to write a real ADC-to-memory DMA driver on a
Nucleo in PHASE without C externs for the hand-off. That is the
"survives a real board" bar for 3.0.
