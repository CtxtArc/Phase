#!/usr/bin/env bash
# Builds the hardware examples for the ARM MPS2 AN385 (Cortex-M3) and AN505
# (Cortex-M33) boards and runs them on QEMU's models of those boards. This
# is a compiler feature -- `phase build --target=<board> --run` -- and this
# script just runs it for each example:
#
#   - examples/uart_hello.phase: a UART driver written in PHASE against a
#     declared register map (spec §3.7)
#   - examples/hw_uart_echo.phase: a buffer handed to the UART device and
#     synced, with the send loop in C (runtime/hw_uart_echo_extern.c)
#   - examples/timer_ticks.phase: five timer interrupts, each waking `wait`
#   - examples/dma_stream.phase (AN505): ten PL081 DMA transfers, each half
#     of the buffer handed to `main` by the transfer-complete interrupt
#
# Requires the GNU Arm Embedded toolchain (arm-none-eabi-gcc) and
# qemu-system-arm on PATH, e.g.
#   sudo apt-get install --no-install-recommends gcc-arm-none-eabi qemu-system-arm
#
# Run from the project root:
#   ./scripts/build_hw_demo.sh

set -euo pipefail
cd "$(dirname "$0")/.."

for tool in arm-none-eabi-gcc qemu-system-arm; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        echo "error: this demo needs arm-none-eabi-gcc and qemu-system-arm on PATH." >&2
        echo "  sudo apt-get install --no-install-recommends gcc-arm-none-eabi qemu-system-arm" >&2
        exit 1
    fi
done

cargo build -q --bin phase
for run in mps2-an385:uart_hello mps2-an385:hw_uart_echo mps2-an385:timer_ticks \
           mps2-an505:dma_stream; do
    board="${run%%:*}"
    example="${run#*:}"
    echo "== $example ($board) =="
    timeout 30 target/debug/phase build --target="$board" --run "examples/$example.phase"
    echo
done
