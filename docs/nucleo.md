# Running PHASE on the NUCLEO-F401RE

`--target=nucleo-f401re` builds a PHASE program for ST's NUCLEO-F401RE
board (STM32F401RE: Cortex-M4, 512 KB flash, 96 KB RAM, two 8-stream DMA
controllers). The board needs nothing but its USB cable: its ST-LINK
flashes it and passes its console (USART2) through as a serial port.

## What to send back

For each program: the console output, and anything that differs from what
is expected below. If one hangs, the last lines before it. That is the
evidence these programs still owe: 3.0 shipped on the NUCLEO-F103RB blink
program (last section), and these are not yet run on a board.

## What you need

- `arm-none-eabi-gcc` with newlib, as for the MPS2 targets.
- One way to flash, for `--run`: `probe-rs`, `openocd` or `st-flash`
  (the first one found is used). Without any, copy the `.bin` to the
  board's USB drive instead.
- Access to the serial port. On Arch Linux:

  ```sh
  sudo pacman -S arm-none-eabi-gcc arm-none-eabi-newlib openocd stlink
  sudo usermod -aG uucp $USER     # /dev/ttyACM0; log in again afterwards
  ```

  (`probe-rs`: `cargo install probe-rs-tools`, plus its udev rules.)

## Build and run

```sh
phase build --target=nucleo-f401re --run examples/nucleo_blink.phase
```

builds `build/nucleo_blink.elf` and `.bin`, flashes the board, and prints
its console until the program's `main` returns:

```
tick 1
tick 2
...
tick 16

[phase] main returned
```

with LD2, the green LED, blinking twice a second. The serial port is found
by name (`/dev/serial/by-id/*STLink*`, then `/dev/ttyACM0`); set
`PHASE_SERIAL=/dev/ttyACM1` to pick another, and `PHASE_RUN_TIMEOUT`
(seconds, default 30) for programs that run longer.

Without `--run`, flash it however you like:

- copy `build/nucleo_blink.bin` to the `NOD_F401RE` USB drive, or
- `probe-rs download --chip STM32F401RETx build/nucleo_blink.elf && probe-rs reset --chip STM32F401RETx`, or
- `openocd -f interface/stlink.cfg -f target/stm32f4x.cfg -c "program build/nucleo_blink.elf verify reset exit"`,

and watch the console with `picocom -b 115200 /dev/ttyACM0`. Press the
black reset button (B2) to run it again.

## The programs to try, in order

1. **`examples/nucleo_blink.phase`** — the board boots, its console works,
   TIM2 interrupts, `wait`/`signal`, and an `rc_w0` flag cleared the safe
   way. Expect the 16 ticks above.
2. **`examples/nucleo_dma_copy.phase`** — DMA2 copies blocks between two
   buffers on two streams; writing `address_of(..)` to the streams'
   address registers hands the buffers to the controller, its interrupts
   hand them back. Expect `block 0 in lo: ok` .. `block 7 in hi: ok`, then
   `ring: blocks 8 and 9`. A `TORN` line, or no output after the first
   blocks, is a finding.
3. **`examples/nucleo_adc_dma.phase`** — the 3.0 driver: ADC1 measures
   the chip's internal reference voltage, DMA2 stream 0 moves 64 samples
   at a time from the ADC's data register into a buffer, and `sync` takes
   the buffer back once the stream's own EN bit drops. Expect five lines
   like `round 1: VREFINT 1497 (1490..1503), factory value 1500, supply
   3306 mV: ok` — your numbers will differ a little. `IMPLAUSIBLE`, or a
   hang (`--run` gives up after 30 s), is a finding.
4. **`examples/embedding/stm32f401`** — a project built from ST's own
   startup file and headers, with a PHASE driver (its README says how).
   Expect `tick`/`tock` five times, then `done`.

## What has been checked without the board

- The board file (`examples/devices/nucleo_f401re.phase`) against ST's
  CMSIS header: every register address, field and IRQ number
  (`scripts/check_against_cmsis.sh`, run by CI).
- Programs 1 and 4 on QEMU's model of the STM32F405 (`netduinoplus2`):
  the same core and memory map, USART2 and TIM2 — real timer interrupts,
  the console, the vector table, the startup code. QEMU has no model of
  the F4's RCC, GPIO or DMA, so the LED and programs 2 and
  3 are the board's to show.
- Programs 1 to 3 as host builds, against simulated hardware, and in
  checked builds (`--checked`).

## What the board runs on

The chip's 16 MHz internal oscillator, as it starts: no PLL, every bus at
16 MHz. The runtime sets up USART2 (PA2/PA3, 115200 baud) before `main`
and prints the last line after it. With a debugger attached the program
then stops with a semihosting exit; without one, the core sleeps.

## The NUCLEO-F103RB (Cortex-M3)

`--target=nucleo-f103rb` builds for ST's NUCLEO-F103RB (STM32F103RB:
Cortex-M3, 128 KB flash, 20 KB RAM, one DMA controller). Same ST-LINK, same
virtual COM port (USART2 on PA2/PA3), same LD2 on PA5 as the F401RE, so
the steps above apply: build, flash with `--run`, read the console at
115200 baud.

```sh
phase build --target=nucleo-f103rb --run examples/nucleo_f103rb_blink.phase
```

Expect `tick 1` .. `tick 16` and LD2 blinking four times a second, as with
`nucleo_blink`. What differs from the F401RE:

- The chip starts on its 8 MHz internal oscillator, so every bus runs at
  8 MHz; the console's baud rate and TIM2's prescaler are set for that.
- The GPIO ports have no MODER/AFR registers; a pin's mode and
  configuration sit in four-bit fields of CRL/CRH, and port A's clock is
  `RCC.APB2ENR.IOPAEN`. `examples/devices/nucleo_f103rb.phase` declares
  RCC, GPIO, USART1-3, TIM2-4 and the NVIC, with ST's IRQ numbers.
- No DMA yet: the F103 has DMA1 only, with channels, not streams, so
  `nucleo_dma_copy` and `nucleo_adc_dma` need their own versions.
- Flashing uses `probe-rs --chip STM32F103RBTx` or OpenOCD's
  `target/stm32f1x.cfg`. A board is not the chip you think it is if the
  debugger reports a Cortex-M3 where you expected an M4: `phase` builds
  for the board you name, so name the one in front of you.

Verified on a real NUCLEO-F103RB: the blink program builds, flashes with
probe-rs, prints `tick 1` .. `tick 16` and `[phase] main returned` on the
ST-LINK's serial port, and blinks LD2 (the console, TIM2 interrupts and
the F1's GPIO configuration all work on the chip). If the board was
running another program, its last console lines can show before the
first `tick`.

Not yet done: the board file has not been compared with ST's CMSIS header
(`stm32f103xb.h`) the way the F401RE's is, and there is no DMA support for
this board.
