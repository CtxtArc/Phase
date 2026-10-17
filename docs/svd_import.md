# Board files from SVD

Every Cortex-M vendor describes its chips' registers in a CMSIS-SVD file
(an XML file per chip, usually in the vendor's "device family pack" and
collected in the [cmsis-svd-data](https://github.com/cmsis-svd/cmsis-svd-data)
repository). `phase import-svd` turns one into a PHASE board file:

```sh
phase import-svd STM32F401.svd > stm32f401.phase              # every peripheral
phase import-svd --only=RCC,GPIOA,USART2,DMA2,ADC1 STM32F401.svd > board.phase
```

The board file goes to stdout; what was left out goes to stderr as notes.

## What it keeps

- Each peripheral as a `device` at its base address, with its registers:
  offset, size, access (`read`, `write`, `read write`), reset value and
  fields.
- Peripherals that share a layout (`derivedFrom` in SVD) as a
  `device type` and its instances — when two or more of them are chosen.
- SVD arrays (`dim`) and clusters as register arrays. Registers a vendor
  wrote out one by one — `S0NDTR` .. `S7NDTR`, evenly spaced and laid out
  alike — become one array, `SxNDTR[8]`.
- Fields marked `modifiedWriteValues` `oneToClear` / `zeroToClear`, and
  `readAction` `clear`, as `w1c`, `rc_w0` and `rc`.
- A field that covers its whole register is dropped: it says nothing the
  register doesn't, and an `address` register must have no fields.
- Every interrupt, as `const NAME_IRQn: u32 = N;` — CMSIS's names, so an
  `interrupt fn h() @ irq NAME_IRQn` is also emitted as `NAME_IRQHandler`.

## What it can't know

- **Which registers hold DMA addresses.** Mark them `address`, and say
  when the device is done with the memory (`address until CR.EN == 0`),
  by hand: then `address_of(buf)` hands buffers over and `sync` waits on
  the device.
- **Which flags clear on write**, when the vendor left
  `modifiedWriteValues` out. ST does, for every status register; mark the
  `w1c` / `rc_w0` fields from the reference manual so PHASE can catch
  field writes that clear them.

## What it leaves out, and says so

- A second register at an offset another register already has (SVD's
  alternate views, like a timer's `CCMR1_Input` beside `CCMR1_Output`).
- Fields that share bits with another field, or are declared twice.
- Registers that aren't 8, 16 or 32 bits wide.
- Registers placed like an array whose descriptions differ. ST's SVD for
  the STM32F401 gives DMA stream 0's `S0CR` one field fewer than streams
  1 to 7, so `S0CR` .. `S7CR` stay eight registers.

## How it was checked

ST's own `STM32F401.svd` imports whole — 54 peripherals, 391 register
declarations — into a file `phase check` accepts. Every one of the 212
registers in `examples/devices/nucleo_f401re.phase` (itself checked against
ST's CMSIS header, `scripts/check_against_cmsis.sh`) is at the same
address in the import, and all 662 fields they share by name have the
same position and width.
