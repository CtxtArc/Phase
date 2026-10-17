# Using PHASE inside an existing project

PHASE does not have to own the build, the startup code or `main`. A PHASE
file can be a driver — the part of a firmware that hands buffers to DMA
engines, talks to registers and handles interrupts — compiled to C and
built with everything else. `examples/embedding/stm32f401` is a complete
example: an STM32 project built from ST's startup file and CMSIS header,
with its TIM2 driver in PHASE.

## 1. Generate the C

```sh
phase emit-c [--out-dir=DIR] driver.phase
```

checks the program exactly like `phase check`, then writes three files to
`DIR` (default: next to `driver.phase`):

| File | What it is |
|---|---|
| `driver.gen.h` | Register maps, the program's functions and its `extern fn` prototypes |
| `driver.gen.c` | The program |
| `phase_runtime.h` | The few runtime declarations the program uses |

Nothing else is needed from the PHASE tree. A program used this way
usually has no `main`: its functions are called from the project's C.

In a Makefile:

```make
build/driver.gen.c build/driver.gen.h: driver.phase
	phase emit-c --out-dir=build driver.phase
```

In CMake:

```cmake
add_custom_command(
  OUTPUT  ${CMAKE_BINARY_DIR}/driver.gen.c ${CMAKE_BINARY_DIR}/driver.gen.h
  COMMAND phase emit-c --out-dir=${CMAKE_BINARY_DIR} ${CMAKE_SOURCE_DIR}/driver.phase
  DEPENDS driver.phase)
target_sources(firmware PRIVATE ${CMAKE_BINARY_DIR}/driver.gen.c)
target_include_directories(firmware PRIVATE ${CMAKE_BINARY_DIR})
```

List the imported board file in `DEPENDS` as well.

## 2. Compile it with two defines

- **`PHASE_TARGET_HW`**: devices are at their real addresses. Without it,
  the generated C simulates them in memory (a host build).
- **`PHASE_NO_SHORT_NAMES`**: recommended next to a vendor header. The
  generated header names everything with a `PHASE_` prefix; without this
  define it also defines short aliases, which can collide with CMSIS
  (a device type named `USART` would define `USART_SR_TXE_Pos`, as
  `stm32f401xe.h` does).

| PHASE | Generated C | Short alias |
|---|---|---|
| `device Usart2: Stm32Usart @ ..` | `PHASE_DEV_Usart2->SR` | `Usart2->SR` |
| field `Stm32Usart.SR.TXE` | `PHASE_Stm32Usart_SR_TXE_Pos`, `_Msk` | `Stm32Usart_SR_TXE_Pos` |
| register struct | `phase_devtype_Stm32Usart_regs` | — |
| `event rx` | `phase_event_rx` (don't touch it from C) | — |

The program's own functions keep their PHASE names: they are its API.

## 3. Interrupt handlers

`interrupt fn h() @ irq 28` is emitted as `IRQ28_Handler`, which only
PHASE's own startup code (`--target=..` builds) uses. Give the IRQ as a
constant named the CMSIS way instead:

```phase
const TIM2_IRQn: u32 = 28;          // the board files declare these
interrupt fn on_tim2() @ irq TIM2_IRQn { .. }
```

and it is also emitted as `TIM2_IRQHandler`, the name every vendor startup
file gives that vector (as a weak symbol), so linking the generated C in
installs the handler. Don't define the same handler in C as well: the link
fails, which is the point.

## 4. Provide the runtime

The generated C calls, depending on what the program uses:

- `sim_dma_wait`, `sim_device_wait` — `sync`. Link
  `runtime/hw/phase_runtime_hw.c` (with the board's `-DPHASE_BOARD_..`),
  or write your own: return when the hardware is done with the buffer.
- `phase_addr_set`, `phase_addr_get` — `address_of`. Inline in
  `phase_runtime.h` under `PHASE_TARGET_HW`; nothing to link.

A program that only uses registers, interrupts and `wait` needs none of
them, like the example's.

## 5. Keep the boundary's promises

PHASE checks every hand-off inside the program. C calling into it, and C
behind its `extern fn`s, must keep the same rules by hand — docs/boundary.md
lists them. The short version: give PHASE functions buffers the CPU owns,
and don't touch a buffer a PHASE function handed to the hardware until
PHASE code has handed it back.

`--checked` builds are for running whole PHASE programs on the host; they
don't apply to C calling into PHASE.
