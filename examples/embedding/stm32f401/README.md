# A PHASE driver in an ordinary STM32 project

This is how PHASE is meant to be used in a real firmware: as one checked
layer, not the whole program. The project here is laid out like any
STM32F401 project — ST's startup file and CMSIS header, a `main.c` written
against `USART2->SR` and `RCC->APB1ENR` — and its TIM2 driver,
`ticker.phase`, is written in PHASE.

- `ticker.phase` has no `main`. It exports `ticker_start`, `ticker_wait`
  and `ticker_stop`, and an interrupt handler for `TIM2_IRQn`, which PHASE
  emits as `TIM2_IRQHandler` — the weak symbol ST's startup file puts in
  the vector table — so linking it in is all it takes.
- The `Makefile` runs `phase emit-c` (check, then write C) and compiles the
  result with everything else, with `-DPHASE_TARGET_HW` (devices at their
  real addresses) and `-DPHASE_NO_SHORT_NAMES` (the generated header uses
  only `PHASE_`-prefixed names, so it can't collide with ST's).
- `main.c` includes both headers and calls the driver.

```sh
make PHASE=path/to/phase CMSIS_DEVICE=path/to/cmsis-device-f4 CMSIS_CORE=path/to/CMSIS_5/CMSIS/Core
```

then copy `build/app.bin` to the NUCLEO's USB drive. The console (the
ST-LINK's serial port, 115200 baud) prints `tick`/`tock` five times, ten
a second, then `done`. `scripts/check_against_cmsis.sh` downloads ST's and
ARM's headers, builds this project and runs it on QEMU's STM32F4 model.

What the C side promises the PHASE side, and the other way round, is
`docs/boundary.md`; how to set this up in another build system is
`docs/embedding.md`.
