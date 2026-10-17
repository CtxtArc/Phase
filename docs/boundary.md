# The C boundary: what PHASE checks, and what it trusts

PHASE is the layer of a firmware that decides who owns a buffer and when:
the CPU, a DMA engine or a device. It checks every hand-off it can see. The
computation itself — filling a buffer, checking it, decoding a packet —
stays in C, behind `extern fn`. Anything PHASE can't see, it has to trust,
and that trust is the boundary contract on this page: the promises the C
side must keep for PHASE's guarantees to hold. Every rule here is one
the compiler can't check; the ones it *can* check are errors, listed in
the specification (§6.8).

There are two directions: PHASE calling C (`extern fn`), and C calling
PHASE (a program embedded in someone else's project, docs/embedding.md).

## PHASE calling C: what an `extern fn` promises

```phase
extern fn produce(block: write buffer<u32, 16> @RAM);
extern fn check(h: read buffer<u32, 16> @RAM) -> bool;
extern fn start_capture(b: write buffer<u8, 512> @RAM -> @DMA);
```

PHASE generates the prototype (`void produce(uint32_t *block, size_t
block_len)`) and checks every call: the buffer is the CPU's, it isn't
borrowed in a way the mode conflicts with, it isn't passed twice. The C
body then promises:

1. **It touches only what it is given.** Only the buffers passed to it,
   only within their length (`block_len` elements), and only as the
   parameter says: a `read` parameter is a `const` pointer, so writing
   through it is a C compiler error; a `write` one may be read and
   written.
2. **It keeps nothing.** No copy of a buffer pointer survives the call —
   no global, no static, no queue — unless the parameter says the buffer
   goes to the hardware (rule 3). PHASE assumes that after an ordinary
   call returns, nothing but the program refers to the buffer.
3. **A domain change in the signature is true.** `b: write buffer<..>
   @RAM -> @DMA` says the call hands `b` to a DMA engine: when it returns,
   a transfer into `b` has been started, and `b` belongs to the hardware
   until `sync(b)` or `wait E for b`. A parameter without `->` promises
   the opposite: when the call returns, no hardware is reading or writing
   the buffer. (Since 2.4 a program can do the hand-off itself, by writing
   `address_of(b)` to an `address` register, and needs no C for it.)
4. **Typestate results are true.** `-> Packet<Decoded>` promises the
   value returned really is decoded.
5. **It doesn't race the interrupt handlers.** C that writes a device
   register a PHASE interrupt handler also writes does so with interrupts
   disabled — the rule PHASE enforces on its own field writes (§3.16).
   C must not write registers whose flags are `w1c` or `rc_w0` field by
   field (`SR &= ~FLAG` on a write-0-to-clear register clears flags set in
   between; bug gallery #46).
6. **It doesn't wait, and doesn't call back.** An extern does not call
   PHASE functions, and does not set PHASE's event flags (`phase_event_*`
   in the generated C): those are the program's own control flow, and
   PHASE's checks of `wait` and `signal` assume only PHASE code moves them.

A checked build (`phase build --checked`) verifies at run time what it can
at the boundary: each argument's domain and size against what the callee
is told, aliasing between arguments, and the domain change a signature
declares. It cannot see inside the C, so rules 1, 2, 5 and 6 stay
promises.

## C calling PHASE: what the caller promises

`phase emit-c` puts the program's own functions in the generated header
(not `main`, and not interrupt handlers, which only the hardware calls),
so a vendor project's C can call them. Inside the program, PHASE checked
every call to them; a call from C must keep the same promises:

1. **Buffers passed in are the CPU's**: not being transferred by any DMA
   engine, not borrowed elsewhere, and at least as long as the parameter's
   declared length (`buffer<u32, 16>`: 16 elements; the `_len` argument
   is that number).
2. **A buffer a function hands to the hardware stays untouched.** After
   calling `fn copy_lo(src: write buffer<u32, 16> @RAM -> @DMA, ..)`, the
   C must not touch `src` until the PHASE code that hands it back has run
   (`wait lo_done for src_lo, lo`). That is the same rule PHASE enforces
   on its own calls; from C, it is a promise.
3. **No two arguments overlap**, unless both are `read`.
4. **Not from an interrupt handler** unless the function is written for
   it: PHASE functions can `wait`, which never returns in handler code.

Checked builds run whole PHASE programs; a PHASE function called from C
isn't tracked by the run-time checker.

## The runtime

The generated C calls a few functions it doesn't define:

| Function | Who provides it | Promise |
|---|---|---|
| `sim_dma_wait`, `sim_device_wait` | `runtime/phase_runtime.c` (host), `runtime/hw/phase_runtime_hw.c` (boards) | `sync(b)` returns only when the hardware is done with `b` |
| `phase_addr_set`, `phase_addr_get` | `phase_runtime.h` (inline on a board), `phase_runtime.c` (host) | the register holds the buffer's address |
| `phase_sim_hardware` (optional) | a host build's extern file | plays the hardware between steps of the program |

On a board, `sync` waits on a stand-in — the console draining — not on
the DMA engine's own completion flag: PHASE does not yet know which engine
a buffer was handed to (docs/real_boards.md). Until it does, a program on
real hardware gets a buffer back with `wait E for b` on the engine's
interrupt, which is exact, and uses `sync` only where the wait is known
to cover the transfer.

## What PHASE does not cover at all

- Memory the program doesn't declare: a C global buffer handed to a DMA
  engine from C is invisible to PHASE.
- Caches. On a Cortex-M7 a DMA engine and the CPU can see different
  contents of the same buffer; the hand-offs are where cache maintenance
  belongs, and PHASE doesn't do it yet. The STM32F401 and the MPS2 boards
  have no data cache.
- Interrupt priorities and nesting: PHASE models one priority level.
