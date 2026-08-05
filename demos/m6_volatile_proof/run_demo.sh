#!/usr/bin/env bash
# M6 demo: proves the bug PHASE's volatile MMIO handling prevents is real,
# by reproducing it in hand-written C and showing PHASE's generated code
# doesn't have it. Run from the phase/ project root:
#
#   ./demos/m6_volatile_proof/run_demo.sh

set -euo pipefail
cd "$(dirname "$0")/../.."   # phase/ project root

DEMO_DIR="demos/m6_volatile_proof"
BUILD_DIR="$DEMO_DIR/build"
mkdir -p "$BUILD_DIR"

echo "=============================================================="
echo " Step 1: build the naive hand-written version (no 'volatile')"
echo "=============================================================="
cat "$DEMO_DIR/naive_no_volatile.c"
echo
cc -std=c11 -O2 -S -o "$BUILD_DIR/naive.s" "$DEMO_DIR/naive_no_volatile.c"
echo "--- real -O2 assembly, UART_CONTROL only ---"
grep "UART_CONTROL(%rip)" "$BUILD_DIR/naive.s" || echo "(none found)"
NAIVE_COUNT=$(grep -c "UART_CONTROL(%rip)" "$BUILD_DIR/naive.s" || true)
echo "-> $NAIVE_COUNT store(s) to UART_CONTROL survived optimization."
echo "   The source has TWO writes (reset=1, then enable=2)."
echo "   On real hardware, the reset write silently never happens."
echo

echo "=============================================================="
echo " Step 2: build the PHASE-generated version of the same sequence"
echo "=============================================================="
echo "  (source: examples/mmio_registers.phase)"
cargo run -q --bin phase -- build examples/mmio_registers.phase
cc -std=c11 -O2 -S -I build -I runtime \
   -o "$BUILD_DIR/phase_version.s" build/mmio_registers.gen.c
echo "--- real -O2 assembly, UART_CONTROL only ---"
grep "UART_CONTROL(%rip)" "$BUILD_DIR/phase_version.s" || echo "(none found)"
PHASE_COUNT=$(grep -c "UART_CONTROL(%rip)" "$BUILD_DIR/phase_version.s" || true)
echo "-> $PHASE_COUNT store(s) to UART_CONTROL survived optimization."
echo

echo "=============================================================="
echo " Result"
echo "=============================================================="
if [ "$NAIVE_COUNT" -lt 2 ] && [ "$PHASE_COUNT" -ge 2 ]; then
  echo "CONFIRMED: the naive version lost a real hardware write to"
  echo "optimization ($NAIVE_COUNT/2 writes survived). PHASE's generated"
  echo "code, from the same logical sequence, keeps both ($PHASE_COUNT/2)"
  echo "because the register compiled to a 'volatile' global -- this is"
  echo "enforced by the C standard, not by convention or code review."
  exit 0
else
  echo "UNEXPECTED: naive=$NAIVE_COUNT phase=$PHASE_COUNT -- demo assumptions"
  echo "may not hold on this toolchain/architecture."
  exit 1
fi
