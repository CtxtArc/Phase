#!/bin/sh
# PHASE against ST's own files for the STM32F401:
#
# 1. examples/devices/nucleo_f401re.phase against ST's description of the
#    chip: every register's address, every field's position and width,
#    and every IRQ number, compared with stm32f401xe.h (ST's CMSIS header)
#    by a C compiler.
# 2. examples/embedding/stm32f401: a vendor-style project -- ST's startup
#    file and CMSIS header, its own main.c -- with a driver written in
#    PHASE, built with its Makefile, and run on QEMU's STM32F4 board model
#    when qemu-system-arm is installed.
#
# Needs git, python3, make and arm-none-eabi-gcc with newlib; downloads
# the headers (pinned below) once into $PHASE_CMSIS_DIR (default
# ~/.cache/phase-cmsis).
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
cache=${PHASE_CMSIS_DIR:-$HOME/.cache/phase-cmsis}
st_rev=9192c7b9df75a142f2027ab266601fe061fc00b3    # STMicroelectronics/cmsis-device-f4
arm_rev=55b19837f5703e418ca37894d5745b1dc05e4c91   # ARM-software/CMSIS_5

fetch() { # url dir rev
    if [ ! -d "$2" ]; then
        git clone -q --filter=blob:none --no-checkout "$1" "$2"
    fi
    git -C "$2" checkout -q "$3"
}
mkdir -p "$cache"
fetch https://github.com/STMicroelectronics/cmsis-device-f4.git "$cache/cmsis-device-f4" "$st_rev"
fetch https://github.com/ARM-software/CMSIS_5.git "$cache/CMSIS_5" "$arm_rev"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
board="$root/examples/devices/nucleo_f401re.phase"
printf 'import "%s";\nfn main() { }\n' "$board" > "$work/board.phase"
"${PHASE:-$root/target/debug/phase}" emit-c --out-dir="$work" "$work/board.phase" > /dev/null
vendor="$cache/cmsis-device-f4/Include/stm32f401xe.h"
python3 "$root/scripts/check_against_cmsis.py" "$work/board.gen.h" "$work/check.c" "$vendor" "$board"
arm-none-eabi-gcc -mcpu=cortex-m4 -mthumb -std=c11 -fsyntax-only \
    -I"$work" -I"$cache/cmsis-device-f4/Include" -I"$cache/CMSIS_5/CMSIS/Core/Include" \
    "$work/check.c"
echo "OK: nucleo_f401re.phase matches stm32f401xe.h"

example="$root/examples/embedding/stm32f401"
make -s -C "$example" clean
make -s -C "$example" PHASE="${PHASE:-$root/target/debug/phase}" BUILD="$work/embed" \
    CMSIS_DEVICE="$cache/cmsis-device-f4" CMSIS_CORE="$cache/CMSIS_5/CMSIS/Core" > /dev/null
echo "OK: built $example with ST's startup file and headers"
if command -v qemu-system-arm > /dev/null; then
    # The program ends by sleeping; give it a few seconds, then compare.
    timeout 5 qemu-system-arm -M netduinoplus2 -nographic -monitor none \
        -serial null -serial "file:$work/console.txt" -kernel "$work/embed/app.elf" || true
    expected='vendor main, PHASE ticker
tick
tock
tick
tock
tick
done'
    got=$(tr -d '\r' < "$work/console.txt")
    if [ "$got" != "$expected" ]; then
        echo "error: the embedded example printed:"
        echo "$got"
        exit 1
    fi
    echo "OK: it runs on QEMU's STM32F4 model (netduinoplus2), TIM2 interrupts and all"
fi
