#!/usr/bin/env bash
# M10: cross-compiles examples/hw_uart_echo.phase for a real ARM
# Cortex-M3 target (the MPS2 AN385 board) and runs the resulting binary
# on QEMU's hardware-accurate model of that board -- not the host
# machine, not a simulated address space. See phase_specification.md §9,
# Milestone M10, and runtime/hw/ for the hardware-specific runtime.
#
# Requires (both available via `apt-get install`, see the error message
# below if missing):
#   - arm-none-eabi-gcc  (package: gcc-arm-none-eabi)
#   - qemu-system-arm    (package: qemu-system-arm)
#
# Run from the phase/ project root:
#   ./scripts/build_hw_demo.sh

set -euo pipefail
cd "$(dirname "$0")/.."   # phase/ project root

if ! command -v arm-none-eabi-gcc >/dev/null 2>&1 || ! command -v qemu-system-arm >/dev/null 2>&1; then
    echo "error: this demo needs arm-none-eabi-gcc and qemu-system-arm." >&2
    echo "  sudo apt-get install --no-install-recommends gcc-arm-none-eabi qemu-system-arm" >&2
    exit 1
fi

echo "== phase build (generates build/hw_uart_echo.gen.{c,h}; the final host-cc link is expected to fail -- that's fine, we only need the generated C) =="
cargo build -q --bin phase
target/debug/phase build examples/hw_uart_echo.phase >/dev/null 2>&1 || true

if [[ ! -f build/hw_uart_echo.gen.c ]]; then
    echo "error: build/hw_uart_echo.gen.c wasn't generated -- phase build must have failed before writing it" >&2
    exit 1
fi

echo "== arm-none-eabi-gcc: cross-compiling for a real Cortex-M3 (MPS2 AN385) =="
arm-none-eabi-gcc \
    -mcpu=cortex-m3 -mthumb -ffreestanding -nostdlib -nostartfiles -O1 -std=c11 -Wall -Werror \
    -T runtime/hw/mps2an385.ld \
    -I build -I runtime -I runtime/hw \
    -o build/hw_uart_echo.elf \
    runtime/hw/startup.c \
    runtime/hw/phase_runtime_hw.c \
    build/hw_uart_echo.gen.c \
    runtime/hw_uart_echo_extern.c

echo "== qemu-system-arm: running on the real MPS2 AN385 hardware model =="
timeout 3 qemu-system-arm -M mps2-an385 -nographic -kernel build/hw_uart_echo.elf || true
echo
echo "== done: the line above (\"PHASE\") is real UART0 output from real ARM machine code =="
