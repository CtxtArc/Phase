# M6 demo: the bug volatile MMIO handling actually prevents

**Run it:** `./demos/m6_volatile_proof/run_demo.sh` (from the `phase/` project root)

## The claim

PHASE's compiler (spec bug-gallery item #7) guarantees that
`volatile_read`/`volatile_write` accesses on `@MMIO` registers are never
reordered or eliminated by optimization. `crates/driver/tests/mmio_volatile.rs`
already proves this as an automated `cargo test`. This demo exists to make
the *stakes* of that guarantee concrete and visible in about 10 seconds,
for someone who has never seen the codebase before.

## The setup

Two files implement the exact same logical sequence — read a status
register, write "reset" then "enable" to a control register, write the
status back out to a data register:

- **`naive_no_volatile.c`** — hand-written C, the way a driver author
  might write it if they forgot (or didn't know) that hardware registers
  need `volatile`. Nothing about this code looks wrong on a read-through.
- **PHASE's generated code** (`build/mmio_registers.gen.c`, from
  [`examples/mmio_registers.phase`](../../examples/mmio_registers.phase))
  — the same sequence, but every register PHASE knows is `@MMIO` compiles
  to a `volatile` global automatically. The programmer never has to
  remember to write `volatile` at all.

## The result

Both are compiled with `cc -O2 -S` — a real optimizing compiler, not a
toy. The demo greps the resulting assembly for stores to `UART_CONTROL`:

```
naive_no_volatile.c   (no volatile):  1 store survives   -- BUG
mmio_registers.phase  (PHASE):        2 stores survive   -- correct
```

The naive version's "reset the UART" write is silently deleted by dead
store elimination, since nothing reads `UART_CONTROL` before it's
overwritten by the very next line. On real hardware this means the UART
gets enabled *without ever being reset first* — a real, silent bug. The
C source that produces this is completely unremarkable; a code reviewer
has to already know to look for a missing `volatile` keyword on a global
declared somewhere else, possibly in a different file, to catch it.

PHASE's version doesn't have this failure mode available to it at all —
not because the compiler happened to get it right this time, but because
`@MMIO`-tagged registers are *always* lowered to `volatile` storage,
enforced by the C standard itself once that qualifier is applied.

## Why this matters beyond one UART example

This is the general shape of a real, well-documented bug class in
embedded and driver code: a register access that "looks fine" gets
silently optimized away or reordered because the compiler wasn't told the
memory has side effects. PHASE doesn't ask the programmer to remember
`volatile` (or to catch its absence in review) — the type system makes
"this is a hardware register" a fact the compiler already knows from the
`@MMIO` domain tag, and the code generator never gets a chance to forget it.
