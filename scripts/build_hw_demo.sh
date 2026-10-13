#!/usr/bin/env bash
# Builds the hardware examples for the ARM MPS2 AN385 board (Cortex-M3) and
# runs them on QEMU's model of that board. Since v2.0 this is a compiler
# feature -- `phase build --target=mps2-an385 --run` -- and this script just
# runs it for both examples:
#
#   - examples/uart_hello.phase: a UART driver written in PHASE against a
#     declared register map (spec §3.7)
#   - examples/hw_uart_echo.phase: a buffer handed to the UART device and
#     synced, with the send loop in C (runtime/hw_uart_echo_extern.c)
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
for example in uart_hello hw_uart_echo; do
    echo "== $example =="
    timeout 30 target/debug/phase build --target=mps2-an385 --run "examples/$example.phase"
    echo
done
